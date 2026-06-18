//! Capabilities-as-data: named agent definitions and skills loaded from disk.
//!
//! Architecture north star P3 (`docs/designs/architecture-north-star.md` §6.3
//! "capabilities — data as plugin", §7.5 skill/agent-def contract): named agents
//! and skills are *data*, discovered from directories with no recompile.
//! Precedence is **project > global > built-in**, so a project can shadow a
//! global or built-in definition.
//!
//! - **Agent definitions** are JSON files (`<name>.json`) mapping to the same
//!   fields the orchestrator's `AgentDef` exposes, plus a `skills` list. JSON is
//!   used for consistency with `--mcp-config` / `--provider-config` and to avoid
//!   adding a YAML dependency (the build stays offline-safe).
//! - **Skills** are plain markdown files (`<name>.md`); an agent's listed skills
//!   are composed into its system prompt, in order, after the base prompt.
//!
//! Discovery base directories, highest precedence first — each holds `agents/`
//! and `skills/` subdirectories:
//! - project: `<root>/.nerve/`
//! - global:  `config_home()` (`$NERVE_HOME` / `$XDG_CONFIG_HOME/nerve` / OS config dir)
//! - built-in: embedded defaults (`BUILTIN_AGENTS` / `BUILTIN_SKILLS`).
//!
//! Composition happens here, at the binary (the composition root); the
//! orchestrator stays unaware of skills/agents.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Built-in agent definitions, embedded so a fresh install has a working
/// `--agent`. Each entry is `(name, raw-json)`; consulted only after the project
/// and global directories.
const BUILTIN_AGENTS: &[(&str, &str)] = &[("coder", include_str!("../assets/agents/coder.json"))];

/// Built-in skills, embedded markdown. Consulted only after the project and
/// global directories.
const BUILTIN_SKILLS: &[(&str, &str)] = &[(
    "nerve-tools",
    include_str!("../assets/skills/nerve-tools.md"),
)];

/// On-disk shape of an agent-definition JSON file. Every field is optional: an
/// agent may rely entirely on CLI flags and/or skills. The file name (without
/// `.json`) is the agent's name.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AgentDefFile {
    /// Base system prompt; skill bodies are appended after it.
    pub(crate) system_prompt: Option<String>,
    /// Model id (overridable by `--model`).
    pub(crate) model: Option<String>,
    /// Provider name (overridable by `--provider`).
    pub(crate) provider: Option<String>,
    /// Maximum agent turns (overridable by `--max-turns`).
    pub(crate) max_turns: Option<u32>,
    /// Sampling temperature (overridable by `--temperature`).
    pub(crate) temperature: Option<f32>,
    /// Reasoning-effort hint (overridable by `--reasoning-effort`).
    pub(crate) reasoning_effort: Option<String>,
    /// Optional tool allowlist passed through to the orchestrator.
    pub(crate) tool_filter: Option<Vec<String>>,
    /// Skill names to compose into the system prompt, in order.
    #[serde(default)]
    pub(crate) skills: Vec<String>,
}

/// A fully resolved agent: its definition with all referenced skills composed
/// into `system_prompt`. The CLI applies explicit flag overrides on top of this.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedAgent {
    /// Composed system prompt (base prompt followed by each skill body), or
    /// `None` when the definition supplies neither — leaving the caller's default.
    pub(crate) system_prompt: Option<String>,
    /// Model id from the definition, if any.
    pub(crate) model: Option<String>,
    /// Provider name from the definition, if any.
    pub(crate) provider: Option<String>,
    /// Turn cap from the definition, if any.
    pub(crate) max_turns: Option<u32>,
    /// Sampling temperature from the definition, if any.
    pub(crate) temperature: Option<f32>,
    /// Reasoning-effort hint from the definition, if any.
    pub(crate) reasoning_effort: Option<String>,
    /// Tool allowlist from the definition, if any.
    pub(crate) tool_filter: Option<Vec<String>>,
}

/// Resolves named agents and skills from a precedence-ordered set of base
/// directories, falling back to embedded built-ins.
///
/// Each base directory holds `agents/` and `skills/` subdirectories. Bases are
/// stored highest-precedence first (project before global); the first match
/// wins, and built-ins are consulted last.
pub(crate) struct Capabilities {
    bases: Vec<PathBuf>,
}

impl Capabilities {
    /// Build the standard discovery chain: project (`<root>/.nerve`) then global
    /// (`config_home()`). A missing config home is skipped rather than failing —
    /// built-ins still resolve.
    pub(crate) fn discover(project_dir: Option<&Path>) -> Self {
        let mut bases = Vec::new();
        if let Some(root) = project_dir {
            bases.push(root.join(".nerve"));
        }
        if let Ok(home) = nerve_agent::auth::config_home() {
            bases.push(home);
        }
        Self { bases }
    }

    /// Construct from explicit base directories (highest precedence first),
    /// bypassing environment-derived discovery. Test-only.
    #[cfg(test)]
    fn from_bases(bases: Vec<PathBuf>) -> Self {
        Self { bases }
    }

    /// Resolve `name` to a [`ResolvedAgent`], composing its skills into the
    /// system prompt. Errors if the agent or any referenced skill is missing.
    pub(crate) fn resolve_agent(&self, name: &str) -> Result<ResolvedAgent> {
        let def = self.load_agent_def(name)?;
        let system_prompt = self.compose_system_prompt(&def)?;
        Ok(ResolvedAgent {
            system_prompt,
            model: def.model,
            provider: def.provider,
            max_turns: def.max_turns,
            temperature: def.temperature,
            reasoning_effort: def.reasoning_effort,
            tool_filter: def.tool_filter,
        })
    }

    /// Concatenate the base prompt and each referenced skill body, separated by
    /// blank lines. Returns `None` when nothing contributes text.
    fn compose_system_prompt(&self, def: &AgentDefFile) -> Result<Option<String>> {
        let mut sections: Vec<String> = Vec::new();
        if let Some(base) = def.system_prompt.as_deref() {
            let base = base.trim();
            if !base.is_empty() {
                sections.push(base.to_string());
            }
        }
        for skill in &def.skills {
            let body = self.load_skill(skill)?;
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                sections.push(trimmed.to_string());
            }
        }
        if sections.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sections.join("\n\n")))
        }
    }

    /// Load and parse the agent definition `name`, honoring precedence.
    fn load_agent_def(&self, name: &str) -> Result<AgentDefFile> {
        validate_name(name)?;
        for base in &self.bases {
            let path = base.join("agents").join(format!("{name}.json"));
            if path.is_file() {
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read agent def: {}", path.display()))?;
                return parse_agent_def(&raw, &path.display().to_string());
            }
        }
        if let Some(raw) = builtin(BUILTIN_AGENTS, name) {
            return parse_agent_def(raw, &format!("<built-in agent {name}>"));
        }
        bail!("unknown agent '{name}': not found in project (.nerve/agents), global, or built-ins");
    }

    /// Load the markdown body of skill `name`, honoring precedence.
    fn load_skill(&self, name: &str) -> Result<String> {
        validate_name(name)?;
        for base in &self.bases {
            let path = base.join("skills").join(format!("{name}.md"));
            if path.is_file() {
                return std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read skill: {}", path.display()));
            }
        }
        if let Some(raw) = builtin(BUILTIN_SKILLS, name) {
            return Ok(raw.to_string());
        }
        bail!("skill '{name}' not found in project (.nerve/skills), global, or built-ins");
    }
}

/// Parse an agent definition from raw JSON, tagging errors with `source`.
fn parse_agent_def(raw: &str, source: &str) -> Result<AgentDefFile> {
    serde_json::from_str(raw).with_context(|| format!("failed to parse agent def: {source}"))
}

/// Look up an embedded built-in by name.
fn builtin(table: &[(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, raw)| *raw)
}

/// Reject names that could escape the discovery directories or are empty. Names
/// are simple identifiers — ASCII alphanumerics plus `-` and `_` (no path
/// separators or dots) — so `<name>.json` / `<name>.md` always stays in-dir.
fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        bail!("invalid capability name '{name}': use only letters, digits, '-' and '_'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: PathBuf, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn agent_file(base: &Path, name: &str, json: &str) {
        write(base.join("agents").join(format!("{name}.json")), json);
    }

    fn skill_file(base: &Path, name: &str, md: &str) {
        write(base.join("skills").join(format!("{name}.md")), md);
    }

    #[test]
    fn builtin_agent_composes_skill() {
        let caps = Capabilities::from_bases(vec![]);
        let resolved = caps.resolve_agent("coder").expect("resolve coder");
        let prompt = resolved.system_prompt.expect("system prompt");
        // Base prompt and the composed built-in skill are both present.
        assert!(prompt.contains("You are Coder"));
        assert!(prompt.contains("Nerve tools"));
        // Built-in coder leaves model/provider to the CLI.
        assert!(resolved.model.is_none());
        assert!(resolved.provider.is_none());
    }

    #[test]
    fn unknown_agent_errors() {
        let err = Capabilities::from_bases(vec![])
            .resolve_agent("does-not-exist")
            .expect_err("should fail");
        assert!(err.to_string().contains("unknown agent"));
    }

    #[test]
    fn project_overrides_builtin() {
        let dir = tempdir().unwrap();
        agent_file(dir.path(), "coder", r#"{"system_prompt":"PROJECT CODER"}"#);
        let caps = Capabilities::from_bases(vec![dir.path().to_path_buf()]);
        let prompt = caps.resolve_agent("coder").unwrap().system_prompt.unwrap();
        // Project file fully shadows the built-in (no composed built-in skill).
        assert_eq!(prompt, "PROJECT CODER");
        assert!(!prompt.contains("Nerve tools"));
    }

    #[test]
    fn project_base_overrides_global_base() {
        let project = tempdir().unwrap();
        let global = tempdir().unwrap();
        agent_file(project.path(), "foo", r#"{"system_prompt":"FROM PROJECT"}"#);
        agent_file(global.path(), "foo", r#"{"system_prompt":"FROM GLOBAL"}"#);
        let caps = Capabilities::from_bases(vec![
            project.path().to_path_buf(),
            global.path().to_path_buf(),
        ]);
        assert_eq!(
            caps.resolve_agent("foo").unwrap().system_prompt.unwrap(),
            "FROM PROJECT"
        );
    }

    #[test]
    fn skill_override_and_order_compose() {
        let dir = tempdir().unwrap();
        agent_file(
            dir.path(),
            "multi",
            r#"{"system_prompt":"HEAD","skills":["nerve-tools","extra"]}"#,
        );
        // Project skill shadows the built-in nerve-tools skill.
        skill_file(dir.path(), "nerve-tools", "OVERRIDE TOOLS");
        skill_file(dir.path(), "extra", "EXTRA SKILL");
        let prompt = Capabilities::from_bases(vec![dir.path().to_path_buf()])
            .resolve_agent("multi")
            .unwrap()
            .system_prompt
            .unwrap();
        // Base first, then skills in listed order, blank-line separated.
        assert_eq!(prompt, "HEAD\n\nOVERRIDE TOOLS\n\nEXTRA SKILL");
    }

    #[test]
    fn missing_skill_errors() {
        let dir = tempdir().unwrap();
        agent_file(dir.path(), "broken", r#"{"skills":["ghost"]}"#);
        let err = Capabilities::from_bases(vec![dir.path().to_path_buf()])
            .resolve_agent("broken")
            .expect_err("should fail");
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn def_fields_pass_through() {
        let dir = tempdir().unwrap();
        agent_file(
            dir.path(),
            "tuned",
            r#"{"model":"m1","provider":"p1","max_turns":7,"temperature":0.25,"reasoning_effort":"high","tool_filter":["read_file","edit"]}"#,
        );
        let resolved = Capabilities::from_bases(vec![dir.path().to_path_buf()])
            .resolve_agent("tuned")
            .unwrap();
        assert_eq!(resolved.model.as_deref(), Some("m1"));
        assert_eq!(resolved.provider.as_deref(), Some("p1"));
        assert_eq!(resolved.max_turns, Some(7));
        assert!((resolved.temperature.unwrap() - 0.25).abs() < 1e-6);
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            resolved.tool_filter,
            Some(vec!["read_file".to_string(), "edit".to_string()])
        );
        // No system_prompt / skills -> default left to the caller.
        assert!(resolved.system_prompt.is_none());
    }

    #[test]
    fn invalid_names_are_rejected() {
        let caps = Capabilities::from_bases(vec![]);
        for bad in ["../evil", "a/b", "", "dots.here", "back\\slash"] {
            assert!(
                caps.resolve_agent(bad).is_err(),
                "expected '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn empty_def_yields_no_prompt() {
        let dir = tempdir().unwrap();
        agent_file(dir.path(), "blank", "{}");
        let resolved = Capabilities::from_bases(vec![dir.path().to_path_buf()])
            .resolve_agent("blank")
            .unwrap();
        assert!(resolved.system_prompt.is_none());
        assert!(resolved.model.is_none());
    }
}
