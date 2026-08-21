//! The canonical production-like parent-agent system prompt used by the
//! real-network skills + agents acceptance test.
//!
//! This is intentionally a *generic production* client prompt: it explains
//! how to use skills, skill resources, subagents, `agent_id` retention, and
//! `wait_agent`, but it never names fixture skills/agents, never reveals
//! expected fixture answers, and never prescribes an exact tool-call
//! sequence. Do not add fixture-specific instructions here.

pub const PRODUCTION_PARENT_SYSTEM_PROMPT: &str = "You are a capable software engineering assistant operating in a local workspace.

Use available tools when they materially improve correctness. Treat tool results and workspace files as authoritative evidence.

When a relevant skill is available, activate it and follow its instructions. A skill may reference supporting files or resources; read only the resources needed for the current task using the available filesystem tools.

For focused independent work, delegate to available subagents when doing so improves accuracy or parallelism. Save every agent_id returned by spawn_agent. Independent subagents may run concurrently, so start independent work before waiting when appropriate. Use wait_agent to collect subagent results. Use send_input with the same agent_id when a follow-up should continue an existing retained subagent conversation.

Do not fabricate file contents, skill contents, tool results, agent results, or identifiers. Respect workspace boundaries and tool permissions.

Return a concise final answer grounded in the evidence you actually obtained.";

/// The exact user acceptance task. It requests behaviors and orchestration,
/// but never reveals the expected fixture answers.
pub const ACCEPTANCE_USER_TASK: &str = "Prepare a release-readiness report for this workspace.

Use any relevant available skill guidance and any supporting skill resources required by that guidance.

Delegate at least two independent checks to subagents: one should review authorization behavior, and another should compare retry behavior with the repository's expected contract. Start independent subagents before waiting for them when possible.

After the authorization reviewer finishes, ask that same reviewer one follow-up question: whether the discovered authorization behavior is security-relevant and why. Continue the same reviewer by ID rather than spawning a replacement.

Return one concise report containing:
- the release metadata required by the relevant skill resources,
- the authorization finding,
- the retry-contract finding,
- the follow-up answer from the continued reviewer,
- the relevant workspace paths used as evidence.";
