use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use schemars::JsonSchema;
use serde::Serialize;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{
    activity::{
        ActivityPhase, ActivityReporter, ActivitySnapshot, AgentActivity, AgentActivityEvent,
        millis,
    },
    child_mcp::ChildMcpManager,
    definition::AgentDefinition,
    discovery::AgentRegistry,
    provider::{ConversationState, ProviderClient, ProviderCredential},
    timeouts::{MAX_WAIT_AGENT_TIMEOUT_MS, RUNTIME_SHUTDOWN_TIMEOUT},
};
use crate::skills::SkillRegistry;

const QUEUE_LIMIT: usize = 16;
pub(crate) const MAX_RETAINED_TERMINAL_SESSIONS: usize = 64;

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct RuntimeError {
    pub(crate) kind: String,
    pub(crate) message: String,
}
impl RuntimeError {
    pub(crate) fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    Running,
    Completed,
    Failed,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct AgentResult {
    #[serde(rename = "agent_id")]
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(rename = "status")]
    pub(crate) state: AgentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<RuntimeError>,
    pub(crate) total_elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<ActivitySnapshot>,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct SpawnResult {
    #[serde(rename = "agent_id")]
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(rename = "status")]
    pub(crate) state: AgentState,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct InputAck {
    #[serde(rename = "agent_id")]
    pub(crate) id: String,
    pub(crate) accepted: bool,
    #[serde(rename = "status")]
    pub(crate) state: AgentState,
}
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(crate) struct WaitResult {
    pub(crate) agents: Vec<AgentResult>,
    pub(crate) timed_out: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct WaitObservation {
    pub(crate) result: WaitResult,
    pub(crate) wait_timeout_remaining_ms: u64,
}

struct SessionData {
    conversation: Option<ConversationState>,
    queue: VecDeque<String>,
    cancel: Option<CancellationToken>,
    interrupt_pending: bool,
    state: AgentState,
    result: Option<String>,
    error: Option<RuntimeError>,
    resumable: bool,
    revision: u64,
    activity: Option<AgentActivity>,
    terminal_at: Option<Instant>,
    last_accessed_at: Instant,
}
struct Session {
    definition: Arc<AgentDefinition>,
    context: String,
    created_at: Instant,
    data: Mutex<SessionData>,
}
struct Inner {
    workspace: PathBuf,
    registry: Arc<AgentRegistry>,
    skills: Arc<SkillRegistry>,
    provider: ProviderClient,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    capacity: Arc<Semaphore>,
    version: watch::Sender<u64>,
}
#[derive(Clone)]
pub(crate) struct AgentRuntime {
    inner: Arc<Inner>,
}

impl AgentRuntime {
    pub(crate) fn new(workspace: PathBuf) -> Result<Self, RuntimeError> {
        let registry = AgentRegistry::discover(&workspace).map_err(|_| {
            RuntimeError::new(
                "configuration_error",
                "unable to discover agent definitions",
            )
        })?;
        let skills = SkillRegistry::discover(&workspace)
            .map_err(|_| RuntimeError::new("configuration_error", "unable to discover skills"))?;
        let provider = ProviderClient::new().map_err(|_| {
            RuntimeError::new(
                "configuration_error",
                "unable to initialize provider client",
            )
        })?;
        let workspace = registry.workspace().to_path_buf();
        let (version, _) = watch::channel(0u64);
        Ok(Self {
            inner: Arc::new(Inner {
                workspace,
                registry: Arc::new(registry),
                skills: Arc::new(skills),
                provider,
                sessions: Mutex::new(BTreeMap::new()),
                capacity: Arc::new(Semaphore::new(8)),
                version,
            }),
        })
    }
    pub(crate) fn registry(&self) -> &AgentRegistry {
        &self.inner.registry
    }
    pub(crate) async fn spawn(&self, name: &str, task: &str) -> Result<SpawnResult, RuntimeError> {
        if task.trim().is_empty() {
            return Err(RuntimeError::new(
                "invalid_request",
                "task must not be empty",
            ));
        }
        let definition = self
            .inner
            .registry
            .get(name)
            .ok_or_else(|| RuntimeError::new("unknown_agent", "unknown agent"))?;
        let credential = ProviderCredential::resolve(&definition)
            .map_err(|e| RuntimeError::new(e.kind, e.message))?;
        let context = self.context(&definition)?;
        cleanup_terminal_sessions(&self.inner).await;
        let permit = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| RuntimeError::new("capacity_exceeded", "agent runtime is at capacity"))?;
        let id = format!("agt_{}", uuid::Uuid::now_v7().simple());
        let now = Instant::now();
        let session = Arc::new(Session {
            definition: definition.clone(),
            context,
            data: Mutex::new(SessionData {
                conversation: Some(ConversationState::new(&definition.wire_api)),
                queue: VecDeque::new(),
                cancel: None,
                interrupt_pending: false,
                state: AgentState::Running,
                result: None,
                error: None,
                resumable: true,
                revision: 1,
                activity: Some(AgentActivity::new(AgentActivityEvent::new(
                    ActivityPhase::Starting,
                    "Starting agent",
                ))),
                terminal_at: None,
                last_accessed_at: now,
            }),
            created_at: now,
        });
        self.inner
            .sessions
            .lock()
            .await
            .insert(id.clone(), session.clone());
        self.inner.version.send_modify(|v| *v = v.wrapping_add(1));
        tracing::info!(agent_id = %id, agent = %definition.name, event = "spawned", "agent activity");
        self.launch(id.clone(), session, task.to_owned(), credential, permit);
        Ok(SpawnResult {
            id,
            name: definition.name.clone(),
            state: AgentState::Running,
        })
    }
    fn context(&self, definition: &AgentDefinition) -> Result<String, RuntimeError> {
        let mut out = format!(
            "{}\n\nWorkspace: {}",
            definition.instructions,
            self.inner.workspace.display()
        );
        for name in &definition.skills {
            let skill = self.inner.skills.load(name).map_err(|_| {
                RuntimeError::new("configuration_error", "configured skill is unavailable")
            })?;
            out.push_str("\n\n");
            out.push_str(&skill.instructions);
        }
        Ok(out)
    }
    fn launch(
        &self,
        id: String,
        session: Arc<Session>,
        message: String,
        credential: ProviderCredential,
        permit: OwnedSemaphorePermit,
    ) {
        let inner = self.inner.clone();
        tokio::spawn(
            async move { run_worker(inner, id, session, message, credential, permit).await },
        );
    }
    pub(crate) async fn send_input(
        &self,
        target: &str,
        message: &str,
        interrupt: bool,
    ) -> Result<InputAck, RuntimeError> {
        if message.trim().is_empty() {
            return Err(RuntimeError::new(
                "invalid_request",
                "message must not be empty",
            ));
        }
        let session = self.session(target).await?;
        {
            let mut data = session.data.lock().await;
            data.last_accessed_at = Instant::now();
            if data.state == AgentState::Running {
                queue_input(&mut data, message, interrupt)?;
                return Ok(InputAck {
                    id: target.into(),
                    accepted: true,
                    state: AgentState::Running,
                });
            }
            ensure_resumable(&data)?;
        }
        let credential = ProviderCredential::resolve(&session.definition)
            .map_err(|e| RuntimeError::new(e.kind, e.message))?;
        let permit = self
            .inner
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| RuntimeError::new("capacity_exceeded", "agent runtime is at capacity"))?;
        let sessions = self.inner.sessions.lock().await;
        if sessions
            .get(target)
            .is_none_or(|retained| !Arc::ptr_eq(retained, &session))
        {
            return Err(RuntimeError::new("unknown_agent", "unknown agent session"));
        }
        let mut data = session.data.lock().await;
        data.last_accessed_at = Instant::now();
        if data.state == AgentState::Running {
            queue_input(&mut data, message, interrupt)?;
            return Ok(InputAck {
                id: target.into(),
                accepted: true,
                state: AgentState::Running,
            });
        }
        ensure_resumable(&data)?;
        data.state = AgentState::Running;
        data.result = None;
        data.error = None;
        data.terminal_at = None;
        data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
            ActivityPhase::Starting,
            "Starting agent",
        )));
        data.revision = data.revision.wrapping_add(1);
        drop(data);
        drop(sessions);
        self.inner.version.send_modify(|v| *v = v.wrapping_add(1));
        self.launch(target.into(), session, message.into(), credential, permit);
        cleanup_terminal_sessions(&self.inner).await;
        Ok(InputAck {
            id: target.into(),
            accepted: true,
            state: AgentState::Running,
        })
    }
    async fn session(&self, id: &str) -> Result<Arc<Session>, RuntimeError> {
        self.inner
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| RuntimeError::new("unknown_agent", "unknown agent session"))
    }
    pub(crate) async fn wait(
        &self,
        targets: &[String],
        timeout_ms: u64,
    ) -> Result<WaitResult, RuntimeError> {
        self.wait_inner(targets, timeout_ms, None).await
    }
    pub(crate) async fn wait_observing(
        &self,
        targets: &[String],
        timeout_ms: u64,
        updates: mpsc::Sender<WaitObservation>,
    ) -> Result<WaitResult, RuntimeError> {
        self.wait_inner(targets, timeout_ms, Some(updates)).await
    }
    async fn wait_inner(
        &self,
        targets: &[String],
        timeout_ms: u64,
        updates: Option<mpsc::Sender<WaitObservation>>,
    ) -> Result<WaitResult, RuntimeError> {
        if targets.is_empty() || timeout_ms > MAX_WAIT_AGENT_TIMEOUT_MS {
            return Err(RuntimeError::new(
                "invalid_request",
                "targets must be nonempty and timeout_ms must be at most 300000",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !targets.iter().all(|x| seen.insert(x)) {
            return Err(RuntimeError::new(
                "invalid_request",
                "targets must be unique",
            ));
        }
        let mut rx = self.inner.version.subscribe();
        let (initial, mut revisions) = self.snapshot_with_revisions(targets).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        send_observation(&updates, initial.clone(), deadline);
        if initial
            .agents
            .iter()
            .all(|a| a.state != AgentState::Running)
        {
            return Ok(WaitResult {
                agents: initial.agents,
                timed_out: false,
            });
        }
        if timeout_ms == 0 {
            return Ok(WaitResult {
                agents: initial.agents,
                timed_out: true,
            });
        }
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => { let (s, _) = self.snapshot_with_revisions(targets).await?; return Ok(WaitResult { timed_out: s.agents.iter().any(|a| a.state == AgentState::Running), agents: s.agents }); }
                changed = rx.changed() => {
                    let (s, next_revisions) = self.snapshot_with_revisions(targets).await?;
                    if changed.is_err() { return Ok(WaitResult { timed_out: s.agents.iter().any(|a| a.state == AgentState::Running), agents: s.agents }); }
                    let changed_agents = WaitResult { agents: s.agents.iter().zip(&next_revisions).zip(&revisions).filter(|((_, next), previous)| next != previous).map(|((agent, _), _)| agent.clone()).collect(), timed_out: false };
                    revisions = next_revisions;
                    if !changed_agents.agents.is_empty() { send_observation(&updates, changed_agents, deadline); }
                    if s.agents.iter().all(|a| a.state != AgentState::Running) { return Ok(WaitResult { agents: s.agents, timed_out: false }); }
                }
            }
        }
    }
    async fn snapshot_with_revisions(
        &self,
        targets: &[String],
    ) -> Result<(WaitResult, Vec<u64>), RuntimeError> {
        let sessions = self.inner.sessions.lock().await;
        let mut agents = Vec::with_capacity(targets.len());
        let mut revisions = Vec::with_capacity(targets.len());
        for id in targets {
            let session = sessions
                .get(id)
                .ok_or_else(|| RuntimeError::new("unknown_agent", "unknown agent session"))?;
            let mut d = session.data.lock().await;
            d.last_accessed_at = Instant::now();
            revisions.push(d.revision);
            let now = d.terminal_at.unwrap_or_else(Instant::now);
            agents.push(AgentResult {
                id: id.clone(),
                name: Some(session.definition.name.clone()),
                state: d.state.clone(),
                result: d.result.clone(),
                error: d.error.clone(),
                total_elapsed_ms: millis(now.saturating_duration_since(session.created_at)),
                activity: (d.state == AgentState::Running)
                    .then(|| d.activity.as_ref().map(|a| a.snapshot(now)))
                    .flatten(),
            });
        }
        Ok((
            WaitResult {
                agents,
                timed_out: false,
            },
            revisions,
        ))
    }
    pub(crate) async fn shutdown(&self) {
        let sessions: Vec<_> = self.inner.sessions.lock().await.values().cloned().collect();
        for s in &sessions {
            if let Some(c) = &s.data.lock().await.cancel {
                c.cancel();
            }
        }
        let deadline = tokio::time::Instant::now() + RUNTIME_SHUTDOWN_TIMEOUT;
        let mut updates = self.inner.version.subscribe();
        loop {
            let running = {
                let mut running = false;
                for session in &sessions {
                    if session.data.lock().await.state == AgentState::Running {
                        running = true;
                        break;
                    }
                }
                running
            };
            if !running || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::select! { _ = tokio::time::sleep_until(deadline) => break, changed = updates.changed() => { if changed.is_err() { break; } } }
        }
    }
}

fn send_observation(
    updates: &Option<mpsc::Sender<WaitObservation>>,
    result: WaitResult,
    deadline: tokio::time::Instant,
) {
    if let Some(updates) = updates {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let _ = updates.try_send(WaitObservation {
            result,
            wait_timeout_remaining_ms: millis(remaining),
        });
    }
}

fn queue_input(data: &mut SessionData, message: &str, interrupt: bool) -> Result<(), RuntimeError> {
    if data.queue.len() == QUEUE_LIMIT {
        return Err(RuntimeError::new("queue_full", "agent input queue is full"));
    }
    data.queue.push_back(message.to_owned());
    if interrupt {
        if let Some(cancel) = &data.cancel {
            cancel.cancel();
        } else {
            data.interrupt_pending = true;
        }
    }
    Ok(())
}

fn install_first_turn_cancel(
    data: &mut SessionData,
    startup_cancel: &CancellationToken,
) -> CancellationToken {
    let turn_cancel = CancellationToken::new();
    if startup_cancel.is_cancelled() || std::mem::take(&mut data.interrupt_pending) {
        turn_cancel.cancel();
    }
    data.cancel = Some(turn_cancel.clone());
    turn_cancel
}

fn ensure_resumable(data: &SessionData) -> Result<(), RuntimeError> {
    if data.state == AgentState::Failed && !data.resumable {
        Err(RuntimeError::new(
            "non_resumable",
            "agent session cannot be resumed",
        ))
    } else {
        Ok(())
    }
}

fn set_terminal(data: &mut SessionData, outcome: Result<String, super::provider::ProviderError>) {
    let now = Instant::now();
    data.cancel = None;
    data.terminal_at = Some(now);
    data.last_accessed_at = now;
    data.revision = data.revision.wrapping_add(1);
    match outcome {
        Ok(result) => {
            data.state = AgentState::Completed;
            data.result = Some(result);
            data.error = None;
            data.resumable = true;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Completed,
                "Completed",
            )));
        }
        Err(error) => {
            data.state = AgentState::Failed;
            data.result = None;
            data.error = Some(RuntimeError::new(error.kind, error.message));
            data.resumable = error.resumable;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Failed,
                "Failed",
            )));
        }
    }
}

async fn finish_failed(session: &Session, error: RuntimeError, resumable: bool) {
    let mut data = session.data.lock().await;
    let now = Instant::now();
    data.cancel = None;
    data.state = AgentState::Failed;
    data.result = None;
    data.error = Some(error);
    data.resumable = resumable;
    data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
        ActivityPhase::Failed,
        "Failed",
    )));
    data.terminal_at = Some(now);
    data.last_accessed_at = now;
    data.revision = data.revision.wrapping_add(1);
}

async fn cleanup_terminal_sessions(inner: &Arc<Inner>) {
    let entries: Vec<_> = inner
        .sessions
        .lock()
        .await
        .iter()
        .map(|(id, session)| (id.clone(), session.clone()))
        .collect();
    let mut terminal = Vec::new();
    for (id, session) in entries {
        let data = session.data.lock().await;
        if data.state != AgentState::Running {
            terminal.push((data.last_accessed_at, id, session.clone()));
        }
    }
    if terminal.len() <= MAX_RETAINED_TERMINAL_SESSIONS {
        return;
    }
    terminal.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let remove = terminal.len() - MAX_RETAINED_TERMINAL_SESSIONS;
    for (accessed, id, candidate) in terminal.into_iter().take(remove) {
        let mut sessions = inner.sessions.lock().await;
        let Some(retained) = sessions.get(&id).cloned() else {
            continue;
        };
        if !Arc::ptr_eq(&retained, &candidate) {
            continue;
        }
        let data = retained.data.lock().await;
        let evict = data.state != AgentState::Running && data.last_accessed_at == accessed;
        drop(data);
        if evict {
            sessions.remove(&id);
        }
    }
}

async fn run_worker(
    inner: Arc<Inner>,
    id: String,
    session: Arc<Session>,
    mut message: String,
    credential: ProviderCredential,
    permit: OwnedSemaphorePermit,
) {
    let reporter = {
        let inner = inner.clone();
        let session = session.clone();
        let id = id.clone();
        ActivityReporter::new(move |event| {
            let inner = inner.clone();
            let session = session.clone();
            let id = id.clone();
            Box::pin(async move {
                let mut data = session.data.lock().await;
                let prior = data
                    .activity
                    .as_ref()
                    .map(|activity| millis(activity.started_at.elapsed()));
                let prior_phase = data
                    .activity
                    .as_ref()
                    .map(|activity| activity.phase.clone());
                let prior_tool = data
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.tool.clone());
                let prior_deadline_ms = data
                    .activity
                    .as_ref()
                    .and_then(|activity| activity.deadline)
                    .map(|deadline| millis(deadline.saturating_duration_since(Instant::now())));
                data.activity = Some(AgentActivity::new(event.clone()));
                data.revision = data.revision.wrapping_add(1);
                drop(data);
                inner.version.send_modify(|v| *v = v.wrapping_add(1));
                match event.kind {
                    "model_started" => {
                        tracing::info!(agent_id = %id, event = "model_started", "agent activity")
                    }
                    "tool_started" => {
                        tracing::info!(agent_id = %id, event = "tool_started", tool = ?event.tool, target = ?event.target, "agent activity")
                    }
                    "tool_completed" => {
                        tracing::info!(agent_id = %id, event = "tool_completed", tool = ?prior_tool, prior_activity_ms = ?prior, prior_deadline_ms = ?prior_deadline_ms, prior_phase = ?prior_phase, "agent activity")
                    }
                    "tool_failed" => {
                        tracing::info!(agent_id = %id, event = "tool_failed", tool = ?prior_tool, prior_activity_ms = ?prior, "agent activity")
                    }
                    "tool_timed_out" => {
                        tracing::info!(agent_id = %id, event = "tool_timed_out", tool = ?prior_tool, prior_activity_ms = ?prior, "agent activity")
                    }
                    _ => {}
                }
            })
        })
    };
    'run: loop {
        let cancel = {
            let mut d = session.data.lock().await;
            let cancel = CancellationToken::new();
            if std::mem::take(&mut d.interrupt_pending) {
                cancel.cancel();
            }
            d.cancel = Some(cancel.clone());
            cancel
        };
        reporter
            .report(AgentActivityEvent {
                phase: ActivityPhase::Starting,
                summary: "Starting child MCP servers".into(),
                target: None,
                tool: None,
                deadline: None,
                kind: "child_mcp_starting",
            })
            .await;
        let mut child =
            match ChildMcpManager::connect(&session.definition, &inner.workspace, &cancel).await {
                Ok(child) => child,
                Err(_) => {
                    if cancel.is_cancelled() {
                        let mut data = session.data.lock().await;
                        if let Some(next) = data.queue.pop_front() {
                            data.cancel = None;
                            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                                ActivityPhase::Starting,
                                "Starting agent",
                            )));
                            data.revision = data.revision.wrapping_add(1);
                            drop(data);
                            inner.version.send_modify(|v| *v = v.wrapping_add(1));
                            message = next;
                            continue 'run;
                        }
                    }
                    finish_failed(
                        &session,
                        RuntimeError::new(
                            "child_mcp_startup_error",
                            "unable to start configured child MCP servers",
                        ),
                        true,
                    )
                    .await;
                    inner.version.send_modify(|v| *v = v.wrapping_add(1));
                    break 'run;
                }
            };
        if cancel.is_cancelled() {
            let mut data = session.data.lock().await;
            if let Some(next) = data.queue.pop_front() {
                data.cancel = None;
                data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                    ActivityPhase::Starting,
                    "Starting agent",
                )));
                data.revision = data.revision.wrapping_add(1);
                drop(data);
                child.shutdown().await;
                inner
                    .version
                    .send_modify(|value| *value = value.wrapping_add(1));
                message = next;
                continue 'run;
            }
            drop(data);
            child.shutdown().await;
            finish_failed(
                &session,
                RuntimeError::new("run_interrupted", "agent run was interrupted"),
                true,
            )
            .await;
            inner
                .version
                .send_modify(|value| *value = value.wrapping_add(1));
            break 'run;
        }
        let mut first_turn = true;
        let outcome = loop {
            let (conversation, turn_cancel) = {
                let mut d = session.data.lock().await;
                let turn_cancel = if first_turn {
                    first_turn = false;
                    install_first_turn_cancel(&mut d, &cancel)
                } else {
                    let turn_cancel = CancellationToken::new();
                    d.cancel = Some(turn_cancel.clone());
                    turn_cancel
                };
                (d.conversation.clone(), turn_cancel)
            };
            let Some(mut candidate) = conversation else {
                break Err(super::provider::ProviderError {
                    kind: "internal_error",
                    message: "agent conversation is unavailable".into(),
                    resumable: false,
                });
            };
            let outcome = inner
                .provider
                .run(
                    &session.definition,
                    &credential,
                    &session.context,
                    &message,
                    &mut candidate,
                    &child,
                    &turn_cancel,
                    &reporter,
                    &inner.workspace,
                )
                .await;
            let mut d = session.data.lock().await;
            if outcome.is_ok() {
                d.conversation = Some(candidate);
            }
            if let Some(next) = d.queue.pop_front() {
                d.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                    ActivityPhase::Waiting,
                    "Waiting for next input",
                )));
                d.revision = d.revision.wrapping_add(1);
                drop(d);
                inner.version.send_modify(|v| *v = v.wrapping_add(1));
                message = next;
                continue;
            }
            d.cancel = None;
            break outcome;
        };
        child.shutdown().await;
        let mut d = session.data.lock().await;
        if let Some(next) = d.queue.pop_front() {
            d.interrupt_pending = false;
            d.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Starting,
                "Starting agent",
            )));
            d.revision = d.revision.wrapping_add(1);
            drop(d);
            inner.version.send_modify(|v| *v = v.wrapping_add(1));
            message = next;
            continue 'run;
        }
        set_terminal(&mut d, outcome);
        drop(d);
        // Error text is deliberately not logged: provider errors may be externally supplied.
        let terminal = session.data.lock().await;
        let state = terminal.state.clone();
        let error_kind = terminal.error.as_ref().map(|error| error.kind.clone());
        let total = terminal
            .terminal_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(session.created_at);
        drop(terminal);
        match state {
            AgentState::Completed => {
                tracing::info!(agent_id = %id, event = "completed", total_ms = millis(total), "agent activity")
            }
            AgentState::Failed => {
                tracing::info!(agent_id = %id, event = "failed", total_ms = millis(total), error_kind = ?error_kind, "agent activity")
            }
            AgentState::Running => {}
        }
        inner.version.send_modify(|v| *v = v.wrapping_add(1));
        break 'run;
    }
    drop(permit);
    cleanup_terminal_sessions(&inner).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::definition::{
        IsolationMode, ModelProviderKind, PermissionPolicy, SandboxMode, WireApi,
    };
    use std::{collections::BTreeMap, time::Instant};
    use tempfile::tempdir;
    use url::Url;

    fn definition() -> Arc<AgentDefinition> {
        Arc::new(AgentDefinition {
            name: "test".into(),
            description: "test".into(),
            instructions: "test".into(),
            model: "test".into(),
            provider: ModelProviderKind::Custom,
            base_url: Url::parse("https://example.test").unwrap(),
            env_key: "TEST_KEY".into(),
            wire_api: WireApi::Responses,
            reasoning_effort: None,
            temperature: None,
            max_turns: 1,
            permission: PermissionPolicy::default(),
            sandbox: SandboxMode::Default,
            isolation: IsolationMode::None,
            skills: vec![],
            mcp_servers: BTreeMap::new(),
            source_path: PathBuf::new(),
        })
    }
    async fn insert(runtime: &AgentRuntime, id: &str, state: AgentState, result: Option<&str>) {
        insert_at(runtime, id, state, result, Instant::now()).await;
    }
    async fn insert_at(
        runtime: &AgentRuntime,
        id: &str,
        state: AgentState,
        result: Option<&str>,
        created_at: Instant,
    ) {
        runtime.inner.sessions.lock().await.insert(
            id.into(),
            Arc::new(Session {
                definition: definition(),
                context: String::new(),
                data: Mutex::new(SessionData {
                    conversation: None,
                    queue: VecDeque::new(),
                    cancel: None,
                    interrupt_pending: false,
                    state: state.clone(),
                    result: result.map(str::to_owned),
                    error: None,
                    resumable: true,
                    activity: (state == AgentState::Running).then(|| {
                        AgentActivity::new(AgentActivityEvent::new(
                            ActivityPhase::Starting,
                            "Starting agent",
                        ))
                    }),
                    terminal_at: (state != AgentState::Running).then(Instant::now),
                    last_accessed_at: Instant::now(),
                    revision: 1,
                }),
                created_at,
            }),
        );
    }
    async fn runtime() -> AgentRuntime {
        AgentRuntime::new(tempdir().unwrap().keep()).unwrap()
    }

    #[tokio::test]
    async fn wait_is_immediate_non_consuming_and_zero_timeout_is_a_snapshot() {
        let runtime = runtime().await;
        insert(&runtime, "done", AgentState::Completed, Some("answer")).await;
        insert(&runtime, "running", AgentState::Running, None).await;
        let done = vec!["done".into()];
        assert!(!runtime.wait(&done, 10).await.unwrap().timed_out);
        assert_eq!(
            runtime.wait(&done, 0).await.unwrap().agents[0]
                .result
                .as_deref(),
            Some("answer")
        );
        assert!(
            runtime
                .wait(&["running".into()], 0)
                .await
                .unwrap()
                .timed_out
        );
    }

    #[tokio::test]
    async fn wait_observes_event_driven_and_post_subscription_transition() {
        let runtime = runtime().await;
        insert_at(
            &runtime,
            "one",
            AgentState::Running,
            None,
            Instant::now() - Duration::from_secs(9),
        )
        .await;
        let waiter = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.wait(&["one".into()], 2_000).await.unwrap() })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        let started = Instant::now();
        let session = runtime.session("one").await.unwrap();
        session.data.lock().await.state = AgentState::Completed;
        runtime
            .inner
            .version
            .send_modify(|v| *v = v.wrapping_add(1));
        assert!(!waiter.await.unwrap().timed_out);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test]
    async fn snapshots_keep_activity_and_terminal_timing_stable() {
        let runtime = runtime().await;
        insert_at(
            &runtime,
            "one",
            AgentState::Running,
            None,
            Instant::now() - Duration::from_secs(9),
        )
        .await;
        let session = runtime.session("one").await.unwrap();
        {
            let mut data = session.data.lock().await;
            data.activity.as_mut().unwrap().started_at = Instant::now() - Duration::from_secs(2);
        }
        let initial = runtime
            .wait(&["one".into()], 0)
            .await
            .unwrap()
            .agents
            .remove(0);
        assert!((8_900..=9_100).contains(&initial.total_elapsed_ms));
        assert!((1_900..=2_100).contains(&initial.activity.unwrap().activity_elapsed_ms));
        {
            let mut data = session.data.lock().await;
            data.activity.as_mut().unwrap().started_at = Instant::now() - Duration::from_secs(1);
        }
        let replaced = runtime
            .wait(&["one".into()], 0)
            .await
            .unwrap()
            .agents
            .remove(0);
        assert!((8_900..=9_100).contains(&replaced.total_elapsed_ms));
        assert!((900..=1_100).contains(&replaced.activity.unwrap().activity_elapsed_ms));
        {
            let mut data = session.data.lock().await;
            data.state = AgentState::Completed;
            data.terminal_at = Some(Instant::now() - Duration::from_secs(3));
            data.revision += 1;
        }
        runtime.inner.version.send_modify(|v| *v += 1);
        let first = runtime
            .wait(&["one".into()], 0)
            .await
            .unwrap()
            .agents
            .remove(0);
        let second = runtime
            .wait(&["one".into()], 0)
            .await
            .unwrap()
            .agents
            .remove(0);
        assert!(first.activity.is_none());
        assert!((5_900..=6_100).contains(&first.total_elapsed_ms));
        assert_eq!(first.total_elapsed_ms, second.total_elapsed_ms);
    }

    #[tokio::test]
    async fn observations_are_initial_then_only_revised_targets() {
        let runtime = runtime().await;
        insert(&runtime, "one", AgentState::Running, None).await;
        insert(&runtime, "other", AgentState::Running, None).await;
        let (tx, mut rx) = mpsc::channel(16);
        let task = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .wait_observing(&["one".into()], 100, tx)
                    .await
                    .unwrap()
            })
        };
        assert_eq!(rx.recv().await.unwrap().result.agents.len(), 1);
        // A global version change and another session's revision must not emit.
        {
            let other = runtime.session("other").await.unwrap();
            other.data.lock().await.revision += 1;
        }
        runtime.inner.version.send_modify(|v| *v += 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), rx.recv())
                .await
                .is_err()
        );
        let session = runtime.session("one").await.unwrap();
        {
            let mut data = session.data.lock().await;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::tool(
                "Reading src/lib.rs".into(),
                "read_file".into(),
                Some("src/lib.rs".into()),
            )));
            data.revision += 1;
        }
        runtime.inner.version.send_modify(|v| *v += 1);
        let update = rx.recv().await.unwrap();
        assert_eq!(update.result.agents.len(), 1);
        assert_eq!(
            update.result.agents[0]
                .activity
                .as_ref()
                .unwrap()
                .tool
                .as_deref(),
            Some("read_file")
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn activity_updates_observe_without_completing_the_wait() {
        let runtime = runtime().await;
        insert(&runtime, "one", AgentState::Running, None).await;
        let (tx, mut rx) = mpsc::channel(16);
        let task = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .wait_observing(&["one".into()], 2_000, tx)
                    .await
                    .unwrap()
            })
        };
        rx.recv().await.unwrap();
        let session = runtime.session("one").await.unwrap();
        {
            let mut data = session.data.lock().await;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Model,
                "Waiting for model response",
            )));
            data.revision += 1;
        }
        runtime.inner.version.send_modify(|v| *v += 1);
        let observed = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("activity update is event-driven")
            .unwrap();
        assert_eq!(observed.result.agents[0].state, AgentState::Running);
        assert!(!task.is_finished(), "activity alone must not end the wait");
        {
            let mut data = session.data.lock().await;
            data.state = AgentState::Completed;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Completed,
                "Completed",
            )));
            data.terminal_at = Some(Instant::now());
            data.revision += 1;
        }
        runtime.inner.version.send_modify(|v| *v += 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn progress_backpressure_is_lossy_and_never_blocks_completion() {
        let runtime = runtime().await;
        insert(&runtime, "one", AgentState::Running, None).await;
        let (tx, _rx) = mpsc::channel(1);
        let waiter = {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime
                    .wait_observing(&["one".into()], 2_000, tx)
                    .await
                    .unwrap()
            })
        };
        tokio::task::yield_now().await;
        let session = runtime.session("one").await.unwrap();
        for index in 0..100 {
            let mut data = session.data.lock().await;
            data.activity = Some(AgentActivity::new(AgentActivityEvent::new(
                ActivityPhase::Model,
                format!("Update {index}"),
            )));
            data.revision += 1;
            drop(data);
            runtime.inner.version.send_modify(|value| *value += 1);
        }
        {
            let mut data = session.data.lock().await;
            data.state = AgentState::Completed;
            data.result = Some("final".into());
            data.terminal_at = Some(Instant::now());
            data.revision += 1;
        }
        runtime.inner.version.send_modify(|value| *value += 1);
        let result = tokio::time::timeout(Duration::from_millis(200), waiter)
            .await
            .expect("a full progress channel cannot block the wait")
            .unwrap();
        assert_eq!(result.agents[0].result.as_deref(), Some("final"));
    }

    #[tokio::test]
    async fn terminal_retention_is_lru_bounded_and_never_evicts_running_sessions() {
        let runtime = runtime().await;
        let tied = Instant::now() - Duration::from_secs(10);
        for index in 0..=MAX_RETAINED_TERMINAL_SESSIONS {
            let id = format!("terminal-{index:03}");
            insert(&runtime, &id, AgentState::Completed, Some("answer")).await;
            runtime
                .session(&id)
                .await
                .unwrap()
                .data
                .lock()
                .await
                .last_accessed_at = tied;
        }
        insert(&runtime, "running", AgentState::Running, None).await;
        cleanup_terminal_sessions(&runtime.inner).await;
        assert!(runtime.session("running").await.is_ok());
        let error = match runtime.session("terminal-000").await {
            Ok(_) => panic!("least-recent terminal session was retained"),
            Err(error) => error,
        };
        assert_eq!(error.kind, "unknown_agent");
        assert!(runtime.session("terminal-001").await.is_ok());
        assert_eq!(
            runtime.inner.sessions.lock().await.len(),
            MAX_RETAINED_TERMINAL_SESSIONS + 1
        );
    }

    #[tokio::test]
    async fn wait_and_send_input_refresh_session_recency() {
        let runtime = runtime().await;
        insert(&runtime, "done", AgentState::Completed, Some("answer")).await;
        insert(&runtime, "running", AgentState::Running, None).await;
        let old = Instant::now() - Duration::from_secs(10);
        runtime
            .session("done")
            .await
            .unwrap()
            .data
            .lock()
            .await
            .last_accessed_at = old;
        runtime
            .session("running")
            .await
            .unwrap()
            .data
            .lock()
            .await
            .last_accessed_at = old;
        runtime.wait(&["done".into()], 0).await.unwrap();
        assert!(
            runtime
                .session("done")
                .await
                .unwrap()
                .data
                .lock()
                .await
                .last_accessed_at
                > old
        );
        runtime
            .send_input("running", "queued", false)
            .await
            .unwrap();
        assert!(
            runtime
                .session("running")
                .await
                .unwrap()
                .data
                .lock()
                .await
                .last_accessed_at
                > old
        );
    }

    #[tokio::test]
    async fn interrupt_intent_is_preserved_before_a_run_installs_its_token() {
        let runtime = runtime().await;
        insert(&runtime, "starting", AgentState::Running, None).await;
        runtime
            .send_input("starting", "replacement", true)
            .await
            .unwrap();
        let session = runtime.session("starting").await.unwrap();
        let data = session.data.lock().await;
        assert!(data.interrupt_pending);
        assert_eq!(data.queue.front().map(String::as_str), Some("replacement"));
    }

    #[tokio::test]
    async fn interrupt_is_transferred_across_the_startup_to_turn_token_handoff() {
        let runtime = runtime().await;
        insert(&runtime, "handoff", AgentState::Running, None).await;
        let session = runtime.session("handoff").await.unwrap();
        let startup_cancel = CancellationToken::new();
        let mut data = session.data.lock().await;
        data.cancel = Some(startup_cancel.clone());

        // Reproduce the exact race: the worker has passed its startup check,
        // then send_input cancels the published startup token before handoff.
        assert!(!startup_cancel.is_cancelled());
        queue_input(&mut data, "replacement", true).unwrap();
        assert!(startup_cancel.is_cancelled());
        let turn_cancel = install_first_turn_cancel(&mut data, &startup_cancel);

        assert!(turn_cancel.is_cancelled());
        assert!(data.cancel.as_ref().unwrap().is_cancelled());
        assert_eq!(data.queue.front().map(String::as_str), Some("replacement"));
    }
}
