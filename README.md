# Remora

Remora records what an AI coding agent does to a project's files and lets you rewind the project to any recorded point. It captures at the filesystem level, so changes made by shell commands (`rm`, `mv`, `sed -i`, build tools) are restored as well as changes made by the agent's own edit tools. Every turn goes into a signed, hash-chained journal. It runs locally, makes no network calls and sends no telemetry.

The first host is [Claude Code](https://code.claude.com/docs/en/overview), through its hooks. Linux, macOS and Windows are supported.

## Status

Remora is in early development and has no release yet. Nothing here is ready to install.

- [Technical specification](docs/design/remora.md): the design v1 is built against.
- [Roadmap](ROADMAP.md): milestones M0 to M7 and their deliverables, each tracked as a [GitHub issue](https://github.com/wolfware-labs/remora/issues).

## Building

Requires Rust 1.99 or later; `rust-toolchain.toml` selects the exact version.

```sh
cargo build --workspace
cargo test --workspace
cargo xtask layers
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Contributions are dual licensed as above and require signing the [Contributor Licence Agreement](CLA.md); see [CONTRIBUTING.md](CONTRIBUTING.md#contributor-licence-agreement).
