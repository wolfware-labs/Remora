use std::io::IsTerminal;
use std::path::Path;

use remora_ipc::{ClientError, ErrorKind, Request, Response};

use super::rewind::{busy, confirm};
use super::{CliError, Ctx, EXIT_USAGE, unexpected};

/// `remora forget`: remove everything recorded for this project.
pub fn run(ctx: &Ctx, root: &Path, yes: bool, force: bool) -> Result<(), CliError> {
    if !yes && !std::io::stdin().is_terminal() {
        return Err(CliError::new(
            EXIT_USAGE,
            "not on a terminal, so there is no one to confirm: pass --yes",
        ));
    }
    let mut client = ctx.client()?;
    if !yes {
        println!(
            "Forget {}: its journal, restore points and stored files are removed. This cannot be undone.",
            root.display()
        );
        if !confirm()? {
            println!("nothing removed");
            return Ok(());
        }
    }
    let answer = client.call(Request::Forget {
        root: root.to_path_buf(),
        force,
    });
    match answer {
        Ok(Response::Forgotten { data_dir, blob_home }) => {
            println!("forgot {}", root.display());
            println!("  removed {}", data_dir.display());
            if let Some(home) = blob_home {
                println!("  removed {}", home.display());
            }
            println!("a new agent session in this project starts recording it again");
            Ok(())
        }
        Ok(other) => Err(unexpected(&other)),
        Err(ClientError::Daemon(body)) if matches!(body.kind, ErrorKind::TurnInProgress { .. }) => {
            Err(busy(&mut client, root))
        }
        Err(e) => Err(e.into()),
    }
}
