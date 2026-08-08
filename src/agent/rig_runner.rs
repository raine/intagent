use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;

use rig_agent::agent::PromptResponse;
use rig_agent::agent::run::AgentRun;
use rig_core::client::CompletionClient;
use rig_core::completion::{CompletionError, CompletionModel, CompletionResponse};
use rig_core::message::{AssistantContent, Message, ReasoningContent, ToolCall};
use rig_core::providers::chatgpt;
use tokio_util::sync::CancellationToken;

use super::auth::{AuthPaths, authorize, chatgpt_client};
use super::command_policy::CommandPolicy;
use super::conclusions::{
    ToolObservation, ToolObservationOutcome, fallback_conclusion, observed_actions,
    parse_conclusion, strip_conclusion,
};
use super::context::CompactionConfig;
use super::driver::{
    AgentEngine, AgentObserver, CompletionScope, EngineError, RequestTools, RigModel, SpanFinish,
    ToolOutcome, duration_millis, tool_names,
};
pub use super::driver::{ProviderRetryPolicy, ToolExecutor};
use super::errors::completion_category;
pub use super::errors::{DriverError, TriageError};
use super::model::ThinkingLevel;
pub use super::prompts::build_event_prompt;
use super::prompts::{github_repository_from_event, system_prompt};
use super::read_policy::ReadPolicy;
use super::skills::{format_skill_catalog, validate_skills};
use super::telemetry::{CancellationTelemetry, PrototypeObserver, PrototypeTelemetry};
use super::tools::{ProductionTools, ToolCallResult};
use crate::config::{IntagentConfig, ThinkingLevel as ConfigThinkingLevel, expand_path};
use crate::database::{
    CompactionFinish, CompactionId, ErrorCategory, EventRecord, IntagentDatabase, RetryId,
    RetryStart, RunFinish, RunId, RunMetadata, RunOutcome, SpanOutcome, ToolId, TriageConclusion,
    TurnFinish, TurnId, reported_usage,
};
use crate::logging::{DurableLogStore, TriageRunLog};
use crate::project_registry::{find_likely_project, load_project_inventory};

pub trait TriageRunner: Send + Sync {
    fn run(
        &self,
        event: EventRecord,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<(), TriageError>> + Send;
}

#[derive(Clone)]
pub struct TriageRunnerCore {
    config: Arc<IntagentConfig>,
    database: IntagentDatabase,
    command_policy: Arc<CommandPolicy>,
    logs: DurableLogStore,
    output: Arc<Mutex<Box<dyn Write + Send>>>,
    registry_path: Arc<PathBuf>,
    retry_policy: ProviderRetryPolicy,
    wall_timeout: Duration,
}

impl TriageRunnerCore {
    pub fn new(
        config: IntagentConfig,
        database: IntagentDatabase,
        command_policy: Arc<CommandPolicy>,
        logs: DurableLogStore,
        output: impl Write + Send + 'static,
        registry_path: PathBuf,
    ) -> Self {
        let wall_timeout = Duration::from_secs(config.triage.timeout_minutes.saturating_mul(60));
        Self {
            config: Arc::new(config),
            database,
            command_policy,
            logs,
            output: Arc::new(Mutex::new(Box::new(output))),
            registry_path: Arc::new(registry_path),
            retry_policy: ProviderRetryPolicy::default(),
            wall_timeout,
        }
    }

    pub fn with_retry_policy(mut self, retry_policy: ProviderRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn with_wall_timeout(mut self, wall_timeout: Duration) -> Self {
        self.wall_timeout = wall_timeout;
        self
    }

    async fn start(&self, event: &EventRecord) -> Result<StartedRun, TriageError> {
        let run_id = self
            .database
            .start_triage_run(event.id, event.attempt_count, Utc::now())
            .await?;
        let mut log = self.logs.triage(event);
        log.start().await;
        self.report(format!(
            "▶ triage #{} {}",
            event.id,
            self.filtered(&event.title)
        ));
        Ok(StartedRun {
            run_id,
            log,
            started_at: Instant::now(),
            conclusion: None,
            tool_observations: Vec::new(),
        })
    }

    async fn finish(
        &self,
        mut started: StartedRun,
        result: Result<(), TriageError>,
    ) -> Result<(), TriageError> {
        let (outcome, category, termination) = match &result {
            Ok(()) => (RunOutcome::Succeeded, None, "completed"),
            Err(TriageError::Cancelled) => (
                RunOutcome::Interrupted,
                Some(ErrorCategory::Aborted),
                "aborted",
            ),
            Err(TriageError::WallTimeout) => (
                RunOutcome::Failed,
                Some(ErrorCategory::Timeout),
                "wall_timeout",
            ),
            Err(error) => (
                RunOutcome::Failed,
                Some(error.category()),
                error.termination_reason(),
            ),
        };
        started.log.finish(outcome, category, termination).await;
        let recording_complete = !started.log.recording_failed()
            && !matches!(&result, Err(TriageError::RecordingFailure));
        let mut conclusion = started
            .conclusion
            .take()
            .unwrap_or_else(|| fallback_conclusion(&result, &started.tool_observations));
        if conclusion.actions.is_empty() {
            conclusion.actions = observed_actions(&started.tool_observations);
        }
        let finish_result = self
            .database
            .finish_triage_run_with_conclusion(
                started.run_id,
                RunFinish {
                    outcome,
                    termination_reason: termination.into(),
                    failure_category: category,
                    recording_complete,
                },
                Some(conclusion),
                Utc::now(),
            )
            .await;
        let elapsed = started.started_at.elapsed().as_secs_f64();
        match &result {
            Ok(()) => self.report(format!("✓ triage finished in {elapsed:.1}s")),
            Err(error) => self.report(format!(
                "✗ triage failed after {elapsed:.1}s: {}",
                self.filtered(&error.to_string())
            )),
        }
        finish_result?;
        result
    }

    async fn run_model<M: CompletionModel + Send + Sync>(
        &self,
        model: &M,
        event: &EventRecord,
        started: &mut StartedRun,
        cancellation: CancellationToken,
    ) -> Result<(), TriageError> {
        if cancellation.is_cancelled() {
            return Err(TriageError::Cancelled);
        }
        let prepared = self.prepare(event).await?;
        if cancellation.is_cancelled() {
            return Err(TriageError::Cancelled);
        }
        let metadata = RunMetadata {
            model_id: Some(self.config.triage.model.clone()),
            model_provider: Some("chatgpt".into()),
            thinking_level: Some(
                thinking(self.config.triage.thinking_level)
                    .wire_name()
                    .into(),
            ),
            context_window: None,
            max_tokens: None,
        };
        self.database
            .set_triage_run_metadata(started.run_id, metadata.clone(), Utc::now())
            .await?;
        let tool_names = tool_names().into_iter().collect::<Vec<_>>();
        started.log.metadata(&metadata, &tool_names).await;
        self.database
            .record_triage_run_prompt(
                started.run_id,
                "system".into(),
                prepared.system_prompt.clone(),
                Utc::now(),
            )
            .await?;
        self.database
            .record_triage_run_prompt(
                started.run_id,
                "user".into(),
                prepared.user_prompt.clone(),
                Utc::now(),
            )
            .await?;
        started.log.prompt(&prepared.user_prompt).await;

        let local = CancellationToken::new();
        let mut driver = Box::pin(self.drive(model, event, started, prepared, local.clone()));
        let timeout = tokio::time::sleep(self.wall_timeout);
        tokio::pin!(timeout);
        tokio::select! {
            result = &mut driver => result,
            _ = cancellation.cancelled() => {
                local.cancel();
                let _ = (&mut driver).await;
                Err(TriageError::Cancelled)
            }
            _ = &mut timeout => {
                local.cancel();
                let _ = (&mut driver).await;
                Err(TriageError::WallTimeout)
            }
        }
    }

    async fn prepare(&self, event: &EventRecord) -> Result<PreparedRun, TriageError> {
        let payload = event
            .payload
            .as_deref()
            .ok_or(TriageError::UnavailableEvent)?;
        let _: serde_json::Value = serde_json::from_str(payload)?;
        let skills = validate_skills(&self.config)?;
        if !skills.diagnostics.is_empty() {
            return Err(TriageError::SkillValidation(skills.diagnostics.join("\n")));
        }
        let inventory =
            load_project_inventory(&self.registry_path, &self.config.project_roots).await?;
        let repository = github_repository_from_event(event);
        let matched = repository.as_ref().and_then(|repository| {
            inventory.projects.iter().find(|project| {
                project
                    .github_repositories
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(repository))
            })
        });
        let likely = if matched.is_some() {
            None
        } else if let Some(repository) = repository {
            find_likely_project(&repository, &self.config.project_roots).await?
        } else {
            None
        };
        let cwd = if let Some(project) = matched.or(likely.as_ref()) {
            std::fs::canonicalize(&project.path)?
        } else {
            std::fs::canonicalize(expand_path(self.config.project_roots.first().ok_or_else(
                || TriageError::Configuration("project_roots is empty".into()),
            )?)?)?
        };
        let mut read_roots = self.config.project_roots.clone();
        read_roots.extend(self.config.skills.approved_roots.clone());
        read_roots.push(self.registry_path.to_string_lossy().into_owned());
        let expanded_read_roots = read_roots
            .iter()
            .map(expand_path)
            .collect::<Result<Vec<_>, _>>()?;
        let read_policy =
            ReadPolicy::new(expanded_read_roots, self.config.commands.max_output_bytes)?;
        let tools = ProductionTools::new(
            self.command_policy.clone(),
            read_policy,
            self.database.clone(),
            event.id,
            cwd,
            self.registry_path.as_ref().clone(),
            self.config.project_roots.clone(),
        );
        Ok(PreparedRun {
            system_prompt: system_prompt(
                &self.config,
                &inventory,
                likely.as_ref(),
                &self.registry_path,
                &format_skill_catalog(&skills.skills),
            )?,
            user_prompt: build_event_prompt(event)?,
            tools,
        })
    }

    async fn drive<M: CompletionModel + Send + Sync>(
        &self,
        model: &M,
        _event: &EventRecord,
        started: &mut StartedRun,
        prepared: PreparedRun,
        cancellation: CancellationToken,
    ) -> Result<(), TriageError> {
        let model = RigModel::new(
            model,
            prepared.system_prompt,
            thinking(self.config.triage.thinking_level),
            RequestTools::Production,
        );
        let observer = ProductionObserver::new(self, started);
        AgentEngine::new(
            &model,
            &prepared.tools,
            observer,
            cancellation,
            self.retry_policy.clone(),
            CompactionConfig {
                trigger_tokens: self.config.triage.compaction_trigger_tokens,
                keep_recent_groups: self.config.triage.compaction_keep_recent_messages,
                max_compactions: 1,
            },
        )
        .run(
            prepared.user_prompt,
            Vec::new(),
            self.config.triage.max_turns,
        )
        .await
        .map(|_| ())
        .map_err(map_engine_error)
    }

    async fn record_response(
        &self,
        started: &mut StartedRun,
        turn_id: TurnId,
        ordinal: u32,
        response: &CompletionResponse,
    ) -> Result<(), TriageError> {
        let summaries = reasoning_summaries(response);
        for summary in summaries {
            let bounded = bounded_chars(&summary, 4000);
            self.database
                .record_reasoning(
                    started.run_id,
                    Some(turn_id),
                    Some(bounded.clone()),
                    Utc::now(),
                )
                .await?;
            started.log.reasoning(Some(&bounded)).await;
            self.report(format!("thinking │ {}", self.filtered(&bounded)));
        }
        for text in assistant_texts(response) {
            if let Some(conclusion) = parse_conclusion(text, &self.command_policy) {
                started.conclusion = Some(conclusion);
            }
            let visible = strip_conclusion(text).trim();
            if visible.is_empty() {
                continue;
            }
            let bounded = bounded_chars(visible, 4000);
            self.database
                .record_assistant_text(started.run_id, Some(turn_id), bounded.clone(), Utc::now())
                .await?;
            started.log.assistant(&bounded).await;
            self.report(format!("assistant │ {}", self.filtered(&bounded)));
        }
        let usage = reported_usage(Some(&response.usage), response.usage.has_values());
        self.database
            .finish_turn(
                started.run_id,
                turn_id,
                TurnFinish {
                    stop_reason: Some("accepted".into()),
                    usage,
                    context_tokens: None,
                    context_window: None,
                },
                Utc::now(),
            )
            .await?;
        started
            .log
            .finish_turn(ordinal, serde_json::json!({ "stopReason": "accepted" }))
            .await;
        Ok(())
    }

    async fn finish_failed_turn(
        &self,
        started: &mut StartedRun,
        turn_id: TurnId,
        ordinal: u32,
        reason: &str,
    ) {
        let _ = self
            .database
            .finish_turn(
                started.run_id,
                turn_id,
                TurnFinish {
                    stop_reason: Some(reason.into()),
                    usage: None,
                    context_tokens: None,
                    context_window: None,
                },
                Utc::now(),
            )
            .await;
        started
            .log
            .finish_turn(ordinal, serde_json::json!({ "stopReason": reason }))
            .await;
    }

    fn filtered(&self, value: &str) -> String {
        self.command_policy.filter(value)
    }

    fn report(&self, value: String) {
        let mut output = match self.output.lock() {
            Ok(output) => output,
            Err(poisoned) => poisoned.into_inner(),
        };
        for line in value.lines() {
            let _ = writeln!(output, "{} {line}", Utc::now().format("%Y-%m-%d %H:%M:%S"));
        }
        let _ = output.flush();
    }
}

pub struct RigTriageRunner<M> {
    core: TriageRunnerCore,
    model: M,
}

impl<M> RigTriageRunner<M> {
    pub fn new(core: TriageRunnerCore, model: M) -> Self {
        Self { core, model }
    }
}

impl<M: CompletionModel + Send + Sync> TriageRunner for RigTriageRunner<M> {
    async fn run(
        &self,
        event: EventRecord,
        cancellation: CancellationToken,
    ) -> Result<(), TriageError> {
        let mut started = self.core.start(&event).await?;
        let result = self
            .core
            .run_model(&self.model, &event, &mut started, cancellation)
            .await;
        self.core.finish(started, result).await
    }
}

pub struct ChatGptTriageRunner {
    core: TriageRunnerCore,
    auth_paths: AuthPaths,
}

impl ChatGptTriageRunner {
    pub fn new(core: TriageRunnerCore, auth_paths: AuthPaths) -> Self {
        Self { core, auth_paths }
    }
}

impl TriageRunner for ChatGptTriageRunner {
    async fn run(
        &self,
        event: EventRecord,
        cancellation: CancellationToken,
    ) -> Result<(), TriageError> {
        let mut started = self.core.start(&event).await?;
        let result = async {
            authorize(&self.auth_paths, false).await?;
            let client = chatgpt_client(&self.auth_paths.cache, false)?;
            let model: chatgpt::ResponsesCompletionModel =
                client.completion_model(&self.core.config.triage.model);
            self.core
                .run_model(&model, &event, &mut started, cancellation)
                .await
        }
        .await;
        self.core.finish(started, result).await
    }
}

struct StartedRun {
    run_id: RunId,
    log: TriageRunLog,
    started_at: Instant,
    conclusion: Option<TriageConclusion>,
    tool_observations: Vec<ToolObservation>,
}

struct PreparedRun {
    system_prompt: String,
    user_prompt: String,
    tools: ProductionTools,
}

struct ProductionObserver<'a> {
    core: &'a TriageRunnerCore,
    started: &'a mut StartedRun,
    current_turn: Option<TurnId>,
}

impl<'a> ProductionObserver<'a> {
    fn new(core: &'a TriageRunnerCore, started: &'a mut StartedRun) -> Self {
        Self {
            core,
            started,
            current_turn: None,
        }
    }
}

impl AgentObserver for ProductionObserver<'_> {
    type Error = TriageError;
    type Retry = RetryId;
    type Compaction = CompactionId;
    type Tool = ToolId;

    async fn checkpoint(&mut self, run: &AgentRun) -> Result<(), Self::Error> {
        serde_json::to_string(run)?;
        Ok(())
    }

    async fn turn_started(&mut self, ordinal: u32) -> Result<(), Self::Error> {
        self.current_turn = Some(
            self.core
                .database
                .start_turn(self.started.run_id, Utc::now())
                .await?,
        );
        self.started.log.start_turn(ordinal).await;
        self.core.report(format!("turn {ordinal}"));
        Ok(())
    }

    async fn turn_completed(
        &mut self,
        ordinal: u32,
        response: &CompletionResponse,
    ) -> Result<(), Self::Error> {
        let turn_id = self
            .current_turn
            .ok_or_else(|| TriageError::Configuration("model turn telemetry is missing".into()))?;
        self.core
            .record_response(self.started, turn_id, ordinal, response)
            .await
    }

    async fn turn_failed(&mut self, ordinal: u32, reason: &str) -> Result<(), Self::Error> {
        if let Some(turn_id) = self.current_turn.take() {
            self.core
                .finish_failed_turn(self.started, turn_id, ordinal, reason)
                .await;
        }
        Ok(())
    }

    async fn retry_started(
        &mut self,
        scope: CompletionScope,
        attempt: usize,
        max_retries: usize,
        error: &CompletionError,
        delay: Duration,
    ) -> Result<Self::Retry, Self::Error> {
        let category = completion_category(error);
        let turn_id = (scope == CompletionScope::Model)
            .then_some(self.current_turn)
            .flatten();
        let retry_id = self
            .core
            .database
            .start_retry(
                self.started.run_id,
                turn_id,
                RetryStart {
                    attempt: attempt as u32,
                    max_attempts: max_retries as u32,
                    delay_ms: duration_millis(delay),
                    error_category: Some(category),
                },
                Utc::now(),
            )
            .await?;
        self.started
            .log
            .start_retry(
                attempt as u32,
                max_retries as u32,
                duration_millis(delay),
                Some(category),
            )
            .await;
        self.core.report(format!(
            "↻ {} retry {attempt}: {}",
            scope.as_str(),
            super::driver::retry_reason(error)
        ));
        Ok(retry_id)
    }

    async fn retry_finished(
        &mut self,
        retry: Self::Retry,
        finish: SpanFinish,
    ) -> Result<(), Self::Error> {
        self.core
            .database
            .finish_retry(
                self.started.run_id,
                retry,
                match finish {
                    SpanFinish::Completed => SpanOutcome::Succeeded,
                    SpanFinish::Failed => SpanOutcome::Failed,
                    SpanFinish::Interrupted => SpanOutcome::Aborted,
                },
                None,
                Utc::now(),
            )
            .await?;
        self.started
            .log
            .finish_retry(finish == SpanFinish::Completed)
            .await;
        Ok(())
    }

    async fn compaction_started(
        &mut self,
        reason: &str,
        _source_message_count: usize,
    ) -> Result<Self::Compaction, Self::Error> {
        let compaction_id = self
            .core
            .database
            .start_compaction(self.started.run_id, None, reason.into(), Utc::now())
            .await?;
        self.started.log.start_compaction(reason).await;
        self.core.report("◇ compacting context".into());
        Ok(compaction_id)
    }

    async fn compaction_finished(
        &mut self,
        compaction: Self::Compaction,
        finish: SpanFinish,
        usage: Option<rig_core::completion::Usage>,
    ) -> Result<(), Self::Error> {
        self.core
            .database
            .finish_compaction(
                self.started.run_id,
                compaction,
                CompactionFinish {
                    outcome: match finish {
                        SpanFinish::Completed => SpanOutcome::Succeeded,
                        SpanFinish::Failed => SpanOutcome::Failed,
                        SpanFinish::Interrupted => SpanOutcome::Aborted,
                    },
                    aborted: finish == SpanFinish::Interrupted,
                    will_retry: false,
                    tokens_before: None,
                    estimated_tokens_after: None,
                    usage: usage
                        .as_ref()
                        .and_then(|usage| reported_usage(Some(usage), usage.has_values())),
                },
                Utc::now(),
            )
            .await?;
        self.started
            .log
            .finish_compaction(serde_json::json!({
                "outcome": match finish {
                    SpanFinish::Completed => "succeeded",
                    SpanFinish::Failed => "failed",
                    SpanFinish::Interrupted => "aborted",
                }
            }))
            .await;
        Ok(())
    }

    async fn tool_started(&mut self, call: &ToolCall) -> Result<Self::Tool, Self::Error> {
        let name = call.function.name.clone();
        let summary = tool_summary(call, &self.core.command_policy);
        let tool_id = self
            .core
            .database
            .start_tool(
                self.started.run_id,
                self.current_turn,
                name.clone(),
                summary.clone(),
                Utc::now(),
            )
            .await?;
        self.started.log.start_tool(&name, summary.as_deref()).await;
        self.core
            .report(format!("◆ {name} {}", summary.unwrap_or_default()));
        Ok(tool_id)
    }

    async fn tool_finished(
        &mut self,
        tool: Self::Tool,
        call: &ToolCall,
        result: &ToolCallResult,
        outcome: ToolOutcome,
    ) -> Result<(), Self::Error> {
        let name = call.function.name.clone();
        self.started.tool_observations.push(ToolObservation {
            name: name.clone(),
            outcome: match outcome {
                ToolOutcome::Succeeded => ToolObservationOutcome::Succeeded,
                ToolOutcome::Failed => ToolObservationOutcome::Failed,
                ToolOutcome::Denied => ToolObservationOutcome::Denied,
            },
        });
        self.core
            .database
            .finish_tool(
                self.started.run_id,
                tool,
                match outcome {
                    ToolOutcome::Succeeded => SpanOutcome::Succeeded,
                    ToolOutcome::Failed => SpanOutcome::Failed,
                    ToolOutcome::Denied => SpanOutcome::Aborted,
                },
                Utc::now(),
            )
            .await?;
        self.started
            .log
            .finish_tool(&name, result.failed, Some(&result.output))
            .await;
        self.core.report(format!(
            "{} {name}\n{}",
            if result.failed { "✗" } else { "✓" },
            terminal_preview(&self.core.filtered(&result.output))
        ));
        Ok(())
    }
}

fn map_engine_error(error: EngineError<TriageError>) -> TriageError {
    match error {
        EngineError::Prompt(error) => TriageError::Prompt(error),
        EngineError::Completion(error) => TriageError::Completion(error),
        EngineError::ContextLimit(error) => TriageError::ContextLimit(error),
        EngineError::Cancelled => TriageError::Cancelled,
        EngineError::Projection(error) => TriageError::Projection(error),
        EngineError::Serialization(error) => TriageError::Json(error),
        EngineError::Observer(error) => error,
        EngineError::RecordingFailure => TriageError::RecordingFailure,
        EngineError::NoSafeCompactionBoundary => TriageError::NoSafeCompactionBoundary,
        EngineError::EmptyCompactionSummary => {
            TriageError::Driver(DriverError::EmptyCompactionSummary)
        }
    }
}

fn thinking(level: ConfigThinkingLevel) -> ThinkingLevel {
    match level {
        ConfigThinkingLevel::Off => ThinkingLevel::Off,
        ConfigThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ConfigThinkingLevel::Low => ThinkingLevel::Low,
        ConfigThinkingLevel::Medium => ThinkingLevel::Medium,
        ConfigThinkingLevel::High => ThinkingLevel::High,
        ConfigThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ConfigThinkingLevel::Max => ThinkingLevel::Max,
    }
}

fn assistant_texts(response: &CompletionResponse) -> impl Iterator<Item = &str> {
    response.choice.iter().filter_map(|content| match content {
        AssistantContent::Text(text) => Some(text.text.as_str()),
        _ => None,
    })
}

fn reasoning_summaries(response: &CompletionResponse) -> Vec<String> {
    response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Reasoning(reasoning) => Some(&reasoning.content),
            _ => None,
        })
        .flatten()
        .filter_map(|content| match content {
            ReasoningContent::Summary(summary) => Some(summary.clone()),
            _ => None,
        })
        .collect()
}

fn tool_summary(call: &ToolCall, policy: &CommandPolicy) -> Option<String> {
    let value = match call.function.name.as_str() {
        "bash" => call.function.arguments.get("command"),
        "read" | "write" => call.function.arguments.get("path"),
        _ => None,
    }?
    .as_str()?;
    Some(bounded_chars(&policy.filter(value), 1000))
}

fn terminal_preview(value: &str) -> String {
    bounded_chars(value, 4000)
}

fn bounded_chars(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… output truncated", &value[..end])
}

pub struct ExplicitDriver<'a, M, T> {
    model: &'a M,
    tools: &'a T,
    telemetry: PrototypeTelemetry,
    cancellation_telemetry: CancellationTelemetry,
    cancellation: CancellationToken,
    system_instructions: String,
    thinking: ThinkingLevel,
    retry_policy: ProviderRetryPolicy,
    compaction_retry_policy: ProviderRetryPolicy,
    compaction: CompactionConfig,
}

impl<'a, M, T> ExplicitDriver<'a, M, T>
where
    M: CompletionModel + Send + Sync,
    T: ToolExecutor,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: &'a M,
        tools: &'a T,
        telemetry: PrototypeTelemetry,
        cancellation_telemetry: CancellationTelemetry,
        cancellation: CancellationToken,
        system_instructions: impl Into<String>,
        thinking: ThinkingLevel,
        retry_policy: ProviderRetryPolicy,
        compaction: CompactionConfig,
    ) -> Self {
        Self {
            model,
            tools,
            telemetry,
            cancellation_telemetry,
            cancellation,
            system_instructions: system_instructions.into(),
            thinking,
            compaction_retry_policy: retry_policy.clone(),
            retry_policy,
            compaction,
        }
    }

    pub fn with_compaction_retry_policy(mut self, policy: ProviderRetryPolicy) -> Self {
        self.compaction_retry_policy = policy;
        self
    }

    pub async fn run(
        &self,
        event_prompt: impl Into<Message>,
        history: Vec<Message>,
        max_turns: usize,
    ) -> Result<PromptResponse, DriverError> {
        let model = RigModel::new(
            self.model,
            self.system_instructions.clone(),
            self.thinking,
            RequestTools::Compatibility,
        );
        let observer =
            PrototypeObserver::new(self.telemetry.clone(), self.cancellation_telemetry.clone());
        AgentEngine::new(
            &model,
            self.tools,
            observer,
            self.cancellation.clone(),
            self.retry_policy.clone(),
            self.compaction.clone(),
        )
        .with_compaction_retry_policy(self.compaction_retry_policy.clone())
        .run(event_prompt, history, max_turns)
        .await
        .map_err(DriverError::from)
    }
}
