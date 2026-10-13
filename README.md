# Remora

Remora records what an AI coding agent does to your project and lets you rewind it.

Each prompt you give the agent, and each shell command it runs, gets a restore point: a
snapshot of the project taken just before. If the agent deletes, rewrites or breaks something,
`remora rewind` puts the files back as they were at that point. Every turn is also written to
a signed, hash-chained journal on your machine, so you can see later what happened.

Remora works with [Claude Code](https://docs.claude.com/en/docs/claude-code) on macOS, Linux
and Windows. It needs no account, sends nothing anywhere and makes no network calls.

## What it can and cannot undo

Remora restores **files in your project folder**. It cannot undo what a command did outside it:
a `git push`, a deployment, a database write, an email sent, an API call. `remora rewind` lists
the commands that ran after the point you rewind to, so you can review those yourself.

## Install

| Platform | Command |
|----------|---------|
| macOS, Linux (Homebrew) | `brew install wolfware-labs/tap/remora` |
| Windows (winget) | `winget install Wolfware.Remora` |
| Any (Rust toolchain) | `cargo install remora-cli remora-daemon` |

Or download an archive for your platform from the
[releases page](https://github.com/wolfware-labs/remora/releases): it holds `remora` and
`remorad`. Put both in a folder on your `PATH`.

To check a download, compare it with `SHA256SUMS` from the same release, and check where it was
built with the [GitHub CLI](https://cli.github.com):

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify remora-<version>-<platform>.tar.gz --repo wolfware-labs/remora
```

## Five-minute start

```sh
remora init        # installs Claude Code hooks for your user, and starts the daemon at login
```

Then, in a git repository, use Claude Code as usual. Every prompt and every Bash command it
runs leaves a restore point.

```sh
remora log         # restore points, #1 newest
remora rewind 3    # shows what would change, asks, then puts the files back as they were at #3
```

A rewind is itself recorded, so you can undo it: it prints the snapshot to go back to
(`remora rewind --snapshot <id>`).

`remora status` shows what is recorded for the current project, whether the daemon runs, and
whether the hooks and the login service point at the `remora` you have installed.

## Commands

| Command | What it does |
|---------|--------------|
| `remora init` / `remora uninit` | Install the Claude Code hooks and the login service / remove the hooks (`remora service uninstall` removes the service) |
| `remora status` | The daemon, the hooks, and what is recorded for this project |
| `remora log` | Restore points, newest first (`--all`, `-n`, `--json`) |
| `remora rewind <n>` | Rewind to restore point `n` (`--yes` to skip the question) |
| `remora prune` | Remove restore points the retention policy no longer keeps (`--dry-run`) |
| `remora forget` | Remove everything Remora recorded for this project |
| `remora verify` | Check the journal for tampering |
| `remora service install` / `uninstall` / `status` | Start the daemon at login, or stop doing so |

Commands work on the git repository around the current folder; `--root <folder>` picks another.

## How it works

- **Hooks.** `remora init` adds hooks to Claude Code's user settings. On each prompt and each
  Bash command, Claude Code runs `remora hook claude-code`, which hands the event to the daemon
  and lets the agent continue.
- **Daemon.** `remorad` takes the snapshots and writes the journal. It starts at login, or the
  first hook starts it. After an upgrade, the first newer `remora` replaces the old daemon.
- **Snapshots.** Files are stored by content, so unchanged files cost nothing. On filesystems
  that can clone files (APFS, btrfs, XFS, ReFS), they are cloned instead of copied. A project on
  another volume keeps its stored files on that volume, in a private `.remora-store` folder.
- **Journal.** Each turn and each rewind is an entry in a SQLite journal, chained by hash and
  signed with a key kept on your machine. `remora verify` checks the chain.
- **Crashes.** A rewind cut short by a crash or power loss is rolled back the next time the
  project opens; Claude Code is told once.

`node_modules`, `target` and `.venv` folders are not recorded, nor files over 100 MB.

## Configuration

Remora keeps restore points for **30 days** and at most **10 GB** of stored files per project,
then removes the oldest. To change that, create `config.toml` in the data folder and restart the
daemon (`remora service install`):

```toml
[retention]
days = 30
max_store_gb = 10
```

The newest restore point, the points of a turn in progress and the state before the latest
rewind are never removed.

The data folder is `~/.local/share/remora` on Linux, `~/Library/Application Support/remora` on
macOS and `%LOCALAPPDATA%\remora` on Windows (`REMORA_DATA_DIR` overrides it).

## Performance

A hook adds little to each step of the agent. CI checks that the 99th percentile of 200 calls
stays under 50 ms, both for a hook that takes no snapshot and for a checkpoint of an unchanged
1,000-file project.

Benchmarks (`cargo bench -p remora-snap -p remora-engine --bench snapshot --bench hook`):

| Operation | 1,000 files | 10,000 files |
|-----------|------------:|-------------:|
| First snapshot (every file stored) | 103 ms | 1.03 s |
| Snapshot of an unchanged project | 10 ms | 130 ms |
| Snapshot after 1% of files changed | 11 ms | 137 ms |
| Rewind after every file changed | 100 ms | |
| Hook: checkpoint before a command | 12 ms | |
| Hook: turn start (closes the previous turn) | 26 ms | |

Measured on Linux with an Intel Core i9-11950H, on a RAM-backed filesystem (tmpfs), files of
about 4 KB. Run the `bench` workflow for macOS and Windows figures.

## Uninstall

```sh
remora uninit                 # remove the Claude Code hooks
remora service uninstall      # stop starting the daemon at login, and stop it
remora forget                 # (optional) remove what was recorded for a project
```

Then remove the package (`brew uninstall remora`, `winget uninstall Wolfware.Remora`,
`cargo uninstall remora-cli remora-daemon`) and, if you want, the data folder.

## Known limits

- Claude Code is the only agent recorded so far.
- On macOS, a cloned file keeps any access control list (ACL) the original had.
- Paths that are not valid UTF-8 are not recorded (`remora status` lists them).

## Security

See [SECURITY.md](SECURITY.md) to report a vulnerability.

## License

Remora is licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
