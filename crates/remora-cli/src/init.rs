//! `remora init` / `remora uninit`: Remora's hooks in Claude Code's user settings
//! (`~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Every hook Remora installs runs `<remora> hook claude-code`; this suffix identifies them.
pub const HOOK_SUFFIX: &str = "hook claude-code";

/// The hooks Remora installs: Claude Code event, tool matcher, timeout in seconds.
pub const HOOKS: [(&str, Option<&str>, u64); 7] = [
    ("UserPromptSubmit", None, 30),
    ("PreToolUse", Some("Bash"), 30),
    ("PostToolUse", Some("Bash"), 10),
    ("PostToolUseFailure", Some("Bash"), 10),
    ("Stop", None, 10),
    ("StopFailure", None, 10),
    ("SessionEnd", None, 5),
];

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("cannot find the home directory")]
    NoHome,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "{path} is not valid JSON (line {line}, column {column}); fix it first, Remora left it untouched"
    )]
    InvalidJson {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    #[error("{path} does not have the shape Claude Code settings have ({detail}); Remora left it untouched")]
    UnexpectedShape { path: PathBuf, detail: String },
}

/// Where Claude Code keeps the user's settings.
pub fn settings_path() -> Result<PathBuf, InitError> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("settings.json")),
        _ => Ok(dirs::home_dir()
            .ok_or(InitError::NoHome)?
            .join(".claude/settings.json")),
    }
}

/// The hook command for the `remora` executable at `exe`, quoted for the shell Claude Code
/// runs hooks with.
pub fn hook_command(exe: &Path) -> String {
    let mut quoted = String::from("\"");
    for c in exe.to_string_lossy().chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!("{quoted}\" {HOOK_SUFFIX}")
}

/// The `remora` executable the installed hooks run: the first hook Remora added, its
/// command's quoted path read back (see [`hook_command`]). `None` when none is installed.
pub fn installed_exe(settings: &Value) -> Option<PathBuf> {
    let hooks = settings.get("hooks")?.as_object()?;
    let command = hooks
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group.get("hooks")?.as_array())
        .flatten()
        .filter_map(|handler| handler.get("command")?.as_str())
        .find(|command| is_ours(command))?;
    let mut chars = command.strip_prefix('"')?.chars();
    let mut path = String::new();
    loop {
        match chars.next()? {
            '\\' => path.push(chars.next()?),
            '"' => return Some(PathBuf::from(path)),
            c => path.push(c),
        }
    }
}

/// Whether a hook command is one Remora installed.
pub fn is_ours(command: &str) -> bool {
    command.trim_end().ends_with(HOOK_SUFFIX) && command.contains("remora")
}

/// Add Remora's hooks for `exe`, replacing any it added before. Hooks Remora did not add are
/// never touched. Returns whether the settings changed.
pub fn install(settings: &mut Value, exe: &Path) -> Result<bool, String> {
    let before = settings.clone();
    uninstall(settings);
    let command = hook_command(exe);
    let root = settings.as_object_mut().ok_or("the top level is not an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" is not an object")?;
    for (event, matcher, timeout) in HOOKS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{event}\" is not a list"))?;
        let handler = json!({ "type": "command", "command": command, "timeout": timeout });
        groups.push(match matcher {
            Some(matcher) => json!({ "matcher": matcher, "hooks": [handler] }),
            None => json!({ "hooks": [handler] }),
        });
    }
    Ok(*settings != before)
}

/// Remove the hooks Remora added, and any group or event list that only they made up.
/// Returns whether anything was removed.
pub fn uninstall(settings: &mut Value) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut removed_any = false;
    let mut emptied_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let before = groups.len();
        let mut removed_here = false;
        groups.retain_mut(|group| {
            let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let count = list.len();
            list.retain(|handler| {
                !handler
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_ours)
            });
            let removed = list.len() != count;
            removed_here |= removed;
            !(removed && list.is_empty())
        });
        removed_any |= removed_here;
        if removed_here && groups.is_empty() && before > 0 {
            emptied_events.push(event.clone());
        }
    }
    for event in emptied_events {
        hooks.remove(&event);
    }
    if removed_any
        && hooks.is_empty()
        && let Some(root) = settings.as_object_mut()
    {
        root.remove("hooks");
    }
    removed_any
}

/// Apply `change` to the settings file at `path`. A missing or empty file counts as `{}`;
/// invalid JSON is refused. The file is replaced atomically, keeping its permissions and
/// the previous version as `settings.json.remora.bak`; a symlinked file is updated where it
/// points. Returns whether the file changed.
pub fn update_file(
    path: &Path,
    change: impl FnOnce(&mut Value) -> Result<bool, String>,
) -> Result<bool, InitError> {
    let io_err = |path: &Path| {
        let path = path.to_path_buf();
        move |source| InitError::Io { path, source }
    };
    // Dotfile managers often symlink settings.json: write the file it points to.
    let target = match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path).map_err(io_err(path))?,
        _ => path.to_path_buf(),
    };
    let original = match fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(&target)(e)),
    };
    let mut settings = match original.as_deref() {
        None => json!({}),
        Some(text) if text.trim().is_empty() => json!({}),
        Some(text) => serde_json::from_str(text).map_err(|e| InitError::InvalidJson {
            path: target.clone(),
            line: e.line(),
            column: e.column(),
        })?,
    };
    let changed = change(&mut settings).map_err(|detail| InitError::UnexpectedShape {
        path: target.clone(),
        detail,
    })?;
    if !changed {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut text = serde_json::to_string_pretty(&settings).map_err(|e| io_err(&target)(e.into()))?;
    text.push('\n');
    let tmp = target.with_extension("json.remora.tmp");
    write_private(&tmp, text.as_bytes()).map_err(io_err(&tmp))?;
    if let Some(original) = &original {
        // Settings can hold secrets (an `env` block with tokens): the backup and the new file
        // are owner-only from the start, then get the original's permissions.
        let permissions = fs::metadata(&target).map_err(io_err(&target))?.permissions();
        let backup = target.with_extension("json.remora.bak");
        write_private(&backup, original.as_bytes()).map_err(io_err(&backup))?;
        fs::set_permissions(&backup, permissions.clone()).map_err(io_err(&backup))?;
        fs::set_permissions(&tmp, permissions).map_err(io_err(&tmp))?;
    }
    fs::rename(&tmp, &target).map_err(io_err(&target))?;
    Ok(true)
}

/// Write `bytes` to a new `path`, created owner-only (Unix) so nothing can read it before
/// its permissions are set. A leftover file at `path` is replaced.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_commands_are_quoted_for_the_shell() {
        assert_eq!(
            hook_command(Path::new("/home/me/.cargo/bin/remora")),
            r#""/home/me/.cargo/bin/remora" hook claude-code"#
        );
        assert_eq!(
            hook_command(Path::new("/opt/my $tools/re\"mora")),
            r#""/opt/my \$tools/re\"mora" hook claude-code"#
        );
        assert!(is_ours(&hook_command(Path::new("/x/remora"))));
        assert!(!is_ours("prettier --write"));
    }

    #[test]
    fn the_installed_executable_is_read_back_from_the_hooks() {
        let exe = Path::new("/opt/my $tools/\"q\" \\ `x`/remora");
        let mut settings =
            json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "other" }] }] } });
        assert_eq!(installed_exe(&settings), None);

        install(&mut settings, exe).unwrap();

        assert_eq!(installed_exe(&settings).as_deref(), Some(exe));
        uninstall(&mut settings);
        assert_eq!(installed_exe(&settings), None);
    }
}
