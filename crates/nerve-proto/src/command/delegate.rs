//! Delegate-specific command payload types (DA-7): the autonomy posture and the
//! behavior-role preset carried by `delegate.start`. Split out of `command.rs` to
//! keep that file within the size budget; re-exported from there so the protocol
//! vocabulary path (`nerve_proto::DelegateAutonomy` / `DelegateRole`) is unchanged.

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Autonomy posture handed to a delegated external agent CLI, mapping to each
/// vendor's sandbox/permission flag: codex `--sandbox`, claude `--permission-mode`,
/// gemini `--approval-mode` (read-only | edit | full). Defaults to the most
/// restricted ([`Self::ReadOnly`]) so an omitted field never grants more than read
/// access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DelegateAutonomy {
    /// The delegated agent may only read; no edits, no command execution.
    #[default]
    ReadOnly,
    /// The delegated agent may read and edit workspace files.
    Edit,
    /// The delegated agent may read, edit, and run commands.
    Full,
}

/// The *role* a delegated agent plays (DA-7): a curated behavior preset the host
/// materializes on top of the raw agent CLI. Defaults to [`Self::Standard`] (no
/// preset — the `task`/`autonomy` are used verbatim).
///
/// [`Self::Scout`] is a read-only **repository-exploration** preset: the host
/// wraps the task in an explore-and-cite instruction and forces read-only
/// autonomy, so the agent returns compact `path:line-range` citations instead of
/// editing — a cheap context sub-agent that keeps the caller's context window
/// clean (the FastContext pattern, run on an existing CLI). The role is plain
/// vocabulary here; the host (`delegate_roles`) owns the prompt + posture it maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DelegateRole {
    /// No preset: the task and autonomy are passed through unchanged.
    #[default]
    Standard,
    /// Read-only repository-exploration preset — forces read-only autonomy and an
    /// explore-and-cite instruction (see the type docs).
    Scout,
}

impl DelegateRole {
    /// Whether this is the default ([`Self::Standard`]) — used to keep an unset
    /// `role` off the wire (serde `skip_serializing_if`).
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Standard)
    }
}
