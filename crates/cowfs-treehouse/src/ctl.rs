use cowfs_ctl::{
    default_socket_path, BaseMeta, BaseRefreshReport, Client, ClientOptions, ImportReport,
    MountInfo, ProcessInfo, Request, Response, SnapshotInfo, Status,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::naming::from_client_error;

/// A connection to the cowfs daemon, with the companion's calls on it. Every method is one control
/// request; nothing here interprets a result beyond turning a mismatched `kind` into an error.
#[derive(Debug)]
pub struct Daemon {
    client: Client,
    socket: PathBuf,
}

impl Daemon {
    /// Connects to the daemon, honouring `--socket` and `--timeout` like the `cowfs` CLI.
    pub fn connect(socket: Option<&Path>, timeout: Option<u64>) -> Result<Daemon> {
        let path = match socket {
            Some(p) => p.to_path_buf(),
            None => default_socket_path(),
        };
        let mut opts = ClientOptions::default();
        if let Some(secs) = timeout {
            opts.idle_timeout = Duration::from_secs(secs);
        }
        let client = Client::connect_with(&path, opts).map_err(|e| from_client_error(&e))?;
        Ok(Daemon {
            client,
            socket: path,
        })
    }

    /// The socket this daemon is on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn take<T>(
        &mut self,
        request: Request,
        want: &str,
        f: impl FnOnce(Response) -> Option<T>,
    ) -> Result<T> {
        let response = self
            .client
            .call(request)
            .map_err(|e| from_client_error(&e))?;
        let kind = response.kind().to_owned();
        f(response).ok_or_else(|| {
            Error::Cowfs(format!(
                "the daemon answered {kind} where {want} was expected"
            ))
        })
    }

    /// `mount_info`.
    pub fn mount_info(&mut self) -> Result<MountInfo> {
        self.take(
            Request::MountInfo(Default::default()),
            "mount_info",
            |r| match r {
                Response::MountInfo(m) => Some(m),
                _ => None,
            },
        )
    }

    /// `status`.
    pub fn status(&mut self) -> Result<Status> {
        self.take(Request::Status(Default::default()), "status", |r| match r {
            Response::Status(s) => Some(s),
            _ => None,
        })
    }

    /// `snapshot_list`.
    pub fn snapshot_list(&mut self) -> Result<Vec<SnapshotInfo>> {
        self.take(
            Request::SnapshotList(Default::default()),
            "snapshot_list",
            |r| match r {
                Response::SnapshotList(l) => Some(l.snapshots),
                _ => None,
            },
        )
    }

    /// `snapshot_create`.
    pub fn snapshot_create(&mut self, name: &str, from: Option<&str>) -> Result<SnapshotInfo> {
        let params = cowfs_ctl::SnapshotCreate {
            name: name.to_owned(),
            from: from.map(str::to_owned),
        };
        self.take(Request::SnapshotCreate(params), "snapshot", |r| match r {
            Response::Snapshot(s) => Some(s),
            _ => None,
        })
    }

    /// `snapshot_rm`.
    pub fn snapshot_rm(&mut self, name: &str, expect_no_holders: bool) -> Result<()> {
        let params = cowfs_ctl::SnapshotRm {
            name: name.to_owned(),
            expect_no_holders,
        };
        self.take(Request::SnapshotRm(params), "ok", |r| match r {
            Response::Ok(_) => Some(()),
            _ => None,
        })
    }

    /// `snapshot_reset`, the atomic swap a slot create and a slot return both need.
    pub fn snapshot_reset(
        &mut self,
        name: &str,
        from: &str,
        expect_no_holders: bool,
    ) -> Result<SnapshotInfo> {
        let params = cowfs_ctl::SnapshotReset {
            name: name.to_owned(),
            from: from.to_owned(),
            expect_no_holders,
        };
        self.take(Request::SnapshotReset(params), "snapshot", |r| match r {
            Response::Snapshot(s) => Some(s),
            _ => None,
        })
    }

    /// `snapshot_create` unless the name is already taken, which is what makes a repeated wrapper
    /// run converge instead of failing on the second attempt.
    pub fn ensure_snapshot(&mut self, name: &str, from: Option<&str>) -> Result<SnapshotInfo> {
        if let Some(existing) = self.snapshot_list()?.into_iter().find(|s| s.name == name) {
            return Ok(existing);
        }
        self.snapshot_create(name, from)
    }

    /// `snapshot_promote`.
    pub fn snapshot_promote(&mut self, name: &str) -> Result<SnapshotInfo> {
        let params = cowfs_ctl::SnapshotName {
            name: name.to_owned(),
        };
        self.take(Request::SnapshotPromote(params), "snapshot", |r| match r {
            Response::Snapshot(s) => Some(s),
            _ => None,
        })
    }

    /// `base_refresh`.
    pub fn base_refresh(
        &mut self,
        repo: &Path,
        git_ref: &str,
        name: Option<&str>,
    ) -> Result<BaseRefreshReport> {
        let params = cowfs_ctl::BaseRefreshParams {
            // Canonical, because `base.repo` is an identity: a later lookup by the physical path
            // must find it even when the caller passed a symlinked one.
            repo: crate::naming::canonical(repo).display().to_string(),
            git_ref: git_ref.to_owned(),
            name: name.map(str::to_owned),
        };
        self.take(Request::BaseRefresh(params), "base_refresh", |r| match r {
            Response::BaseRefresh(b) => Some(b),
            _ => None,
        })
    }

    /// `ps`, which is what makes issue #20 answerable: it reports cwd, open-file and lock holds.
    pub fn ps(&mut self, snapshot: &str) -> Result<Vec<ProcessInfo>> {
        let params = cowfs_ctl::PsParams {
            snapshot: snapshot.to_owned(),
        };
        self.take(Request::Ps(params), "processes", |r| match r {
            Response::Processes(p) => Some(p.processes),
            _ => None,
        })
    }

    /// `import`.
    pub fn import(&mut self, path: &Path, name: &str) -> Result<ImportReport> {
        let params = cowfs_ctl::ImportParams {
            path: path.display().to_string(),
            name: name.to_owned(),
        };
        self.take(Request::Import(params), "import", |r| match r {
            Response::Import(i) => Some(i),
            _ => None,
        })
    }

    /// The warm base of `repo`, found the way the control API documents: by `base.repo`.
    pub fn find_base(&mut self, repo: &Path) -> Result<Option<SnapshotInfo>> {
        let wanted = crate::naming::canonical(repo).display().to_string();
        Ok(self.snapshot_list()?.into_iter().find(|s| {
            s.base
                .as_ref()
                .and_then(|b: &BaseMeta| b.repo.as_deref())
                .is_some_and(|r| r == wanted)
        }))
    }
}
