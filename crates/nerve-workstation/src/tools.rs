use crate::xai;
use nerve_core::WorkspaceRegistry;
use nerve_runtime::{Runtime, RuntimeError, RuntimeToolAdapter};
use serde_json::Value;

pub(crate) type NerveRuntime = Runtime<WorkspaceRegistry>;

struct XaiToolAdapter;

impl RuntimeToolAdapter<WorkspaceRegistry> for XaiToolAdapter {
    fn tool_specs(&self) -> Vec<Value> {
        xai::tool_specs()
    }

    fn handle_tool_call(
        &self,
        registry: &WorkspaceRegistry,
        params: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        xai::handle_tool_call(registry, params)
            .map_err(|err| RuntimeError::adapter(err.to_string()))
    }
}

pub(crate) fn runtime(registry: WorkspaceRegistry) -> NerveRuntime {
    Runtime::new(registry).with_adapter(XaiToolAdapter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerve_core::{FsCatalogProvider, WorkspaceRegistry};
    use std::collections::HashSet;

    #[test]
    fn runtime_tool_specs_include_core_and_xai_tools() {
        let registry: WorkspaceRegistry<FsCatalogProvider> = WorkspaceRegistry::new();
        let runtime = runtime(registry);
        let specs = runtime.tool_specs();
        let names: Vec<_> = specs
            .as_array()
            .expect("tool specs array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        let mut seen = HashSet::new();
        for name in &names {
            assert!(seen.insert(*name), "duplicate tool spec: {name}");
        }
        assert!(names.contains(&"file_search"));
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"x_search"));
        assert!(names.contains(&"xai_responses"));
        assert!(names.contains(&"xai_x_search"));
    }
}
