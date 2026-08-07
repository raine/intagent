mod support;

use std::process::Stdio;

use intake::protocol::{PollRequest, PollResponse};
use intake::sources::{fastmail::poll_fastmail, http_client};
use serde_json::{Map, Value, json};
use support::FixtureServer;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

fn request(base_url: &str, checkpoint: Value) -> PollRequest {
    PollRequest {
        protocol_version: 1,
        source: "fastmail".into(),
        checkpoint,
        now: "2026-08-03T10:10:00.000Z".into(),
        item_limit: 10,
        options: Map::from_iter([
            ("mailbox_id".into(), json!("inbox")),
            ("session_url".into(), json!(format!("{base_url}/session"))),
        ]),
    }
}

fn session(base_url: &str) -> Value {
    json!({
        "apiUrl": format!("{base_url}/jmap"),
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "account-1" },
    })
}

fn jmap(method: &str, value: Value, call_id: &str) -> Value {
    json!({ "methodResponses": [[method, value, call_id]] })
}

fn email(id: &str, received_at: &str, value: &str) -> Value {
    json!({
        "id": id,
        "threadId": "thread-1",
        "subject": "Question",
        "from": [{ "name": "Sender", "email": "sender@example.test" }],
        "to": [{ "email": "recipient@example.test" }],
        "receivedAt": received_at,
        "mailboxIds": { "inbox": true },
        "textBody": [{ "partId": "body", "type": "text/plain" }],
        "bodyValues": { "body": { "value": value } },
        "bodyStructure": {
            "subParts": [{
                "blobId": "blob-secret",
                "name": "report.pdf",
                "type": "application/pdf",
                "size": 42,
                "disposition": "attachment",
            }],
        },
    })
}

#[tokio::test]
async fn establishes_a_mailbox_query_baseline_without_historical_events() {
    let server = FixtureServer::start(|base| {
        vec![
            session(base),
            jmap(
                "Mailbox/get",
                json!({
                    "list": [
                        { "id": "inbox", "role": "inbox" },
                        { "id": "sent", "role": "sent" },
                    ],
                }),
                "mailboxes",
            ),
            jmap(
                "Email/query",
                json!({ "queryState": "query-state-1", "ids": [] }),
                "query",
            ),
        ]
    })
    .await;
    let result = poll_fastmail(
        request(&server.base_url, Value::Null),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    assert!(result.items.is_empty());
    assert_eq!(
        result.checkpoint,
        json!({
            "queryState": "query-state-1",
            "mailboxId": "inbox",
            "sentMailboxId": "sent",
        })
    );
    let calls = server.finish().await;
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].target, "/session");
    assert_eq!(calls[1].method, "POST");
    assert_eq!(calls[1].target, "/jmap");
    assert!(
        calls
            .iter()
            .all(|call| call.headers.contains("Bearer source-only-token"))
    );
    assert_eq!(calls[1].body["methodCalls"][0][0], "Mailbox/get");
}

#[tokio::test]
async fn emits_stable_events_with_bounded_threads_and_attachment_metadata() {
    let message_1 = {
        let mut value = email("message-1", "2026-08-03T10:00:00.000Z", "Initial request");
        value["sentAt"] = json!("2026-08-03T10:00:00.000Z");
        value["mailboxIds"] = json!({ "sent": true });
        value
    };
    let message_2 = email("message-2", "2026-08-03T10:05:00.000Z", "Follow up");
    let server = FixtureServer::start(|base| {
        vec![
            session(base),
            jmap(
                "Email/queryChanges",
                json!({
                    "added": [{ "id": "message-2", "index": 0 }],
                    "removed": [],
                    "newQueryState": "query-state-2",
                    "hasMoreChanges": false,
                }),
                "changes",
            ),
            jmap(
                "Email/get",
                json!({ "list": [message_2.clone()] }),
                "emails",
            ),
            jmap(
                "Thread/get",
                json!({ "list": [{ "id": "thread-1", "emailIds": ["message-1", "message-2"] }] }),
                "thread",
            ),
            jmap(
                "Email/get",
                json!({ "list": [message_1, message_2] }),
                "emails",
            ),
        ]
    })
    .await;
    let result = poll_fastmail(
        request(
            &server.base_url,
            json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
        ),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    assert_eq!(result.items.len(), 1);
    let item = &result.items[0];
    assert_eq!(item.entity_id, "fastmail:account-1:thread:thread-1");
    assert_eq!(item.revision_id, "message-2");
    assert!(item.body.contains("Initial request"));
    assert!(item.body.contains("Follow up"));
    assert_eq!(item.metadata["attachments"].as_array().unwrap().len(), 2);
    assert!(
        !serde_json::to_string(&item.metadata)
            .unwrap()
            .contains("blob-secret")
    );
    let calls = server.finish().await;
    assert_eq!(calls[1].body["methodCalls"][0][1]["maxChanges"], 10);
    assert_eq!(
        calls[1].body["methodCalls"][0][1]["filter"],
        json!({ "inMailbox": "inbox" })
    );
}

#[tokio::test]
async fn excludes_messages_by_header_before_thread_assembly() {
    let mut push = email(
        "push-message",
        "2026-08-03T10:05:00.000Z",
        "Pushed one commit",
    );
    push["header:X-GitHub-Reason:asText"] = json!("push");
    let server = FixtureServer::start(|base| {
        vec![
            session(base),
            jmap(
                "Email/queryChanges",
                json!({ "added": [{ "id": "push-message", "index": 0 }], "removed": [], "newQueryState": "query-state-2", "hasMoreChanges": false }),
                "changes",
            ),
            jmap("Email/get", json!({ "list": [push] }), "emails"),
        ]
    })
    .await;
    let mut request = request(
        &server.base_url,
        json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
    );
    request.options.insert(
        "exclude_headers".into(),
        json!({ "X-GitHub-Reason": ["push"] }),
    );
    let result = poll_fastmail(request, &http_client().unwrap(), "source-only-token")
        .await
        .unwrap();
    assert!(result.items.is_empty());
    let calls = server.finish().await;
    assert!(
        calls[2].body["methodCalls"][0][1]["properties"]
            .as_array()
            .unwrap()
            .contains(&json!("header:X-GitHub-Reason:asText"))
    );
}

#[tokio::test]
async fn allowlists_headers_and_github_resource_types() {
    let mut comment = email(
        "comment-message",
        "2026-08-03T10:00:00.000Z",
        "Useful comment",
    );
    comment["header:X-GitHub-Reason:asText"] = json!("comment");
    comment["messageId"] = json!(["raine/example/issues/1/123@github.com"]);
    let mut subscribed = email(
        "subscribed-message",
        "2026-08-03T10:01:00.000Z",
        "Useful pull request",
    );
    subscribed["header:X-GitHub-Reason:asText"] = json!("subscribed");
    subscribed["messageId"] = json!(["raine/example/pull/2@github.com"]);
    let mut mention = email(
        "mention-message",
        "2026-08-03T10:02:00.000Z",
        "Noisy mention",
    );
    mention["header:X-GitHub-Reason:asText"] = json!("mention");
    mention["messageId"] = json!(["raine/example/issues/3@github.com"]);
    let mut release = email(
        "release-message",
        "2026-08-03T10:03:00.000Z",
        "Noisy release",
    );
    release["header:X-GitHub-Reason:asText"] = json!("subscribed");
    release["messageId"] = json!(["raine/example/releases/123@github.com"]);
    let messages = vec![comment, subscribed, mention, release];
    let ids: Vec<Value> = messages
        .iter()
        .map(|message| message["id"].clone())
        .collect();
    let server_messages = messages.clone();
    let server = FixtureServer::start(move |base| {
        vec![
            session(base),
            jmap(
                "Email/queryChanges",
                json!({ "added": ids.iter().enumerate().map(|(index, id)| json!({ "id": id, "index": index })).collect::<Vec<_>>(), "removed": [], "newQueryState": "query-state-2", "hasMoreChanges": false }),
                "changes",
            ),
            jmap("Email/get", json!({ "list": server_messages.clone() }), "emails"),
            jmap(
                "Thread/get",
                json!({ "list": [{ "id": "thread-1", "emailIds": ids }] }),
                "thread",
            ),
            jmap("Email/get", json!({ "list": server_messages }), "emails"),
        ]
    })
    .await;
    let mut request = request(
        &server.base_url,
        json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
    );
    request.options.insert(
        "include_headers".into(),
        json!({ "X-GitHub-Reason": ["comment", "subscribed"] }),
    );
    request.options.insert(
        "include_message_id_contains".into(),
        json!(["/issues/", "/pull/"]),
    );
    let result = poll_fastmail(request, &http_client().unwrap(), "source-only-token")
        .await
        .unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .map(|item| item.revision_id.as_str())
            .collect::<Vec<_>>(),
        ["comment-message", "subscribed-message"]
    );
    assert!(!result.items[0].body.contains("Noisy mention"));
    assert!(!result.items[0].body.contains("Noisy release"));
    server.finish().await;
}

#[tokio::test]
async fn omits_excluded_messages_from_later_thread_context() {
    let mut push = email(
        "push-message",
        "2026-08-03T10:00:00.000Z",
        "Pushed one commit",
    );
    push["header:X-GitHub-Reason:asText"] = json!("push");
    let mut comment = email(
        "comment-message",
        "2026-08-03T10:05:00.000Z",
        "Useful review comment",
    );
    comment["header:X-GitHub-Reason:asText"] = json!("subscribed");
    let server = FixtureServer::start(|base| {
        vec![
            session(base),
            jmap("Email/queryChanges", json!({ "added": [{ "id": "comment-message", "index": 0 }], "removed": [], "newQueryState": "query-state-2", "hasMoreChanges": false }), "changes"),
            jmap("Email/get", json!({ "list": [comment.clone()] }), "emails"),
            jmap("Thread/get", json!({ "list": [{ "id": "thread-1", "emailIds": ["push-message", "comment-message"] }] }), "thread"),
            jmap("Email/get", json!({ "list": [push, comment] }), "emails"),
        ]
    }).await;
    let mut request = request(
        &server.base_url,
        json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
    );
    request.options.insert(
        "exclude_headers".into(),
        json!({ "X-GitHub-Reason": ["push"] }),
    );
    let result = poll_fastmail(request, &http_client().unwrap(), "source-only-token")
        .await
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert!(result.items[0].body.contains("Useful review comment"));
    assert!(!result.items[0].body.contains("Pushed one commit"));
    server.finish().await;
}

#[tokio::test]
async fn suppresses_a_thread_when_its_newest_message_is_sent_mail() {
    let incoming = email("message-1", "2026-08-03T10:00:00.000Z", "Initial request");
    let mut reply = email("message-2", "2026-08-03T10:05:00.000Z", "Sent reply");
    reply["sentAt"] = json!("2026-08-03T10:06:00.000Z");
    reply["mailboxIds"] = json!({ "sent": true });
    let server = FixtureServer::start(|base| vec![
        session(base),
        jmap("Mailbox/get", json!({ "list": [{ "id": "inbox", "role": "inbox" }, { "id": "sent", "role": "sent" }] }), "mailboxes"),
        jmap("Email/queryChanges", json!({ "added": [{ "id": "message-1", "index": 0 }], "removed": [], "newQueryState": "query-state-2", "hasMoreChanges": false }), "changes"),
        jmap("Email/get", json!({ "list": [incoming.clone()] }), "emails"),
        jmap("Thread/get", json!({ "list": [{ "id": "thread-1", "emailIds": ["message-1", "message-2"] }] }), "thread"),
        jmap("Email/get", json!({ "list": [incoming, reply] }), "emails"),
    ]).await;
    let result = poll_fastmail(
        request(
            &server.base_url,
            json!({ "queryState": "query-state-1", "mailboxId": "inbox" }),
        ),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    assert!(result.items.is_empty());
    assert_eq!(result.checkpoint["sentMailboxId"], "sent");
    server.finish().await;
}

#[tokio::test]
async fn advances_through_removals_without_emitting_items() {
    let server = FixtureServer::start(|base| vec![
        session(base),
        jmap("Email/queryChanges", json!({ "added": [], "removed": ["message-1"], "newQueryState": "query-state-2", "hasMoreChanges": false }), "changes"),
    ]).await;
    let result = poll_fastmail(
        request(
            &server.base_url,
            json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
        ),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    assert!(result.items.is_empty());
    assert_eq!(result.checkpoint["queryState"], "query-state-2");
    server.finish().await;
}

#[tokio::test]
async fn enforces_body_thread_message_and_attachment_limits() {
    let mut messages = Vec::new();
    for index in 0..=100 {
        let marker = if index == 0 {
            "UNWANTED_FIRST".to_owned()
        } else {
            format!("body-{index}")
        };
        messages.push(email(
            &format!("message-{index}"),
            "2026-08-03T10:00:00.000Z",
            &format!("{marker}{}", "x".repeat(3_000)),
        ));
    }
    let selected = messages[100].clone();
    let thread_messages = messages[1..].to_vec();
    let ids: Vec<Value> = messages
        .iter()
        .map(|message| message["id"].clone())
        .collect();
    let server = FixtureServer::start(move |base| {
        vec![
            session(base),
            jmap(
                "Email/queryChanges",
                json!({ "added": [{ "id": "message-100", "index": 0 }], "removed": [], "newQueryState": "query-state-2", "hasMoreChanges": false }),
                "changes",
            ),
            jmap("Email/get", json!({ "list": [selected] }), "emails"),
            jmap(
                "Thread/get",
                json!({ "list": [{ "id": "thread-1", "emailIds": ids }] }),
                "thread",
            ),
            jmap(
                "Email/get",
                json!({ "list": thread_messages }),
                "emails",
            ),
        ]
    })
    .await;
    let result = poll_fastmail(
        request(
            &server.base_url,
            json!({ "queryState": "query-state-1", "mailboxId": "inbox", "sentMailboxId": "sent" }),
        ),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    let item = &result.items[0];
    assert_eq!(item.metadata["threadMessageCount"], 100);
    assert_eq!(item.metadata["attachments"].as_array().unwrap().len(), 100);
    assert_eq!(item.body.encode_utf16().count(), 64 * 1024 * 4);
    assert!(!item.body.contains("UNWANTED_FIRST"));
    server.finish().await;
}

#[tokio::test]
async fn source_binary_writes_one_bounded_response_without_stdout_diagnostics() {
    let server = FixtureServer::start(|base| vec![
        session(base),
        jmap("Mailbox/get", json!({ "list": [{ "id": "inbox", "role": "inbox" }, { "id": "sent", "role": "sent" }] }), "mailboxes"),
        jmap("Email/query", json!({ "queryState": "query-state-1", "ids": [] }), "query"),
    ]).await;
    let mut request = request(&server.base_url, Value::Null);
    request.item_limit = 1;
    let mut child = Command::new(env!("CARGO_BIN_EXE_intake-fastmail-source"))
        .env_clear()
        .env("FASTMAIL_API_TOKEN", "binary-token")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("binary-token"));
    let response: PollResponse = serde_json::from_slice(&output.stdout).unwrap();
    assert!(response.items.len() <= request.item_limit);
    let calls = server.finish().await;
    assert!(calls.iter().all(|call| {
        !serde_json::to_string(&call.body)
            .unwrap()
            .contains("binary-token")
    }));
}
