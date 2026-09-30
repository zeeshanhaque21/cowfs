use crate::sys;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The default control socket path for this user. See `docs/v1-control-api.md`.
pub fn default_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir.join("cowfs").join("control.sock"),
        _ => std::env::temp_dir()
            .join(format!("cowfs-{}", sys::current_uid()))
            .join("control.sock"),
    }
}

fn err(kind: io::ErrorKind, msg: String) -> io::Error {
    io::Error::new(kind, msg)
}

fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(md) => {
            if !md.is_dir() {
                return Err(err(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a directory", dir.display()),
                ));
            }
            if md.uid() != sys::current_uid() || md.mode() & 0o077 != 0 {
                return Err(err(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "{} must be owned by you and have mode 0700; put the socket in a private directory",
                        dir.display()
                    ),
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)
        }
        Err(e) => Err(e),
    }
}

/// Creates the private directory, takes the single-instance lock, removes a stale socket and binds.
///
/// Returns the listener and the lock file, which must be kept open for the server's lifetime.
pub(crate) fn bind(path: &Path) -> io::Result<(UnixListener, File)> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure_private_dir(dir)?;

    let mut lock_name: OsString = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_name)?;
    lock.try_lock().map_err(|e| match e {
        fs::TryLockError::WouldBlock => err(
            io::ErrorKind::AddrInUse,
            format!(
                "another cowfs server holds {}",
                Path::new(&lock_name).display()
            ),
        ),
        fs::TryLockError::Error(e) => e,
    })?;

    match fs::symlink_metadata(path) {
        Ok(md) => {
            if !md.file_type().is_socket() {
                return Err(err(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(err(
                        io::ErrorKind::AddrInUse,
                        format!("a live server is listening on {}", path.display()),
                    ))
                }
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok((listener, lock))
}
