use super::reader::*;

#[test]
fn a_row_carries_the_revision_and_every_binding() {
    let p = Provenance {
        revision: Revision {
            head: "deadbeef".into(),
            dirty: false,
            diff_sha256: String::new(),
        },
        bound: vec![Bound {
            kind: "source-blob",
            path: "crates/cowfs-nfs/src/adapter.rs".into(),
            digest: "abc123".into(),
        }],
    };
    let row = Row {
        case: "fsync-parent-dir",
        rep: 1,
        pid: 4242,
        new_name_kept: true,
        old_name_back: false,
        new_digest: "f9fb".into(),
        fsck: "ok: 2 blocks".into(),
    };
    let j = row.to_json(&p);
    for k in [
        "deadbeef",
        "committed",
        "crates/cowfs-nfs/src/adapter.rs",
        "abc123",
        "fsync-parent-dir",
        "4242",
        "survived",
    ] {
        assert!(j.contains(k), "the row must carry {k}: {j}");
    }
}

#[test]
fn a_dirty_tree_is_labelled_as_code_under_test() {
    let clean = Revision {
        head: "a".into(),
        dirty: false,
        diff_sha256: String::new(),
    };
    let dirty = Revision {
        head: "a".into(),
        dirty: true,
        diff_sha256: "cafe".into(),
    };
    assert_eq!(clean.label(), "committed");
    assert_eq!(dirty.label(), "code-under-test (uncommitted)");
    assert!(dirty.to_json().contains("cafe"));
}

#[test]
fn a_lost_name_is_never_reported_as_survived() {
    let rows = vec![
        Row {
            case: "c",
            rep: 1,
            pid: 1,
            new_name_kept: true,
            old_name_back: false,
            new_digest: String::new(),
            fsck: String::new(),
        },
        Row {
            case: "c",
            rep: 2,
            pid: 2,
            new_name_kept: false,
            old_name_back: true,
            new_digest: String::new(),
            fsck: String::new(),
        },
    ];
    assert_eq!(verdict(&rows), "some-lost");
    assert_eq!(verdict(&rows[..1]), "all-survived");
    assert_eq!(verdict(&[]), "no-rows");
    assert!(rows[1]
        .to_json(&Provenance {
            revision: Revision {
                head: "h".into(),
                dirty: false,
                diff_sha256: String::new()
            },
            bound: vec![],
        })
        .contains("\"verdict\":\"lost\""));
}

#[test]
fn sha256_of_text_matches_the_system() {
    // "abc" has a published sha256, so a reader can check this without running anything.
    assert_eq!(
        sha256_of("abc").as_deref(),
        Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
}
