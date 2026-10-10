use std::path::Path;

use remora_engine::PruneReason;
use remora_ipc::{Request, Response};

use super::{CliError, Ctx, unexpected};

/// `remora prune`: apply the retention policy to this project now.
pub fn run(ctx: &Ctx, root: &Path, dry_run: bool) -> Result<(), CliError> {
    let mut client = ctx.client()?;
    let report = match client.call(Request::Prune {
        root: root.to_path_buf(),
        dry_run,
    })? {
        Response::Pruned { report } => report,
        other => return Err(unexpected(&other)),
    };
    let by_age = report
        .pruned
        .iter()
        .filter(|p| p.reason == PruneReason::Age)
        .count();
    let by_size = report.pruned.len() - by_age;
    let verb = if dry_run { "would prune" } else { "pruned" };
    if report.pruned.is_empty() {
        println!("nothing to prune: every restore point is within the retention policy");
    } else {
        println!(
            "{verb} {} restore points ({by_age} past the retention period, {by_size} over the size cap)",
            report.pruned.len()
        );
    }
    let freed = if dry_run { "would free" } else { "freed" };
    println!(
        "{freed} {} in {} stored files  ·  store {}",
        size(report.bytes_freed),
        report.blobs_removed,
        size(report.store_bytes)
    );
    if !report.unreadable.is_empty() {
        println!(
            "⚠ {} snapshots could not be read: they were kept, and no stored file was removed",
            report.unreadable.len()
        );
    }
    Ok(())
}

/// Bytes for people: "512 B", "3.4 MB", "1.2 GB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1_000 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)] // shown to one decimal
    let mut value = bytes as f64 / 1_000.0;
    let mut unit = 0;
    while value >= 1_000.0 && unit < UNITS.len() - 1 {
        value /= 1_000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::size;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_500), "1.5 KB");
        assert_eq!(size(10_000_000_000), "10.0 GB");
    }
}
