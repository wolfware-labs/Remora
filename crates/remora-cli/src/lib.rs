//! `remora`: records what Claude Code does to a git repository and rewinds it.
//!
//! `remora init` installs Claude Code hooks that run `remora hook claude-code`; each hook
//! forwards its event to the `remorad` daemon and always lets the agent continue.

pub mod claude;
pub mod root;
