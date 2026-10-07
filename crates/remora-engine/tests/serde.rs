#![allow(clippy::unwrap_used)] // tests may panic on failure

mod common;

use common::{Fixture, agent};
use remora_engine::{PointKind, RestorePoint, Status};

#[test]
fn restore_points_round_trip_through_json() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), Some("edit"), 1_000).unwrap();
    p.before_command(&agent(), "rm -rf build", None, 1_100).unwrap();

    for point in p.restore_points().unwrap() {
        let json = serde_json::to_string(&point).unwrap();
        assert_eq!(serde_json::from_str::<RestorePoint>(&json).unwrap(), point);
    }
}

#[test]
fn point_kinds_are_snake_case_on_the_wire() {
    let kinds = [
        PointKind::TurnStart,
        PointKind::BeforeCommand,
        PointKind::BeforeRewind,
    ];
    let json: Vec<_> = kinds.iter().map(|k| serde_json::to_string(k).unwrap()).collect();
    assert_eq!(
        json,
        [r#""turn_start""#, r#""before_command""#, r#""before_rewind""#]
    );
}

#[test]
fn a_restore_point_with_a_malformed_snapshot_id_is_rejected() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    let mut json: serde_json::Value = serde_json::to_value(&p.restore_points().unwrap()[0]).unwrap();
    json["snapshot"] = "../../etc".into();

    assert!(serde_json::from_value::<RestorePoint>(json).is_err());
}

#[test]
fn status_round_trips_through_json() {
    let fx = Fixture::new();
    let mut p = fx.open();
    p.begin_turn(&agent(), None, 1_000).unwrap();
    let status = p.status().unwrap();

    let json = serde_json::to_string(&status).unwrap();

    assert_eq!(serde_json::from_str::<Status>(&json).unwrap(), status);
}
