#![allow(clippy::unwrap_used)] // tests may panic on failure

use remora_snap::{ChangeKind, Manifest, Node, SnapError, SnapshotId};

fn file(hash: &str) -> Node {
    Node::File {
        hash: hash.into(),
        size: 1,
        mode: 0o644,
        stored: true,
    }
}

#[test]
fn id_accepts_64_lowercase_hex() {
    let s = "a".repeat(64);
    assert_eq!(SnapshotId::parse(&s).unwrap().as_str(), s);
}

#[test]
fn id_rejects_traversal_and_bad_length() {
    for bad in ["../../etc/passwd", "", &"A".repeat(64), &"a".repeat(63)] {
        assert!(
            matches!(SnapshotId::parse(bad), Err(SnapError::InvalidId(_))),
            "{bad}"
        );
    }
}

#[test]
fn diff_reports_added_modified_deleted_sorted() {
    let mut old = Manifest::default();
    old.nodes.insert("a.txt".into(), file("1"));
    old.nodes.insert("b.txt".into(), file("2"));
    let mut new = Manifest::default();
    new.nodes.insert("b.txt".into(), file("3"));
    new.nodes.insert("c.txt".into(), file("4"));

    let kinds: Vec<_> = old.diff(&new).into_iter().map(|c| (c.path, c.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            ("a.txt".into(), ChangeKind::Deleted),
            ("b.txt".into(), ChangeKind::Modified),
            ("c.txt".into(), ChangeKind::Added),
        ]
    );
}

#[test]
fn diff_of_identical_manifests_is_empty() {
    let mut m = Manifest::default();
    m.nodes.insert("a".into(), Node::Dir { mode: 0o755 });
    assert!(m.diff(&m.clone()).is_empty());
}

#[test]
fn protects_gap_covers_descendants_of_opaque_dirs() {
    let mut m = Manifest::default();
    m.nodes.insert("secret".into(), Node::Opaque);
    m.nodes.insert(
        "big.bin".into(),
        Node::File {
            hash: "x".into(),
            size: 9,
            mode: 0o644,
            stored: false,
        },
    );
    m.nodes.insert("ok.txt".into(), file("1"));
    assert!(m.protects_gap("secret"));
    assert!(m.protects_gap("secret/inner/file"));
    assert!(m.protects_gap("big.bin"));
    assert!(!m.protects_gap("ok.txt"));
    assert!(!m.protects_gap("secretive.txt"));
}
