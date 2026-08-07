mod support;

use std::fs;
use std::process::Stdio;

use intake::protocol::{PollRequest, PollResponse};
use intake::sources::github::{discover_github_repositories, github_identity, poll_github};
use intake::sources::http_client;
use serde_json::{Map, Value, json};
use support::FixtureServer;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

fn repository_root() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    let git = root.path().join("project/.git");
    fs::create_dir_all(&git).unwrap();
    fs::write(
        git.join("config"),
        "[remote \"origin\"]\n  url = git@github.com:Example/Project.git\n[remote \"backup\"]\n  url = https://gitlab.test/example/project.git\n",
    )
    .unwrap();
    root
}

fn request(root: &TempDir, base_url: &str, checkpoint: Value) -> PollRequest {
    PollRequest {
        protocol_version: 1,
        source: "github".into(),
        checkpoint,
        now: "2026-08-03T11:00:00.000Z".into(),
        item_limit: 10,
        options: Map::from_iter([
            (
                "project_roots".into(),
                json!([root.path().to_string_lossy()]),
            ),
            ("api_base_url".into(), json!(base_url)),
            ("max_pages".into(), json!(3)),
        ]),
    }
}

fn issue(number: u64, created_at: &str) -> Value {
    json!({
        "number": number,
        "title": format!("Item {number}"),
        "body": "Details",
        "html_url": format!("https://github.com/example/project/issues/{number}"),
        "created_at": created_at,
        "updated_at": created_at,
        "user": { "login": "reporter" },
        "labels": [{ "name": "bug" }],
    })
}

#[test]
fn discovers_canonical_remotes_from_repositories_and_worktree_markers() {
    let root = repository_root();
    fs::create_dir_all(root.path().join("linked-worktree")).unwrap();
    fs::create_dir_all(root.path().join("shared.git")).unwrap();
    fs::write(
        root.path().join("shared.git/config"),
        "[REMOTE \"origin\"]\n  URL = git://github.com/Example/Linked.git\n",
    )
    .unwrap();
    fs::write(
        root.path().join("linked-worktree/.git"),
        "gitdir: ../shared.git\n",
    )
    .unwrap();
    let mut repositories =
        discover_github_repositories(&[root.path().to_string_lossy().into_owned()]).unwrap();
    repositories.sort();
    assert_eq!(repositories, ["example/linked", "example/project"]);
    assert_eq!(
        github_identity("ssh://git@github.com/Owner/Repo.git").as_deref(),
        Some("owner/repo")
    );
    assert_eq!(github_identity("https://example.test/Owner/Repo.git"), None);
}

#[tokio::test]
async fn establishes_a_per_repository_baseline() {
    let root = repository_root();
    let server = FixtureServer::start(|_| {
        vec![json!([
            issue(8, "2026-08-03T10:00:00.000Z"),
            issue(7, "2026-08-03T10:00:00.000Z"),
            issue(6, "2026-08-03T09:59:59.000Z"),
        ])]
    })
    .await;
    let result = poll_github(
        request(&root, &server.base_url, Value::Null),
        &http_client().unwrap(),
        "source-only-token",
    )
    .await
    .unwrap();
    assert!(result.items.is_empty());
    assert_eq!(
        result.checkpoint,
        json!({
            "repositories": {
                "example/project": {
                    "createdAt": "2026-08-03T10:00:00.000Z",
                    "numbersAtTimestamp": [8, 7],
                },
            },
        })
    );
    let calls = server.finish().await;
    assert!(calls[0].target.contains("per_page=100&page=1"));
    assert_eq!(calls[0].method, "GET");
    assert_eq!(calls[0].body, Value::Null);
    assert!(calls[0].headers.contains("Bearer source-only-token"));
    assert!(
        calls[0]
            .headers
            .to_ascii_lowercase()
            .contains("x-github-api-version: 2022-11-28")
    );
}

#[tokio::test]
async fn paginates_to_checkpoint_and_loads_pull_request_head_metadata() {
    let root = repository_root();
    let mut first_page = Vec::new();
    first_page.push({
        let mut pull = issue(109, "2026-08-03T10:05:00.000Z");
        pull["pull_request"] = json!({ "url": "https://github.test/pulls/109" });
        pull
    });
    for number in (10..109).rev() {
        first_page.push(issue(number, "2026-08-03T10:02:00.000Z"));
    }
    assert_eq!(first_page.len(), 100);
    let server = FixtureServer::start(move |_| {
        vec![
            json!(first_page),
            json!([
                issue(9, "2026-08-03T10:01:00.000Z"),
                issue(7, "2026-08-03T10:00:00.000Z"),
            ]),
            json!({
                "head": { "ref": "feature", "sha": "abc", "repo": { "full_name": "contributor/project" } },
                "base": { "ref": "main", "sha": "def" },
                "draft": false,
            }),
        ]
    })
    .await;
    let mut request = request(
        &root,
        &server.base_url,
        json!({
            "repositories": {
                "example/project": {
                    "createdAt": "2026-08-03T10:00:00.000Z",
                    "numbersAtTimestamp": [7],
                },
            },
        }),
    );
    request.item_limit = 1000;
    let result = poll_github(request, &http_client().unwrap(), "source-only-token")
        .await
        .unwrap();
    assert_eq!(
        result.items.first().unwrap().revision_id,
        "created:2026-08-03T10:01:00.000Z"
    );
    assert_eq!(
        result.items.last().unwrap().entity_id,
        "github:example/project:pull:109"
    );
    assert_eq!(
        result.items.last().unwrap().metadata["pullRequest"]["head"]["ref"],
        "feature"
    );
    assert_eq!(
        result.checkpoint["repositories"]["example/project"]["createdAt"],
        "2026-08-03T10:05:00.000Z"
    );
    assert_eq!(
        result.checkpoint["repositories"]["example/project"]["numbersAtTimestamp"],
        json!([109])
    );
    let calls = server.finish().await;
    assert!(calls[0].target.ends_with("page=1"));
    assert!(calls[1].target.ends_with("page=2"));
    assert!(calls[2].target.ends_with("/pulls/109"));
}

#[tokio::test]
async fn preserves_timestamp_and_id_checkpoint_order_at_item_bound() {
    let root = repository_root();
    let server = FixtureServer::start(|_| {
        vec![json!([
            issue(9, "2026-08-03T10:05:00.000Z"),
            issue(8, "2026-08-03T10:02:00.000Z"),
            issue(7, "2026-08-03T10:00:00.000Z"),
        ])]
    })
    .await;
    let mut request = request(
        &root,
        &server.base_url,
        json!({ "repositories": { "example/project": { "createdAt": "2026-08-03T10:00:00.000Z", "numbersAtTimestamp": [7] } } }),
    );
    request.item_limit = 1;
    let result = poll_github(request, &http_client().unwrap(), "source-only-token")
        .await
        .unwrap();
    assert_eq!(
        result.items[0].revision_id,
        "created:2026-08-03T10:02:00.000Z"
    );
    assert_eq!(
        result.checkpoint["repositories"]["example/project"]["createdAt"],
        "2026-08-03T10:02:00.000Z"
    );
    server.finish().await;
}

#[tokio::test]
async fn source_binary_writes_one_bounded_response_without_stdout_diagnostics() {
    let root = repository_root();
    let server = FixtureServer::start(|_| {
        vec![json!([
            issue(8, "2026-08-03T10:00:00.000Z"),
            issue(7, "2026-08-03T09:59:59.000Z"),
        ])]
    })
    .await;
    let mut request = request(&root, &server.base_url, Value::Null);
    request.item_limit = 1;
    let mut child = Command::new(env!("CARGO_BIN_EXE_intake-github-source"))
        .env_clear()
        .env("GITHUB_TOKEN", "binary-token")
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
    server.finish().await;
}
