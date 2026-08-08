use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::{expand_path, is_within};
use crate::protocol::{IntakeItem, IntakeItemKind, PollRequest, PollResponse, ProtocolError};

use super::source_error;

const PAGE_SIZE: usize = 100;
const DEFAULT_MAX_PAGES: usize = 100;
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoCheckpoint {
    created_at: String,
    numbers_at_timestamp: Vec<i64>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Deserialize, Serialize)]
struct GithubCheckpoint {
    repositories: HashMap<String, RepoCheckpoint>,
}

#[derive(Clone, Deserialize)]
struct GithubItem {
    number: i64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    user: Option<GithubUser>,
    #[serde(default)]
    labels: Vec<GithubLabel>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Clone, Deserialize)]
struct GithubUser {
    login: String,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum GithubLabel {
    Name(String),
    Object {
        #[serde(default)]
        name: Option<String>,
    },
}

pub async fn poll_github(
    request: PollRequest,
    client: &Client,
    token: &str,
) -> Result<PollResponse, ProtocolError> {
    if token.is_empty() {
        return Err(source_error("GITHUB_TOKEN is required"));
    }
    let roots = string_array_option(&request, "project_roots");
    if roots.is_empty() {
        return Err(source_error(
            "GitHub source options.project_roots must contain at least one path",
        ));
    }
    let mut repositories = discover_github_repositories(&roots)?;
    let previous = if request.checkpoint.is_null() {
        GithubCheckpoint {
            repositories: HashMap::new(),
        }
    } else {
        parse_checkpoint(&request.checkpoint)?
    };
    let mut next = previous.clone();
    let mut items = Vec::new();
    let api_base =
        string_option(&request, "api_base_url").unwrap_or_else(|| "https://api.github.com".into());
    let max_pages = max_pages_option(&request)?.unwrap_or(DEFAULT_MAX_PAGES);

    repositories.sort();
    for repository in repositories {
        let Some(checkpoint) = previous.repositories.get(&repository) else {
            let baseline =
                baseline_repository(&repository, &api_base, token, max_pages, client).await?;
            next.repositories.insert(repository, baseline);
            continue;
        };
        if items.len() >= request.item_limit {
            continue;
        }
        let unseen =
            list_new_items(&repository, checkpoint, &api_base, token, max_pages, client).await?;
        for item in unseen.into_iter().take(request.item_limit - items.len()) {
            let pull = if item.pull_request.is_some() {
                Some(
                    github_get_value(
                        &format!("{api_base}/repos/{repository}/pulls/{}", item.number),
                        token,
                        client,
                    )
                    .await?,
                )
            } else {
                None
            };
            items.push(normalize_github_item(&repository, &item, pull.as_ref()));
            advance_checkpoint(&mut next, &repository, &item);
        }
    }

    Ok(PollResponse {
        protocol_version: 1,
        checkpoint: serde_json::to_value(next)
            .map_err(|_| source_error("GitHub checkpoint serialization failed"))?,
        items,
    })
}

pub fn discover_github_repositories(roots: &[String]) -> Result<Vec<String>, ProtocolError> {
    let mut repositories = HashSet::new();
    for root_value in roots {
        let expanded =
            expand_path(root_value).map_err(|_| source_error("GitHub project root is invalid"))?;
        let root = fs::canonicalize(expanded)
            .map_err(|_| source_error("GitHub project root is unavailable"))?;
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_name() == ".git" {
                    let Ok(canonical) = fs::canonicalize(&path) else {
                        continue;
                    };
                    if !is_within(&canonical, std::slice::from_ref(&root)) {
                        continue;
                    }
                    let config = read_repository_config(&path);
                    for remote in git_remote_urls(&config) {
                        if let Some(identity) = github_identity(&remote) {
                            repositories.insert(identity);
                        }
                    }
                    continue;
                }
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(path);
                }
            }
        }
    }
    Ok(repositories.into_iter().collect())
}

fn read_repository_config(marker: &Path) -> String {
    let Ok(metadata) = fs::symlink_metadata(marker) else {
        return String::new();
    };
    if metadata.is_dir() {
        return fs::read_to_string(marker.join("config")).unwrap_or_default();
    }
    if !metadata.is_file() {
        return String::new();
    }
    let marker_text = fs::read_to_string(marker).unwrap_or_default();
    let Some(git_dir_value) = marker_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("gitdir")
            .then(|| value.trim())
    }) else {
        return String::new();
    };
    let git_directory = absolute_from(marker.parent().unwrap_or(Path::new(".")), git_dir_value);
    let direct = fs::read_to_string(git_directory.join("config")).unwrap_or_default();
    if !direct.is_empty() {
        return direct;
    }
    let common = fs::read_to_string(git_directory.join("commondir"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if common.is_empty() {
        return String::new();
    }
    fs::read_to_string(absolute_from(&git_directory, &common).join("config")).unwrap_or_default()
}

fn absolute_from(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn git_remote_urls(config: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut in_remote = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = &trimmed[1..trimmed.len() - 1].trim();
            in_remote = section
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("remote"))
                && section.get(6..).is_some_and(|rest| {
                    rest.chars().next().is_some_and(char::is_whitespace)
                        && rest.trim_start().starts_with('"')
                });
            continue;
        }
        if !in_remote {
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("url") && !value.trim().is_empty() {
            urls.push(value.trim().to_owned());
        }
    }
    urls
}

pub fn github_identity(remote: &str) -> Option<String> {
    let mut normalized = remote.trim();
    if let Some(value) = normalized.strip_suffix(".git/") {
        normalized = value;
    } else if let Some(value) = normalized.strip_suffix(".git") {
        normalized = value;
    }
    let lower = normalized.to_ascii_lowercase();
    let hosts = [
        "https://github.com",
        "http://github.com",
        "git://github.com",
        "ssh://git@github.com",
        "git@github.com",
    ];
    let host = hosts.iter().find(|host| lower.starts_with(**host))?;
    let separator = normalized.as_bytes().get(host.len()).copied()?;
    if !matches!(separator, b'/' | b':') {
        return None;
    }
    let identity = &normalized[host.len() + 1..];
    let mut parts = identity.split('/');
    let owner = parts.next()?;
    let repository = parts.next()?;
    if parts.next().is_some() || !valid_identity_part(owner) || !valid_identity_part(repository) {
        return None;
    }
    Some(format!("{owner}/{repository}").to_ascii_lowercase())
}

fn valid_identity_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

async fn baseline_repository(
    repository: &str,
    api_base: &str,
    token: &str,
    max_pages: usize,
    client: &Client,
) -> Result<RepoCheckpoint, ProtocolError> {
    let mut created_at: Option<String> = None;
    let mut numbers_at_timestamp = Vec::new();
    for page in 1..=max_pages {
        let response = github_get_items(repository, api_base, token, page, client).await?;
        let Some(first) = response.first() else {
            return Ok(RepoCheckpoint {
                created_at: created_at.unwrap_or_else(|| "1970-01-01T00:00:00.000Z".into()),
                numbers_at_timestamp,
                extra: Map::new(),
            });
        };
        let timestamp = created_at.get_or_insert_with(|| first.created_at.clone());
        for item in &response {
            if item.created_at != *timestamp {
                return Ok(RepoCheckpoint {
                    created_at: timestamp.clone(),
                    numbers_at_timestamp,
                    extra: Map::new(),
                });
            }
            numbers_at_timestamp.push(item.number);
        }
        if response.len() < PAGE_SIZE {
            return Ok(RepoCheckpoint {
                created_at: timestamp.clone(),
                numbers_at_timestamp,
                extra: Map::new(),
            });
        }
    }
    Err(source_error(format!(
        "GitHub baseline pagination bound reached for {repository} at one creation timestamp"
    )))
}

async fn list_new_items(
    repository: &str,
    checkpoint: &RepoCheckpoint,
    api_base: &str,
    token: &str,
    max_pages: usize,
    client: &Client,
) -> Result<Vec<GithubItem>, ProtocolError> {
    let mut unseen = Vec::new();
    let mut reached_boundary = false;
    for page in 1..=max_pages {
        let response = github_get_items(repository, api_base, token, page, client).await?;
        for item in &response {
            if item.created_at < checkpoint.created_at {
                reached_boundary = true;
                break;
            }
            if item.created_at == checkpoint.created_at
                && checkpoint.numbers_at_timestamp.contains(&item.number)
            {
                continue;
            }
            unseen.push(item.clone());
        }
        if reached_boundary || response.len() < PAGE_SIZE {
            reached_boundary = true;
            break;
        }
    }
    if !reached_boundary {
        return Err(source_error(format!(
            "GitHub pagination bound reached for {repository} without finding its checkpoint"
        )));
    }
    unseen.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.number.cmp(&right.number))
    });
    Ok(unseen)
}

async fn github_get_items(
    repository: &str,
    api_base: &str,
    token: &str,
    page: usize,
    client: &Client,
) -> Result<Vec<GithubItem>, ProtocolError> {
    let value = github_get_value(
        &format!(
            "{api_base}/repos/{repository}/issues?state=all&sort=created&direction=desc&per_page={PAGE_SIZE}&page={page}"
        ),
        token,
        client,
    )
    .await?;
    serde_json::from_value(value).map_err(|_| source_error("GitHub API response is invalid"))
}

async fn github_get_value(url: &str, token: &str, client: &Client) -> Result<Value, ProtocolError> {
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "intagent-source")
        .send()
        .await
        .map_err(|_| source_error("GitHub API request failed"))?;
    if !response.status().is_success() {
        return Err(source_error(format!(
            "GitHub API request failed with {}",
            response.status().as_u16()
        )));
    }
    response
        .json()
        .await
        .map_err(|_| source_error("GitHub API response is invalid"))
}

fn normalize_github_item(repository: &str, item: &GithubItem, pull: Option<&Value>) -> IntakeItem {
    let is_pull = item.pull_request.is_some();
    let labels: Vec<String> = item
        .labels
        .iter()
        .map(|label| match label {
            GithubLabel::Name(name) => name.clone(),
            GithubLabel::Object { name } => name.clone().unwrap_or_default(),
        })
        .collect();
    let pull_request = pull.map_or(Value::Null, |pull| {
        json!({
            "head": pull.get("head").cloned().unwrap_or(Value::Null),
            "base": pull.get("base").cloned().unwrap_or(Value::Null),
            "draft": pull.get("draft").filter(|value| !value.is_null()).cloned().unwrap_or(Value::Bool(false)),
        })
    });
    let metadata = json!({
        "repository": repository,
        "number": item.number,
        "itemType": if is_pull { "pull-request" } else { "issue" },
        "author": item.user.as_ref().map(|user| user.login.as_str()),
        "labels": labels,
        "updatedAt": item.updated_at,
        "pullRequest": pull_request,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    IntakeItem {
        entity_id: format!(
            "github:{repository}:{}:{}",
            if is_pull { "pull" } else { "issue" },
            item.number
        ),
        revision_id: format!("created:{}", item.created_at),
        kind: if is_pull {
            IntakeItemKind::GithubPullRequest
        } else {
            IntakeItemKind::GithubIssue
        },
        title: item.title.clone(),
        body: truncate_utf16(item.body.as_deref().unwrap_or(""), 256 * 1024),
        url: Some(item.html_url.clone()),
        occurred_at: item.created_at.clone(),
        metadata,
    }
}

fn advance_checkpoint(checkpoint: &mut GithubCheckpoint, repository: &str, item: &GithubItem) {
    match checkpoint.repositories.get_mut(repository) {
        None => {
            checkpoint.repositories.insert(
                repository.into(),
                RepoCheckpoint {
                    created_at: item.created_at.clone(),
                    numbers_at_timestamp: vec![item.number],
                    extra: Map::new(),
                },
            );
        }
        Some(current) if item.created_at > current.created_at => {
            *current = RepoCheckpoint {
                created_at: item.created_at.clone(),
                numbers_at_timestamp: vec![item.number],
                extra: Map::new(),
            };
        }
        Some(current)
            if item.created_at == current.created_at
                && !current.numbers_at_timestamp.contains(&item.number) =>
        {
            current.numbers_at_timestamp.push(item.number);
        }
        Some(_) => {}
    }
}

fn parse_checkpoint(value: &Value) -> Result<GithubCheckpoint, ProtocolError> {
    let object = value
        .as_object()
        .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
    let repositories = object
        .get("repositories")
        .and_then(Value::as_object)
        .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
    let mut parsed = HashMap::new();
    for (repository, entry) in repositories {
        let object = entry
            .as_object()
            .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
        let created_at = object
            .get("createdAt")
            .and_then(Value::as_str)
            .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
        let numbers = object
            .get("numbersAtTimestamp")
            .and_then(Value::as_array)
            .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
        let mut numbers_at_timestamp = Vec::new();
        for number in numbers {
            let value = number
                .as_i64()
                .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
                .ok_or_else(|| source_error("GitHub checkpoint is invalid"))?;
            numbers_at_timestamp.push(value);
        }
        let mut extra = object.clone();
        extra.remove("createdAt");
        extra.remove("numbersAtTimestamp");
        parsed.insert(
            repository.clone(),
            RepoCheckpoint {
                created_at: created_at.into(),
                numbers_at_timestamp,
                extra,
            },
        );
    }
    Ok(GithubCheckpoint {
        repositories: parsed,
    })
}

fn string_option(request: &PollRequest, name: &str) -> Option<String> {
    request
        .options
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn max_pages_option(request: &PollRequest) -> Result<Option<usize>, ProtocolError> {
    let Some(value) = request.options.get("max_pages") else {
        return Ok(None);
    };
    let valid = value
        .as_f64()
        .filter(|value| value.fract() == 0.0 && (1.0..=1000.0).contains(value));
    valid.map(|value| Some(value as usize)).ok_or_else(|| {
        source_error("GitHub source options.max_pages must be an integer from 1 to 1000")
    })
}

fn string_array_option(request: &PollRequest, name: &str) -> Vec<String> {
    let Some(values) = request.options.get(name).and_then(Value::as_array) else {
        return Vec::new();
    };
    if values.iter().any(|value| !value.is_string()) {
        return Vec::new();
    }
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn truncate_utf16(value: &str, limit: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|character| {
            let next = units + character.len_utf16();
            if next > limit {
                false
            } else {
                units = next;
                true
            }
        })
        .collect()
}
