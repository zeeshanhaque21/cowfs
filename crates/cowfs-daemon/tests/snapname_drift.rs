//! The snapshot-name rule is one rule, and the backend and the control API must not drift.
//!
//! Both crates used to carry their own copy of it, and they had already: the control API accepted a
//! snapshot name reserved for an interrupted snapshot swap and the backend refused it. The rule now
//! lives in `cowfs-snapname`; this is the regression that fails if a copy comes back.
//!
//! No aliasing pair is in the table on purpose: two names that fold onto one another are one name,
//! so the backend's answer would be `Exists` for a name the control API calls legal.

use cowfs_core::{name_key, Core, Options};

const RESERVED: &str = "conf_base.cowfs-swap0";

/// A real store, and the table of names both surfaces must judge the same way.
const TABLE: &[&str] = &[
    "",
    ".",
    "..",
    ".x",
    "._x",
    ".nfs1",
    "a",
    "a/b",
    "a\0b",
    "a\nb",
    "a\u{1b}b",
    "\u{85}",
    "caf\u{e9}",
    "slot-1",
    "conf_base",
    "conf_slot",
    "cowfs-swap",
    RESERVED,
    "conf_base.cowfs-swap1",
    "conf_base.cowfs-swap",
    "a.cowfs-swap0/repo",
];

#[test]
fn a_real_store_holds_exactly_the_names_the_control_api_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(
        dir.path(),
        Options {
            background: false,
            ..Default::default()
        },
    )
    .expect("a store opens");

    for name in TABLE {
        let api = cowfs_ctl::validate_snapshot_name(name);
        let store = core.create_snapshot(name);
        assert_eq!(
            api.is_ok(),
            store.is_ok(),
            "{name:?}: the control API says {}, the backend says {}",
            verdict(&api),
            verdict(&store),
        );
        if api.is_ok() {
            assert!(core.snapshot_view(name).is_ok(), "{name:?} did not open");
        }
    }

    // the reserved name is also unreachable through the other two entry points, and refusing it
    // leaves the snapshot it was derived from exactly as it was
    assert!(core.snapshot_view(RESERVED).is_err());
    assert!(core.rename_snapshot("conf_base", RESERVED).is_err());
    assert!(core
        .list_snapshots()
        .unwrap()
        .iter()
        .any(|e| e.name == "conf_base"));

    let mut held: Vec<_> = core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    held.sort();
    assert_eq!(
        held,
        [
            "a",
            "caf\u{e9}",
            "conf_base",
            "conf_slot",
            // legal: the marker needs its leading dot, so this is not a reserved name
            "cowfs-swap",
            "slot-1",
        ],
        "the store must hold every legal name in the table and nothing else",
    );
}

#[test]
fn the_control_api_and_the_backend_produce_the_same_collision_key() {
    let long = "\u{130}".repeat(126);
    let names: Vec<String> = TABLE
        .iter()
        .copied()
        .filter(|n| cowfs_ctl::validate_snapshot_name(n).is_ok())
        .map(str::to_owned)
        .chain(["x".repeat(255), long.clone(), format!("{long}a")])
        .collect();
    for name in names {
        assert_eq!(cowfs_ctl::name_key(&name), name_key(&name), "{name:?}");
        let key = name_key(&name);
        // neither side may hand out a key the other would refuse as a name
        assert!(
            key.len() <= cowfs_ctl::MAX_NAME_BYTES,
            "{} bytes",
            key.len()
        );
        assert!(
            cowfs_ctl::validate_snapshot_name(&key).is_ok(),
            "the key of {name:?} is not a legal name: {key:?}"
        );
    }
}

#[test]
fn a_staging_name_is_refused_by_the_control_api_before_it_reaches_the_backend() {
    // the exact name `Core`'s swap derives for target `conf_base`, so this is the real collision
    assert!(cowfs_core::validate_snapshot_name(RESERVED).is_err());
    let err = cowfs_ctl::validate_snapshot_name(RESERVED).expect_err("must be refused");
    assert_eq!(err.code, cowfs_ctl::ErrorCode::InvalidParams, "{err}");
    assert!(
        err.message.contains("reserved"),
        "the reason must name the reservation: {err}"
    );
}

#[test]
fn bytes_from_a_wire_go_through_the_same_rule() {
    for name in ["a", "conf_base", RESERVED, "a/b", ".x", "a\0b"] {
        assert_eq!(
            cowfs_core::validate_snapshot_name_bytes(name.as_bytes()).is_ok(),
            cowfs_ctl::validate_snapshot_name(name).is_ok(),
            "{name:?}"
        );
    }
    assert_eq!(
        cowfs_core::validate_snapshot_name_bytes(&[0xff, 0xfe]).unwrap_err(),
        cowfs_core::ControlError::InvalidName("not valid UTF-8"),
    );
}

fn verdict<T, E: std::fmt::Debug>(r: &Result<T, E>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(_) => "refused",
    }
}
