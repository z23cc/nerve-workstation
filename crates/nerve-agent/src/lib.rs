//! `nerve-agent` — a synchronous, multi-provider agentic runtime.
//!
//! This crate wraps the LLM providers (Anthropic, OpenAI Responses, xAI) behind
//! a single [`provider::LlmProvider`] trait, exposes nerve's tools through a
//! [`provider::ToolBox`], and drives a tool-use loop in [`orchestrator`].
//!
//! It is deliberately blocking (`ureq` v3 + std threads); there is no async
//! runtime. Credentials and OAuth flows live under [`auth`], and the
//! provider-neutral wire types live in [`message`].

pub mod auth;
pub mod error;
pub mod message;
pub mod orchestrator;
pub mod provider;

pub use auth::{Credential, ProviderId};
pub use error::{AgentError, AgentResult};
pub use message::*;
pub use orchestrator::{
    AgentDef, AgentEvent, Hook, ModelCapabilities, Orchestrator, ResumeState, RunOutcome,
};
pub use provider::{LlmProvider, ToolBox};
