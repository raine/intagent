use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use rig_agent::agent::PromptResponse;
use rig_agent::agent::hook::InvalidToolCallAction;
use rig_agent::agent::run::{AgentRun, AgentRunStep, ModelTurn, ModelTurnOutcome};
use rig_core::OneOrMany;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use rig_core::message::{AssistantContent, Message, ToolCall, ToolResultContent, UserContent};
use tokio_util::sync::CancellationToken;

use super::context::{CompactionConfig, ContextManager, ProjectionError};
use super::model::{
    ThinkingLevel, completion_request_for_history, summary_completion_request_for_history,
    triage_completion_request_for_history,
};
use super::tools::ToolCallResult;

#[derive(Clone, Debug)]
pub struct ProviderRetryPolicy {
    pub max_retries: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ProviderRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionScope {
    Model,
    Compaction,
}

impl CompletionScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Compaction => "compaction",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestTools {
    Compatibility,
    Production,
}

pub trait ModelBoundary: Send + Sync {
    fn complete(
        &self,
        scope: CompletionScope,
        history: Vec<Message>,
    ) -> impl Future<Output = Result<CompletionResponse, CompletionError>> + Send;
}

pub struct RigModel<'a, M> {
    model: &'a M,
    system_instructions: String,
    thinking: ThinkingLevel,
    tools: RequestTools,
}

impl<'a, M> RigModel<'a, M> {
    pub fn new(
        model: &'a M,
        system_instructions: impl Into<String>,
        thinking: ThinkingLevel,
        tools: RequestTools,
    ) -> Self {
        Self {
            model,
            system_instructions: system_instructions.into(),
            thinking,
            tools,
        }
    }

    fn request(&self, scope: CompletionScope, history: Vec<Message>) -> CompletionRequest {
        match (scope, self.tools) {
            (CompletionScope::Compaction, RequestTools::Production) => {
                summary_completion_request_for_history(
                    self.system_instructions.clone(),
                    history,
                    self.thinking,
                )
            }
            (CompletionScope::Model, RequestTools::Production) => {
                triage_completion_request_for_history(
                    self.system_instructions.clone(),
                    history,
                    self.thinking,
                )
            }
            (_, RequestTools::Compatibility) => completion_request_for_history(
                self.system_instructions.clone(),
                history,
                self.thinking,
            ),
        }
    }
}

impl<M: CompletionModel + Send + Sync> ModelBoundary for RigModel<'_, M> {
    async fn complete(
        &self,
        scope: CompletionScope,
        history: Vec<Message>,
    ) -> Result<CompletionResponse, CompletionError> {
        self.model.completion(self.request(scope, history)).await
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpanFinish {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    Denied,
}

pub trait AgentObserver: Send {
    type Error;
    type Retry: Send;
    type Compaction: Send;
    type Tool: Send;

    fn checkpoint(
        &mut self,
        run: &AgentRun,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn turn_started(
        &mut self,
        ordinal: u32,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn turn_completed(
        &mut self,
        ordinal: u32,
        response: &CompletionResponse,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn turn_failed(
        &mut self,
        ordinal: u32,
        reason: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn retry_started(
        &mut self,
        scope: CompletionScope,
        attempt: usize,
        max_retries: usize,
        error: &CompletionError,
        delay: Duration,
    ) -> impl Future<Output = Result<Self::Retry, Self::Error>> + Send;

    fn retry_finished(
        &mut self,
        retry: Self::Retry,
        finish: SpanFinish,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn compaction_started(
        &mut self,
        reason: &str,
        source_message_count: usize,
    ) -> impl Future<Output = Result<Self::Compaction, Self::Error>> + Send;

    fn compaction_finished(
        &mut self,
        compaction: Self::Compaction,
        finish: SpanFinish,
        usage: Option<Usage>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn tool_started(
        &mut self,
        call: &ToolCall,
    ) -> impl Future<Output = Result<Self::Tool, Self::Error>> + Send;

    fn tool_finished(
        &mut self,
        tool: Self::Tool,
        call: &ToolCall,
        result: &ToolCallResult,
        outcome: ToolOutcome,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

pub trait ToolExecutor: Send + Sync {
    fn authorize<'a>(
        &'a self,
        call: &'a ToolCall,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolCallResult> + Send + 'a>>;

    fn recording_failed(&self) -> bool {
        false
    }
}

pub struct AgentEngine<'a, M, T, O> {
    model: &'a M,
    tools: &'a T,
    observer: O,
    cancellation: CancellationToken,
    retry_policy: ProviderRetryPolicy,
    compaction_retry_policy: ProviderRetryPolicy,
    compaction: CompactionConfig,
}

impl<'a, M, T, O> AgentEngine<'a, M, T, O>
where
    M: ModelBoundary,
    T: ToolExecutor,
    O: AgentObserver,
{
    pub fn new(
        model: &'a M,
        tools: &'a T,
        observer: O,
        cancellation: CancellationToken,
        retry_policy: ProviderRetryPolicy,
        compaction: CompactionConfig,
    ) -> Self {
        Self {
            model,
            tools,
            observer,
            cancellation,
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
        mut self,
        event_prompt: impl Into<Message>,
        history: Vec<Message>,
        max_turns: usize,
    ) -> Result<PromptResponse, EngineError<O::Error>> {
        let event_prompt = event_prompt.into();
        let mut context = ContextManager::new(self.compaction.clone(), event_prompt.clone());
        let mut run = AgentRun::new(event_prompt)
            .with_history(history)
            .max_turns(max_turns);
        let tool_names = tool_names();

        loop {
            match run.next_step()? {
                AgentRunStep::CallModel {
                    prompt,
                    history,
                    turn,
                } => {
                    self.checkpoint(&run).await?;
                    let mut canonical = history;
                    canonical.push(prompt);
                    if context.should_compact() && context.candidate(&canonical).is_some() {
                        self.compact(&run, &mut context, &canonical, "proactive")
                            .await?;
                    }
                    let ordinal = turn as u32;
                    self.observer
                        .turn_started(ordinal)
                        .await
                        .map_err(EngineError::Observer)?;
                    let response = match self
                        .complete(&run, &context, &canonical, CompletionScope::Model)
                        .await
                    {
                        Err(EngineError::Completion(error)) if is_context_limit(&error) => {
                            if !context.begin_emergency_compaction() {
                                self.fail_turn(ordinal, "context_limit").await?;
                                return Err(EngineError::ContextLimit(error.to_string()));
                            }
                            if let Err(error) = self
                                .compact(&run, &mut context, &canonical, "emergency")
                                .await
                            {
                                self.fail_turn(ordinal, error.turn_reason()).await?;
                                return Err(error);
                            }
                            match self
                                .complete(&run, &context, &canonical, CompletionScope::Model)
                                .await
                            {
                                Err(EngineError::Completion(second))
                                    if is_context_limit(&second) =>
                                {
                                    self.fail_turn(ordinal, "context_limit").await?;
                                    return Err(EngineError::ContextLimit(second.to_string()));
                                }
                                Err(error) => {
                                    self.fail_turn(ordinal, error.turn_reason()).await?;
                                    return Err(error);
                                }
                                Ok(response) => response,
                            }
                        }
                        Err(error) => {
                            self.fail_turn(ordinal, error.turn_reason()).await?;
                            return Err(error);
                        }
                        Ok(response) => response,
                    };
                    context.observe_input_tokens(response.usage.input_tokens);
                    self.observer
                        .turn_completed(ordinal, &response)
                        .await
                        .map_err(EngineError::Observer)?;
                    let outcome = run.model_response(ModelTurn::new(
                        response.message_id,
                        response.choice,
                        response.usage,
                        tool_names.clone(),
                        tool_names.clone(),
                    ))?;
                    resolve_invalid_calls(&mut run, outcome)?;
                }
                AgentRunStep::CallTools { calls } => {
                    let mut results = Vec::with_capacity(calls.len());
                    for call in calls {
                        self.checkpoint(&run).await?;
                        if let Some(result) = call.preresolved_result {
                            results.push(result);
                            continue;
                        }
                        let tool = self
                            .observer
                            .tool_started(&call.tool_call)
                            .await
                            .map_err(EngineError::Observer)?;
                        let authorization = tokio::select! {
                            biased;
                            _ = self.cancellation.cancelled() => {
                                return Err(EngineError::Cancelled);
                            }
                            result = self.tools.authorize(&call.tool_call) => result,
                        };
                        let result = if let Err(reason) = authorization {
                            ToolCallResult::denied(reason)
                        } else {
                            tokio::select! {
                                biased;
                                _ = self.cancellation.cancelled() => {
                                    return Err(EngineError::Cancelled);
                                }
                                result = self.tools.execute(
                                    &call.tool_call,
                                    self.cancellation.clone(),
                                ) => result,
                            }
                        };
                        let outcome = if result.denied {
                            ToolOutcome::Denied
                        } else if result.failed {
                            ToolOutcome::Failed
                        } else {
                            ToolOutcome::Succeeded
                        };
                        self.observer
                            .tool_finished(tool, &call.tool_call, &result, outcome)
                            .await
                            .map_err(EngineError::Observer)?;
                        results.push(tool_result(call.tool_call, result.output));
                    }
                    run.tool_results(results)?;
                }
                AgentRunStep::Done(_) if self.tools.recording_failed() => {
                    return Err(EngineError::RecordingFailure);
                }
                AgentRunStep::Done(response) => return Ok(response),
            }
        }
    }

    async fn checkpoint(&mut self, run: &AgentRun) -> Result<(), EngineError<O::Error>> {
        self.observer
            .checkpoint(run)
            .await
            .map_err(EngineError::Observer)
    }

    async fn fail_turn(&mut self, ordinal: u32, reason: &str) -> Result<(), EngineError<O::Error>> {
        self.observer
            .turn_failed(ordinal, reason)
            .await
            .map_err(EngineError::Observer)
    }

    async fn complete(
        &mut self,
        run: &AgentRun,
        context: &ContextManager,
        canonical: &[Message],
        scope: CompletionScope,
    ) -> Result<CompletionResponse, EngineError<O::Error>> {
        let projected = context.project(canonical)?;
        self.complete_with_retries(run, projected, scope).await
    }

    async fn compact(
        &mut self,
        run: &AgentRun,
        context: &mut ContextManager,
        canonical: &[Message],
        reason: &str,
    ) -> Result<(), EngineError<O::Error>> {
        let candidate = context
            .candidate(canonical)
            .ok_or(EngineError::NoSafeCompactionBoundary)?;
        let prompt = candidate.summary_prompt()?;
        self.checkpoint(run).await?;
        let compaction = self
            .observer
            .compaction_started(reason, candidate.messages().len())
            .await
            .map_err(EngineError::Observer)?;
        let response = match self
            .complete_with_retries(
                run,
                vec![Message::user(prompt)],
                CompletionScope::Compaction,
            )
            .await
        {
            Ok(response) => response,
            Err(error @ EngineError::Cancelled) => {
                self.observer
                    .compaction_finished(compaction, SpanFinish::Interrupted, None)
                    .await
                    .map_err(EngineError::Observer)?;
                return Err(error);
            }
            Err(error) => {
                self.observer
                    .compaction_finished(compaction, SpanFinish::Failed, None)
                    .await
                    .map_err(EngineError::Observer)?;
                return Err(error);
            }
        };
        let summary = match assistant_text(&response) {
            Some(summary) => summary,
            None => {
                self.observer
                    .compaction_finished(compaction, SpanFinish::Failed, Some(response.usage))
                    .await
                    .map_err(EngineError::Observer)?;
                return Err(EngineError::EmptyCompactionSummary);
            }
        };
        let usage = response.usage;
        context.apply(candidate, summary);
        self.observer
            .compaction_finished(compaction, SpanFinish::Completed, Some(usage))
            .await
            .map_err(EngineError::Observer)?;
        Ok(())
    }

    async fn complete_with_retries(
        &mut self,
        run: &AgentRun,
        history: Vec<Message>,
        scope: CompletionScope,
    ) -> Result<CompletionResponse, EngineError<O::Error>> {
        let policy = if scope == CompletionScope::Compaction {
            self.compaction_retry_policy.clone()
        } else {
            self.retry_policy.clone()
        };
        let mut retries = 0;
        loop {
            self.checkpoint(run).await?;
            let result = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => return Err(EngineError::Cancelled),
                result = self.model.complete(scope, history.clone()) => result,
            };
            match result {
                Ok(response) => return Ok(response),
                Err(error) if is_retryable(&error) && retries < policy.max_retries => {
                    retries += 1;
                    let delay = retry_delay(&policy, retries);
                    let retry = self
                        .observer
                        .retry_started(scope, retries, policy.max_retries, &error, delay)
                        .await
                        .map_err(EngineError::Observer)?;
                    let finish = tokio::select! {
                        biased;
                        _ = self.cancellation.cancelled() => SpanFinish::Interrupted,
                        _ = tokio::time::sleep(delay) => SpanFinish::Completed,
                    };
                    self.observer
                        .retry_finished(retry, finish)
                        .await
                        .map_err(EngineError::Observer)?;
                    if finish == SpanFinish::Interrupted {
                        return Err(EngineError::Cancelled);
                    }
                }
                Err(error) => return Err(EngineError::Completion(error)),
            }
        }
    }
}

fn tool_result(call: ToolCall, output: String) -> UserContent {
    let content = OneOrMany::one(ToolResultContent::text(output));
    if let Some(call_id) = call.call_id {
        UserContent::tool_result_with_call_id(call.id, call_id, content)
    } else {
        UserContent::tool_result(call.id, content)
    }
}

fn resolve_invalid_calls(
    run: &mut AgentRun,
    mut outcome: ModelTurnOutcome,
) -> Result<(), rig_agent::completion::PromptError> {
    loop {
        match outcome {
            ModelTurnOutcome::NeedsResolution(_) => {
                outcome = run.resolve_invalid_tool_call(InvalidToolCallAction::skip(
                    "denied: tool is outside the intake capability set",
                ))?;
            }
            ModelTurnOutcome::Continue { .. } | ModelTurnOutcome::TurnRetried => return Ok(()),
        }
    }
}

pub fn tool_names() -> BTreeSet<String> {
    ["bash", "read", "write"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn assistant_text(response: &CompletionResponse) -> Option<String> {
    let text = response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn retry_delay(policy: &ProviderRetryPolicy, retry: usize) -> Duration {
    let exponent = u32::try_from(retry.saturating_sub(1)).unwrap_or(u32::MAX);
    policy
        .initial_delay
        .checked_mul(2_u32.saturating_pow(exponent))
        .unwrap_or(policy.max_delay)
        .min(policy.max_delay)
}

pub fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

pub fn retry_reason(error: &CompletionError) -> &'static str {
    match error
        .provider_response_status()
        .map(|status| status.as_u16())
    {
        Some(429) => "rate_limit",
        Some(500..=599) => "server",
        Some(_) => "provider",
        None if error.to_string().contains("429") => "rate_limit",
        None if error.to_string().contains("500") => "server",
        None => "transport",
    }
}

pub fn is_retryable(error: &CompletionError) -> bool {
    if is_context_limit(error) {
        return false;
    }
    match error
        .provider_response_status()
        .map(|status| status.as_u16())
    {
        Some(408 | 409 | 425 | 429 | 500 | 502 | 503 | 504) => true,
        Some(_) => false,
        None => match error {
            CompletionError::HttpError(_) => true,
            CompletionError::ProviderError(message) => {
                let message = message.to_ascii_lowercase();
                [
                    "http 408",
                    "http 409",
                    "http 425",
                    "http 429",
                    "http 500",
                    "http 502",
                    "http 503",
                    "http 504",
                    "connection",
                    "timeout",
                    "temporarily unavailable",
                    "overloaded",
                ]
                .iter()
                .any(|marker| message.contains(marker))
            }
            _ => false,
        },
    }
}

pub fn is_context_limit(error: &CompletionError) -> bool {
    let display = error.to_string().to_ascii_lowercase();
    if display.contains("context_length_exceeded")
        || display.contains("context limit")
        || display.contains("maximum context")
    {
        return true;
    }
    error
        .provider_response_json()
        .ok()
        .flatten()
        .and_then(|body| {
            body.pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|code| code == "context_length_exceeded")
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError<E> {
    #[error("agent run failed: {0}")]
    Prompt(#[from] rig_agent::completion::PromptError),
    #[error("provider completion failed: {0}")]
    Completion(CompletionError),
    #[error("context limit: {0}")]
    ContextLimit(String),
    #[error("agent run was canceled")]
    Cancelled,
    #[error("history projection failed: {0}")]
    Projection(#[from] ProjectionError),
    #[error("agent state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("agent observer failed")]
    Observer(E),
    #[error("tool telemetry recording failed")]
    RecordingFailure,
    #[error("history has no safe compaction boundary")]
    NoSafeCompactionBoundary,
    #[error("compaction response contained no summary text")]
    EmptyCompactionSummary,
}

impl<E> EngineError<E> {
    fn turn_reason(&self) -> &'static str {
        match self {
            Self::ContextLimit(_) => "context_limit",
            Self::Cancelled => "cancelled",
            _ => "error",
        }
    }
}
