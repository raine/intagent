use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use rig_agent::agent::PromptResponse;
use rig_agent::agent::hook::InvalidToolCallAction;
use rig_agent::agent::run::{AgentRun, AgentRunStep, ModelTurn, ModelTurnOutcome};
use rig_agent::completion::PromptError;
use rig_core::OneOrMany;
use rig_core::completion::{CompletionError, CompletionModel, CompletionResponse};
use rig_core::message::{AssistantContent, Message, ToolCall, ToolResultContent, UserContent};
use tokio_util::sync::CancellationToken;

use super::context::{CompactionConfig, ContextManager, ProjectionError};
use super::model::{ThinkingLevel, completion_request_for_history};
use super::telemetry::{CancellationTelemetry, PrototypeTelemetry};
use super::tools::{CountingTools, RecordingExecutableTools, ToolCallResult};

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
    M: CompletionModel,
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
        let event_prompt = event_prompt.into();
        let mut context = ContextManager::new(self.compaction.clone(), event_prompt.clone());
        let mut run = AgentRun::new(event_prompt)
            .with_history(history)
            .max_turns(max_turns);
        let tool_names = tool_names();

        loop {
            match run.next_step()? {
                AgentRunStep::CallModel {
                    prompt, history, ..
                } => {
                    self.cancellation_telemetry.checkpoint(&run)?;
                    let mut canonical = history;
                    canonical.push(prompt);
                    if context.should_compact() {
                        self.compact(&run, &mut context, &canonical, "proactive")
                            .await?;
                    }
                    let response = match self.complete(&run, &context, &canonical, "model").await {
                        Err(DriverError::Completion(error)) if is_context_limit(&error) => {
                            if !context.begin_emergency_compaction() {
                                return Err(DriverError::ContextLimit(error.to_string()));
                            }
                            self.compact(&run, &mut context, &canonical, "emergency")
                                .await?;
                            match self.complete(&run, &context, &canonical, "model").await {
                                Err(DriverError::Completion(second))
                                    if is_context_limit(&second) =>
                                {
                                    return Err(DriverError::ContextLimit(second.to_string()));
                                }
                                result => result?,
                            }
                        }
                        result => result?,
                    };
                    context.observe_input_tokens(response.usage.input_tokens);
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
                        self.cancellation_telemetry.checkpoint(&run)?;
                        if let Some(result) = call.preresolved_result {
                            results.push(result);
                            continue;
                        }
                        let result = if let Err(reason) = authorize_tool_call(&call.tool_call) {
                            ToolCallResult::denied(reason)
                        } else {
                            tokio::select! {
                                biased;
                                _ = self.cancellation.cancelled() => return Err(DriverError::Cancelled),
                                result = self.tools.execute(&call.tool_call, self.cancellation.clone()) => result,
                            }
                        };
                        let content = OneOrMany::one(ToolResultContent::text(result.output));
                        let tool_result = if let Some(call_id) = call.tool_call.call_id {
                            UserContent::tool_result_with_call_id(
                                call.tool_call.id,
                                call_id,
                                content,
                            )
                        } else {
                            UserContent::tool_result(call.tool_call.id, content)
                        };
                        results.push(tool_result);
                    }
                    run.tool_results(results)?;
                }
                AgentRunStep::Done(response) => return Ok(response),
            }
        }
    }

    async fn complete(
        &self,
        run: &AgentRun,
        context: &ContextManager,
        canonical: &[Message],
        scope: &str,
    ) -> Result<CompletionResponse, DriverError> {
        let projected = context.project(canonical)?;
        let request = completion_request_for_history(
            self.system_instructions.clone(),
            projected,
            self.thinking,
        );
        self.complete_with_retries(run, request, scope).await
    }

    async fn compact(
        &self,
        run: &AgentRun,
        context: &mut ContextManager,
        canonical: &[Message],
        reason: &str,
    ) -> Result<(), DriverError> {
        let candidate = context
            .candidate(canonical)
            .ok_or(DriverError::NoSafeCompactionBoundary)?;
        let prompt = candidate.summary_prompt()?;
        self.cancellation_telemetry.checkpoint(run)?;
        let span = self
            .telemetry
            .start_compaction(reason, candidate.messages().len())?;
        let request = completion_request_for_history(
            self.system_instructions.clone(),
            vec![Message::user(prompt)],
            self.thinking,
        );
        let response = match self.complete_with_retries(run, request, "compaction").await {
            Ok(response) => response,
            Err(error @ DriverError::Cancelled) => return Err(error),
            Err(error) => {
                span.fail(None);
                return Err(error);
            }
        };
        let summary = match assistant_text(&response) {
            Ok(summary) => summary,
            Err(error) => {
                span.fail(Some(response.usage));
                return Err(error);
            }
        };
        let usage = response.usage;
        context.apply(candidate, summary);
        span.complete(Some(usage));
        Ok(())
    }

    async fn complete_with_retries(
        &self,
        run: &AgentRun,
        request: rig_core::completion::CompletionRequest,
        scope: &str,
    ) -> Result<CompletionResponse, DriverError> {
        let policy = if scope == "compaction" {
            &self.compaction_retry_policy
        } else {
            &self.retry_policy
        };
        let mut retries = 0;
        loop {
            self.cancellation_telemetry.checkpoint(run)?;
            let result = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => return Err(DriverError::Cancelled),
                result = self.model.completion(request.clone()) => result,
            };
            match result {
                Ok(response) => return Ok(response),
                Err(error) if is_retryable(&error) && retries < policy.max_retries => {
                    retries += 1;
                    let delay = retry_delay(policy, retries);
                    let reason = retry_reason(&error);
                    let span = self.telemetry.start_retry(
                        scope,
                        retries,
                        reason,
                        duration_millis(delay),
                    )?;
                    tokio::select! {
                        biased;
                        _ = self.cancellation.cancelled() => return Err(DriverError::Cancelled),
                        _ = tokio::time::sleep(delay) => span.complete(None),
                    }
                }
                Err(error) => return Err(DriverError::Completion(error)),
            }
        }
    }
}

pub trait ToolExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolCallResult> + Send + 'a>>;
}

impl ToolExecutor for RecordingExecutableTools {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolCallResult> + Send + 'a>> {
        Box::pin(async move {
            self.call(&call.function.name, &call.function.arguments, cancellation)
                .await
        })
    }
}

impl ToolExecutor for CountingTools {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ToolCallResult> + Send + 'a>> {
        Box::pin(async move {
            let value = call
                .function
                .arguments
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            self.call(&call.function.name, value)
        })
    }
}

fn authorize_tool_call(call: &ToolCall) -> Result<(), String> {
    if !matches!(call.function.name.as_str(), "bash" | "read" | "write") {
        return Err("tool is outside the intake capability set".to_string());
    }
    let Some(arguments) = call.function.arguments.as_object() else {
        return Err("tool arguments must be an object".to_string());
    };
    if arguments.len() != 1
        || arguments
            .get("value")
            .and_then(serde_json::Value::as_str)
            .is_none()
    {
        return Err("tool arguments failed compatibility policy".to_string());
    }
    Ok(())
}

fn resolve_invalid_calls(
    run: &mut AgentRun,
    mut outcome: ModelTurnOutcome,
) -> Result<(), PromptError> {
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

fn tool_names() -> BTreeSet<String> {
    ["bash", "read", "write"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn assistant_text(response: &CompletionResponse) -> Result<String, DriverError> {
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
        Err(DriverError::EmptyCompactionSummary)
    } else {
        Ok(text)
    }
}

fn retry_delay(policy: &ProviderRetryPolicy, retry: usize) -> Duration {
    let exponent = u32::try_from(retry.saturating_sub(1)).unwrap_or(u32::MAX);
    policy
        .initial_delay
        .checked_mul(2_u32.saturating_pow(exponent))
        .unwrap_or(policy.max_delay)
        .min(policy.max_delay)
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn retry_reason(error: &CompletionError) -> &'static str {
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

fn is_retryable(error: &CompletionError) -> bool {
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

fn is_context_limit(error: &CompletionError) -> bool {
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
pub enum DriverError {
    #[error("agent run failed: {0}")]
    Prompt(#[from] PromptError),
    #[error("provider completion failed: {0}")]
    Completion(CompletionError),
    #[error("context limit: {0}")]
    ContextLimit(String),
    #[error("agent run was canceled")]
    Cancelled,
    #[error("history projection failed: {0}")]
    Projection(#[from] ProjectionError),
    #[error("telemetry database failed: {0}")]
    Telemetry(#[from] rusqlite::Error),
    #[error("agent state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("history has no safe compaction boundary")]
    NoSafeCompactionBoundary,
    #[error("compaction response contained no summary text")]
    EmptyCompactionSummary,
}
