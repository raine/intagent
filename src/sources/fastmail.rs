use std::collections::{HashMap, HashSet};

use reqwest::Client;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::protocol::{IntakeItem, IntakeItemKind, PollRequest, PollResponse, ProtocolError};

use super::source_error;

const EMAIL_ACCOUNT_CAPABILITY: &str = "urn:ietf:params:jmap:mail";
const CORE_CAPABILITY: &str = "urn:ietf:params:jmap:core";
const BODY_LIMIT: usize = 64 * 1024;
const THREAD_MESSAGE_LIMIT: usize = 100;
const ATTACHMENT_LIMIT: usize = 100;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JmapSession {
    api_url: String,
    primary_accounts: HashMap<String, String>,
}

#[derive(Clone)]
struct Checkpoint {
    query_state: String,
    mailbox_id: String,
    sent_mailbox_id: Option<String>,
    has_sent_mailbox_id: bool,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Email {
    id: String,
    thread_id: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    from: Vec<Address>,
    #[serde(default)]
    to: Vec<Address>,
    #[serde(default)]
    cc: Vec<Address>,
    #[serde(default)]
    bcc: Vec<Address>,
    received_at: String,
    #[serde(default)]
    sent_at: Option<String>,
    #[serde(default)]
    message_id: Vec<String>,
    #[serde(default)]
    mailbox_ids: HashMap<String, bool>,
    #[serde(default)]
    body_values: HashMap<String, BodyValue>,
    #[serde(default)]
    text_body: Vec<BodyReference>,
    #[serde(default)]
    html_body: Vec<BodyReference>,
    #[serde(default)]
    body_structure: Option<BodyPart>,
    #[serde(flatten)]
    properties: Map<String, Value>,
}

#[derive(Clone, Default, Deserialize, serde::Serialize)]
struct Address {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    email: String,
}

#[derive(Clone, Default, Deserialize)]
struct BodyValue {
    #[serde(default)]
    value: String,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BodyReference {
    #[serde(default)]
    part_id: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BodyPart {
    #[serde(default)]
    blob_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    media_type: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    disposition: Option<String>,
    #[serde(default)]
    cid: Option<String>,
    #[serde(default)]
    sub_parts: Vec<BodyPart>,
}

struct HeaderRule {
    property: String,
    values: HashSet<String>,
}

struct EmailFilters {
    include_headers: Vec<HeaderRule>,
    exclude_headers: Vec<HeaderRule>,
    include_message_id_contains: Vec<String>,
}

pub async fn poll_fastmail(
    request: PollRequest,
    client: &Client,
    token: &str,
) -> Result<PollResponse, ProtocolError> {
    if token.is_empty() {
        return Err(source_error("FASTMAIL_API_TOKEN is required"));
    }
    let session_url = string_option(&request, "session_url")
        .unwrap_or_else(|| "https://api.fastmail.com/jmap/session".into());
    let session_response = client
        .get(session_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| source_error("Fastmail JMAP session request failed"))?;
    if !session_response.status().is_success() {
        return Err(source_error(format!(
            "Fastmail JMAP session request failed with {}",
            session_response.status().as_u16()
        )));
    }
    let session: JmapSession = session_response
        .json()
        .await
        .map_err(|_| source_error("Fastmail JMAP session response is invalid"))?;
    let account_id = string_option(&request, "account_id")
        .or_else(|| {
            session
                .primary_accounts
                .get(EMAIL_ACCOUNT_CAPABILITY)
                .cloned()
        })
        .ok_or_else(|| source_error("Fastmail JMAP session has no mail account"))?;
    let has_checkpoint = !request.checkpoint.is_null();
    let checkpoint = if has_checkpoint {
        parse_checkpoint(&request.checkpoint)?
    } else {
        None
    };
    let mut mailbox_id = string_option(&request, "mailbox_id")
        .or_else(|| checkpoint.as_ref().map(|value| value.mailbox_id.clone()));
    let mut sent_mailbox_id = checkpoint
        .as_ref()
        .and_then(|value| value.sent_mailbox_id.clone());
    let has_sent_mailbox_id = checkpoint
        .as_ref()
        .is_some_and(|value| value.has_sent_mailbox_id);
    if mailbox_id.is_none() || !has_sent_mailbox_id {
        let mailboxes = find_mailboxes(&session.api_url, &account_id, token, client).await?;
        if mailbox_id.is_none() {
            mailbox_id = mailboxes.0;
        }
        sent_mailbox_id = mailboxes.1;
    }
    let mailbox_id =
        mailbox_id.ok_or_else(|| source_error("Fastmail account has no inbox mailbox"))?;
    let bootstrap_limit = integer_option(&request, "bootstrap_limit")
        .unwrap_or(0)
        .min(request.item_limit);
    let filters = email_filters(&request)?;

    if !has_checkpoint {
        let baseline = query_mailbox(
            &session.api_url,
            &account_id,
            token,
            &mailbox_id,
            bootstrap_limit,
            client,
        )
        .await?;
        let messages = if baseline.1.is_empty() {
            Vec::new()
        } else {
            get_emails(
                &session.api_url,
                &account_id,
                token,
                &baseline.1,
                &filters,
                client,
            )
            .await?
        };
        let items = normalize_messages(
            &session.api_url,
            &account_id,
            token,
            messages,
            sent_mailbox_id.as_deref(),
            &filters,
            client,
        )
        .await?;
        return Ok(response(baseline.0, mailbox_id, sent_mailbox_id, items));
    }

    let Some(checkpoint) = checkpoint else {
        let baseline =
            query_mailbox(&session.api_url, &account_id, token, &mailbox_id, 0, client).await?;
        return Ok(response(
            baseline.0,
            mailbox_id,
            sent_mailbox_id,
            Vec::new(),
        ));
    };
    if checkpoint.mailbox_id != mailbox_id {
        let baseline =
            query_mailbox(&session.api_url, &account_id, token, &mailbox_id, 0, client).await?;
        return Ok(response(
            baseline.0,
            mailbox_id,
            sent_mailbox_id,
            Vec::new(),
        ));
    }

    let changes = jmap_call(
        &session.api_url,
        token,
        "Email/queryChanges",
        json!({
            "accountId": account_id,
            "filter": { "inMailbox": mailbox_id },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "sinceQueryState": checkpoint.query_state,
            "maxChanges": request.item_limit,
        }),
        "changes",
        client,
    )
    .await?;
    let new_query_state = changes
        .get("newQueryState")
        .and_then(Value::as_str)
        .ok_or_else(|| source_error("Fastmail query changes response has no query state"))?
        .to_owned();
    let ids: Vec<String> = changes
        .get("added")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|addition| addition.get("id").and_then(Value::as_str))
        .take(request.item_limit)
        .map(str::to_owned)
        .collect();
    let messages = if ids.is_empty() {
        Vec::new()
    } else {
        get_emails(&session.api_url, &account_id, token, &ids, &filters, client).await?
    };
    let items = normalize_messages(
        &session.api_url,
        &account_id,
        token,
        messages,
        sent_mailbox_id.as_deref(),
        &filters,
        client,
    )
    .await?;
    Ok(response(
        new_query_state,
        mailbox_id,
        sent_mailbox_id,
        items,
    ))
}

fn response(
    query_state: String,
    mailbox_id: String,
    sent_mailbox_id: Option<String>,
    items: Vec<IntakeItem>,
) -> PollResponse {
    PollResponse {
        protocol_version: 1,
        checkpoint: json!({
            "queryState": query_state,
            "mailboxId": mailbox_id,
            "sentMailboxId": sent_mailbox_id,
        }),
        items,
    }
}

async fn query_mailbox(
    api_url: &str,
    account_id: &str,
    token: &str,
    mailbox_id: &str,
    limit: usize,
    client: &Client,
) -> Result<(String, Vec<String>), ProtocolError> {
    let result = jmap_call(
        api_url,
        token,
        "Email/query",
        json!({
            "accountId": account_id,
            "filter": { "inMailbox": mailbox_id },
            "sort": [{ "property": "receivedAt", "isAscending": false }],
            "limit": limit,
        }),
        "query",
        client,
    )
    .await?;
    let state = result
        .get("queryState")
        .and_then(Value::as_str)
        .ok_or_else(|| source_error("Fastmail mailbox query response has no query state"))?
        .to_owned();
    let ids = result
        .get("ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Ok((state, ids))
}

async fn find_mailboxes(
    api_url: &str,
    account_id: &str,
    token: &str,
    client: &Client,
) -> Result<(Option<String>, Option<String>), ProtocolError> {
    let result = jmap_call(
        api_url,
        token,
        "Mailbox/get",
        json!({ "accountId": account_id, "properties": ["id", "role"] }),
        "mailboxes",
        client,
    )
    .await?;
    let mut inbox = None;
    let mut sent = None;
    for mailbox in result
        .get("list")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = mailbox.get("id").and_then(Value::as_str);
        match mailbox.get("role").and_then(Value::as_str) {
            Some("inbox") => inbox = id.map(str::to_owned),
            Some("sent") => sent = id.map(str::to_owned),
            _ => {}
        }
    }
    Ok((inbox, sent))
}

async fn get_emails(
    api_url: &str,
    account_id: &str,
    token: &str,
    ids: &[String],
    filters: &EmailFilters,
    client: &Client,
) -> Result<Vec<Email>, ProtocolError> {
    let mut properties = vec![
        "id".to_owned(),
        "threadId".to_owned(),
        "subject".to_owned(),
        "from".to_owned(),
        "to".to_owned(),
        "cc".to_owned(),
        "bcc".to_owned(),
        "receivedAt".to_owned(),
        "sentAt".to_owned(),
        "messageId".to_owned(),
        "mailboxIds".to_owned(),
        "textBody".to_owned(),
        "htmlBody".to_owned(),
        "bodyValues".to_owned(),
        "bodyStructure".to_owned(),
    ];
    for rule in filters
        .include_headers
        .iter()
        .chain(&filters.exclude_headers)
    {
        if !properties.contains(&rule.property) {
            properties.push(rule.property.clone());
        }
    }
    let result = jmap_call(
        api_url,
        token,
        "Email/get",
        json!({
            "accountId": account_id,
            "ids": ids,
            "properties": properties,
            "fetchTextBodyValues": true,
            "fetchHTMLBodyValues": true,
            "maxBodyValueBytes": BODY_LIMIT,
        }),
        "emails",
        client,
    )
    .await?;
    serde_json::from_value(result.get("list").cloned().unwrap_or_else(|| json!([])))
        .map_err(|_| source_error("Fastmail email response is invalid"))
}

async fn get_thread(
    api_url: &str,
    account_id: &str,
    token: &str,
    thread_id: &str,
    filters: &EmailFilters,
    client: &Client,
) -> Result<Vec<Email>, ProtocolError> {
    let result = jmap_call(
        api_url,
        token,
        "Thread/get",
        json!({ "accountId": account_id, "ids": [thread_id] }),
        "thread",
        client,
    )
    .await?;
    let all_ids: Vec<String> = result
        .get("list")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|thread| thread.get("emailIds"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let start = all_ids.len().saturating_sub(THREAD_MESSAGE_LIMIT);
    let mut emails = get_emails(
        api_url,
        account_id,
        token,
        &all_ids[start..],
        filters,
        client,
    )
    .await?;
    emails.retain(|email| is_allowed(email, filters));
    emails.sort_by(|left, right| message_timestamp(left).cmp(message_timestamp(right)));
    Ok(emails)
}

async fn normalize_messages(
    api_url: &str,
    account_id: &str,
    token: &str,
    mut messages: Vec<Email>,
    sent_mailbox_id: Option<&str>,
    filters: &EmailFilters,
    client: &Client,
) -> Result<Vec<IntakeItem>, ProtocolError> {
    messages.sort_by(|left, right| message_timestamp(left).cmp(message_timestamp(right)));
    let mut thread_cache: HashMap<String, Vec<Email>> = HashMap::new();
    let mut items = Vec::new();
    for email in messages {
        if !is_allowed(&email, filters) {
            continue;
        }
        if !thread_cache.contains_key(&email.thread_id) {
            let thread = get_thread(
                api_url,
                account_id,
                token,
                &email.thread_id,
                filters,
                client,
            )
            .await?;
            thread_cache.insert(email.thread_id.clone(), thread);
        }
        let thread = &thread_cache[&email.thread_id];
        if sent_mailbox_id.is_some_and(|sent| {
            thread
                .last()
                .and_then(|latest| latest.mailbox_ids.get(sent))
                .copied()
                .unwrap_or(false)
        }) {
            continue;
        }
        items.push(normalize_email(account_id, &email, thread));
    }
    Ok(items)
}

async fn jmap_call(
    api_url: &str,
    token: &str,
    method: &str,
    arguments: Value,
    call_id: &str,
    client: &Client,
) -> Result<Value, ProtocolError> {
    let response = client
        .post(api_url)
        .bearer_auth(token)
        .json(&json!({
            "using": [EMAIL_ACCOUNT_CAPABILITY, CORE_CAPABILITY],
            "methodCalls": [[method, arguments, call_id]],
        }))
        .send()
        .await
        .map_err(|_| source_error("Fastmail JMAP request failed"))?;
    if !response.status().is_success() {
        return Err(source_error(format!(
            "Fastmail JMAP request failed with {}",
            response.status().as_u16()
        )));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| source_error("Fastmail JMAP response is invalid"))?;
    let result = body
        .get("methodResponses")
        .and_then(Value::as_array)
        .and_then(|responses| responses.first())
        .and_then(Value::as_array)
        .ok_or_else(|| source_error("Fastmail JMAP response has no method result"))?;
    if result.first().and_then(Value::as_str) == Some("error") {
        let kind = result
            .get(1)
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(source_error(format!("Fastmail JMAP error: {kind}")));
    }
    result
        .get(1)
        .cloned()
        .ok_or_else(|| source_error("Fastmail JMAP response has no method result"))
}

fn normalize_email(account_id: &str, email: &Email, thread: &[Email]) -> IntakeItem {
    let thread_body = truncate_utf16(
        &thread
            .iter()
            .map(|message| {
                let recipients: Vec<Address> = message
                    .to
                    .iter()
                    .chain(&message.cc)
                    .chain(&message.bcc)
                    .cloned()
                    .collect();
                format!(
                    "From: {}\nTo: {}\nDate: {}\nSubject: {}\n\n{}",
                    format_addresses(&message.from),
                    format_addresses(&recipients),
                    message.received_at,
                    message.subject.as_deref().unwrap_or("(no subject)"),
                    body_text(message)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
        BODY_LIMIT * 4,
    );
    let attachments: Vec<Value> = thread
        .iter()
        .flat_map(|message| attachment_metadata(message.body_structure.as_ref()))
        .take(ATTACHMENT_LIMIT)
        .collect();
    let metadata = json!({
        "messageId": email.id,
        "threadId": email.thread_id,
        "from": email.from,
        "to": email.to,
        "cc": email.cc,
        "bcc": email.bcc,
        "attachments": attachments,
        "threadMessageCount": thread.len(),
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    IntakeItem {
        entity_id: format!("fastmail:{account_id}:thread:{}", email.thread_id),
        revision_id: email.id.clone(),
        kind: IntakeItemKind::Email,
        title: email
            .subject
            .clone()
            .unwrap_or_else(|| "(no subject)".into()),
        body: thread_body,
        url: None,
        occurred_at: email.received_at.clone(),
        metadata,
    }
}

fn body_text(email: &Email) -> String {
    let parts = if email.text_body.is_empty() {
        &email.html_body
    } else {
        &email.text_body
    };
    let value = parts
        .iter()
        .map(|part| {
            part.part_id
                .as_ref()
                .and_then(|id| email.body_values.get(id))
                .map(|body| body.value.as_str())
                .unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate_utf16(&value, BODY_LIMIT)
}

fn attachment_metadata(root: Option<&BodyPart>) -> Vec<Value> {
    let Some(root) = root else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut pending = vec![root];
    while let Some(part) = pending.pop() {
        if result.len() >= ATTACHMENT_LIMIT {
            break;
        }
        pending.extend(part.sub_parts.iter());
        if part.blob_id.is_none()
            || (part.name.is_none() && part.disposition.as_deref() != Some("attachment"))
        {
            continue;
        }
        result.push(json!({
            "name": part.name,
            "type": part.media_type.as_deref().unwrap_or("application/octet-stream"),
            "size": part.size.unwrap_or(0),
            "disposition": part.disposition,
            "cid": part.cid,
        }));
    }
    result
}

fn format_addresses(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(|address| match &address.name {
            Some(name) => format!("{name} <{}>", address.email),
            None => address.email.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_checkpoint(value: &Value) -> Result<Option<Checkpoint>, ProtocolError> {
    let object = value
        .as_object()
        .ok_or_else(|| source_error("Fastmail checkpoint is invalid"))?;
    let Some(query_state) = object.get("queryState").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(mailbox_id) = object.get("mailboxId").and_then(Value::as_str) else {
        return Ok(None);
    };
    let has_sent_mailbox_id = object.contains_key("sentMailboxId");
    let sent_mailbox_id = match object.get("sentMailboxId") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Ok(None),
    };
    Ok(Some(Checkpoint {
        query_state: query_state.into(),
        mailbox_id: mailbox_id.into(),
        sent_mailbox_id,
        has_sent_mailbox_id,
    }))
}

fn email_filters(request: &PollRequest) -> Result<EmailFilters, ProtocolError> {
    Ok(EmailFilters {
        include_headers: header_rules(request, "include_headers")?,
        exclude_headers: header_rules(request, "exclude_headers")?,
        include_message_id_contains: string_list_option(request, "include_message_id_contains")?
            .into_iter()
            .map(|value| value.to_lowercase())
            .collect(),
    })
}

fn header_rules(request: &PollRequest, name: &str) -> Result<Vec<HeaderRule>, ProtocolError> {
    let Some(value) = request.options.get(name) else {
        return Ok(Vec::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| source_error(format!("Fastmail {name} must map header names to values")))?;
    object
        .iter()
        .map(|(header, values)| {
            if header.is_empty()
                || !header
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(source_error(format!(
                    "Fastmail {name} header name is invalid: {header}"
                )));
            }
            let values = values.as_array().ok_or_else(|| {
                source_error(format!(
                    "Fastmail {name} header {header} must have a non-empty value list"
                ))
            })?;
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
            {
                return Err(source_error(format!(
                    "Fastmail {name} header {header} must have a non-empty value list"
                )));
            }
            Ok(HeaderRule {
                property: format!("header:{header}:asText"),
                values: values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|value| value.trim().to_lowercase())
                    .collect(),
            })
        })
        .collect()
}

fn is_allowed(email: &Email, filters: &EmailFilters) -> bool {
    let included_by_message_id = filters.include_message_id_contains.is_empty()
        || email.message_id.iter().any(|message_id| {
            let message_id = message_id.to_lowercase();
            filters
                .include_message_id_contains
                .iter()
                .any(|required| message_id.contains(required))
        });
    included_by_message_id
        && filters
            .include_headers
            .iter()
            .all(|rule| matches_header_rule(email, rule))
        && !filters
            .exclude_headers
            .iter()
            .any(|rule| matches_header_rule(email, rule))
}

fn matches_header_rule(email: &Email, rule: &HeaderRule) -> bool {
    email
        .properties
        .get(&rule.property)
        .and_then(Value::as_str)
        .is_some_and(|value| rule.values.contains(&value.trim().to_lowercase()))
}

fn string_list_option(request: &PollRequest, name: &str) -> Result<Vec<String>, ProtocolError> {
    let Some(value) = request.options.get(name) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| source_error(format!("Fastmail {name} must be a non-empty string list")))?;
    if values.is_empty()
        || values
            .iter()
            .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
    {
        return Err(source_error(format!(
            "Fastmail {name} must be a non-empty string list"
        )));
    }
    Ok(values
        .iter()
        .filter_map(Value::as_str)
        .map(|value| value.trim().to_owned())
        .collect())
}

fn string_option(request: &PollRequest, name: &str) -> Option<String> {
    request
        .options
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn integer_option(request: &PollRequest, name: &str) -> Option<usize> {
    request
        .options
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value <= 9_007_199_254_740_991)
}

fn message_timestamp(email: &Email) -> &str {
    email.sent_at.as_deref().unwrap_or(&email.received_at)
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
