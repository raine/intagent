use url::Url;

use crate::database::{
    ConclusionSource, DispatchTrigger, EventRecord, EventStatus, Timestamp, TriageConclusion,
    TriageDecision, TriageRunRecord,
};

pub(crate) struct PresentedRun {
    pub run: TriageRunRecord,
    pub dispatch_sequence: u32,
    pub dispatch_trigger: DispatchTrigger,
    pub dispatch_recorded: bool,
    pub conclusion: TriageConclusion,
}

pub(crate) fn present_run(mut run: TriageRunRecord, event: &EventRecord) -> PresentedRun {
    if run.outcome.is_none() && event.status != EventStatus::Processing {
        run.ended_at = run.ended_at.or_else(|| Some(run.last_activity_at.clone()));
        run.outcome = Some("interrupted".into());
    }

    let (dispatch_trigger, dispatch_recorded) = match run.dispatch_trigger {
        Some(trigger) => (trigger, true),
        None if event.source == "manual-injection" => (DispatchTrigger::ManualInjection, false),
        None if run.attempt > 1 => (DispatchTrigger::BackoffRetry, false),
        None => (DispatchTrigger::Initial, false),
    };
    let dispatch_sequence = run.dispatch_sequence.unwrap_or(run.attempt.max(1));
    let conclusion = displayed_conclusion(&run);

    PresentedRun {
        run,
        dispatch_sequence,
        dispatch_trigger,
        dispatch_recorded,
        conclusion,
    }
}

pub(crate) struct EventPresentation {
    pub id: i64,
    pub source: String,
    pub entity_id: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub occurred_at: Timestamp,
    pub observed_at: Timestamp,
    pub status: EventStatus,
    pub aven_ref: Option<String>,
    pub investigation_handle: Option<String>,
}

pub(crate) fn present_event(event: &EventRecord) -> EventPresentation {
    EventPresentation {
        id: event.id,
        source: event.source.clone(),
        entity_id: event.entity_id.clone(),
        kind: event.kind.clone(),
        title: event.title.clone(),
        url: safe_event_url(event),
        occurred_at: event.occurred_at.clone(),
        observed_at: event.observed_at.clone(),
        status: event.status,
        aven_ref: event.aven_ref.clone(),
        investigation_handle: event.investigation_handle.clone(),
    }
}

pub fn safe_event_url(event: &EventRecord) -> Option<String> {
    let metadata: serde_json::Value = serde_json::from_str(&event.operational_metadata).ok()?;
    let mut url = Url::parse(metadata.get("url")?.as_str()?).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

fn displayed_conclusion(run: &TriageRunRecord) -> TriageConclusion {
    if let Some(conclusion) = &run.conclusion {
        return conclusion.clone();
    }
    let (decision, summary, outcome, follow_up, source) = match run.outcome.as_deref() {
        None => (
            TriageDecision::NeedsFollowUp,
            "Triage is in progress; the final decision is pending.",
            "The attempt has not reached a terminal outcome.",
            None,
            ConclusionSource::Derived,
        ),
        Some("failed") => (
            TriageDecision::Failed,
            "A model-authored conclusion is unavailable for this run.",
            "The recorded attempt failed.",
            Some("Review the failure category and timeline for available evidence."),
            ConclusionSource::Unavailable,
        ),
        Some("interrupted") => (
            TriageDecision::Canceled,
            "A model-authored conclusion is unavailable for this run.",
            "The recorded attempt was interrupted.",
            Some("Review the timeline before deciding whether to retry."),
            ConclusionSource::Unavailable,
        ),
        _ => (
            TriageDecision::NeedsFollowUp,
            "A model-authored conclusion is unavailable for this run.",
            "The recorded attempt completed, but its decision was not captured.",
            Some("Review the recorded effects and timeline for available evidence."),
            ConclusionSource::Unavailable,
        ),
    };
    TriageConclusion {
        decision,
        summary: summary.into(),
        evidence: Vec::new(),
        actions: Vec::new(),
        outcome: outcome.into(),
        follow_up: follow_up.map(str::to_string),
        source,
    }
}
