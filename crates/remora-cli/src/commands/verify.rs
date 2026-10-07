use std::path::Path;

use remora_ipc::{JournalCheck, Request, Response};

use super::{CliError, Ctx, EXIT_ERROR, unexpected};

/// `remora verify`: check the signed journal's hash chain.
pub fn run(ctx: &Ctx, root: &Path, json: bool) -> Result<(), CliError> {
    let mut client = ctx.client()?;
    let check = match client.call(Request::Verify {
        root: root.to_path_buf(),
    })? {
        Response::Verified { journal } => journal,
        other => return Err(unexpected(&other)),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&check).map_err(|e| CliError::new(EXIT_ERROR, e.to_string()))?
        );
    }
    match check {
        JournalCheck::Intact { entries } => {
            if !json {
                println!("journal intact: {entries} entries");
            }
            Ok(())
        }
        JournalCheck::Broken { seq, reason } => Err(CliError::new(
            EXIT_ERROR,
            format!("journal broken at entry #{seq}: {reason}"),
        )),
    }
}
