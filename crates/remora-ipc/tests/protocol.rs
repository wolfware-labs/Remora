#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::path::PathBuf;

use remora_engine::{PointKind, RestorePoint, Status};
use remora_ipc::{
    ErrorBody, ErrorKind, FailedPath, JournalCheck, Outcome, Preview, Request, RequestEnvelope, Response,
    ResponseEnvelope, Rewound,
};
use remora_journal::CommandRecord;
use remora_snap::SnapshotId;
use serde_json::{Value, json};

fn snap(c: char) -> SnapshotId {
    SnapshotId::parse(&c.to_string().repeat(64)).unwrap()
}

fn point() -> RestorePoint {
    RestorePoint {
        position: 3,
        snapshot: snap('a'),
        kind: PointKind::BeforeCommand,
        session_id: "s1".into(),
        at_ms: 1_100,
        label: Some("rm -rf build".into()),
        open: false,
    }
}

fn point_json() -> Value {
    json!({"position": 3, "snapshot": "a".repeat(64), "kind": "before_command", "session_id": "s1",
           "at_ms": 1100, "label": "rm -rf build", "open": false})
}

/// `value` serializes to exactly `expected`, and `expected` deserializes back to `value`.
fn golden<T>(value: &T, expected: Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn request(req: Request, expected_req: Value) {
    golden(
        &RequestEnvelope { v: 1, id: 7, req },
        json!({"v": 1, "id": 7, "req": expected_req}),
    );
}

fn root() -> PathBuf {
    PathBuf::from("/home/me/proj")
}

#[test]
fn every_request_has_a_stable_wire_form() {
    request(
        Request::Hello {
            client_version: "0.1.0".into(),
            protocol: 1,
        },
        json!({"type": "hello", "client_version": "0.1.0", "protocol": 1}),
    );
    request(
        Request::BeginTurn {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
            prompt: Some("fix it".into()),
        },
        json!({"type": "begin_turn", "root": "/home/me/proj", "session": "s1", "agent": "claude-code", "prompt": "fix it"}),
    );
    request(
        Request::BeginTurn {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
            prompt: None,
        },
        json!({"type": "begin_turn", "root": "/home/me/proj", "session": "s1", "agent": "claude-code"}),
    );
    request(
        Request::BeforeCommand {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
            command: "rm -rf build".into(),
            tool_use_id: Some("toolu_1".into()),
        },
        json!({"type": "before_command", "root": "/home/me/proj", "session": "s1", "agent": "claude-code",
               "command": "rm -rf build", "tool_use_id": "toolu_1"}),
    );
    request(
        Request::AfterCommand {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
            command: "ls".into(),
            tool_use_id: Some("toolu_1".into()),
            exit_code: Some(2),
        },
        json!({"type": "after_command", "root": "/home/me/proj", "session": "s1", "agent": "claude-code",
               "command": "ls", "tool_use_id": "toolu_1", "exit_code": 2}),
    );
    request(
        Request::EndTurn {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
        },
        json!({"type": "end_turn", "root": "/home/me/proj", "session": "s1", "agent": "claude-code"}),
    );
    request(
        Request::RestorePoints {
            root: root(),
            skip: 50,
            limit: Some(20),
        },
        json!({"type": "restore_points", "root": "/home/me/proj", "skip": 50, "limit": 20}),
    );
    request(
        Request::Rewind {
            root: root(),
            point: point(),
            force: true,
        },
        json!({"type": "rewind", "root": "/home/me/proj", "point": point_json(), "force": true}),
    );
    request(
        Request::Status { root: root() },
        json!({"type": "status", "root": "/home/me/proj"}),
    );
    request(
        Request::Verify { root: root() },
        json!({"type": "verify", "root": "/home/me/proj"}),
    );
    request(
        Request::EndSession {
            root: root(),
            session: "s1".into(),
            agent: "claude-code".into(),
        },
        json!({"type": "end_session", "root": "/home/me/proj", "session": "s1", "agent": "claude-code"}),
    );
    request(
        Request::Preview {
            root: root(),
            point: point(),
        },
        json!({"type": "preview", "root": "/home/me/proj", "point": point_json()}),
    );
    request(
        Request::RestoreSnapshot {
            root: root(),
            snapshot: snap('e'),
            force: false,
        },
        json!({"type": "restore_snapshot", "root": "/home/me/proj", "snapshot": "e".repeat(64), "force": false}),
    );
    request(Request::Shutdown, json!({"type": "shutdown"}));
}

#[test]
fn optional_request_fields_may_be_left_out() {
    let req: Request =
        serde_json::from_value(json!({"type": "rewind", "root": "/p", "point": point_json()})).unwrap();
    assert!(matches!(req, Request::Rewind { force: false, .. }));
    let req: Request = serde_json::from_value(json!({"type": "after_command", "root": "/p", "session": "s",
        "agent": "a", "command": "ls"}))
    .unwrap();
    assert!(matches!(req, Request::AfterCommand { exit_code: None, .. }));
    let req: Request = serde_json::from_value(json!({"type": "restore_points", "root": "/p"})).unwrap();
    assert!(matches!(
        req,
        Request::RestorePoints {
            skip: 0,
            limit: None,
            ..
        }
    ));
}

fn ok(response: Response, expected: Value) {
    golden(
        &ResponseEnvelope {
            v: 1,
            id: 7,
            outcome: Outcome::Ok(response),
        },
        json!({"v": 1, "id": 7, "ok": expected}),
    );
}

#[test]
fn every_response_has_a_stable_wire_form() {
    ok(
        Response::Hello {
            daemon_version: "0.1.0".into(),
            protocol: 1,
        },
        json!({"type": "hello", "daemon_version": "0.1.0", "protocol": 1}),
    );
    ok(
        Response::Snapshot { snapshot: snap('b') },
        json!({"type": "snapshot", "snapshot": "b".repeat(64)}),
    );
    ok(Response::Recorded, json!({"type": "recorded"}));
    ok(
        Response::TurnEnded { seq: Some(4) },
        json!({"type": "turn_ended", "seq": 4}),
    );
    ok(
        Response::TurnEnded { seq: None },
        json!({"type": "turn_ended", "seq": null}),
    );
    ok(
        Response::RestorePoints {
            points: vec![point()],
            total: 120,
        },
        json!({"type": "restore_points", "points": [point_json()], "total": 120}),
    );
    ok(
        Response::Rewound(Rewound {
            safety_snapshot: snap('c'),
            written: 2,
            deleted: 1,
            skipped: vec!["big.iso".into()],
            failed: vec![FailedPath {
                path: "locked/a".into(),
                error: "permission denied".into(),
            }],
            commands_after: vec![CommandRecord {
                command: "git push".into(),
                exit_code: Some(0),
                snapshot_id: None,
                at_ms: Some(2_000),
                tool_use_id: None,
            }],
            commands_omitted: 3,
            seq: 9,
        }),
        json!({"type": "rewound", "safety_snapshot": "c".repeat(64), "written": 2, "deleted": 1,
               "skipped": ["big.iso"], "failed": [{"path": "locked/a", "error": "permission denied"}],
               "commands_after": [{"command": "git push", "exit_code": 0, "at_ms": 2000}],
               "commands_omitted": 3, "seq": 9}),
    );
    ok(
        Response::Status {
            status: Status {
                root: root(),
                open_sessions: vec!["s1".into()],
                journal_entries: 2,
                restore_points: 5,
                last_unprotected: vec![PathBuf::from("/home/me/proj/secret")],
            },
        },
        json!({"type": "status", "status": {"root": "/home/me/proj", "open_sessions": ["s1"], "journal_entries": 2,
               "restore_points": 5, "last_unprotected": ["/home/me/proj/secret"]}}),
    );
    ok(
        Response::Verified {
            journal: JournalCheck::Intact { entries: 3 },
        },
        json!({"type": "verified", "journal": {"state": "intact", "entries": 3}}),
    );
    ok(
        Response::Verified {
            journal: JournalCheck::Broken {
                seq: 2,
                reason: "hash_mismatch".into(),
            },
        },
        json!({"type": "verified", "journal": {"state": "broken", "seq": 2, "reason": "hash_mismatch"}}),
    );
    ok(
        Response::Preview(Preview {
            restore: vec!["src/main.rs".into()],
            restore_total: 1,
            delete: vec![],
            delete_total: 0,
            revert: vec!["Cargo.toml".into()],
            revert_total: 250,
            commands_after: vec![],
            commands_omitted: 0,
        }),
        json!({"type": "preview", "restore": ["src/main.rs"], "restore_total": 1, "delete": [], "delete_total": 0,
               "revert": ["Cargo.toml"], "revert_total": 250, "commands_after": [], "commands_omitted": 0}),
    );
    ok(Response::ShuttingDown, json!({"type": "shutting_down"}));
}

fn err(kind: ErrorKind, expected: Value) {
    let mut expected_err = expected;
    expected_err["message"] = "boom".into();
    golden(
        &ResponseEnvelope {
            v: 1,
            id: 7,
            outcome: Outcome::Err(ErrorBody::new(kind, "boom")),
        },
        json!({"v": 1, "id": 7, "err": expected_err}),
    );
}

#[test]
fn every_error_has_a_stable_wire_form() {
    err(ErrorKind::InvalidRequest, json!({"kind": "invalid_request"}));
    err(ErrorKind::VersionMismatch, json!({"kind": "version_mismatch"}));
    err(
        ErrorKind::TurnInProgress { session: "s1".into() },
        json!({"kind": "turn_in_progress", "session": "s1"}),
    );
    err(
        ErrorKind::UnknownRestorePoint,
        json!({"kind": "unknown_restore_point"}),
    );
    err(
        ErrorKind::RootOverlapsDataDir,
        json!({"kind": "root_overlaps_data_dir"}),
    );
    err(
        ErrorKind::RewindIncomplete {
            safety_snapshot: "d".repeat(64),
        },
        json!({"kind": "rewind_incomplete", "safety_snapshot": "d".repeat(64)}),
    );
    err(ErrorKind::LockTimeout, json!({"kind": "lock_timeout"}));
    err(ErrorKind::InvalidSessionId, json!({"kind": "invalid_session_id"}));
    err(ErrorKind::Internal, json!({"kind": "internal"}));
}

#[test]
fn a_shutdown_request_has_the_wire_form_every_version_accepts() {
    let stable = RequestEnvelope {
        v: 1,
        id: 0,
        req: Request::Shutdown,
    };
    assert_eq!(
        serde_json::to_string(&stable).unwrap(),
        r#"{"v":1,"id":0,"req":{"type":"shutdown"}}"#
    );
}

#[test]
fn an_unknown_request_type_is_rejected() {
    let parsed =
        serde_json::from_value::<RequestEnvelope>(json!({"v": 1, "id": 1, "req": {"type": "format_disk"}}));
    assert!(parsed.is_err());
}
