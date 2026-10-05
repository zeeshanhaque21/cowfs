//! Durable warm-base provenance: which repository, ref and commit a base snapshot was built from.
//!
//! `base_refresh` publishes a warm base by promoting a snapshot and recording where it came from.
//! Returning that record in the response is not publication: a caller that reconnects, or a daemon
//! that restarts, must be able to find the base again and be told the same commit. So the record is
//! written here, on disk, in the store the backend already owns, and it is read back the same way on
//! both backends.
//!
//! Where it lives: one directory per base under `<store>/.cowfs-base-meta/<name>/`, holding
//! `base.json`. It is outside every snapshot subtree, so no snapshot's contents, hash or Merkle root
//! change because of it and a clone never copies it. The metadata root begins with a dot, which is not
//! a valid snapshot name, so `snapshot list` never returns it and it cannot collide with a snapshot.
//! It is, however, a real directory inside the store: on the path backend a client that lists the mount
//! root sees `.cowfs-base-meta` and can read through it. That is deliberate and not a secret: the
//! contents are a repository path, a git ref and a commit, all of which the working tree already shows,
//! and the store is single-user with no encryption.
//!
//! What it is not: a second source of truth about the tree. The commit is provenance about a build,
//! not a claim about the bytes; `base status` compares it with the repository's current commit, and a
//! base whose provenance is missing reports itself stale rather than fresh.

use cowfs_ctl::BaseMeta;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The directory holding one subdirectory per base, inside the store.
const DIR: &str = ".cowfs-base-meta";
/// The file inside a base's directory.
const FILE: &str = "base.json";

/// Counts temporary files within one process, so two threads publishing the same base never choose the
/// same temporary path. The store's own lock already serialises publication; this is what makes the
/// name unique rather than merely unlikely to collide.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The on-disk shape. Every field is optional so a store written by an older build, or a file that
/// lost a field, loads as "provenance unknown" rather than as a claim.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
struct Record {
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    commit: Option<String>,
    /// The base is promoted even if its provenance is gone, which is what "no fields but the name"
    /// means. Recorded explicitly so a half-written record is distinguishable from an old one.
    #[serde(default)]
    promoted: bool,
}

impl From<&BaseMeta> for Record {
    fn from(m: &BaseMeta) -> Record {
        Record {
            repo: m.repo.clone(),
            git_ref: m.git_ref.clone(),
            commit: m.commit.clone(),
            promoted: true,
        }
    }
}

impl From<Record> for BaseMeta {
    fn from(r: Record) -> BaseMeta {
        BaseMeta {
            repo: r.repo,
            git_ref: r.git_ref,
            commit: r.commit,
        }
    }
}

/// Rejects a name that could reach outside the metadata root.
///
/// This is deliberately weaker than `validate_snapshot_name`, which is what a *base* name must
/// satisfy: `import` also removes and renames the private staging snapshots it creates, and those are
/// named `.cowfs-import-<name>`, which is not a valid snapshot name. Both are single path components
/// under the same root, which is all this has to guarantee.
fn check_private_name(name: &str) -> io::Result<()> {
    let bad = |why: &'static str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
    if name.is_empty() {
        return Err(bad("an empty snapshot name"));
    }
    if name == "." || name == ".." {
        return Err(bad("a snapshot name of . or .."));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(bad("a snapshot name must be one path component"));
    }
    Ok(())
}

/// The durable record of every base in one store, loaded when the backend opens and written on
/// publication.
///
/// `records` is also the publication lock. It is held from reading the map through publishing the file
/// and updating the map, so two threads cannot interleave a read, a write and a cache update and leave
/// the map disagreeing with the store.
#[derive(Debug)]
pub(crate) struct BaseMetaStore {
    root: PathBuf,
    records: std::sync::Arc<std::sync::Mutex<BTreeMap<String, Record>>>,
}

impl Clone for BaseMetaStore {
    fn clone(&self) -> BaseMetaStore {
        BaseMetaStore {
            root: self.root.clone(),
            records: std::sync::Arc::clone(&self.records),
        }
    }
}

impl BaseMetaStore {
    /// Opens the store's record, reading what is already there.
    ///
    /// An unreadable file is a load error, not a silent empty store: a store whose provenance cannot
    /// be read would report every base as unknown, and treating that as "no bases" would let a
    /// refresh publish a second base under a name that already has one.
    pub(crate) fn open(store: &Path) -> io::Result<Self> {
        let root = store.join(DIR);
        let mut records = BTreeMap::new();
        if root.is_dir() {
            for entry in std::fs::read_dir(&root)? {
                let dir = entry?;
                // `DirEntry::file_type` does not follow a symlink, so a link is skipped rather than
                // read through: nothing outside the store can become a record this way.
                if !dir.file_type()?.is_dir() {
                    continue;
                }
                let file = dir.path().join(FILE);
                if !file.is_file() {
                    continue;
                }
                let text = std::fs::read_to_string(&file).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("cannot read the base record {}: {e}", file.display()),
                    )
                })?;
                let record: Record = serde_json::from_str(&text).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} is not a base record: {e}", file.display()),
                    )
                })?;
                records.insert(dir.file_name().to_string_lossy().into_owned(), record);
            }
        }
        Ok(BaseMetaStore {
            root,
            records: std::sync::Arc::new(std::sync::Mutex::new(records)),
        })
    }

    /// The provenance of `name`, or `None` when the store holds no record of it.
    pub(crate) fn get(&self, name: &str) -> Option<BaseMeta> {
        let records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records.get(name).cloned().map(BaseMeta::from)
    }

    /// Records `meta` for `name`, durably, before this returns.
    pub(crate) fn set(&self, name: &str, meta: &BaseMeta) -> io::Result<()> {
        cowfs_ctl::validate_snapshot_name(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_locked(&self.root, &mut records, name, &Record::from(meta))
    }

    /// Records that `name` is a base whose provenance is not known, which is what promoting a
    /// snapshot without provenance means.
    pub(crate) fn promote(&self, name: &str) -> io::Result<()> {
        self.set(
            name,
            &BaseMeta {
                repo: None,
                git_ref: None,
                commit: None,
            },
        )
    }

    /// Forgets `name`, durably, before this returns.
    ///
    /// The record is dropped from the map only when the store agrees it is gone. A deletion that fails
    /// leaves the map holding what is still on disk, so a live process and a process that reopens the
    /// store answer the same question the same way. A deletion that fails *after* the record file is
    /// already unlinked is the other case: the durable truth is then "no record", so the map is
    /// dropped to match it. Either way the failure is reported and never silent.
    pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
        check_private_name(name)?;
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        remove_locked(&self.root, &mut records, name)
    }

    /// Moves `from`'s record to `to`, so a rename does not lose the provenance.
    ///
    /// The destination is written and fsynced first, so the record is never absent: if the destination
    /// cannot be written nothing is removed and the source is exactly as it was. If the source then
    /// cannot be removed, both records exist and the map says so, which is what a reopened process
    /// would read. A destination that already holds a different record is refused rather than
    /// overwritten, because replacing it would destroy a record this call did not write.
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        check_private_name(from)?;
        cowfs_ctl::validate_snapshot_name(to)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(record) = records.get(from).cloned() else {
            return Ok(());
        };
        if let Some(existing) = records.get(to) {
            if *existing != record {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{to:?} already has a different base record"),
                ));
            }
        }
        write_locked(&self.root, &mut records, to, &record)?;
        match remove_locked(&self.root, &mut records, from) {
            Ok(()) => Ok(()),
            Err(e) => Err(io::Error::new(
                e.kind(),
                format!("{to:?} now holds the record but {from:?} could not be removed: {e}"),
            )),
        }
    }
}

fn record_file(root: &Path, name: &str) -> PathBuf {
    root.join(name).join(FILE)
}

/// Refuses a metadata directory that is a symlink, so a record cannot be written or read through a
/// link that leaves the store.
fn no_symlinked_dir(dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is a symbolic link", dir.display()),
        )),
        _ => Ok(()),
    }
}

/// Writes `record` for `name` and publishes it atomically, then updates `records`.
///
/// Written to a temporary file and renamed, so a reader never sees a partial record and a crash leaves
/// either the old record or the new one. The file is fsynced and so is its directory, so the record
/// survives a power loss rather than only a process exit.
fn write_locked(
    root: &Path,
    records: &mut BTreeMap<String, Record>,
    name: &str,
    record: &Record,
) -> io::Result<()> {
    let dir = root.join(name);
    no_symlinked_dir(&dir)?;
    std::fs::create_dir_all(&dir).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("cannot create the record directory {dir:?}: {e}"),
        )
    })?;
    let final_path = dir.join(FILE);
    let body = serde_json::to_vec_pretty(record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // A name no other call in this process can pick, and `create_new` so that even a process-wide
    // counter that wrapped cannot truncate a temporary file another call is writing.
    let (tmp, mut file) = loop {
        let candidate = dir.join(format!(
            "{FILE}.tmp{}.{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => break (candidate, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot create a temporary record in {dir:?}: {e}"),
                ))
            }
        }
    };

    let written = (|| -> io::Result<()> {
        std::io::Write::write_all(&mut file, &body)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &final_path)?;
        // The directory entry, not the file: without this the rename can be lost even though the
        // file's contents were fsynced.
        std::fs::File::open(&dir)?.sync_all()
    })();
    if let Err(e) = written {
        // The temporary file is this call's own and inside this base's own directory. If removing even
        // that fails, say so rather than leaving a stray file for the next reader to trip over.
        return Err(match std::fs::remove_file(&tmp) {
            Ok(()) => e,
            Err(cleanup) => io::Error::new(
                e.kind(),
                format!("{e}; and the temporary record {tmp:?} could not be removed: {cleanup}"),
            ),
        });
    }
    records.insert(name.to_owned(), record.clone());
    Ok(())
}

/// Deletes `name`'s record and updates `records` only to match what the store now holds.
fn remove_locked(
    root: &Path,
    records: &mut BTreeMap<String, Record>,
    name: &str,
) -> io::Result<()> {
    let dir = root.join(name);
    match std::fs::remove_dir_all(&dir) {
        // Nothing to remove is the state being asked for, not a failure to reach it.
        Ok(()) => {
            records.remove(name);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            records.remove(name);
            Ok(())
        }
        Err(e) => {
            // The map must agree with the store either way. `remove_dir_all` unlinks the record
            // before removing its directory, so a failure can leave either state: if the file is still
            // there the record is intact and stays in the map, and if it is gone the store has no
            // record and the map must stop claiming one. A reopened process reads exactly that file,
            // so this is what keeps one answer rather than two.
            if !record_file(root, name).exists() {
                records.remove(name);
            }
            Err(io::Error::new(
                e.kind(),
                format!("cannot remove the base record {}: {e}", dir.display()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn meta(repo: &str, git_ref: &str, commit: &str) -> BaseMeta {
        BaseMeta {
            repo: Some(repo.to_owned()),
            git_ref: Some(git_ref.to_owned()),
            commit: Some(commit.to_owned()),
        }
    }

    fn store_in(dir: &Path) -> BaseMetaStore {
        BaseMetaStore::open(dir).unwrap()
    }

    /// What a process that opens this store fresh would see, which is the view a restart reports.
    fn reopened(dir: &Path) -> BaseMetaStore {
        store_in(dir)
    }

    fn commit_of(store: &BaseMetaStore, name: &str) -> Option<String> {
        store.get(name).and_then(|m| m.commit)
    }

    /// Where a base's record is on disk, from the store's own path.
    fn record_in(store: &Path, name: &str) -> PathBuf {
        store.join(DIR).join(name).join(FILE)
    }

    #[test]
    fn a_record_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        drop(store);

        let reopened = reopened(dir.path());
        assert_eq!(commit_of(&reopened, "warm").as_deref(), Some("abc"));
        assert_eq!(
            reopened.get("warm").unwrap().git_ref.as_deref(),
            Some("main")
        );
    }

    #[test]
    fn an_unwritten_base_is_unknown_rather_than_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        assert_eq!(commit_of(&store, "never-published"), None);
    }

    #[test]
    fn an_old_record_without_provenance_is_a_base_with_unknown_fields() {
        // A store written before provenance existed: the name is known, the fields are not.
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().join(DIR).join("warm");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::write(base_dir.join(FILE), r#"{"promoted":true}"#).unwrap();
        let store = store_in(dir.path());
        assert_eq!(
            store.get("warm"),
            Some(BaseMeta {
                repo: None,
                git_ref: None,
                commit: None
            }),
            "missing fields read as unknown, not as a claim"
        );
    }

    #[test]
    fn an_unreadable_record_is_an_error_rather_than_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().join(DIR).join("warm");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::write(base_dir.join(FILE), b"this is not json").unwrap();
        assert_eq!(
            BaseMetaStore::open(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn the_record_lives_outside_every_snapshot_tree() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        // The path a snapshot tree is cloned from must not contain the record, or a clone would
        // carry it and its hash would change.
        assert!(!dir.path().join("warm").join(DIR).exists());
        // And the record's own directory is not a valid snapshot name, so listing cannot return it.
        assert!(cowfs_ctl::validate_snapshot_name(DIR).is_err());
    }

    #[test]
    fn an_invalid_snapshot_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        assert!(store.set("../escape", &meta("/r", "main", "abc")).is_err());
        assert!(store.set("", &meta("/r", "main", "abc")).is_err());
        // `set` is for a base, so it also refuses a name no base could ever have.
        assert!(store
            .set(".cowfs-import-warm", &meta("/r", "main", "abc"))
            .is_err());
    }

    #[test]
    fn removing_a_base_forgets_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        store.remove("warm").unwrap();
        assert_eq!(commit_of(&store, "warm"), None);
        assert_eq!(commit_of(&reopened(dir.path()), "warm"), None);
    }

    #[test]
    fn remove_refuses_a_name_that_could_leave_the_metadata_root() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        for name in ["..", ".", "../escape", "a/b", ""] {
            assert!(
                store.remove(name).is_err(),
                "{name:?} must be refused by remove"
            );
        }
    }

    /// P-1: a record that cannot be unlinked must not be reported as deleted.
    ///
    /// Before the fix `remove` dropped the in-memory entry and discarded the error, so the live
    /// process answered "no warm base" while a process reopening the byte-identical store answered
    /// with the recorded commit. Both answers were true of a different store.
    #[test]
    fn a_removal_that_cannot_unlink_the_record_reports_failure_and_keeps_one_answer() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();

        // Unlinking the record needs write permission on the directory holding it, so the base's own
        // directory is what has to be made read-only. Nothing is removed at all in this case.
        let record_dir = dir.path().join(DIR).join("warm");
        let mode = |m: u32| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&record_dir, mode(0o500)).unwrap();
        let refused = std::fs::remove_dir_all(&record_dir).is_err();
        std::fs::set_permissions(&record_dir, mode(0o700)).unwrap();
        if !refused {
            panic!("the record could be removed, so the failure was never injected");
        }
        std::fs::set_permissions(&record_dir, mode(0o500)).unwrap();

        let e = store.remove("warm").unwrap_err();
        assert!(
            e.to_string().contains("cannot remove the base record"),
            "the failure says what did not happen: {e}"
        );
        assert!(
            record_in(dir.path(), "warm").is_file(),
            "the record survived"
        );
        // The record is still on disk, so the live view still has it, and a process reopening the same
        // store answers the same question the same way.
        assert_eq!(commit_of(&store, "warm").as_deref(), Some("abc"));
        assert_eq!(
            commit_of(&store, "warm"),
            commit_of(&reopened(dir.path()), "warm")
        );

        std::fs::set_permissions(&record_dir, mode(0o700)).unwrap();
        assert!(
            record_in(dir.path(), "warm").is_file(),
            "and it is still there"
        );
    }

    /// P-1, second branch: a deletion that unlinks the record and then cannot remove its directory
    /// must leave the map agreeing with the store, which no longer has the record.
    #[test]
    fn a_removal_that_lost_the_record_does_not_keep_claiming_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        assert!(
            record_in(dir.path(), "warm").is_file(),
            "the record is there to begin with"
        );

        // A removal that cannot finish after the record is already gone is the other partial state.
        // Built directly rather than waited for: the record file is removed here, and the directory
        // that holds it cannot be removed either, so the operation has to report a failure while the
        // store has no record left.
        std::fs::remove_file(record_in(dir.path(), "warm")).unwrap();
        let metadata_root = dir.path().join(DIR);
        let mode = |m: u32| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&metadata_root, mode(0o500)).unwrap();

        let e = store.remove("warm").unwrap_err();
        assert!(
            e.to_string().contains("cannot remove the base record"),
            "{e}"
        );
        assert!(
            !record_in(dir.path(), "warm").exists(),
            "the record really is gone from the store"
        );
        assert_eq!(
            commit_of(&store, "warm"),
            commit_of(&reopened(dir.path()), "warm"),
            "the map must not keep a record the store has lost"
        );
        assert_eq!(
            commit_of(&store, "warm"),
            None,
            "and it never invents a fresh one"
        );
        std::fs::set_permissions(&metadata_root, mode(0o700)).unwrap();
    }

    /// P-2: a rename that cannot write the destination must not lose the record.
    #[test]
    fn a_rename_that_cannot_write_the_destination_keeps_the_source_byte_identical() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        let before = std::fs::read(record_in(dir.path(), "warm")).unwrap();

        // The destination directory cannot be created, because its parent cannot be written.
        let mode = |m: u32| std::fs::Permissions::from_mode(m);
        let metadata_root = dir.path().join(DIR);
        std::fs::set_permissions(&metadata_root, mode(0o500)).unwrap();
        let refused = std::fs::create_dir(metadata_root.join("warmer")).is_err();
        std::fs::set_permissions(&metadata_root, mode(0o700)).unwrap();
        if !refused {
            panic!("the destination could be created, so the failure was never injected");
        }
        std::fs::set_permissions(&metadata_root, mode(0o500)).unwrap();

        let e = store.rename("warm", "warmer").unwrap_err();
        assert!(
            e.to_string().contains("warmer"),
            "the refusal names the record it could not write: {e}"
        );

        assert_eq!(
            std::fs::read(record_in(dir.path(), "warm")).unwrap(),
            before,
            "the source record is byte-identical"
        );
        assert_eq!(commit_of(&store, "warm").as_deref(), Some("abc"));
        assert_eq!(
            commit_of(&reopened(dir.path()), "warm").as_deref(),
            Some("abc")
        );
        assert!(
            !dir.path().join(DIR).join("warmer").exists(),
            "no destination record was left behind"
        );

        std::fs::set_permissions(&metadata_root, mode(0o700)).unwrap();
    }

    #[test]
    fn a_rename_moves_the_record_and_leaves_one_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        store.rename("warm", "warmer").unwrap();
        assert_eq!(commit_of(&store, "warm"), None);
        assert_eq!(commit_of(&store, "warmer").as_deref(), Some("abc"));
        assert!(!record_in(dir.path(), "warm").exists());
        assert!(record_in(dir.path(), "warmer").is_file());
        assert_eq!(
            commit_of(&reopened(dir.path()), "warmer").as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn a_rename_never_overwrites_a_different_destination_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.set("warm", &meta("/r", "main", "aaa")).unwrap();
        store.set("warmer", &meta("/r", "main", "bbb")).unwrap();
        assert_eq!(
            store.rename("warm", "warmer").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(commit_of(&store, "warm").as_deref(), Some("aaa"));
        assert_eq!(commit_of(&store, "warmer").as_deref(), Some("bbb"));
        assert_eq!(
            commit_of(&reopened(dir.path()), "warm").as_deref(),
            Some("aaa")
        );
        assert_eq!(
            commit_of(&reopened(dir.path()), "warmer").as_deref(),
            Some("bbb")
        );
    }

    /// P-3: two threads publishing one base must never publish a partial or mixed record.
    #[test]
    fn concurrent_writes_to_one_base_publish_only_complete_records() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(store_in(dir.path()));
        store.set("warm", &meta("/r", "main", "seed")).unwrap();

        // A repository path long enough that the write is not instantaneous, so an unsynchronised
        // publisher has a real window to be caught in.
        let padding = "p".repeat(48 * 1024);
        let commits: Vec<String> = (0..8).map(|i| format!("{i:040x}")).collect();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        let reader = {
            let (store, stop, observed) = (store.clone(), stop.clone(), observed.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Some(m) = store.get("warm") {
                        observed.lock().unwrap().push(m.commit.unwrap_or_default());
                    }
                }
            })
        };

        let writers: Vec<_> = (0..8)
            .map(|t| {
                let (store, commits, padding) = (store.clone(), commits.clone(), padding.clone());
                std::thread::spawn(move || {
                    for round in 0..25 {
                        let n = round * 8 + t;
                        store
                            .set(
                                "warm",
                                &meta(
                                    &format!("{padding}/{n}"),
                                    "main",
                                    &commits[n % commits.len()],
                                ),
                            )
                            .unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();

        let mut allowed_commits = commits.clone();
        allowed_commits.push("seed".to_owned());
        let allowed: std::collections::BTreeSet<&String> = allowed_commits.iter().collect();
        for seen in observed.lock().unwrap().iter() {
            assert!(
                allowed.contains(seen),
                "a published record mixed two publications: {seen:?}"
            );
        }
        // The record that survives is one whole publication, and it survives a reopen.
        let final_commit = commit_of(&store, "warm").unwrap();
        assert!(allowed.contains(&final_commit), "{final_commit}");
        assert_eq!(
            commit_of(&reopened(dir.path()), "warm").unwrap(),
            final_commit
        );
        // No temporary file was left behind for a later reader to trip over.
        let strays: Vec<_> = std::fs::read_dir(dir.path().join(DIR).join("warm"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != FILE)
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");
    }

    #[test]
    fn concurrent_writes_to_different_bases_do_not_interfere() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(store_in(dir.path()));
        let names: Vec<String> = (0..8).map(|i| format!("base-{i}")).collect();
        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let (store, name) = (store.clone(), name.clone());
                std::thread::spawn(move || {
                    for round in 0..25 {
                        store
                            .set(&name, &meta("/r", "main", &format!("{name}-{round}")))
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let reopened = reopened(dir.path());
        for name in &names {
            let commit = commit_of(&store, name).unwrap();
            assert_eq!(
                commit,
                commit_of(&reopened, name).unwrap(),
                "{name} did not survive a reopen"
            );
            let on_disk = std::fs::read_to_string(record_in(dir.path(), name)).unwrap();
            assert!(
                on_disk.contains(&commit),
                "{name}'s file does not hold its commit"
            );
        }
    }
}
