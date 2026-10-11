//! What a hook costs the agent inside the engine: a turn start and a checkpoint before a
//! command, on an unchanged 1,000-file project. Run with `cargo bench -p remora-engine`.

use std::fs;
use std::time::{Duration, SystemTime};

use criterion::{Criterion, criterion_group, criterion_main};
use remora_engine::{Engine, SessionRef};
use tempfile::TempDir;

fn hooks(c: &mut Criterion) {
    let data = TempDir::new().expect("a data folder");
    let root = TempDir::new().expect("a project folder");
    for i in 0..1_000 {
        let folder = root.path().join(format!("d{}", i / 100));
        fs::create_dir_all(&folder).expect("a folder");
        let file = folder.join(format!("f{i}.txt"));
        fs::write(&file, format!("file {i} ").repeat(400)).expect("a file");
        // Untouched for an hour: files changed in the last two seconds are always re-read.
        fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(SystemTime::now() - Duration::from_secs(3_600)))
            .expect("an old file");
    }
    let engine = Engine::open(data.path()).expect("an engine");
    let mut project = engine.project(root.path()).expect("a project");
    let session = SessionRef::new("bench", "claude-code");
    let mut at = 1_000;
    project.begin_turn(&session, Some("warm up"), at).expect("a turn");

    let mut group = c.benchmark_group("hook");
    group.bench_function("before command, unchanged 1,000 files", |b| {
        b.iter(|| {
            at += 1;
            project
                .before_command(&session, "true", None, at)
                .expect("a checkpoint")
        });
    });
    group.bench_function("turn start, unchanged 1,000 files", |b| {
        b.iter(|| {
            at += 1;
            project.begin_turn(&session, Some("go"), at).expect("a turn")
        });
    });
    group.finish();
}

criterion_group!(benches, hooks);
criterion_main!(benches);
