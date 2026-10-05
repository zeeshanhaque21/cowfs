//! Receipts that a reader can bind to a commit, a source tree and a binary.
//!
//! The independent review was right that a row carrying only `case, rep, pid, new_name, old_name,
//! fsck` cannot be tied to anything: the commit binding lived in prose, so the file alone proved
//! nothing. Every row written here carries the revision it was produced at, the git blob of every
//! source file the outcome depends on, the sha256 of the binary that produced it, and the verdict.
//!
//! Nothing here invents a commit. The revision is whatever `git rev-parse HEAD` says at the moment of
//! the run, and when the working tree is dirty that is labelled `code-under-test` rather than
//! presented as the committed source, because the two are different things and only one of them is
//! on GitHub.

#![allow(dead_code)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// How long any single `git` or `shasum` call may take.
const PROV_BUDGET: Duration = Duration::from_secs(15);

fn run(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// The revision a run happened at, and whether the tree it ran was the committed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub head: String,
    /// True when the working tree has uncommitted changes, in which case `head` does not describe
    /// the code that ran and must not be read as if it did.
    pub dirty: bool,
    /// sha256 of `git diff HEAD` plus `git diff --cached HEAD`, empty when clean.
    pub diff_sha256: String,
}

impl Revision {
    /// What to call this revision in a receipt: the committed one, or the code as edited.
    pub fn label(&self) -> &'static str {
        if self.dirty {
            "code-under-test (uncommitted)"
        } else {
            "committed"
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"head\":{:?},\"state\":{:?},\"diff_sha256\":{:?}}}",
            self.head,
            self.label(),
            self.diff_sha256
        )
    }
}

/// The revision of `repo`, read from git itself rather than assumed.
pub fn revision(repo: &Path) -> Revision {
    let head = run("git", &["rev-parse", "HEAD"], repo).unwrap_or_else(|| "unknown".into());
    let status = run("git", &["status", "--porcelain"], repo).unwrap_or_default();
    let dirty = !status.is_empty();
    let diff = if dirty {
        let unstaged = run("git", &["diff", "HEAD"], repo).unwrap_or_default();
        let untracked =
            run("git", &["ls-files", "--others", "--exclude-standard"], repo).unwrap_or_default();
        sha256_of(&format!("{unstaged}\n--untracked--\n{untracked}")).unwrap_or_default()
    } else {
        String::new()
    };
    Revision {
        head,
        dirty,
        diff_sha256: diff,
    }
}

/// The git blob id of `rel` at `HEAD`, which is what binds a receipt to a source file.
pub fn blob_of(repo: &Path, rel: &str) -> Option<String> {
    run("git", &["rev-parse", &format!("HEAD:{rel}")], repo)
}

/// The sha256 of a file's bytes on disk, for a build artifact that git does not track.
pub fn sha256_of_file(path: &Path) -> Option<String> {
    sha256_of(&std::fs::read_to_string(path).ok()?)
}

/// sha256 via whichever hashing tool the host has, which avoids a dependency for a test.
///
/// `shasum` is macOS and Perl; `sha256sum` is coreutils and is what a Linux runner has. Hardcoding
/// either absolute path means the receipts silently degrade to `"unknown"` on the other platform.
pub fn sha256_of(text: &str) -> Option<String> {
    use std::io::Write as _;
    let (program, args): (&str, &[&str]) = if Path::new("/usr/bin/shasum").exists() {
        ("/usr/bin/shasum", &["-a", "256"])
    } else {
        ("sha256sum", &["-"])
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(text.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout);
    first.split_whitespace().next().map(str::to_string)
}

/// One thing a receipt depends on, with the digest that proves which one.
#[derive(Clone, Debug)]
pub struct Bound {
    pub kind: &'static str,
    pub path: String,
    pub digest: String,
}

impl Bound {
    fn to_json(&self) -> String {
        format!(
            "{{\"kind\":{:?},\"path\":{:?},\"digest\":{:?}}}",
            self.kind, self.path, self.digest
        )
    }
}

/// Everything a set of rows was produced against.
#[derive(Clone, Debug)]
pub struct Provenance {
    pub revision: Revision,
    pub bound: Vec<Bound>,
}

impl Provenance {
    /// Binds the harness sources, by git blob, and the fixture binaries, by sha256.
    pub fn collect(repo: &Path, sources: &[&str], binaries: &[PathBuf]) -> Provenance {
        let mut bound = Vec::new();
        for rel in sources {
            let digest = blob_of(repo, rel).unwrap_or_else(|| "unknown".into());
            bound.push(Bound {
                kind: "source-blob",
                path: (*rel).to_string(),
                digest,
            });
        }
        for bin in binaries {
            let digest = sha256_of_file(bin).unwrap_or_else(|| "unknown".into());
            bound.push(Bound {
                kind: "binary-sha256",
                path: bin.display().to_string(),
                digest,
            });
        }
        Provenance {
            revision: revision(repo),
            bound,
        }
    }

    /// The manifest, as one JSON object, written once per attempt next to its rows.
    pub fn manifest(&self, attempt: &str, rows: &[Row]) -> String {
        let b: Vec<String> = self.bound.iter().map(Bound::to_json).collect();
        format!(
            "{{\"attempt\":{:?},\"revision\":{},\"bound\":[{}],\"rows\":{},\"verdict\":{:?}}}",
            attempt,
            self.revision.to_json(),
            b.join(","),
            rows.len(),
            verdict(rows),
        )
    }
}

/// One measured rep, with the provenance repeated on the row so a reader of the rows alone is not
/// relying on the manifest having been shipped alongside.
pub struct Row {
    pub case: &'static str,
    pub rep: usize,
    pub pid: u32,
    pub new_name_kept: bool,
    pub old_name_back: bool,
    pub new_digest: String,
    pub fsck: String,
}

impl Row {
    pub fn to_json(&self, p: &Provenance) -> String {
        let b: Vec<String> = p.bound.iter().map(Bound::to_json).collect();
        format!(
            "{{\"attempt_source\":\"{}\",\"revision\":{},\"bound\":[{}],\"case\":{:?},\"rep\":{},\
             \"pid\":{},\"new_name\":{},\"old_name\":{},\"new_digest\":{:?},\"fsck\":{:?},\
             \"verdict\":{:?}}}",
            p.revision.label(),
            p.revision.head,
            b.join(","),
            self.case,
            self.rep,
            self.pid,
            self.new_name_kept,
            self.old_name_back,
            self.new_digest,
            self.fsck.trim(),
            if self.new_name_kept && !self.old_name_back {
                "survived"
            } else {
                "lost"
            },
        )
    }
}

/// The overall verdict for a set of rows: every row kept the name and none brought the old one back.
pub fn verdict(rows: &[Row]) -> &'static str {
    if rows.is_empty() {
        return "no-rows";
    }
    if rows.iter().all(|r| r.new_name_kept && !r.old_name_back) {
        "all-survived"
    } else {
        "some-lost"
    }
}

/// Writes `rows-<attempt>.jsonl` and `manifest-<attempt>.json` under `dir`, flushed per line so a run
/// that dies keeps what it already proved.
pub fn write_attempt(
    dir: &Path,
    attempt: &str,
    p: &Provenance,
    rows: &[Row],
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut r = std::io::BufWriter::new(std::fs::File::create(
        dir.join(format!("rows-{attempt}.jsonl")),
    )?);
    for row in rows {
        writeln!(r, "{}", row.to_json(p))?;
        r.flush()?;
    }
    let mut m = std::io::BufWriter::new(std::fs::File::create(
        dir.join(format!("manifest-{attempt}.json")),
    )?);
    writeln!(m, "{}", p.manifest(attempt, rows))?;
    m.flush()?;
    Ok(())
}
