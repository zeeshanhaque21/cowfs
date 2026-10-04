//! Wire format: round trips, tolerance of unknown fields and variants, and a golden file that
//! makes any accidental protocol change fail CI.

use cowfs_ctl::*;
use serde_json::json;

fn snap(name: &str) -> SnapshotInfo {
    SnapshotInfo {
        name: name.into(),
        parent: Some("main".into()),
        base: None,
        created_unix_ms: 1_700_000_000_000,
    }
}

fn client_frames() -> Vec<(&'static str, ClientFrame)> {
    let req = |request| ClientFrame::Request { id: 7, request };
    vec![
        (
            "client hello",
            ClientFrame::Hello(Hello {
                versions: vec![1, 2],
                client: "cowfs-cli/0.0.0".into(),
            }),
        ),
        ("client cancel", ClientFrame::Cancel { id: 7 }),
        ("req ping", req(Request::Ping(Empty {}))),
        ("req version", req(Request::Version(Empty {}))),
        ("req status", req(Request::Status(Empty {}))),
        ("req snapshot_list", req(Request::SnapshotList(Empty {}))),
        (
            "req snapshot_create empty",
            req(Request::SnapshotCreate(SnapshotCreate {
                name: "a".into(),
                from: None,
            })),
        ),
        (
            "req snapshot_create from",
            req(Request::SnapshotCreate(SnapshotCreate {
                name: "a".into(),
                from: Some("base".into()),
            })),
        ),
        (
            "req snapshot_rm",
            req(Request::SnapshotRm(SnapshotRm {
                name: "a".into(),
                expect_no_holders: true,
            })),
        ),
        (
            "req snapshot_rm force",
            req(Request::SnapshotRm(SnapshotRm {
                name: "a".into(),
                expect_no_holders: false,
            })),
        ),
        (
            "req snapshot_reset",
            req(Request::SnapshotReset(SnapshotReset {
                name: "slot".into(),
                from: "base".into(),
                expect_no_holders: true,
            })),
        ),
        (
            "req snapshot_rename",
            req(Request::SnapshotRename(SnapshotRename {
                from: "a".into(),
                to: "b".into(),
            })),
        ),
        (
            "req snapshot_promote",
            req(Request::SnapshotPromote(SnapshotName { name: "a".into() })),
        ),
        ("req gc", req(Request::Gc(GcParams { dry_run: true }))),
        ("req fsck", req(Request::Fsck(Empty {}))),
        (
            "req import",
            req(Request::Import(ImportParams {
                path: "/srv/slot".into(),
                name: "slot1".into(),
            })),
        ),
        (
            "req base_refresh",
            req(Request::BaseRefresh(BaseRefreshParams {
                repo: "/srv/repo".into(),
                git_ref: "main".into(),
                name: None,
            })),
        ),
        (
            "req ps",
            req(Request::Ps(PsParams {
                snapshot: "slot1".into(),
            })),
        ),
        ("req mount_info", req(Request::MountInfo(Empty {}))),
        (
            "req mount_snapshot",
            req(Request::MountSnapshot(crate::MountSnapshot {
                name: "slot1".into(),
                path: "/srv/pool/slot1/repo".into(),
                expect_no_holders: true,
            })),
        ),
        (
            "req unmount_snapshot",
            req(Request::UnmountSnapshot(crate::UnmountSnapshot {
                path: "/srv/pool/slot1/repo".into(),
            })),
        ),
        ("req shutdown", req(Request::Shutdown(NoParams {}))),
    ]
}

fn server_frames() -> Vec<(&'static str, ServerFrame)> {
    let resp = |result| ServerFrame::Response { id: 7, result };
    vec![
        (
            "server hello",
            ServerFrame::Hello(ServerHello {
                version: 1,
                server: "cowfs-ctl/0.0.0".into(),
                methods: METHODS.iter().map(|m| (*m).to_owned()).collect(),
            }),
        ),
        (
            "server progress",
            ServerFrame::Progress {
                id: 7,
                event: ProgressEvent {
                    phase: "mark".into(),
                    done: 120,
                    total: Some(400),
                    unit: Unit::Items,
                    message: None,
                },
            },
        ),
        ("res pong", resp(Response::Pong(Empty {}))),
        (
            "res version",
            resp(Response::Version(VersionInfo {
                protocol: 1,
                server: "cowfs-ctl/0.0.0".into(),
                ctl: "0.0.0".into(),
            })),
        ),
        (
            "res status",
            resp(Response::Status(Status {
                store_path: "/s".into(),
                mount_path: "/m".into(),
                snapshot_count: 2,
                block_count: 10,
                logical_bytes: 1000,
                stored_bytes: 400,
                uptime_secs: 5,
            })),
        ),
        (
            "res snapshot_list",
            resp(Response::SnapshotList(SnapshotList {
                snapshots: vec![snap("a")],
            })),
        ),
        (
            "res snapshot base",
            resp(Response::Snapshot(SnapshotInfo {
                base: Some(BaseMeta {
                    repo: Some("/srv/repo".into()),
                    git_ref: Some("main".into()),
                    commit: Some("abc123".into()),
                }),
                ..snap("b")
            })),
        ),
        ("res ok", resp(Response::Ok(Empty {}))),
        (
            "res gc",
            resp(Response::Gc(GcReport {
                dry_run: true,
                candidate_blocks: 3,
                candidate_bytes: 196_608,
                freed_blocks: 0,
                freed_bytes: 0,
                gross_removed_bytes: 0,
                rewrite_bytes: Some(0),
                net_reclaimed_bytes: Some(0),
            })),
        ),
        (
            "res fsck",
            resp(Response::Fsck(FsckReport {
                ok: false,
                blocks_checked: 9,
                bytes_checked: 900,
                snapshots_checked: 2,
                problems: vec![FsckProblem {
                    kind: "bad_hash".into(),
                    detail: "block 00ff".into(),
                }],
            })),
        ),
        (
            "res import",
            resp(Response::Import(ImportReport {
                name: "slot1".into(),
                files: 12,
                bytes: 3456,
                verified: false,
                hash_algorithm: "blake3".into(),
                source_root_hash: "aa".repeat(32),
                imported_root_hash: "bb".repeat(32),
                mismatches: vec![ImportMismatch {
                    path: "src/a.rs".into(),
                    reason: "content".into(),
                }],
                mismatches_truncated: true,
                stored_bytes: Some(1728),
            })),
        ),
        (
            "res base_refresh",
            resp(Response::BaseRefresh(BaseRefreshReport {
                snapshot: snap("repo-base"),
                previous_commit: Some("abc123".into()),
            })),
        ),
        (
            "res processes",
            resp(Response::Processes(ProcessList {
                processes: vec![ProcessInfo {
                    pid: 4242,
                    command: "sleep 100".into(),
                    holds: vec![
                        Hold {
                            kind: HoldKind::Cwd,
                            path: "/m/slot1".into(),
                        },
                        Hold {
                            kind: HoldKind::Lock,
                            path: "/m/slot1/.lock".into(),
                        },
                    ],
                }],
            })),
        ),
        (
            "res mount_info",
            resp(Response::MountInfo(MountInfo {
                mount_path: "/m".into(),
                adapter: "nfs".into(),
                mounted: true,
            })),
        ),
        (
            "error with id",
            ServerFrame::Error {
                id: Some(7),
                error: CtlError::not_found("snapshot \"a\" does not exist"),
            },
        ),
        (
            "error connection level",
            ServerFrame::Error {
                id: None,
                error: CtlError::new(ErrorCode::UnsupportedVersion, "no common protocol version")
                    .with_details(json!({"supported": [1]})),
            },
        ),
    ]
    .into_iter()
    .chain(ErrorCode::ALL.iter().map(|c| {
        (
            Box::leak(format!("error code {c}").into_boxed_str()) as &'static str,
            ServerFrame::Error {
                id: Some(7),
                error: CtlError::new(c.clone(), "m"),
            },
        )
    }))
    .collect()
}

fn text(bytes: Vec<u8>) -> String {
    let s = String::from_utf8(bytes).unwrap();
    assert!(
        s.ends_with('\n') && s.matches('\n').count() == 1,
        "one line per frame"
    );
    s.trim_end().to_owned()
}

#[test]
fn every_client_frame_round_trips() {
    for (label, frame) in client_frames() {
        let line = frame.encode();
        let back = ClientFrame::decode(&line[..line.len() - 1]);
        assert_eq!(back, Ok(frame), "{label}");
    }
}

#[test]
fn every_server_frame_round_trips() {
    for (label, frame) in server_frames() {
        let line = frame.encode();
        let back = ServerFrame::decode(&line[..line.len() - 1]);
        assert_eq!(back, Ok(frame), "{label}");
    }
}

#[test]
fn golden_covers_every_method_and_response_kind_and_error_code() {
    let frames = client_frames();
    let mut methods: Vec<&str> = frames
        .iter()
        .filter_map(|(_, f)| match f {
            ClientFrame::Request { request, .. } => Some(request.method()),
            _ => None,
        })
        .collect();
    methods.dedup();
    assert_eq!(
        methods, METHODS,
        "METHODS and the golden requests must agree, in order"
    );

    let frames = server_frames();
    let mut kinds: Vec<&str> = frames
        .iter()
        .filter_map(|(_, f)| match f {
            ServerFrame::Response { result, .. } => Some(result.kind()),
            _ => None,
        })
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    let mut want: Vec<&str> = RESPONSE_KINDS.to_vec();
    want.sort_unstable();
    assert_eq!(kinds, want);

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wire.tsv");
    let golden = std::fs::read_to_string(path).unwrap();
    for c in ErrorCode::ALL {
        assert!(
            golden.contains(&format!("\"code\":\"{}\"", c.as_str())),
            "error code {c} is not in the golden file"
        );
    }
}

#[test]
fn wire_format_matches_golden_file() {
    let mut actual = String::new();
    for (label, f) in client_frames() {
        actual += &format!("{label}\t{}\n", text(f.encode()));
    }
    for (label, f) in server_frames() {
        actual += &format!("{label}\t{}\n", text(f.encode()));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wire.tsv");
    if std::env::var_os("COWFS_UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &actual).unwrap();
    }
    let golden = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        actual, golden,
        "wire format changed; if intended, see the evolution rules in docs/v1-control-api.md and rerun with COWFS_UPDATE_GOLDEN=1"
    );
}

#[test]
fn golden_lines_decode() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/wire.tsv");
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let (label, json) = line.split_once('\t').unwrap();
        let ok = if label.starts_with("client") || label.starts_with("req") {
            ClientFrame::decode(json.as_bytes()).is_ok()
        } else {
            ServerFrame::decode(json.as_bytes()).is_ok()
        };
        assert!(ok, "{label}");
    }
}

#[test]
fn unknown_fields_are_ignored() {
    let f = ClientFrame::decode(
        br#"{"type":"request","id":1,"method":"snapshot_promote","params":{"name":"a","extra":[1]},"future":true}"#,
    );
    assert_eq!(
        f,
        Ok(ClientFrame::Request {
            id: 1,
            request: Request::SnapshotPromote(SnapshotName { name: "a".into() })
        })
    );
    assert!(ClientFrame::decode(br#"{"type":"hello","versions":[1],"x":1}"#).is_ok());
    assert!(ClientFrame::decode(br#"{"type":"cancel","id":3,"x":1}"#).is_ok());
    let s = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"pong","data":{"more":1},"x":1},"y":2}"#,
    );
    assert_eq!(
        s,
        Ok(ServerFrame::Response {
            id: 1,
            result: Response::Pong(Empty {})
        })
    );
    let s = ServerFrame::decode(
        br#"{"type":"error","id":null,"error":{"code":"busy","message":"m","z":1}}"#,
    );
    assert_eq!(
        s,
        Ok(ServerFrame::Error {
            id: None,
            error: CtlError::new(ErrorCode::Busy, "m")
        })
    );
}

#[test]
fn missing_optional_fields_take_defaults() {
    let f = ClientFrame::decode(
        br#"{"type":"request","id":1,"method":"snapshot_rm","params":{"name":"a"}}"#,
    );
    assert_eq!(
        f,
        Ok(ClientFrame::Request {
            id: 1,
            request: Request::SnapshotRm(SnapshotRm {
                name: "a".into(),
                expect_no_holders: true
            })
        })
    );
    let e = ClientFrame::decode(br#"{"type":"request","id":1,"method":"gc"}"#).unwrap_err();
    assert_eq!(e.error.code, ErrorCode::InvalidParams, "gc has no default");
    let f = ClientFrame::decode(
        br#"{"type":"request","id":1,"method":"snapshot_create","params":{"name":"a"}}"#,
    );
    assert!(matches!(
        f,
        Ok(ClientFrame::Request {
            request: Request::SnapshotCreate(SnapshotCreate { from: None, .. }),
            ..
        })
    ));
}

#[test]
fn unknown_variants_give_structured_errors() {
    let e = ClientFrame::decode(br#"{"type":"request","id":4,"method":"teleport","params":{}}"#)
        .unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(4), ErrorCode::UnknownMethod));
    let e = ClientFrame::decode(br#"{"type":"frobnicate","id":5}"#).unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(5), ErrorCode::UnknownFrame));
    let e = ClientFrame::decode(
        br#"{"type":"request","id":6,"method":"snapshot_rm","params":{"name":9}}"#,
    )
    .unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(6), ErrorCode::InvalidParams));
    let e =
        ClientFrame::decode(br#"{"type":"request","id":6,"method":"snapshot_rm"}"#).unwrap_err();
    assert_eq!((e.id, e.error.code), (Some(6), ErrorCode::InvalidParams));
}

#[test]
fn unknown_response_kind_decodes_to_unknown_with_raw_data() {
    let f = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"from_the_future","data":{"x":1}}}"#,
    );
    let Ok(ServerFrame::Response { result, .. }) = f else {
        panic!("{f:?}")
    };
    assert_eq!(result.kind(), "from_the_future");
    assert_eq!(result.data_json(), serde_json::json!({"x": 1}));
    let bad =
        ServerFrame::decode(br#"{"type":"response","id":1,"result":{"kind":"gc","data":{}}}"#);
    assert!(bad.is_err(), "a known kind with bad data is still an error");
}

#[test]
fn an_old_gc_report_without_the_gross_fields_still_decodes() {
    // A pre-#81 server sends the five original fields. The gross is real, but the rewrite cost
    // was never measured, so the net is unknown, not zero: a legacy cycle may have rewritten a
    // pack. Deriving net = gross would report a saving nobody measured.
    let f = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608}}}"#,
    );
    let Ok(ServerFrame::Response { result, .. }) = f else {
        panic!("{f:?}")
    };
    let Response::Gc(g) = result else {
        panic!("{result:?}")
    };
    assert_eq!(g.freed_bytes, 196_608, "the legacy gross field is intact");
    assert_eq!(
        g.gross_removed_bytes, 196_608,
        "an absent explicit gross falls back to freed_bytes, not zero"
    );
    assert_eq!(g.rewrite_bytes, None, "legacy rewrite is unknown, not zero");
    assert_eq!(
        g.net_reclaimed_bytes, None,
        "legacy net is unknown, not a false zero and not gross"
    );
}

#[test]
fn a_gc_report_that_carries_the_gross_fields_keeps_them() {
    let f = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"gross_removed_bytes":196608,"rewrite_bytes":1024,"net_reclaimed_bytes":195584}}}"#,
    );
    let Ok(ServerFrame::Response { result, .. }) = f else {
        panic!("{f:?}")
    };
    let Response::Gc(g) = result else {
        panic!("{result:?}")
    };
    assert_eq!(g.gross_removed_bytes, 196_608);
    assert_eq!(g.rewrite_bytes, Some(1024));
    assert_eq!(g.net_reclaimed_bytes, Some(195_584));
}

#[test]
fn a_gc_report_with_only_some_gross_fields_is_rejected() {
    // Present together or absent together. A half-written report cannot be repaired without
    // guessing, so it is an error rather than a quiet wrong default.
    for data in [
        r#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"gross_removed_bytes":196608}}}"#,
        r#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"rewrite_bytes":1024,"net_reclaimed_bytes":195584}}}"#,
        r#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"gross_removed_bytes":196608,"rewrite_bytes":1024}}}"#,
    ] {
        let err = ServerFrame::decode(data.as_bytes());
        assert!(
            err.is_err(),
            "partial gross fields must be rejected: {data}"
        );
    }
}

#[test]
fn a_gc_report_with_inconsistent_gross_fields_is_rejected() {
    // An explicit gross that disagrees with freed_bytes, or a net that is not gross minus
    // rewrite, is a corrupt report. Accepting it would print a number that never happened.
    let mismatched_gross = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"gross_removed_bytes":100,"rewrite_bytes":0,"net_reclaimed_bytes":100}}}"#,
    );
    assert!(mismatched_gross.is_err(), "gross != freed_bytes");

    let mismatched_net = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":196608,"gross_removed_bytes":196608,"rewrite_bytes":1024,"net_reclaimed_bytes":999}}}"#,
    );
    assert!(mismatched_net.is_err(), "net != gross - rewrite");

    let out_of_range = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"gc","data":{"dry_run":false,"candidate_blocks":3,"candidate_bytes":196608,"freed_blocks":3,"freed_bytes":18446744073709551615,"gross_removed_bytes":18446744073709551615,"rewrite_bytes":0,"net_reclaimed_bytes":0}}}"#,
    );
    assert!(out_of_range.is_err(), "gross - rewrite out of i64 range");
}

#[test]
fn unknown_server_things_do_not_break_clients() {
    assert_eq!(
        ServerFrame::decode(br#"{"type":"heartbeat","n":1}"#),
        Ok(ServerFrame::Unknown("heartbeat".into()))
    );
    let e = ServerFrame::decode(
        br#"{"type":"error","id":1,"error":{"code":"quota_exceeded","message":"m"}}"#,
    );
    assert_eq!(
        e,
        Ok(ServerFrame::Error {
            id: Some(1),
            error: CtlError::new(ErrorCode::Other("quota_exceeded".into()), "m")
        })
    );
    let p = ServerFrame::decode(
        br#"{"type":"progress","id":1,"event":{"phase":"p","done":1,"unit":"pages"}}"#,
    );
    assert!(matches!(
        p,
        Ok(ServerFrame::Progress {
            event: ProgressEvent {
                unit: Unit::Other,
                total: None,
                ..
            },
            ..
        })
    ));
    let r = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"processes","data":{"processes":[{"pid":1,"command":"c","holds":[{"kind":"mmap","path":"/x"}]}]}}}"#,
    );
    assert!(
        matches!(&r, Ok(ServerFrame::Response { result: Response::Processes(p), .. }) if p.processes[0].holds[0].kind == HoldKind::Other)
    );
}

#[test]
fn malformed_input_is_an_error_never_a_panic() {
    let cases: &[&[u8]] = &[
        b"",
        b"   ",
        b"not json",
        b"[1,2,3]",
        b"42",
        b"null",
        b"{",
        b"{\"type\":",
        b"{\"type\":7}",
        b"{\"type\":\"request\"}",
        b"{\"type\":\"request\",\"id\":-1,\"method\":\"ping\"}",
        b"{\"type\":\"request\",\"id\":1.5,\"method\":\"ping\"}",
        b"{\"type\":\"request\",\"id\":1,\"method\":5}",
        b"{\"type\":\"hello\"}",
        b"{\"type\":\"hello\",\"versions\":\"1\"}",
        b"{\"type\":\"cancel\"}",
        b"\xff\xfe\x00garbage",
    ];
    for c in cases {
        assert!(ClientFrame::decode(c).is_err(), "{c:?}");
        let _ = ServerFrame::decode(c);
    }
}

#[test]
fn read_line_is_bounded() {
    let mut buf = Vec::new();
    let mut r: &[u8] = b"abc\ndef";
    assert_eq!(read_line(&mut r, &mut buf, 10).unwrap(), LineRead::Line);
    assert_eq!(buf, b"abc");
    buf.clear();
    assert_eq!(read_line(&mut r, &mut buf, 10).unwrap(), LineRead::Eof);
    assert_eq!(
        buf, b"def",
        "truncated final line stays in the buffer, callers ignore it"
    );

    let mut r: &[u8] = b"0123456789\n";
    buf.clear();
    assert_eq!(read_line(&mut r, &mut buf, 10).unwrap(), LineRead::Line);
    let mut r: &[u8] = b"0123456789x\n";
    buf.clear();
    assert_eq!(read_line(&mut r, &mut buf, 10).unwrap(), LineRead::TooLong);
    assert!(buf.len() <= 10);
    let big = vec![b'x'; 100_000];
    let mut r: &[u8] = &big;
    buf.clear();
    assert_eq!(
        read_line(&mut r, &mut buf, 1000).unwrap(),
        LineRead::TooLong
    );
    assert!(buf.len() <= 1000);
}

#[test]
fn error_codes_are_stable_strings() {
    let all = [
        (ErrorCode::MalformedFrame, "malformed_frame"),
        (ErrorCode::LineTooLong, "line_too_long"),
        (ErrorCode::HandshakeRequired, "handshake_required"),
        (ErrorCode::UnsupportedVersion, "unsupported_version"),
        (ErrorCode::PermissionDenied, "permission_denied"),
        (ErrorCode::UnknownFrame, "unknown_frame"),
        (ErrorCode::UnknownMethod, "unknown_method"),
        (ErrorCode::InvalidParams, "invalid_params"),
        (ErrorCode::DuplicateId, "duplicate_id"),
        (ErrorCode::Busy, "busy"),
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::AlreadyExists, "already_exists"),
        (ErrorCode::Unsupported, "unsupported"),
        (ErrorCode::Cancelled, "cancelled"),
        (ErrorCode::ShuttingDown, "shutting_down"),
        (ErrorCode::IoError, "io_error"),
        (ErrorCode::Internal, "internal"),
        (ErrorCode::TooManyConnections, "too_many_connections"),
        (ErrorCode::Timeout, "timeout"),
    ];
    assert_eq!(all.len(), ErrorCode::ALL.len(), "every code is pinned here");
    for (code, s) in all {
        assert_eq!(code.as_str(), s);
        assert_eq!(ErrorCode::parse(s), code);
    }
}
