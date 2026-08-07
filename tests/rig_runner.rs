use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::time::Duration;

use intake::agent::auth::{AuthPaths, authorize, chatgpt_client, write_cache_atomically};
use intake::agent::model::{ThinkingLevel, completion_request};
use intake::agent::telemetry::CancellationTelemetry;
use intake::agent::tools::{CountingTools, supervise_process};
use rig_agent::agent::hook::InvalidToolCallAction;
use rig_agent::agent::run::{AgentRun, AgentRunStep, ModelTurn, ModelTurnOutcome};
use rig_agent::prelude::PromptError;
use rig_core::OneOrMany;
use rig_core::client::CompletionClient;
use rig_core::completion::{CompletionModel, CompletionRequest};
use rig_core::message::{ToolResultContent, UserContent};
use rig_core::providers::chatgpt::{self, ChatGPTAuth};
use rig_core::test_utils::{MockCompletionModel, MockTurn, RecordingHttpClient};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const SYSTEM_INSTRUCTIONS: &str = "Intake instructions are authoritative.";
const MODEL_ID: &str = "gpt-5.6-luna";

#[test]
fn auth_paths_create_private_valid_json_cache() {
    let temporary = TempDir::new().expect("temporary directory");
    let paths = AuthPaths::under_config_home(temporary.path());

    paths.prepare().expect("prepare authentication paths");

    assert_eq!(
        fs::read_to_string(&paths.cache).expect("read cache"),
        "{}\n"
    );
    assert_eq!(
        fs::metadata(&paths.directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let metadata = fs::metadata(&paths.cache).expect("cache metadata");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
}

#[test]
fn auth_paths_reject_corrupt_truncated_and_symlink_caches() {
    for contents in [b"{".as_slice(), b"null".as_slice()] {
        let temporary = TempDir::new().expect("temporary directory");
        let paths = AuthPaths::under_config_home(temporary.path());
        fs::create_dir_all(&paths.directory).expect("create auth directory");
        fs::write(&paths.cache, contents).expect("write invalid cache");
        assert!(paths.prepare().is_err());
    }

    let temporary = TempDir::new().expect("temporary directory");
    let paths = AuthPaths::under_config_home(temporary.path());
    fs::create_dir_all(&paths.directory).expect("create auth directory");
    let target = temporary.path().join("target.json");
    fs::write(&target, "{}\n").expect("write symlink target");
    symlink(&target, &paths.cache).expect("create cache symlink");
    assert!(paths.prepare().is_err());
}

#[test]
fn atomic_cache_replace_ignores_interrupted_temporary_file() {
    let temporary = TempDir::new().expect("temporary directory");
    let paths = AuthPaths::under_config_home(temporary.path());
    paths.prepare().expect("prepare authentication paths");
    let interrupted = paths.directory.join(".rig-auth.json.interrupted.tmp");
    fs::write(&interrupted, "{\"access_token\":").expect("write interrupted cache");

    write_cache_atomically(&paths.cache, b"{\"access_token\":\"secret\"}\n")
        .expect("replace cache atomically");

    let cache: Value = serde_json::from_slice(&fs::read(&paths.cache).expect("read cache"))
        .expect("cache remains valid JSON");
    assert_eq!(cache["access_token"], "secret");
    assert_eq!(
        fs::metadata(&paths.cache)
            .expect("cache metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(interrupted.exists());
}

#[tokio::test]
async fn noninteractive_auth_fails_quickly_with_operator_action() {
    let temporary = TempDir::new().expect("temporary directory");
    let paths = AuthPaths::under_config_home(temporary.path());
    let result = tokio::time::timeout(Duration::from_millis(250), authorize(&paths, false))
        .await
        .expect("noninteractive auth must not wait for device flow")
        .expect_err("missing credentials should fail");

    assert!(
        result.to_string().contains("Run `intake login`"),
        "{result}"
    );
}

#[tokio::test]
async fn rig_oauth_writer_rejects_symlinks_and_atomically_repairs_mode() {
    let temporary = TempDir::new().expect("temporary directory");
    let target = temporary.path().join("target.json");
    let symlink_cache = temporary.path().join("symlink-auth.json");
    let fixture = oauth_cache_fixture();
    fs::write(&target, &fixture).expect("write OAuth target");
    symlink(&target, &symlink_cache).expect("create OAuth cache symlink");
    let target_before = fs::read(&target).expect("read OAuth target");

    let symlink_client = chatgpt_client(&symlink_cache, false).expect("build symlink client");
    let error = symlink_client
        .authorize()
        .await
        .expect_err("Rig must reject a symlink before credential write");
    assert!(error.to_string().contains("regular file"), "{error}");
    assert_eq!(fs::read(&target).expect("read OAuth target"), target_before);

    let regular_cache = temporary.path().join("regular-auth.json");
    fs::write(&regular_cache, fixture).expect("write OAuth cache");
    fs::set_permissions(&regular_cache, fs::Permissions::from_mode(0o644))
        .expect("set permissive fixture mode");
    let regular_client = chatgpt_client(&regular_cache, false).expect("build regular client");
    regular_client
        .authorize()
        .await
        .expect("cached token should authorize without network");
    assert_eq!(
        fs::metadata(&regular_cache)
            .expect("OAuth cache metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let cache: Value = serde_json::from_slice(&fs::read(&regular_cache).expect("read OAuth cache"))
        .expect("parse OAuth cache");
    assert_eq!(cache["account_id"], "account");
}

#[tokio::test]
async fn request_capture_preserves_chatgpt_contract_for_every_reasoning_level() {
    for level in ThinkingLevel::ALL {
        let http = RecordingHttpClient::with_error_response(
            http::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"fixture stop"}}"#,
        );
        let client = chatgpt::Client::builder()
            .api_key(ChatGPTAuth::AccessToken {
                access_token: "fixture-token".to_string(),
                account_id: Some("fixture-account".to_string()),
            })
            .http_client(http.clone())
            .originator("intake")
            .user_agent("intake/0.1.0")
            .default_instructions("")
            .build()
            .expect("build recording client");
        let model = client.completion_model(MODEL_ID);
        let request = completion_request(SYSTEM_INSTRUCTIONS, "triage one event", level);

        model
            .completion(request)
            .await
            .expect_err("fixture response stops after request capture");

        let requests = http.requests();
        assert_eq!(requests.len(), 1);
        let captured = &requests[0];
        assert_eq!(captured.headers["originator"], "intake");
        assert_eq!(captured.headers["user-agent"], "intake/0.1.0");
        let body: Value = serde_json::from_slice(&captured.body).expect("request JSON");
        assert_eq!(body["model"], MODEL_ID);
        assert_eq!(body["instructions"], SYSTEM_INSTRUCTIONS);
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], level.wire_name());
        assert_eq!(body["reasoning"]["summary"], "detailed");
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert!(body["reasoning"].get("reasoning").is_none());
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        let names = body["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>();
        assert_eq!(names, ["bash", "read", "write"]);
    }
}

#[test]
fn configured_oauth_client_builds_without_a_model_call() {
    let temporary = TempDir::new().expect("temporary directory");
    let cache = temporary.path().join("rig-auth.json");
    let client = chatgpt_client(&cache, false).expect("build OAuth client");
    let _model = client.completion_model(MODEL_ID);
}

#[tokio::test]
async fn rig_and_tools_deny_without_execution_and_allow_once() {
    let tools = CountingTools::default();
    let denied = tools.call("bash", "denied");
    assert!(denied.denied);
    assert!(denied.output.len() <= 512);
    assert_eq!(tools.executions("bash"), 0);

    let allowed = tools.call("bash", "allowed");
    assert!(!allowed.denied);
    assert_eq!(tools.executions("bash"), 1);

    let model = MockCompletionModel::from_turns([MockTurn::tool_call(
        "call-1",
        "network",
        json!({"value": "allowed"}),
    )]);
    let response = model
        .completion(mock_request("call an unavailable tool"))
        .await
        .expect("mock model response");
    let mut run = AgentRun::new("call an unavailable tool").max_turns(2);
    assert!(matches!(
        run.next_step().expect("first model step"),
        AgentRunStep::CallModel { .. }
    ));
    let outcome = run
        .model_response(model_turn(response, &tool_names()))
        .expect("accept model response");
    assert!(matches!(outcome, ModelTurnOutcome::NeedsResolution(_)));
    let feedback = "denied: tool is outside the intake capability set";
    run.resolve_invalid_tool_call(InvalidToolCallAction::skip(feedback))
        .expect("resolve denied tool call");
    let calls = match run.next_step().expect("tool result step") {
        AgentRunStep::CallTools { calls } => calls,
        other => panic!("expected tool calls, got {other:?}"),
    };
    assert_eq!(calls.len(), 1);
    assert!(calls[0].preresolved_result.is_some());
    assert_eq!(tools.executions("network"), 0);
}

#[tokio::test]
async fn max_turns_counts_total_mock_model_calls() {
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("call-1", "bash", json!({"value": "allowed"})),
        MockTurn::tool_call("call-2", "bash", json!({"value": "allowed"})),
        MockTurn::tool_call("call-3", "bash", json!({"value": "allowed"})),
    ]);
    let names = tool_names();
    let mut run = AgentRun::new("loop").max_turns(3);

    for expected_turn in 1..=3 {
        let (prompt, history, turn) = match run.next_step().expect("model step") {
            AgentRunStep::CallModel {
                prompt,
                history,
                turn,
            } => (prompt, history, turn),
            other => panic!("expected model call, got {other:?}"),
        };
        assert_eq!(turn, expected_turn);
        let response = model
            .completion(CompletionRequest {
                model: None,
                preamble: None,
                chat_history: OneOrMany::one(prompt),
                documents: Vec::new(),
                tools: Vec::new(),
                temperature: None,
                max_tokens: None,
                tool_choice: None,
                additional_params: Some(json!({"history_length": history.len()})),
                output_schema: None,
                record_telemetry_content: false,
            })
            .await
            .expect("scripted model turn");
        assert!(matches!(
            run.model_response(model_turn(response, &names))
                .expect("accept model turn"),
            ModelTurnOutcome::Continue { .. }
        ));
        let calls = match run.next_step().expect("tool step") {
            AgentRunStep::CallTools { calls } => calls,
            other => panic!("expected tool calls, got {other:?}"),
        };
        let results = calls
            .into_iter()
            .map(|call| {
                UserContent::tool_result(
                    call.tool_call.id,
                    OneOrMany::one(ToolResultContent::text("ok")),
                )
            })
            .collect();
        run.tool_results(results).expect("feed tool results");
    }

    let error = run
        .next_step()
        .expect_err("fourth model call exceeds budget");
    assert!(matches!(
        error,
        PromptError::MaxTurnsError { max_turns: 3, .. }
    ));
    assert_eq!(model.request_count(), 3);
}

#[tokio::test]
async fn model_timeout_disconnects_http_and_keeps_serializable_state() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let address = listener.local_addr().expect("fixture address");
    let (request_tx, request_rx) = oneshot::channel();
    let (disconnect_tx, disconnect_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept request");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if complete_http_request(&request) {
                break;
            }
        }
        let _ = request_tx.send(());
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => {
                    let _ = disconnect_tx.send(());
                    return;
                }
                Ok(_) => {}
            }
        }
    });

    let client = chatgpt::Client::builder()
        .api_key(ChatGPTAuth::AccessToken {
            access_token: "fixture-token".to_string(),
            account_id: None,
        })
        .base_url(format!("http://{address}"))
        .default_instructions("")
        .originator("intake")
        .user_agent("intake/0.1.0")
        .build()
        .expect("build local client");
    let model = client.completion_model(MODEL_ID);
    let mut run = AgentRun::new("wait for fixture").max_turns(3);
    assert!(matches!(
        run.next_step().expect("model step"),
        AgentRunStep::CallModel { .. }
    ));
    let telemetry = CancellationTelemetry::default();
    telemetry.checkpoint(&run).expect("serialize pending run");

    let request = completion_request(SYSTEM_INSTRUCTIONS, "wait for fixture", ThinkingLevel::Max);
    let completion_task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_millis(100), model.completion(request)).await
    });
    tokio::time::timeout(Duration::from_secs(2), request_rx)
        .await
        .expect("server should observe request")
        .expect("request notification");
    let result = tokio::time::timeout(Duration::from_secs(2), completion_task)
        .await
        .expect("completion task should reach its timeout")
        .expect("completion task should not panic");
    assert!(result.is_err(), "model future should reach wall timeout");

    tokio::time::timeout(Duration::from_secs(2), disconnect_rx)
        .await
        .expect("server should observe disconnect")
        .expect("disconnect notification");
    let serialized = telemetry
        .serialized_state()
        .expect("cancellation state checkpoint");
    let restored: AgentRun = serde_json::from_str(&serialized).expect("deserialize canceled state");
    assert_eq!(restored.turn(), 1);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("fixture server should stop")
        .expect("fixture server task");
}

#[tokio::test]
async fn command_cancellation_kills_child_and_grandchild() {
    let temporary = TempDir::new().expect("temporary directory");
    let child_pid = temporary.path().join("child.pid");
    let grandchild_pid = temporary.path().join("grandchild.pid");
    let script = temporary.path().join("process-tree.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\n( trap '' TERM; while :; do sleep 1; done ) &\nprintf '%s' \"$!\" > '{}'\ntrap '' TERM\nwhile :; do sleep 1; done\n",
            child_pid.display(),
            grandchild_pid.display()
        ),
    )
    .expect("write fixture script");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make fixture executable");

    let cancellation = CancellationToken::new();
    let supervised_script = script.clone();
    let supervised_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        supervise_process(
            &supervised_script,
            std::iter::empty::<&str>(),
            supervised_cancellation,
            Duration::from_millis(50),
        )
        .await
    });
    wait_for_file(&grandchild_pid).await;
    let child = read_pid(&child_pid);
    let grandchild = read_pid(&grandchild_pid);
    assert!(process_exists(child));
    assert!(process_exists(grandchild));

    cancellation.cancel();
    assert!(task.await.expect("supervisor task").is_err());
    wait_for_process_exit(child).await;
    wait_for_process_exit(grandchild).await;
    assert!(!process_exists(child));
    assert!(!process_exists(grandchild));
}

fn oauth_cache_fixture() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "access_token": "x.eyJleHAiOjQxMDI0NDQ4MDAsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50In19.x",
        "expires_at": 4_102_444_800_i64
    }))
    .expect("serialize OAuth cache fixture")
}

fn mock_request(prompt: &str) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::one(rig_core::message::Message::user(prompt)),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
        record_telemetry_content: false,
    }
}

fn tool_names() -> BTreeSet<String> {
    ["bash", "read", "write"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn model_turn(
    response: rig_core::completion::CompletionResponse,
    names: &BTreeSet<String>,
) -> ModelTurn {
    ModelTurn::new(
        response.message_id,
        response.choice,
        response.usage,
        names.clone(),
        names.clone(),
    )
}

fn complete_http_request(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    bytes.len() >= header_end + 4 + content_length
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture process should create PID file");
}

fn read_pid(path: &Path) -> i32 {
    fs::read_to_string(path)
        .expect("read PID file")
        .parse()
        .expect("parse PID")
}

fn process_exists(pid: i32) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

async fn wait_for_process_exit(pid: i32) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture process should terminate");
}
