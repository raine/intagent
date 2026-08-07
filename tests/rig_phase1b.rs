use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use intake::agent::context::CompactionConfig;
use intake::agent::model::ThinkingLevel;
use intake::agent::rig_runner::{ExplicitDriver, ProviderRetryPolicy};
use intake::agent::telemetry::{CancellationTelemetry, PrototypeTelemetry};
use intake::agent::tools::{CountingTools, RecordingExecutableTools};
use rig_agent::agent::hook::{AgentHook, CompletionCallAction, HookContext, RequestPatch};
use rig_agent::agent::{AgentBuilder, CompletionCallEvent};
use rig_core::OneOrMany;
use rig_core::completion::Usage;
use rig_core::message::{
    AssistantContent, Message, Reasoning, ToolCall, ToolFunction, ToolResultContent, UserContent,
};
use rig_core::test_utils::{MockCompletionModel, MockTurn};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const SYSTEM_INSTRUCTIONS: &str = "Intake instructions are authoritative.";

#[tokio::test]
async fn explicit_driver_retries_429_and_500_with_two_real_rows() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let model = MockCompletionModel::from_turns([
        MockTurn::error("scripted HTTP 429"),
        MockTurn::error("scripted HTTP 500"),
        MockTurn::text("done"),
    ]);
    let tools = CountingTools::default();
    let driver = driver(
        &model,
        &tools,
        telemetry.clone(),
        CancellationToken::new(),
        CompactionConfig::default(),
        Duration::from_millis(2),
    );

    let response = driver
        .run("triage the fixture", Vec::new(), 1)
        .await
        .expect("bounded retries should recover");

    assert_eq!(response.output, "done");
    assert_eq!(model.request_count(), 3);
    let rows = telemetry.retry_rows().expect("retry rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].reason, "rate_limit");
    assert_eq!(rows[1].reason, "server");
    assert_eq!(rows[0].delay_ms, 2);
    assert_eq!(rows[1].delay_ms, 4);
    assert!(rows.iter().all(|row| row.outcome == "completed"));
    assert!(
        telemetry
            .compaction_rows()
            .expect("compaction rows")
            .is_empty()
    );
}

#[tokio::test]
async fn compaction_records_one_summary_and_preserves_canonical_recent_groups() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let encrypted_reasoning = AssistantContent::Reasoning(
        Reasoning::encrypted("opaque-reasoning").with_id("reasoning-1".to_string()),
    );
    let tool_call = ToolCall::new(
        "call-1".to_string(),
        ToolFunction::new("bash".to_string(), json!({"value": "allowed"})),
    )
    .with_call_id("provider-call-1".to_string());
    let assistant_group = vec![
        encrypted_reasoning.clone(),
        AssistantContent::ToolCall(tool_call.clone()),
    ];
    let model = MockCompletionModel::from_turns([
        MockTurn::from_contents(assistant_group.clone())
            .expect("assistant content")
            .with_usage(usage(120, 8)),
        MockTurn::text("old context summary").with_usage(usage(18, 6)),
        MockTurn::text("finished").with_usage(usage(24, 4)),
    ]);
    let tools = CountingTools::default();
    let compaction = CompactionConfig {
        trigger_tokens: 100,
        keep_recent_groups: 1,
        max_compactions: 1,
    };
    let driver = driver(
        &model,
        &tools,
        telemetry.clone(),
        CancellationToken::new(),
        compaction,
        Duration::from_millis(1),
    );
    let old_history = vec![Message::user("old one"), Message::assistant("old two")];

    let response = driver
        .run("event prompt", old_history, 2)
        .await
        .expect("compacted run");

    assert_eq!(response.output, "finished");
    assert_eq!(model.request_count(), 3);
    let requests = model.requests();
    let summary_request = &requests[1];
    let summary_wire =
        serde_json::to_string(&summary_request.chat_history).expect("serialize summary request");
    assert!(summary_wire.contains("<untrusted-content>"));
    assert!(summary_wire.contains("old one"));
    assert!(summary_wire.contains("old two"));

    let projected = requests[2].chat_history.iter().cloned().collect::<Vec<_>>();
    assert!(!message_wire(&projected).contains("old one"));
    assert!(message_wire(&projected).contains("old context summary"));
    let expected_assistant = Message::Assistant {
        id: None,
        content: OneOrMany::many(assistant_group).expect("assistant group"),
    };
    assert!(projected.contains(&expected_assistant));
    let expected_result = Message::User {
        content: OneOrMany::one(UserContent::tool_result_with_call_id(
            "call-1",
            "provider-call-1".to_string(),
            OneOrMany::one(ToolResultContent::text("bash executed")),
        )),
    };
    assert!(projected.contains(&expected_result));
    assert!(message_wire(&projected).contains("opaque-reasoning"));

    let rows = telemetry.compaction_rows().expect("compaction rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "proactive");
    assert_eq!(rows[0].source_message_count, 2);
    assert_eq!(rows[0].input_tokens, Some(18));
    assert_eq!(rows[0].output_tokens, Some(6));
    assert_eq!(rows[0].outcome, "completed");
    assert!(telemetry.retry_rows().expect("retry rows").is_empty());
}

#[tokio::test]
async fn cancellation_interrupts_retry_wait_and_closes_the_row() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let cancellation = CancellationToken::new();
    let task_telemetry = telemetry.clone();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let model = MockCompletionModel::from_turns([
            MockTurn::error("scripted HTTP 500"),
            MockTurn::text("must not run"),
        ]);
        let tools = CountingTools::default();
        let driver = driver(
            &model,
            &tools,
            task_telemetry,
            task_cancellation,
            CompactionConfig::default(),
            Duration::from_secs(30),
        );
        driver.run("cancel fixture", Vec::new(), 1).await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if telemetry.retry_rows().expect("retry rows").len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("retry wait should begin");
    cancellation.cancel();
    let error = task
        .await
        .expect("driver task")
        .expect_err("cancellation should stop the run");
    assert!(error.to_string().contains("canceled"));
    let rows = telemetry.retry_rows().expect("retry rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].outcome, "interrupted");
}

#[tokio::test]
async fn compaction_retries_are_bounded_and_attributed_to_the_summary() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("call-1", "bash", json!({"value": "allowed"}))
            .with_usage(usage(120, 8)),
        MockTurn::error("scripted HTTP 429"),
        MockTurn::error("scripted HTTP 500"),
        MockTurn::text("summary after retries").with_usage(usage(20, 5)),
        MockTurn::text("finished"),
    ]);
    let tools = CountingTools::default();
    let driver = driver(
        &model,
        &tools,
        telemetry.clone(),
        CancellationToken::new(),
        CompactionConfig {
            trigger_tokens: 100,
            keep_recent_groups: 1,
            max_compactions: 1,
        },
        Duration::from_millis(1),
    )
    .with_compaction_retry_policy(ProviderRetryPolicy {
        max_retries: 2,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
    });

    let response = driver
        .run(
            "event prompt",
            vec![Message::user("old one"), Message::assistant("old two")],
            2,
        )
        .await
        .expect("summary retries should recover");

    assert_eq!(response.output, "finished");
    assert_eq!(model.request_count(), 5);
    let retries = telemetry.retry_rows().expect("retry rows");
    assert_eq!(retries.len(), 2);
    assert!(retries.iter().all(|row| row.scope == "compaction"));
    assert!(retries.iter().all(|row| row.outcome == "completed"));
    let compactions = telemetry.compaction_rows().expect("compaction rows");
    assert_eq!(compactions.len(), 1);
    assert_eq!(compactions[0].outcome, "completed");
}

#[tokio::test]
async fn cancellation_interrupts_compaction_and_its_retry_wait() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let cancellation = CancellationToken::new();
    let task_telemetry = telemetry.clone();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let model = MockCompletionModel::from_turns([
            MockTurn::tool_call("call-1", "bash", json!({"value": "allowed"}))
                .with_usage(usage(120, 8)),
            MockTurn::error("scripted HTTP 500"),
            MockTurn::text("must not run"),
        ]);
        let tools = CountingTools::default();
        let driver = driver(
            &model,
            &tools,
            task_telemetry,
            task_cancellation,
            CompactionConfig {
                trigger_tokens: 100,
                keep_recent_groups: 1,
                max_compactions: 1,
            },
            Duration::from_secs(30),
        );
        driver
            .run(
                "event prompt",
                vec![Message::user("old one"), Message::assistant("old two")],
                2,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let retries = telemetry.retry_rows().expect("retry rows");
            let compactions = telemetry.compaction_rows().expect("compaction rows");
            if retries.len() == 1 && compactions.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("compaction retry should begin");
    cancellation.cancel();
    let error = task
        .await
        .expect("driver task")
        .expect_err("cancellation should stop compaction");
    assert!(error.to_string().contains("canceled"));
    assert_eq!(
        telemetry.retry_rows().expect("retry rows")[0].outcome,
        "interrupted"
    );
    assert_eq!(
        telemetry.compaction_rows().expect("compaction rows")[0].outcome,
        "interrupted"
    );
}

#[tokio::test]
async fn emergency_compaction_runs_once_and_second_context_limit_is_terminal() {
    let temporary = TempDir::new().expect("temporary directory");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let model = MockCompletionModel::from_turns([
        MockTurn::error("context_length_exceeded"),
        MockTurn::text("emergency summary").with_usage(usage(20, 5)),
        MockTurn::error("context_length_exceeded"),
    ]);
    let tools = CountingTools::default();
    let driver = driver(
        &model,
        &tools,
        telemetry.clone(),
        CancellationToken::new(),
        CompactionConfig {
            trigger_tokens: u64::MAX,
            keep_recent_groups: 1,
            max_compactions: 1,
        },
        Duration::from_millis(1),
    );

    let error = driver
        .run(
            "event prompt",
            vec![Message::user("old one"), Message::assistant("old two")],
            1,
        )
        .await
        .expect_err("second context limit should fail the run");

    assert!(error.to_string().contains("context limit"));
    assert_eq!(model.request_count(), 3);
    let compactions = telemetry.compaction_rows().expect("compaction rows");
    assert_eq!(compactions.len(), 1);
    assert_eq!(compactions[0].reason, "emergency");
    assert_eq!(compactions[0].outcome, "completed");
    assert!(telemetry.retry_rows().expect("retry rows").is_empty());
}

#[tokio::test]
async fn explicit_driver_dispatches_only_to_the_recording_executable() {
    let temporary = TempDir::new().expect("temporary directory");
    let recording = temporary.path().join("recording-tool");
    let log = temporary.path().join("calls.log");
    fs::write(
        &recording,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$1\" \"$2\" >> '{}'\nprintf 'recorded'\n",
            log.display()
        ),
    )
    .expect("write recording executable");
    fs::set_permissions(&recording, fs::Permissions::from_mode(0o700))
        .expect("make recording executable runnable");
    let tools = RecordingExecutableTools::new(
        recording
            .canonicalize()
            .expect("canonical recording executable"),
    )
    .expect("recording tools");
    let telemetry = PrototypeTelemetry::open(&temporary.path().join("telemetry.sqlite"))
        .expect("temporary telemetry database");
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("call-1", "bash", json!({"value": "allowed"})),
        MockTurn::text("finished"),
    ]);
    let driver = ExplicitDriver::new(
        &model,
        &tools,
        telemetry,
        CancellationTelemetry::default(),
        CancellationToken::new(),
        SYSTEM_INSTRUCTIONS,
        ThinkingLevel::Max,
        ProviderRetryPolicy {
            max_retries: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        },
        CompactionConfig::default(),
    );

    let response = driver
        .run("record one call", Vec::new(), 2)
        .await
        .expect("recording fixture");

    assert_eq!(response.output, "finished");
    assert_eq!(tools.executions(), 1);
    let calls = fs::read_to_string(log).expect("recorded calls");
    assert!(calls.starts_with("bash "));
    assert!(calls.contains(r#""value":"allowed""#));
}

#[tokio::test]
async fn agent_runner_history_patch_cannot_observe_provider_retries() {
    const SENTINEL: &str = "stateful compacted projection";

    struct StatefulHistoryPatch {
        projection: Arc<std::sync::Mutex<Vec<Message>>>,
    }

    impl AgentHook for StatefulHistoryPatch {
        async fn on_completion_call(
            &self,
            _context: &HookContext,
            _event: CompletionCallEvent<'_>,
        ) -> CompletionCallAction {
            let projection = self.projection.lock().expect("projection lock").clone();
            CompletionCallAction::patch(RequestPatch::new().history(projection))
        }
    }

    let model = MockCompletionModel::from_turns([
        MockTurn::error("scripted HTTP 429"),
        MockTurn::error("scripted HTTP 500"),
        MockTurn::text("done"),
    ]);
    let probe = model.clone();
    let hook = StatefulHistoryPatch {
        projection: Arc::new(std::sync::Mutex::new(vec![Message::user(SENTINEL)])),
    };
    let result = AgentBuilder::new(model)
        .add_hook(hook)
        .build()
        .runner("event prompt")
        .max_turns(1)
        .run()
        .await;

    assert!(result.is_err());
    assert_eq!(probe.request_count(), 1);
    assert!(
        message_wire(
            &probe.requests()[0]
                .chat_history
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        )
        .contains(SENTINEL)
    );
}

fn driver<'a>(
    model: &'a MockCompletionModel,
    tools: &'a CountingTools,
    telemetry: PrototypeTelemetry,
    cancellation: CancellationToken,
    compaction: CompactionConfig,
    initial_delay: Duration,
) -> ExplicitDriver<'a, MockCompletionModel, CountingTools> {
    ExplicitDriver::new(
        model,
        tools,
        telemetry,
        CancellationTelemetry::default(),
        cancellation,
        SYSTEM_INSTRUCTIONS,
        ThinkingLevel::Max,
        ProviderRetryPolicy {
            max_retries: 2,
            initial_delay,
            max_delay: initial_delay.saturating_mul(2),
        },
        compaction,
    )
}

fn usage(input_tokens: u64, output_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        ..Usage::new()
    }
}

fn message_wire(messages: &[Message]) -> String {
    serde_json::to_string(messages).expect("serialize messages")
}
