//! A real `remorad` killed (SIGKILL) in the middle of a rewind: after a restart the project
//! is entirely as it was before the rewind or entirely rewound, never a mix.
#![cfg(unix)]
#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use remora_engine::{REWIND_FILE, ROLLED_BACK_LABEL, project_id};
use remora_ipc::{Client, ClientError, JournalCheck, Request, Response};

const REMORAD: &str = env!("CARGO_BIN_EXE_remorad");
const FILES: usize = 3_000;

fn start(data: &Path) -> (Child, Client) {
    let daemon = Command::new(REMORAD)
        .args(["run", "--data-dir"])
        .arg(data)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Client::connect(data) {
            Ok(client) => return (daemon, client),
            Err(ClientError::DaemonNotRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("the daemon did not come up: {e}"),
        }
    }
}

fn write_all(root: &Path, version: &str) {
    for i in 0..FILES {
        let dir = root.join(format!("d{}", i % 30));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("f{i}.txt")),
            format!("{version} {i} ").repeat(500),
        )
        .unwrap();
    }
}

/// Every file under `root` with its contents.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

fn turn(client: &mut Client, root: &Path, change: impl FnOnce()) {
    let begin = Request::BeginTurn {
        root: root.to_path_buf(),
        session: "s1".into(),
        agent: "claude-code".into(),
        prompt: Some("rewrite everything".into()),
        deadline_ms: None,
    };
    assert!(matches!(client.call(begin), Ok(Response::Snapshot { .. })));
    change();
    let end = Request::EndTurn {
        root: root.to_path_buf(),
        session: "s1".into(),
        agent: "claude-code".into(),
    };
    client.call(end).unwrap();
}

#[test]
fn a_daemon_killed_during_a_rewind_leaves_the_project_whole() {
    let data = tempfile::TempDir::new().unwrap();
    let project = tempfile::TempDir::new().unwrap();
    let root = project.path().canonicalize().unwrap();
    write_all(&root, "old");
    let old = tree(&root);
    let (mut daemon, mut client) = start(data.path());
    turn(&mut client, &root, || write_all(&root, "new"));
    let new = tree(&root);
    let Ok(Response::RestorePoints { mut points, .. }) = client.call(Request::RestorePoints {
        root: root.clone(),
        skip: 0,
        limit: None,
    }) else {
        panic!("no restore points");
    };
    let target = points.remove(0);

    // Rewind from another connection; kill the daemon as soon as the rewind's record appears.
    let rewinding = {
        let (data, root) = (data.path().to_path_buf(), root.clone());
        std::thread::spawn(move || {
            let mut client = Client::connect(&data).unwrap();
            client.call(Request::Rewind {
                root,
                point: target,
                force: false,
            })
        })
    };
    let intent = data
        .path()
        .join("projects")
        .join(project_id(&root))
        .join(REWIND_FILE);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !intent.exists() && !rewinding.is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let cut_short = intent.exists();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let answered = rewinding.join().unwrap();

    let (mut daemon, mut client) = start(data.path());
    let Ok(Response::RestorePoints { points, .. }) = client.call(Request::RestorePoints {
        root: root.clone(),
        skip: 0,
        limit: None,
    }) else {
        panic!("the project does not open");
    };
    let after = tree(&root);
    if after == new {
        eprintln!("rolled back (the rewind's answer: {answered:?})");
        if cut_short {
            assert_eq!(points[0].label.as_deref(), Some(ROLLED_BACK_LABEL), "journaled");
        }
    } else {
        assert!(after == old, "a mix of the two states");
        eprintln!("the rewind finished before the kill");
    }
    assert!(!intent.exists(), "the record is gone once the project opened");
    let Ok(Response::Verified { journal }) = client.call(Request::Verify { root: root.clone() }) else {
        panic!("verify failed");
    };
    assert!(matches!(journal, JournalCheck::Intact { .. }), "{journal:?}");
    client.call(Request::Shutdown).unwrap();
    daemon.wait().unwrap();
}
