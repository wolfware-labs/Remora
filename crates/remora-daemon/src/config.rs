//! `<data_dir>/config.toml`, read when the daemon starts:
//!
//! ```toml
//! [retention]
//! days = 30          # restore points younger than this are always kept
//! max_store_gb = 10  # per project, blob folder size beyond which the oldest points go
//! ```
//!
//! A missing file means the defaults. A file that cannot be used (unreadable, invalid,
//! unknown keys, values out of range) also means the defaults, and the reason is logged
//! and shown by `remora status`.

use std::path::Path;

use remora_engine::Retention;
use serde::Deserialize;

/// The configuration file's name in the data directory.
pub const CONFIG_FILE: &str = "config.toml";

/// The configuration in effect, and why the file was not used, if it was not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub retention: Retention,
    pub problem: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    retention: Option<RetentionFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionFile {
    days: Option<u64>,
    max_store_gb: Option<u64>,
}

/// Read the configuration from `data_dir`.
pub fn load(data_dir: &Path) -> Config {
    let path = data_dir.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
        Err(e) => return invalid(&path, &e.to_string()),
    };
    match parse(&text) {
        Ok(retention) => Config {
            retention,
            problem: None,
        },
        Err(reason) => invalid(&path, &reason),
    }
}

fn parse(text: &str) -> Result<Retention, String> {
    let file: File = toml::from_str(text).map_err(|e| e.message().to_owned())?;
    let defaults = Retention::default();
    let Some(retention) = file.retention else {
        return Ok(defaults);
    };
    let days = retention.days.unwrap_or(defaults.days);
    let max_store_gb = retention.max_store_gb.unwrap_or(defaults.max_store_gb);
    if days == 0 {
        return Err("retention.days must be at least 1".to_owned());
    }
    if max_store_gb == 0 {
        return Err("retention.max_store_gb must be at least 1".to_owned());
    }
    Ok(Retention { days, max_store_gb })
}

fn invalid(path: &Path, reason: &str) -> Config {
    log::warn!("{} is not used, the defaults apply: {reason}", path.display());
    Config {
        retention: Retention::default(),
        problem: Some(format!("{}: {reason}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_defaults() {
        assert_eq!(parse(""), Ok(Retention::default()));
        assert_eq!(
            parse("[retention]\ndays = 7\nmax_store_gb = 2\n"),
            Ok(Retention {
                days: 7,
                max_store_gb: 2
            })
        );
        assert_eq!(
            parse("[retention]\ndays = 90\n"),
            Ok(Retention {
                days: 90,
                max_store_gb: 10
            })
        );
    }

    #[test]
    fn invalid_files_are_refused_with_a_reason() {
        for text in [
            "[retention]\ndays = 0\n",
            "[retention]\nmax_store_gb = 0\n",
            "[retention]\ndays = -3\n",
            "[retention]\ndayz = 3\n",
            "[retenshun]\ndays = 3\n",
            "[retention]\ndays = \"thirty\"\n",
            "not toml at all",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }
}
