use crate::RuntimeError;
use nerve_core::dispatch::DispatchProvider;
use nerve_core::{CancelToken, WorkspaceResolver};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Coarse risk classification for a tool, ordered least-to-most privileged.
/// Advisory protocol data consumed by a future permission engine (P4); it does
/// not gate execution today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// Pure reads: catalog/search/navigation that never mutate state.
    ReadOnly,
    /// Mutates workspace files (writes, patches, moves, deletes).
    Edit,
    /// Runs arbitrary commands or otherwise escapes the file sandbox.
    Exec,
}

/// Declared capabilities and risk surface of a runtime tool. Advisory only:
/// transport-neutral data a permission/UI layer can reason about. The default
/// is intentionally the *most permissive* so adapters that don't declare a
/// capability are never silently restricted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapability {
    pub risk: RiskTier,
    pub reads_fs: bool,
    pub writes_fs: bool,
    pub network: bool,
}

impl Default for ToolCapability {
    /// Most permissive default: highest risk, all surfaces enabled, so an
    /// adapter that hasn't opted into a narrower descriptor is treated as
    /// fully capable (non-breaking for existing adapters).
    fn default() -> Self {
        Self {
            risk: RiskTier::Exec,
            reads_fs: true,
            writes_fs: true,
            network: true,
        }
    }
}

/// Extension point for host-specific or provider-specific runtime capabilities.
///
/// Adapters are consulted in registration order. The first adapter returning
/// `Ok(Some(_))` claims the tool call. Returning `Ok(None)` means the adapter
/// does not own the requested tool, so runtime dispatch continues.
pub trait RuntimeToolAdapter<R>: Send + Sync
where
    R: WorkspaceResolver,
    R::Provider: DispatchProvider,
{
    /// Tool specifications exposed by this adapter.
    fn tool_specs(&self) -> Vec<Value>;

    /// Try to handle one MCP-style `tools/call` params object.
    fn handle_tool_call(&self, resolver: &R, params: &Value)
    -> Result<Option<Value>, RuntimeError>;

    /// Try to handle one MCP-style `tools/call` params object with cooperative cancellation.
    fn handle_tool_call_cancellable(
        &self,
        resolver: &R,
        params: &Value,
        cancel: &CancelToken,
    ) -> Result<Option<Value>, RuntimeError> {
        if cancel.is_cancelled() {
            return Err(RuntimeError::cancelled());
        }
        self.handle_tool_call(resolver, params)
    }

    /// Declared capability/risk descriptor for the named tool. Advisory only;
    /// defaults to the most permissive [`ToolCapability`] so existing adapters
    /// are unaffected. A future permission engine (P4) consults this.
    fn tool_capability(&self, _name: &str) -> ToolCapability {
        ToolCapability::default()
    }

    /// Whether this adapter owns (claims) the named tool. Defaults to `false`
    /// so the descriptor query is opt-in and never changes dispatch ordering,
    /// which still relies on `handle_tool_call` returning `Ok(Some(_))`.
    fn owns(&self, _name: &str) -> bool {
        false
    }
}
