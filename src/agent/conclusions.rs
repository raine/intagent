use serde::Deserialize;

use crate::database::{ConclusionSource, ErrorCategory, TriageConclusion, TriageDecision};

use super::command_policy::CommandPolicy;
use super::errors::TriageError;

const CONCLUSION_START: &str = "<triage-conclusion>";
const CONCLUSION_END: &str = "</triage-conclusion>";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelConclusion {
    decision: TriageDecision,
    summary: String,
    evidence: Vec<String>,
    actions: Vec<String>,
    outcome: String,
    follow_up: Option<String>,
}

pub(crate) struct ToolObservation {
    pub(crate) name: String,
    pub(crate) outcome: ToolObservationOutcome,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum ToolObservationOutcome {
    Succeeded,
    Failed,
    Denied,
}

pub(crate) fn parse_conclusion(text: &str, policy: &CommandPolicy) -> Option<TriageConclusion> {
    let start = text.rfind(CONCLUSION_START)? + CONCLUSION_START.len();
    let remainder = &text[start..];
    let end = remainder.find(CONCLUSION_END)?;
    if !remainder[end + CONCLUSION_END.len()..].trim().is_empty() || end > 6000 {
        return None;
    }
    let parsed: ModelConclusion = serde_json::from_str(remainder[..end].trim()).ok()?;
    let summary = safe_conclusion_text(policy, &parsed.summary, 600);
    let outcome = safe_conclusion_text(policy, &parsed.outcome, 400);
    if summary.is_empty() || outcome.is_empty() {
        return None;
    }
    Some(TriageConclusion {
        decision: parsed.decision,
        summary,
        evidence: safe_conclusion_items(policy, parsed.evidence),
        actions: safe_conclusion_items(policy, parsed.actions),
        outcome,
        follow_up: parsed
            .follow_up
            .map(|value| safe_conclusion_text(policy, &value, 400))
            .filter(|value| !value.is_empty()),
        source: ConclusionSource::Model,
    })
}

pub(crate) fn strip_conclusion(text: &str) -> &str {
    let Some(start) = text.rfind(CONCLUSION_START) else {
        return text;
    };
    let Some(relative_end) = text[start..].find(CONCLUSION_END) else {
        return text;
    };
    let end = start + relative_end + CONCLUSION_END.len();
    if text[end..].trim().is_empty() {
        &text[..start]
    } else {
        text
    }
}

pub(crate) fn observed_actions(observations: &[ToolObservation]) -> Vec<String> {
    observations
        .iter()
        .take(5)
        .map(|item| {
            format!(
                "{} tool {}.",
                safe_context_label(&item.name, "restricted"),
                match item.outcome {
                    ToolObservationOutcome::Succeeded => "completed",
                    ToolObservationOutcome::Failed => "failed",
                    ToolObservationOutcome::Denied => "was denied",
                }
            )
        })
        .collect()
}

pub(crate) fn fallback_conclusion(
    result: &Result<(), TriageError>,
    observations: &[ToolObservation],
) -> TriageConclusion {
    let denied = observations
        .iter()
        .any(|item| item.outcome == ToolObservationOutcome::Denied);
    let failed = observations
        .iter()
        .any(|item| item.outcome == ToolObservationOutcome::Failed);
    let (decision, summary, outcome, follow_up) = match result {
        Ok(()) => (
            TriageDecision::NoAction,
            "Triage completed without a model-authored conclusion.",
            "The attempt completed successfully.",
            None,
        ),
        Err(TriageError::Cancelled) => (
            TriageDecision::Canceled,
            "Triage was canceled before the agent supplied a conclusion.",
            "The attempt was interrupted.",
            Some("Retry triage if the event still needs review."),
        ),
        Err(TriageError::WallTimeout) => (
            TriageDecision::TimedOut,
            "Triage reached its time limit before the agent supplied a conclusion.",
            "The attempt timed out.",
            Some("Review the recorded activity before retrying."),
        ),
        Err(error) if error.category() == ErrorCategory::TurnLimit => (
            TriageDecision::TurnLimit,
            "Triage reached its turn limit before the agent supplied a conclusion.",
            "The attempt stopped at the configured turn limit.",
            Some("Review the recorded activity before retrying."),
        ),
        Err(_) if denied => (
            TriageDecision::Blocked,
            "Triage could not complete an action because a tool call was denied.",
            "The attempt ended after a denied action.",
            Some("Review the requested action and capability policy."),
        ),
        Err(_) => (
            TriageDecision::Failed,
            "Triage failed before the agent supplied a conclusion.",
            "The attempt failed.",
            Some("Review the failure category and recorded activity before retrying."),
        ),
    };
    let actions = observed_actions(observations);
    let evidence = if failed {
        vec!["At least one recorded tool call failed.".into()]
    } else {
        Vec::new()
    };
    TriageConclusion {
        decision,
        summary: summary.into(),
        evidence,
        actions,
        outcome: outcome.into(),
        follow_up: follow_up.map(str::to_string),
        source: ConclusionSource::Derived,
    }
}

fn safe_context_label(value: &str, fallback: &str) -> String {
    let value = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect::<String>();
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}

fn safe_conclusion_items(policy: &CommandPolicy, values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .take(5)
        .map(|value| safe_conclusion_text(policy, &value, 300))
        .filter(|value| !value.is_empty())
        .collect()
}

fn safe_conclusion_text(policy: &CommandPolicy, value: &str, max_bytes: usize) -> String {
    let filtered = policy.filter(value);
    let urls = regex::Regex::new(r"(?i)https?://\S+").expect("valid URL filter");
    let credentials = regex::Regex::new(
        r"(?i)\b(token|password|secret|authorization|oauth|api[_ -]?key)\s*[:=]\s*\S+",
    )
    .expect("valid credential filter");
    let redacted = urls.replace_all(&filtered, "[link omitted]");
    let redacted = credentials.replace_all(&redacted, "$1=[REDACTED]");
    let normalized = redacted
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&normalized, max_bytes)
}

fn truncate_chars(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.saturating_sub(1);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
