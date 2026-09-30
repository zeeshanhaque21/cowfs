use cowfs_ctl::{ProgressEvent, Response, SnapshotInfo, Unit};
use std::fmt::Write as _;
use std::io::{self, Write};

const BAR_WIDTH: u64 = 20;

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn count(n: u64, unit: Unit) -> String {
    match unit {
        Unit::Bytes => bytes(n),
        Unit::Items | Unit::Other => n.to_string(),
    }
}

/// Formats unix milliseconds as `YYYY-MM-DD HH:MM:SSZ`.
pub fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let rem = secs % 86_400;
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn snapshot_line(s: &SnapshotInfo) -> String {
    let mut line = s.name.clone();
    if let Some(parent) = &s.parent {
        let _ = write!(line, " (clone of {parent})");
    }
    if let Some(base) = &s.base {
        line.push_str(" [base");
        if let Some(repo) = &base.repo {
            let _ = write!(line, " {repo}");
        }
        if let Some(git_ref) = &base.git_ref {
            let _ = write!(line, " @ {git_ref}");
        }
        if let Some(commit) = &base.commit {
            let _ = write!(line, " {commit}");
        }
        line.push(']');
    }
    line
}

/// The human-readable form of a response.
pub fn human(response: &Response) -> String {
    match response {
        Response::Pong(_) => "pong".into(),
        Response::Version(v) => format!(
            "protocol {}\nserver   {}\nctl      {}",
            v.protocol, v.server, v.ctl
        ),
        Response::Status(s) => format!(
            "store:      {}\nmount:      {}\nsnapshots:  {}\nblocks:     {}\nlogical:    {}\nstored:     {}\nuptime:     {}s",
            s.store_path,
            s.mount_path,
            s.snapshot_count,
            s.block_count,
            bytes(s.logical_bytes),
            bytes(s.stored_bytes),
            s.uptime_secs
        ),
        Response::SnapshotList(l) if l.snapshots.is_empty() => "no snapshots".into(),
        Response::SnapshotList(l) => l
            .snapshots
            .iter()
            .map(|s| format!("{}  {}", utc(s.created_unix_ms), snapshot_line(s)))
            .collect::<Vec<_>>()
            .join("\n"),
        Response::Snapshot(s) => snapshot_line(s),
        Response::Ok(_) => "ok".into(),
        Response::Gc(g) if g.dry_run => format!(
            "dry run: {} blocks ({}) would be freed",
            g.candidate_blocks,
            bytes(g.candidate_bytes)
        ),
        Response::Gc(g) => format!("freed {} blocks ({})", g.freed_blocks, bytes(g.freed_bytes)),
        Response::Fsck(f) => {
            let mut out = format!(
                "{}: {} blocks ({}), {} snapshots checked",
                if f.ok { "ok" } else { "PROBLEMS FOUND" },
                f.blocks_checked,
                bytes(f.bytes_checked),
                f.snapshots_checked
            );
            for p in &f.problems {
                let _ = write!(out, "\n  {}: {}", p.kind, p.detail);
            }
            out
        }
        Response::Import(i) => format!(
            "imported {} files ({}) into {}; {}",
            i.files,
            bytes(i.bytes),
            i.name,
            if i.verified {
                "verified by hash"
            } else {
                "NOT verified"
            }
        ),
        Response::BaseRefresh(r) => {
            let mut out = snapshot_line(&r.snapshot);
            if let Some(prev) = &r.previous_commit {
                let _ = write!(out, "\nreplaced commit {prev}");
            }
            out
        }
        Response::Processes(p) if p.processes.is_empty() => "no processes".into(),
        Response::Processes(p) => p
            .processes
            .iter()
            .map(|proc| {
                let holds: Vec<String> = proc
                    .holds
                    .iter()
                    .map(|h| format!("{:?} {}", h.kind, h.path).to_lowercase())
                    .collect();
                format!("{}  {}  [{}]", proc.pid, proc.command, holds.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Response::MountInfo(m) => format!(
            "mount:    {}\nadapter:  {}\nmounted:  {}",
            m.mount_path, m.adapter, m.mounted
        ),
    }
}

/// One line describing a progress event.
pub fn progress_line(e: &ProgressEvent) -> String {
    match e.total {
        Some(total) => format!(
            "{} {}/{}",
            e.phase,
            count(e.done, e.unit),
            count(total, e.unit)
        ),
        None => format!("{} {}", e.phase, count(e.done, e.unit)),
    }
}

/// A progress line with a bar when the total is known.
pub fn progress_bar(e: &ProgressEvent) -> String {
    match e.total {
        Some(total) if total > 0 => {
            let filled = (e.done.min(total) * BAR_WIDTH / total) as usize;
            let empty = BAR_WIDTH as usize - filled;
            format!(
                "[{}{}] {}",
                "#".repeat(filled),
                "-".repeat(empty),
                progress_line(e)
            )
        }
        _ => progress_line(e),
    }
}

/// Progress on stderr: an updating bar on a terminal, one line per phase otherwise.
#[derive(Debug)]
pub struct Progress {
    tty: bool,
    phase: Option<String>,
    drawn: bool,
}

impl Progress {
    /// `tty` selects the updating bar.
    pub fn new(tty: bool) -> Self {
        Progress {
            tty,
            phase: None,
            drawn: false,
        }
    }

    /// Shows one event.
    pub fn update(&mut self, e: &ProgressEvent) {
        let mut err = io::stderr().lock();
        if self.tty {
            let _ = write!(err, "\r\x1b[2K{}", progress_bar(e));
            self.drawn = true;
        } else if self.phase.as_deref() != Some(e.phase.as_str()) {
            let _ = writeln!(err, "{}", progress_line(e));
        }
        self.phase = Some(e.phase.clone());
    }

    /// Clears the bar line on a terminal.
    pub fn finish(&mut self) {
        if self.drawn {
            let _ = write!(io::stderr(), "\r\x1b[2K");
            self.drawn = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(done: u64, total: Option<u64>, unit: Unit) -> ProgressEvent {
        ProgressEvent {
            phase: "mark".into(),
            done,
            total,
            unit,
            message: None,
        }
    }

    #[test]
    fn utc_formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc(1_700_000_000_000), "2023-11-14 22:13:20Z");
        assert_eq!(utc(1_709_164_800_000), "2024-02-29 00:00:00Z");
    }

    #[test]
    fn bytes_are_humanised() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(bytes(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn progress_bar_scales_and_clamps() {
        let half = progress_bar(&event(5, Some(10), Unit::Items));
        assert_eq!(half, "[##########----------] mark 5/10");
        let over = progress_bar(&event(50, Some(10), Unit::Items));
        assert!(over.starts_with("[####################]"));
        assert_eq!(progress_bar(&event(3, None, Unit::Items)), "mark 3");
        assert_eq!(progress_bar(&event(3, Some(0), Unit::Items)), "mark 3/0");
        assert_eq!(
            progress_line(&event(2048, Some(4096), Unit::Bytes)),
            "mark 2.0 KiB/4.0 KiB"
        );
    }
}
