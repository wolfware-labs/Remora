//! Repository checks. Run as `cargo xtask <command>`.

mod layers;

use std::path::Path;
use std::process::ExitCode;

use cargo_metadata::MetadataCommand;

const USAGE: &str = "usage: cargo xtask layers";

/// `cargo metadata --no-deps` for the workspace that contains this crate.
fn metadata_command() -> MetadataCommand {
    let mut command = MetadataCommand::new();
    command
        .manifest_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml"))
        .no_deps();
    command
}

fn run_layers() -> ExitCode {
    let metadata = match metadata_command().exec() {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!("layers: cargo metadata failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let violations = layers::check(&metadata);
    for violation in &violations {
        eprintln!("{violation}");
    }
    if violations.is_empty() {
        println!("layers: {} crates, ok", metadata.workspace_packages().len());
        ExitCode::SUCCESS
    } else {
        eprintln!("layers: {} violation(s)", violations.len());
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("layers") => run_layers(),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
