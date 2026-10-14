# Contributing to Remora

Everyone taking part in the project is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security problems privately, as described in [SECURITY.md](SECURITY.md), never in a public issue.

## Setup

1. Install [rustup](https://rustup.rs).
2. Clone the repository. The toolchain version is pinned in `rust-toolchain.toml`, and `rustup` installs it with `rustfmt` and `clippy` the first time you run `cargo` in the repo.
3. Install `cargo-deny`:

   ```sh
   cargo install cargo-deny --locked
   ```

The design is in the [technical specification](docs/design/remora.md). Section 3.2 lists the crates and which crate may depend on which; `cargo xtask layers` enforces it.

## Checks

CI runs these on every pull request, except one that changes only Markdown files, `.releaserc.json` or `.github/workflows/release.yml`, which skips the build. Run them locally before pushing:

| Check | Command | CI job |
| --- | --- | --- |
| Formatting | `cargo fmt --all --check` | `linux` |
| Lints | `cargo clippy --workspace --all-targets --locked` | `linux`, `macos`, `windows` |
| Tests | `cargo test --workspace --locked` | `linux`, `macos`, `windows` |
| Docs | `cargo doc --workspace --no-deps --locked` with `RUSTDOCFLAGS=-D warnings` | `linux` |
| Crate layers | `cargo xtask layers` | `linux` |
| Dependency policy | `cargo deny check` | `linux` |

CI also sets `RUSTFLAGS=-D warnings`, so a warning fails the build. Set the same flags locally: `export RUSTFLAGS="-D warnings" RUSTDOCFLAGS="-D warnings"` in a Unix shell, or `$env:RUSTFLAGS="-D warnings"` and `$env:RUSTDOCFLAGS="-D warnings"` in PowerShell.

`linux` also lints the crates that compile no C code for the Windows and macOS targets. To run that locally:

```sh
rustup target add x86_64-pc-windows-msvc aarch64-apple-darwin
cargo clippy --locked --target x86_64-pc-windows-msvc -p remora-types -p remora-fs -p remora-ipc -p remora-host
cargo clippy --locked --target aarch64-apple-darwin -p remora-types -p remora-fs -p remora-ipc -p remora-host
```

## Code rules

- `unsafe` code is allowed only in the `remora_fs::sys` module, and every `unsafe` block carries a `// SAFETY:` comment.
- `unwrap()` and `expect()` are denied in product code. Tests, the `remora-testkit` fixtures and the `xtask` tooling may use them. An `expect` in product code that encodes an invariant needs `#[allow(clippy::expect_used)]` and a comment stating the invariant.
- Remora makes no network calls. `deny.toml` bans HTTP clients, TLS stacks and DNS resolvers.
- Each capability has one owning module (section 4 of the spec). Reuse it instead of writing a second version.

## Dependencies

`deny.toml` defines the dependency policy. Third-party dependencies must use one of the allowed licenses, come from crates.io, and have no open security advisories. Workspace crates depend on each other through `path` dependencies. A new dependency that fails `cargo deny check` needs a discussion in its pull request before the policy changes.

## Issues, milestones and labels

The [roadmap](ROADMAP.md) splits the work into milestones M0 to M7. Each deliverable has an ID such as `R1.4` and one issue; each milestone has a tracking issue that lists its deliverables and how they depend on each other. An issue's "blocked by" relationships list what has to land first.

Every issue has these labels:

| Prefix | Meaning | Example |
| --- | --- | --- |
| `kind/` | Kind of work: `feature`, `bug`, `test`, `docs`, `infra`, `spec`, `perf`, `security`, `tracking` | `kind/feature` |
| `area/` | Part of the code the work touches | `area/journal` |
| `platform/` | Only when the work is specific to one OS | `platform/windows` |

Reports from the issue forms also get `needs-triage` until a maintainer reviews them.

## Pull requests

- Keep one issue per pull request, and put `Closes #<number>` in the description.
- Write the title as a conventional commit with the roadmap ID at the end (see below). Pull requests are squash merged, and the title becomes the commit on `main`.
- Keep unrelated changes out, including drive-by formatting.
- Add or update tests for the behavior you change.
- The spec is the reference for behavior. A change that alters behavior updates `docs/design/remora.md` in the same pull request.
- Every commit must be signed.
- Make sure every check above passes.

## Commit messages

Pull request titles and commits follow [Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<scope>): <summary> (<roadmap ID>)
```

- `type` is one of `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `style` or `chore`.
- `scope` is the crate without the `remora-` prefix (`fs`, `journal`, `ipc`), and can be left out for repo-wide changes.
- The summary is in the imperative mood ("add", "fix", "remove"), lowercase, at most 72 characters, with no trailing period.
- Add a body after a blank line when the change needs explaining: what changed and why, wrapped at 72 characters.

```
feat(fs): write files atomically with fsync points (R0.7)

Write to a temporary file in the same directory, fsync it, rename it
over the target, then fsync the directory, so a crash leaves either
the old file or the new one.
```

## Contributor Licence Agreement

Every contributor signs the [Contributor Licence Agreement](CLA.md) once. You keep the copyright to your contribution, and Wolfware LLC can keep distributing the project, including under other licences.

When you open your first pull request, a bot comments with a link to the agreement. Reply with the sentence it asks for, and the `CLA` check passes. A pull request is merged only after every author has signed.

## License

By contributing, you agree that your contributions are dual licensed under the MIT and Apache 2.0 licenses, as described in the [README](README.md#license), and covered by the [Contributor Licence Agreement](CLA.md).
