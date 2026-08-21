//! Shared support for the skills MCP, agents MCP, and combined real-LLM
//! acceptance suites.
//!
//! Each test binary compiles this whole subtree, so items unused by one
//! binary are expected; `dead_code` is allowed at the module level.

#![allow(dead_code)]

pub mod fixture;
pub mod mcp_client;
pub mod openrouter;
pub mod production_prompt;
