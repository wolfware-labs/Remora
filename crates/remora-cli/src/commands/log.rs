use std::path::Path;

use remora_ipc::MAX_POINTS;

use super::{CliError, Ctx, ago, kind_name, now_ms, one_line, points_page, short_session};

/// `remora log`: restore points, `#1` newest.
pub fn run(ctx: &Ctx, root: &Path, count: usize, all: bool, json: bool) -> Result<(), CliError> {
    let mut client = ctx.client()?;
    let mut numbered = Vec::new();
    let mut total = 0;
    loop {
        let want = if all {
            MAX_POINTS
        } else {
            (count - numbered.len()).min(MAX_POINTS)
        };
        if want == 0 {
            break;
        }
        let (page, page_total) = points_page(&mut client, root, numbered.len(), want)?;
        total = page_total;
        let done = page.len() < want;
        numbered.extend(page);
        if done || numbered.len() >= total {
            break;
        }
    }
    if json {
        let rows: Vec<_> = numbered
            .iter()
            .enumerate()
            .map(|(i, point)| serde_json::json!({ "number": i + 1, "point": point }))
            .collect();
        println!("{}", serde_json::Value::Array(rows));
        return Ok(());
    }
    if numbered.is_empty() {
        println!("no restore points yet for {}", root.display());
        return Ok(());
    }
    let now = now_ms();
    for (i, point) in numbered.iter().enumerate() {
        let label = point
            .label
            .as_deref()
            .map(|l| one_line(l, 60))
            .unwrap_or_default();
        let open = if point.open { "  [open]" } else { "" };
        println!(
            "#{:<4} {:<12} {:<15} {}  {label}{open}",
            i + 1,
            ago(now, point.at_ms),
            kind_name(point.kind),
            short_session(&point.session_id)
        );
    }
    if numbered.len() < total {
        println!(
            "… {} older points (`remora log --all` lists them)",
            total - numbered.len()
        );
    }
    Ok(())
}
