//! Durable warm-base provenance: which repository, ref and commit a base snapshot was built from.
//!
//! `base_refresh` publishes a warm base by promoting a snapshot and recording where it came from.
//! Returning that record in the response is not publication: a caller that reconnects, or a daemon
//! that restarts, must be able to find the base again and be told the same commit. So the record is
//! written here, on disk, in the store the backend already owns, and it is read back the same way
//! on both backends.
//!
//! Where it lives: one file per base, `<store>/.cowfs-base-meta/<name>.json`, outside every snapshot
//! tree, so no snapshot's contents, hash or Merkle root change because of it, and a snapshot clone
//! never copies it. A name that starts with a dot is not a valid snapshot name, so the file is
//! invisible to snapshot listing and to the mount root on both backends.
//!
//! What it is not: a second source of truth about the tree. The commit is provenance about a build,
//! not a claim about the bytes; `base status` compares it with the repository's current commit, and a
//! base whose provenance is missing reports itself stale rather than fresh.

use cowfs_ctl::BaseMeta;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The directory holding one JSON file per base, inside the store.
const DIR: &str = ".cowfs-base-meta";
/// The file inside a base's directory.
const FILE: &str = "base.json";

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

/// The durable record of every base in one store, loaded when the backend opens and written on
/// publication.
///
/// `Clone` shares one set of records, the way a backend's snapshot namespace is shared between the
/// backend and its handle.
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
    ///
    /// Written to a temporary file and renamed, so a reader never sees a partial record and a crash
    /// leaves either the old record or the new one. The file is fsynced and so is its directory, so
    /// the record survives a power loss rather than only a process exit.
    pub(crate) fn set(&self, name: &str, meta: &BaseMeta) -> io::Result<()> {
        cowfs_ctl::validate_snapshot_name(name)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir)?;
        let final_path = dir.join(FILE);
        let tmp = dir.join(format!("{FILE}.tmp{}", std::process::id()));
        let body = serde_json::to_vec_pretty(&Record::from(meta))
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        {
            let mut f = std::fs::File::create(&tmp)?;
            std::io::Write::write_all(&mut f, &body)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &final_path)?;
        // The directory entry, not the file: without this the rename can be lost even though the
        // file's contents were fsynced.
        std::fs::File::open(&dir)?.sync_all()?;
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_owned(), Record::from(meta));
        Ok(())
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

    /// Forgets `name`, used when a snapshot is removed.
    pub(crate) fn remove(&self, name: &str) {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
        let _ = std::fs::remove_dir_all(self.root.join(name));
    }

    /// Moves `from`'s record to `to`, so a rename does not lose the provenance.
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let record = self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(from)
            .cloned();
        self.remove(from);
        if let Some(record) = record {
            let meta = BaseMeta::from(record);
            self.set(to, &meta)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(repo: &str, git_ref: &str, commit: &str) -> BaseMeta {
        BaseMeta {
            repo: Some(repo.to_owned()),
            git_ref: Some(git_ref.to_owned()),
            commit: Some(commit.to_owned()),
        }
    }

    #[test]
    fn a_record_survives_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        let written = BaseMetaStore::open(store).unwrap();
        written.set("warm", &meta("/r", "main", "abc")).unwrap();
        drop(written);

        let reopened = BaseMetaStore::open(store).unwrap();
        assert_eq!(reopened.get("warm"), Some(meta("/r", "main", "abc")));
        assert!(reopened.get("warm").is_some());
    }

    #[test]
    fn an_unwritten_base_is_unknown_rather_than_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaseMetaStore::open(dir.path()).unwrap();
        assert_eq!(store.get("never-published"), None);
        assert!(store.get("never-published").is_none());
    }

    #[test]
    fn an_old_record_without_provenance_is_a_base_with_unknown_fields() {
        // A store written before provenance existed: the name is known, the fields are not.
        let dir = tempfile::tempdir().unwrap();
        let base_dir = dir.path().join(DIR).join("warm");
        std::fs::create_dir_all(&base_dir).unwrap();
        std::fs::write(base_dir.join(FILE), r#"{"promoted":true}"#).unwrap();
        let store = BaseMetaStore::open(dir.path()).unwrap();
        assert!(store.get("warm").is_some(), "still a base");
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
        let e = BaseMetaStore::open(dir.path()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
    }

    #[test]
    fn the_record_lives_outside_every_snapshot_tree() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaseMetaStore::open(dir.path()).unwrap();
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
        let store = BaseMetaStore::open(dir.path()).unwrap();
        assert!(store.set("../escape", &meta("/r", "main", "abc")).is_err());
        assert!(store.set("", &meta("/r", "main", "abc")).is_err());
    }

    #[test]
    fn removing_a_base_forgets_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = BaseMetaStore::open(dir.path()).unwrap();
        store.set("warm", &meta("/r", "main", "abc")).unwrap();
        store.remove("warm");
        assert_eq!(store.get("warm"), None);
        assert!(store.get("warm").is_none());
        let reopened = BaseMetaStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.get("warm"),
            None,
            "and it stays gone across a reopen"
        );
    }
}
