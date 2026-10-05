//! The snapshot-name rule is one rule, and the backend and the control API must not drift.
//!
//! Both crates used to carry their own copy of it, and they had already: the control API accepted a
//! snapshot name reserved for an interrupted snapshot swap and the backend refused it. The rule now
//! lives in `cowfs-snapname`; this is the regression that fails if a copy comes back.
//!
//! No aliasing pair is in the table on purpose: two names that fold onto one another are one name,
//! so the backend would answer `Exists` for a name the control API calls legal.

use cowfs_core::{name_key, Core, Options};

/// The exact name `Core`'s swap derives for target `conf_base`, so the collision is the real one.
const RESERVED: &str = "conf_base.cowfs-swap0";

/// Every name the two surfaces must judge the same way.
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
    // legal: the reserved marker needs its leading dot, so this is not one
    "cowfs-swap",
    RESERVED,
    "conf_base.cowfs-swap1",
    "conf_base.cowfs-swap",
    "a.cowfs-swap0/repo",
];

/// The names of [`TABLE`] the rule refuses and the leading-dot rule does not, so only the reserved
/// marker refuses them.
const RESERVED_NAMES: &[&str] = &[
    "conf_base.cowfs-swap0",
    "conf_base.cowfs-swap1",
    "conf_base.cowfs-swap",
    "a.cowfs-swap0/repo",
];

/// Every legal name in [`TABLE`], sorted.
const LEGAL: &[&str] = &[
    "a",
    "caf\u{e9}",
    "conf_base",
    "conf_slot",
    "cowfs-swap",
    "slot-1",
];

#[test]
fn a_real_store_holds_exactly_the_names_the_control_api_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path());

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

    // a reserved name is unreachable through the other two entry points too, and refusing it
    // leaves the snapshot it was derived from exactly as it was
    assert!(core.snapshot_view(RESERVED).is_err());
    assert!(core.rename_snapshot("conf_base", RESERVED).is_err());
    assert!(core
        .list_snapshots()
        .unwrap()
        .iter()
        .any(|e| e.name == "conf_base"));
    assert_eq!(
        held(&core),
        LEGAL,
        "the store must hold every legal name in the table and nothing else"
    );

    // and again after the store is reopened from the directory, so nothing is answered from a cache
    drop(core);
    let reopened = open(dir.path());
    assert_eq!(held(&reopened), LEGAL, "a reopened store holds other names");
    for name in RESERVED_NAMES {
        assert!(reopened.create_snapshot(name).is_err(), "{name:?}");
        assert!(reopened.snapshot_view(name).is_err(), "{name:?}");
        assert!(cowfs_ctl::validate_snapshot_name(name).is_err(), "{name:?}");
    }
    assert_eq!(
        held(&reopened),
        LEGAL,
        "a refused name must not have been stored"
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

fn open(path: &std::path::Path) -> Core {
    Core::open(
        path,
        Options {
            background: false,
            ..Default::default()
        },
    )
    .expect("a store opens")
}

fn held(core: &Core) -> Vec<String> {
    let mut names: Vec<_> = core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
}

fn verdict<T, E: std::fmt::Debug>(r: &Result<T, E>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(_) => "refused",
    }
}
