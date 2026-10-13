//! `cargo xtask layers`: the crate layering check (spec §3.2, rule 4).
//!
//! Every dependency edge between workspace crates must appear in [`ALLOWED`], and every
//! feature an edge enables must appear in [`ALLOWED_FEATURES`]. Dev-dependency edges to
//! `remora-testkit` are always allowed. Normal and build edges must carry a `version`
//! next to their `path` so the crates can be published; dev edges to the unpublished
//! test kit must not.

use std::collections::BTreeSet;
use std::fmt;

use cargo_metadata::semver::VersionReq;
use cargo_metadata::{DependencyKind, Metadata};

/// The dev-only fixture crate. Its own edges are not restricted.
pub const TESTKIT: &str = "remora-testkit";

/// Each workspace crate and the workspace crates it may depend on.
pub const ALLOWED: &[(&str, &[&str])] = &[
    ("remora-types", &[]),
    ("remora-fs", &["remora-types"]),
    ("remora-store", &["remora-types", "remora-fs"]),
    ("remora-journal", &["remora-types", "remora-fs"]),
    (
        "remora-engine",
        &[
            "remora-store",
            "remora-journal",
            "remora-fs",
            "remora-types",
        ],
    ),
    ("remora-ipc", &["remora-types", "remora-fs"]),
    (
        "remora-daemon",
        &[
            "remora-engine",
            "remora-ipc",
            "remora-store",
            "remora-journal",
            "remora-fs",
            "remora-types",
        ],
    ),
    ("remora-host", &["remora-types", "remora-ipc"]),
    (
        "remora-cli",
        &[
            "remora-daemon",
            "remora-host",
            "remora-engine",
            "remora-ipc",
            "remora-store",
            "remora-journal",
            "remora-fs",
            "remora-types",
        ],
    ),
    ("xtask", &[]),
];

/// Features an edge may enable, as `(from, to, feature)`. Any other feature on an
/// edge between workspace crates fails.
pub const ALLOWED_FEATURES: &[(&str, &str, &str)] = &[
    ("remora-store", "remora-fs", "sqlite"),
    ("remora-journal", "remora-fs", "sqlite"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A workspace crate that the table does not list.
    UnknownCrate,
    ForbiddenEdge,
    ForbiddenFeature(String),
    /// A normal or build edge with `path` but no `version`.
    MissingVersion,
    /// A dev edge to the test kit with a `version`, which would make it publishable.
    VersionOnTestkit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub target: Option<String>,
    pub problem: Problem,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.problem == Problem::UnknownCrate {
            return write!(f, "{}: crate is not in the layering table", self.from);
        }
        write!(f, "{} -> {}", self.from, self.to)?;
        if let Problem::ForbiddenFeature(feature) = &self.problem {
            write!(f, "/{feature}")?;
        }
        write!(f, " ({}", self.kind)?;
        if let Some(target) = &self.target {
            write!(f, ", {target}")?;
        }
        let what = match self.problem {
            Problem::UnknownCrate => unreachable!(),
            Problem::ForbiddenEdge => "forbidden edge",
            Problem::ForbiddenFeature(_) => "forbidden feature",
            Problem::MissingVersion => "missing version next to path",
            Problem::VersionOnTestkit => "test kit edge must have a path only",
        };
        write!(f, "): {what}")
    }
}

fn allowed_deps(name: &str) -> Option<&'static [&'static str]> {
    ALLOWED
        .iter()
        .find(|(crate_name, _)| *crate_name == name)
        .map(|(_, deps)| *deps)
}

fn kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Development => "dev",
        DependencyKind::Build => "build",
        _ => "normal",
    }
}

/// Checks every edge between workspace crates and returns the violations found.
pub fn check(metadata: &Metadata) -> Vec<Violation> {
    let packages = metadata.workspace_packages();
    let members: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let mut violations = Vec::new();

    for package in packages {
        let from = package.name.as_str();
        if from == TESTKIT {
            continue;
        }
        let Some(allowed) = allowed_deps(from) else {
            violations.push(Violation {
                from: from.to_owned(),
                to: String::new(),
                kind: "normal",
                target: None,
                problem: Problem::UnknownCrate,
            });
            continue;
        };

        for dep in &package.dependencies {
            let to = dep.name.as_str();
            if dep.path.is_none() || !members.contains(to) {
                continue;
            }
            let violation = |problem| Violation {
                from: from.to_owned(),
                to: to.to_owned(),
                kind: kind_name(dep.kind),
                target: dep.target.as_ref().map(ToString::to_string),
                problem,
            };
            let path_only = dep.req == VersionReq::STAR;

            if dep.kind == DependencyKind::Development && to == TESTKIT {
                if !path_only {
                    violations.push(violation(Problem::VersionOnTestkit));
                }
                continue;
            }
            if !allowed.contains(&to) {
                violations.push(violation(Problem::ForbiddenEdge));
                continue;
            }
            for feature in &dep.features {
                if !ALLOWED_FEATURES.contains(&(from, to, feature.as_str())) {
                    violations.push(violation(Problem::ForbiddenFeature(feature.clone())));
                }
            }
            if dep.kind != DependencyKind::Development && path_only {
                violations.push(violation(Problem::MissingVersion));
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::{Value, json};

    use super::*;

    /// `cargo metadata --no-deps` for this workspace, as JSON, fetched once.
    fn workspace_json() -> Value {
        static JSON: OnceLock<Value> = OnceLock::new();
        JSON.get_or_init(|| {
            let output = crate::metadata_command()
                .cargo_command()
                .output()
                .expect("run cargo metadata");
            assert!(output.status.success(), "cargo metadata failed");
            serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
        })
        .clone()
    }

    fn package<'a>(meta: &'a mut Value, name: &str) -> &'a mut Value {
        meta["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no package {name}"))
    }

    /// Adds a path dependency `from -> to` to the metadata.
    fn add_dep(
        meta: &mut Value,
        from: &str,
        to: &str,
        kind: Option<&str>,
        version: bool,
        target: Option<&str>,
    ) {
        let path = format!("{}/crates/{to}", meta["workspace_root"].as_str().unwrap());
        let dep = json!({
            "name": to,
            "source": null,
            "req": if version { "^0.0.0" } else { "*" },
            "kind": kind,
            "rename": null,
            "optional": false,
            "uses_default_features": true,
            "features": [],
            "target": target,
            "registry": null,
            "path": path,
        });
        package(meta, from)["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(dep);
    }

    fn run(meta: Value) -> Vec<Violation> {
        check(&serde_json::from_value(meta).expect("metadata shape"))
    }

    fn single(meta: Value) -> Violation {
        let violations = run(meta);
        assert_eq!(violations.len(), 1, "{violations:?}");
        violations.into_iter().next().unwrap()
    }

    #[test]
    fn real_workspace_passes() {
        assert_eq!(run(workspace_json()), vec![]);
    }

    #[test]
    fn table_lists_every_workspace_crate() {
        let meta = workspace_json();
        for package in meta["packages"].as_array().unwrap() {
            let name = package["name"].as_str().unwrap();
            assert!(name == TESTKIT || allowed_deps(name).is_some(), "{name}");
        }
    }

    #[test]
    fn forbidden_normal_edge_fails() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-ipc", "remora-engine", None, true, None);
        let v = single(meta);
        assert_eq!(v.problem, Problem::ForbiddenEdge);
        assert_eq!(
            v.to_string(),
            "remora-ipc -> remora-engine (normal): forbidden edge"
        );
    }

    #[test]
    fn upward_edge_fails() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-types", "remora-fs", None, true, None);
        assert_eq!(single(meta).problem, Problem::ForbiddenEdge);
    }

    #[test]
    fn sqlite_feature_from_ipc_fails() {
        let mut meta = workspace_json();
        let ipc = package(&mut meta, "remora-ipc");
        let fs = ipc["dependencies"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|d| d["name"] == "remora-fs")
            .unwrap();
        fs["features"] = json!(["sqlite"]);
        let v = single(meta);
        assert_eq!(v.problem, Problem::ForbiddenFeature("sqlite".into()));
        assert_eq!(
            v.to_string(),
            "remora-ipc -> remora-fs/sqlite (normal): forbidden feature"
        );
    }

    #[test]
    fn forbidden_edge_under_target_table_fails() {
        let mut meta = workspace_json();
        add_dep(
            &mut meta,
            "remora-host",
            "remora-daemon",
            None,
            true,
            Some("cfg(windows)"),
        );
        let v = single(meta);
        assert_eq!(v.problem, Problem::ForbiddenEdge);
        assert!(v.to_string().contains("cfg(windows)"), "{v}");
    }

    #[test]
    fn build_edge_outside_table_fails() {
        let mut meta = workspace_json();
        add_dep(
            &mut meta,
            "remora-fs",
            "remora-store",
            Some("build"),
            true,
            None,
        );
        let v = single(meta);
        assert_eq!((v.kind, v.problem), ("build", Problem::ForbiddenEdge));
    }

    #[test]
    fn dev_edge_to_testkit_passes() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-ipc", TESTKIT, Some("dev"), false, None);
        assert_eq!(run(meta), vec![]);
    }

    #[test]
    fn dev_edge_to_testkit_with_version_fails() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-ipc", TESTKIT, Some("dev"), true, None);
        assert_eq!(single(meta).problem, Problem::VersionOnTestkit);
    }

    #[test]
    fn dev_edge_outside_table_fails() {
        let mut meta = workspace_json();
        add_dep(
            &mut meta,
            "remora-ipc",
            "remora-engine",
            Some("dev"),
            false,
            None,
        );
        let v = single(meta);
        assert_eq!((v.kind, v.problem), ("dev", Problem::ForbiddenEdge));
    }

    #[test]
    fn normal_edge_to_testkit_fails() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-engine", TESTKIT, None, false, None);
        assert_eq!(single(meta).problem, Problem::ForbiddenEdge);
    }

    #[test]
    fn normal_edge_without_version_fails() {
        let mut meta = workspace_json();
        add_dep(&mut meta, "remora-host", "remora-types", None, false, None);
        assert_eq!(single(meta).problem, Problem::MissingVersion);
    }

    #[test]
    fn unknown_crate_fails() {
        let mut meta = workspace_json();
        let mut extra = package(&mut meta, "remora-types").clone();
        let id = format!("{}-extra", extra["id"].as_str().unwrap());
        extra["name"] = json!("remora-extra");
        extra["id"] = json!(id);
        meta["packages"].as_array_mut().unwrap().push(extra);
        meta["workspace_members"]
            .as_array_mut()
            .unwrap()
            .push(json!(id));
        let v = single(meta);
        assert_eq!(v.problem, Problem::UnknownCrate);
        assert_eq!(
            v.to_string(),
            "remora-extra: crate is not in the layering table"
        );
    }
}
