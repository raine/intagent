use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use intake::protocol::{
    IntakeItem, IntakeItemKind, MAX_STANDARD_INPUT_BYTES, PROTOCOL_VERSION, PollRequest,
    PollResponse, ProtocolError, parse_poll_request, parse_poll_response, read_poll_request,
    run_source, write_poll_response,
};
use serde_json::{Map, Value, json};

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/protocol")
        .join(path)
}

fn request_fixture() -> Value {
    serde_json::from_slice(&fs::read(fixture("poll-request.json")).unwrap()).unwrap()
}

fn response_fixture() -> Value {
    serde_json::from_slice(&fs::read(fixture("poll-response.json")).unwrap()).unwrap()
}

#[test]
fn parses_phase_zero_protocol_fixtures() {
    let request_bytes = fs::read(fixture("poll-request.json")).unwrap();
    let response_bytes = fs::read(fixture("poll-response.json")).unwrap();
    let request = parse_poll_request(&request_bytes).unwrap();
    let response = parse_poll_response(&response_bytes).unwrap();
    assert_eq!(request.protocol_version, PROTOCOL_VERSION);
    assert_eq!(request.source, "github");
    assert_eq!(request.item_limit, 1000);
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].kind, IntakeItemKind::GithubIssue);
    assert_eq!(serde_json::to_value(request).unwrap(), request_fixture());
    assert_eq!(serde_json::to_value(response).unwrap(), response_fixture());
}

#[test]
fn rejects_unknown_fields_missing_fields_and_wrong_versions() {
    let mut request = request_fixture();
    request["unknown"] = json!(true);
    assert!(parse_poll_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut request = request_fixture();
    request.as_object_mut().unwrap().remove("checkpoint");
    assert!(parse_poll_request(&serde_json::to_vec(&request).unwrap()).is_err());

    let mut response = response_fixture();
    response["protocolVersion"] = json!(2);
    assert!(parse_poll_response(&serde_json::to_vec(&response).unwrap()).is_err());

    let mut response = response_fixture();
    response["items"][0]["unknown"] = json!(true);
    assert!(parse_poll_response(&serde_json::to_vec(&response).unwrap()).is_err());
}

#[test]
fn matches_shared_zod_differential_corpus() {
    let corpus: Value =
        serde_json::from_slice(&fs::read(fixture("differential.json")).unwrap()).unwrap();
    for test_case in corpus["request"].as_array().unwrap() {
        let mut request = request_fixture();
        request[test_case["field"].as_str().unwrap()] = test_case["value"].clone();
        let accepted = parse_poll_request(&serde_json::to_vec(&request).unwrap()).is_ok();
        assert_eq!(accepted, test_case["accepted"], "{test_case}");
    }
    for test_case in corpus["item"].as_array().unwrap() {
        let value = if let Some(repeated) = test_case.get("repeat") {
            Value::String(
                repeated
                    .as_str()
                    .unwrap()
                    .repeat(test_case["count"].as_u64().unwrap() as usize),
            )
        } else {
            test_case["value"].clone()
        };
        let mut response = response_fixture();
        response["items"][0][test_case["field"].as_str().unwrap()] = value;
        let accepted = parse_poll_response(&serde_json::to_vec(&response).unwrap()).is_ok();
        assert_eq!(accepted, test_case["accepted"], "{test_case}");
    }
    for test_case in corpus["rawRequestNumbers"].as_array().unwrap() {
        let raw = format!(
            "{{\"protocolVersion\":1,\"source\":\"source\",\"checkpoint\":{},\"now\":\"2026-08-07T10:00:00Z\",\"itemLimit\":1,\"options\":{{}}}}",
            test_case["value"].as_str().unwrap()
        );
        let accepted = parse_poll_request(raw.as_bytes()).is_ok();
        assert_eq!(accepted, test_case["accepted"], "{test_case}");
    }
}

#[test]
fn applies_utf16_bounds_instead_of_utf8_byte_bounds() {
    let item = IntakeItem {
        entity_id: "😀".repeat(512),
        revision_id: "revision".into(),
        kind: IntakeItemKind::Generic,
        title: "😀".repeat(2048),
        body: String::new(),
        url: None,
        occurred_at: "2026-08-07T10:00:00Z".into(),
        metadata: Map::new(),
    };
    assert!(item.validate().is_ok());
    let mut too_long = item.clone();
    too_long.entity_id.push('😀');
    assert!(too_long.validate().is_err());
}

#[tokio::test]
async fn reads_bounded_input_and_writes_exactly_one_line() {
    let input = fs::read(fixture("poll-request.json")).unwrap();
    let request = read_poll_request(&input[..]).await.unwrap();
    assert_eq!(request.source, "github");

    let response = parse_poll_response(&fs::read(fixture("poll-response.json")).unwrap()).unwrap();
    let mut output = Vec::new();
    write_poll_response(&mut output, &response).await.unwrap();
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    assert_eq!(parse_poll_response(&output).unwrap(), response);

    let oversized = vec![b' '; MAX_STANDARD_INPUT_BYTES as usize + 1];
    assert!(matches!(
        read_poll_request(&oversized[..]).await,
        Err(ProtocolError::InputTooLarge)
    ));
}

#[tokio::test]
async fn keeps_diagnostics_off_standard_output() {
    let input = fs::read(fixture("poll-request.json")).unwrap();
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    let success = run_source(&input[..], &mut output, &mut diagnostics, |_| async {
        Err(ProtocolError::SourceUnavailable("fixture"))
    })
    .await;
    assert!(!success);
    assert!(output.is_empty());
    assert_eq!(
        String::from_utf8(diagnostics).unwrap(),
        "source polling is unavailable: fixture\n"
    );
}

#[tokio::test]
async fn validates_handler_responses_before_writing() {
    let request = PollRequest {
        protocol_version: 1,
        source: "fixture".into(),
        checkpoint: Value::Null,
        now: "2026-08-07T10:00Z".into(),
        item_limit: 1,
        options: Map::new(),
    };
    let input = serde_json::to_vec(&request).unwrap();
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    let success = run_source(&input[..], &mut output, &mut diagnostics, |_| async {
        Ok(PollResponse {
            protocol_version: 2,
            checkpoint: Value::Null,
            items: Vec::new(),
        })
    })
    .await;
    assert!(!success);
    assert!(output.is_empty());
    assert!(!diagnostics.is_empty());
}

#[test]
fn source_binary_shells_parse_one_request_without_stdout_diagnostics() {
    for executable in [
        env!("CARGO_BIN_EXE_intake-fastmail-source"),
        env!("CARGO_BIN_EXE_intake-github-source"),
    ] {
        let mut child = Command::new(executable)
            .env_remove("FASTMAIL_API_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&fs::read(fixture("poll-request.json")).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let diagnostics = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostics.contains("source polling failed"));
        assert!(diagnostics.contains("_TOKEN is required"));
    }
}
