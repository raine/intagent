use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use url::Url;

use crate::database::{
    DatabaseError, DatabaseReaders, EventRecord, EventStatus, RunId, TriageCompactionRecord,
    TriageEffectRecord, TriageRetryRecord, TriageRunPromptRecord, TriageRunRecord,
    TriageStepRecord, TriageTurnRecord,
};

#[derive(Clone, Debug)]
pub struct RunDetailOptions {
    pub offset: usize,
    pub limit: usize,
    pub max_turns: Option<u32>,
    pub wall_timeout_ms: Option<u64>,
    pub now: DateTime<Utc>,
}

impl Default for RunDetailOptions {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 200,
            max_turns: None,
            wall_timeout_ms: None,
            now: Utc::now(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    pub generated_at: String,
    pub run: RunProjection,
    pub event: EventProjection,
    pub sibling_attempts: Vec<SiblingAttempt>,
    pub metrics: RunMetrics,
    pub effects: Vec<EffectProjection>,
    pub prompts: Vec<PromptProjection>,
    pub limits: RunLimits,
    pub timeline: Timeline,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProjection {
    pub id: i64,
    pub event_id: i64,
    pub attempt: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub last_activity_at: String,
    pub state: String,
    pub termination_reason: Option<String>,
    pub failure_category: Option<String>,
    pub model: ModelProjection,
    pub telemetry: TelemetryProjection,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProjection {
    pub id: Option<String>,
    pub provider: Option<String>,
    pub thinking_level: Option<String>,
    pub context_window: Option<i64>,
    pub max_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryProjection {
    pub schema_version: Option<i64>,
    pub completeness: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventProjection {
    pub id: i64,
    pub source: String,
    pub entity_id: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub occurred_at: String,
    pub observed_at: String,
    pub status: EventStatus,
    pub aven_ref: Option<String>,
    pub investigation_handle: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiblingAttempt {
    pub id: i64,
    pub attempt: u32,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub state: String,
    pub failure_category: Option<String>,
    pub telemetry_completeness: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunMetrics {
    pub duration_ms: DurationMetrics,
    pub tool_call_count: Option<usize>,
    pub failed_tool_count: Option<usize>,
    pub turn_count: Option<usize>,
    pub retry_count: Option<usize>,
    pub compaction_count: Option<usize>,
    pub usage: UsageMetrics,
    pub peak_context_tokens: Option<i64>,
    #[serde(serialize_with = "serialize_js_number_option")]
    pub peak_context_percent: Option<f64>,
    pub source_lag_ms: Option<i64>,
    pub queue_wait_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationMetrics {
    pub wall: i64,
    pub setup: Option<i64>,
    pub thinking: Option<i64>,
    pub tool: Option<i64>,
    pub compaction: Option<i64>,
    pub retry_wait: Option<i64>,
    pub gaps: Option<i64>,
    pub finalization: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetrics {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub total_cost: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TimelineEntry {
    Turn {
        id: i64,
        ordinal: i64,
        #[serde(rename = "startedAt")]
        started_at: String,
        #[serde(rename = "endedAt")]
        ended_at: Option<String>,
        state: String,
        #[serde(rename = "stopReason")]
        stop_reason: Option<String>,
        usage: UsageMetrics,
        #[serde(rename = "contextTokens")]
        context_tokens: Option<i64>,
        #[serde(rename = "contextWindow")]
        context_window: Option<i64>,
    },
    Span {
        id: i64,
        #[serde(rename = "turnOrdinal")]
        turn_ordinal: Option<i64>,
        kind: String,
        label: String,
        summary: Option<String>,
        #[serde(rename = "startedAt")]
        started_at: String,
        #[serde(rename = "endedAt")]
        ended_at: Option<String>,
        state: String,
    },
    Retry {
        id: i64,
        #[serde(rename = "turnOrdinal")]
        turn_ordinal: Option<i64>,
        attempt: i64,
        #[serde(rename = "maxAttempts")]
        max_attempts: i64,
        #[serde(rename = "delayMs")]
        delay_ms: i64,
        #[serde(rename = "startedAt")]
        started_at: String,
        #[serde(rename = "waitEndedAt")]
        wait_ended_at: String,
        #[serde(rename = "endedAt")]
        ended_at: Option<String>,
        state: String,
        #[serde(rename = "errorCategory")]
        error_category: Option<String>,
    },
    Compaction {
        id: i64,
        #[serde(rename = "turnOrdinal")]
        turn_ordinal: Option<i64>,
        reason: Option<String>,
        #[serde(rename = "startedAt")]
        started_at: String,
        #[serde(rename = "endedAt")]
        ended_at: Option<String>,
        state: String,
        aborted: Option<bool>,
        #[serde(rename = "willRetry")]
        will_retry: Option<bool>,
        #[serde(rename = "tokensBefore")]
        tokens_before: Option<i64>,
        #[serde(rename = "estimatedTokensAfter")]
        estimated_tokens_after: Option<i64>,
        #[serde(rename = "totalTokens")]
        total_tokens: Option<i64>,
        #[serde(rename = "totalCost")]
        total_cost: Option<f64>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Timeline {
    pub entries: Vec<TimelineEntry>,
    pub page: TimelinePage,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    pub offset: usize,
    pub limit: usize,
    pub returned: usize,
    pub total: usize,
    pub has_more: bool,
    pub next_offset: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectProjection {
    #[serde(rename = "type")]
    pub effect_type: String,
    pub value: String,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptProjection {
    pub role: String,
    pub content: String,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLimits {
    pub max_turns: Option<u32>,
    pub wall_timeout_ms: Option<u64>,
    pub model_context_window: Option<i64>,
    pub model_max_tokens: Option<i64>,
}

pub async fn run_detail(
    database: &DatabaseReaders,
    run_id: RunId,
    options: RunDetailOptions,
) -> Result<Option<RunDetail>, DatabaseError> {
    let Some(stored_run) = database.triage_run(run_id).await? else {
        return Ok(None);
    };
    let Some(event) = database.event(stored_run.event_id).await? else {
        return Ok(None);
    };
    let run = display_run(stored_run, &event);
    let steps = database.triage_run_steps(run_id).await?;
    let turns = database.triage_run_turns(run_id).await?;
    let retries = database.triage_run_retries(run_id).await?;
    let compactions = database.triage_run_compactions(run_id).await?;
    let siblings = database.triage_runs_for_event(event.id).await?;
    let effects = database.triage_run_effects(run_id).await?;
    let prompts = database.triage_run_prompts(run_id).await?;
    let metrics = run_metrics(
        &run,
        &event,
        &steps,
        &turns,
        &retries,
        &compactions,
        options.now,
    );
    let all_entries = timeline_entries(&run, &steps, &turns, &retries, &compactions);
    let offset = options.offset.min(9_007_199_254_740_991_usize);
    let limit = options.limit.clamp(1, 500);
    let total = all_entries.len();
    let entries = all_entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let has_more = offset.saturating_add(entries.len()) < total;

    Ok(Some(RunDetail {
        generated_at: crate::database::timestamp(options.now),
        run: RunProjection {
            id: run.id,
            event_id: run.event_id,
            attempt: run.attempt,
            started_at: run.started_at.clone(),
            ended_at: run.ended_at.clone(),
            last_activity_at: run.last_activity_at.clone(),
            state: run.outcome.clone().unwrap_or_else(|| "active".into()),
            termination_reason: run.termination_reason.clone(),
            failure_category: run.failure_category.clone(),
            model: ModelProjection {
                id: run.model_id.clone(),
                provider: run.model_provider.clone(),
                thinking_level: run.thinking_level.clone(),
                context_window: run.context_window,
                max_tokens: run.max_tokens,
            },
            telemetry: TelemetryProjection {
                schema_version: run.telemetry_version,
                completeness: run.telemetry_completeness.clone(),
            },
        },
        event: event_projection(&event),
        sibling_attempts: siblings
            .into_iter()
            .map(|sibling| display_run(sibling, &event))
            .map(|sibling| SiblingAttempt {
                id: sibling.id,
                attempt: sibling.attempt,
                started_at: sibling.started_at,
                ended_at: sibling.ended_at,
                state: sibling.outcome.unwrap_or_else(|| "active".into()),
                failure_category: sibling.failure_category,
                telemetry_completeness: sibling.telemetry_completeness,
            })
            .collect(),
        metrics,
        effects: effects.into_iter().map(effect_projection).collect(),
        prompts: prompts.into_iter().map(prompt_projection).collect(),
        limits: RunLimits {
            max_turns: options.max_turns,
            wall_timeout_ms: options.wall_timeout_ms,
            model_context_window: run.context_window,
            model_max_tokens: run.max_tokens,
        },
        timeline: Timeline {
            page: TimelinePage {
                offset,
                limit,
                returned: entries.len(),
                total,
                has_more,
                next_offset: has_more.then(|| offset.saturating_add(entries.len())),
            },
            entries,
        },
    }))
}

pub fn run_metrics(
    run: &TriageRunRecord,
    event: &EventRecord,
    steps: &[TriageStepRecord],
    turns: &[TriageTurnRecord],
    retries: &[TriageRetryRecord],
    compactions: &[TriageCompactionRecord],
    now: DateTime<Utc>,
) -> RunMetrics {
    let start = millis(&run.started_at).unwrap_or(0);
    let active_end = crate::database::timestamp(now);
    let end_source = run.ended_at.as_deref().unwrap_or_else(|| {
        if run.outcome.is_some() {
            &run.last_activity_at
        } else {
            &active_end
        }
    });
    let end = millis(end_source).unwrap_or(start);
    let wall = (end - start).max(0);
    let complete_enough = run.telemetry_completeness != "legacy";
    let context_values = turns.iter().filter_map(|turn| turn.context_tokens);
    let peak_context_tokens = context_values.max();
    let peak_context_percent = turns
        .iter()
        .filter_map(|turn| match (turn.context_tokens, turn.context_window) {
            (Some(tokens), Some(window)) if window != 0 => {
                Some(tokens as f64 / window as f64 * 100.0)
            }
            _ => None,
        })
        .reduce(f64::max);

    RunMetrics {
        duration_ms: if complete_enough {
            partition_durations(run, steps, turns, retries, compactions, start, end)
        } else {
            DurationMetrics {
                wall,
                setup: None,
                thinking: None,
                tool: None,
                compaction: None,
                retry_wait: None,
                gaps: None,
                finalization: None,
            }
        },
        tool_call_count: complete_enough
            .then(|| steps.iter().filter(|step| step.kind == "tool").count()),
        failed_tool_count: complete_enough.then(|| {
            steps
                .iter()
                .filter(|step| step.kind == "tool" && step.outcome.as_deref() == Some("failed"))
                .count()
        }),
        turn_count: complete_enough.then_some(turns.len()),
        retry_count: complete_enough.then_some(retries.len()),
        compaction_count: complete_enough.then_some(compactions.len()),
        usage: UsageMetrics {
            input_tokens: nullable_i64_sum(
                turns
                    .iter()
                    .map(|turn| turn.input_tokens)
                    .chain(compactions.iter().map(|compaction| compaction.input_tokens)),
            ),
            output_tokens: nullable_i64_sum(
                turns.iter().map(|turn| turn.output_tokens).chain(
                    compactions
                        .iter()
                        .map(|compaction| compaction.output_tokens),
                ),
            ),
            cache_read_tokens: nullable_i64_sum(
                turns.iter().map(|turn| turn.cache_read_tokens).chain(
                    compactions
                        .iter()
                        .map(|compaction| compaction.cache_read_tokens),
                ),
            ),
            cache_write_tokens: nullable_i64_sum(
                turns.iter().map(|turn| turn.cache_write_tokens).chain(
                    compactions
                        .iter()
                        .map(|compaction| compaction.cache_write_tokens),
                ),
            ),
            reasoning_tokens: nullable_i64_sum(
                turns.iter().map(|turn| turn.reasoning_tokens).chain(
                    compactions
                        .iter()
                        .map(|compaction| compaction.reasoning_tokens),
                ),
            ),
            total_tokens: nullable_i64_sum(
                turns
                    .iter()
                    .map(|turn| turn.total_tokens)
                    .chain(compactions.iter().map(|compaction| compaction.total_tokens)),
            ),
            total_cost: nullable_f64_sum(
                turns
                    .iter()
                    .map(|turn| turn.total_cost)
                    .chain(compactions.iter().map(|compaction| compaction.total_cost)),
            ),
        },
        peak_context_tokens,
        peak_context_percent,
        source_lag_ms: elapsed(&event.occurred_at, &run.started_at),
        queue_wait_ms: elapsed(&event.observed_at, &run.started_at),
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum DurationCategory {
    Setup,
    Thinking,
    Tool,
    Compaction,
    RetryWait,
    Finalization,
}

struct Interval {
    start: i64,
    end: i64,
    category: DurationCategory,
}

fn partition_durations(
    run: &TriageRunRecord,
    steps: &[TriageStepRecord],
    turns: &[TriageTurnRecord],
    retries: &[TriageRetryRecord],
    compactions: &[TriageCompactionRecord],
    start: i64,
    end: i64,
) -> DurationMetrics {
    let mut intervals = Vec::new();
    add_interval(
        &mut intervals,
        DurationCategory::Setup,
        start,
        turns
            .first()
            .and_then(|turn| millis(&turn.started_at))
            .unwrap_or(end),
        start,
        end,
    );
    if run.ended_at.is_some()
        && let Some(last_turn_end) = turns.last().and_then(|turn| turn.ended_at.as_deref())
        && let Some(last_turn_end) = millis(last_turn_end)
    {
        add_interval(
            &mut intervals,
            DurationCategory::Finalization,
            last_turn_end,
            end,
            start,
            end,
        );
    }
    for step in steps.iter().filter(|step| step.kind != "compaction") {
        let category = if step.kind == "tool" {
            DurationCategory::Tool
        } else {
            DurationCategory::Thinking
        };
        if let Some(step_start) = millis(&step.started_at) {
            let step_end = step
                .ended_at
                .as_deref()
                .or(run.ended_at.as_deref())
                .and_then(millis)
                .unwrap_or(end);
            add_interval(&mut intervals, category, step_start, step_end, start, end);
        }
    }
    for retry in retries {
        if let (Some(retry_start), Some(wait_end)) =
            (millis(&retry.started_at), millis(&retry.wait_ended_at))
        {
            let retry_end = retry
                .ended_at
                .as_deref()
                .or(run.ended_at.as_deref())
                .and_then(millis)
                .unwrap_or(end);
            add_interval(
                &mut intervals,
                DurationCategory::RetryWait,
                retry_start,
                wait_end.min(retry_end),
                start,
                end,
            );
        }
    }
    for compaction in compactions {
        if let Some(compaction_start) = millis(&compaction.started_at) {
            let compaction_end = compaction
                .ended_at
                .as_deref()
                .or(run.ended_at.as_deref())
                .and_then(millis)
                .unwrap_or(end);
            add_interval(
                &mut intervals,
                DurationCategory::Compaction,
                compaction_start,
                compaction_end,
                start,
                end,
            );
        }
    }

    let mut totals = HashMap::from([
        (DurationCategory::Setup, 0),
        (DurationCategory::Thinking, 0),
        (DurationCategory::Tool, 0),
        (DurationCategory::Compaction, 0),
        (DurationCategory::RetryWait, 0),
        (DurationCategory::Finalization, 0),
    ]);
    let mut gaps = 0;
    let mut boundaries = vec![start, end];
    for interval in &intervals {
        boundaries.extend([interval.start, interval.end]);
    }
    boundaries.sort_unstable();
    let priority = [
        DurationCategory::RetryWait,
        DurationCategory::Compaction,
        DurationCategory::Tool,
        DurationCategory::Thinking,
        DurationCategory::Setup,
        DurationCategory::Finalization,
    ];
    for boundary in boundaries.windows(2) {
        let segment_start = boundary[0];
        let segment_end = boundary[1];
        if segment_end <= segment_start {
            continue;
        }
        let active = priority.iter().find(|category| {
            intervals.iter().any(|interval| {
                interval.category == **category
                    && interval.start < segment_end
                    && interval.end > segment_start
            })
        });
        if let Some(category) = active {
            *totals.entry(*category).or_default() += segment_end - segment_start;
        } else {
            gaps += segment_end - segment_start;
        }
    }

    DurationMetrics {
        wall: (end - start).max(0),
        setup: Some(totals[&DurationCategory::Setup]),
        thinking: Some(totals[&DurationCategory::Thinking]),
        tool: Some(totals[&DurationCategory::Tool]),
        compaction: Some(totals[&DurationCategory::Compaction]),
        retry_wait: Some(totals[&DurationCategory::RetryWait]),
        gaps: Some(gaps),
        finalization: Some(totals[&DurationCategory::Finalization]),
    }
}

fn timeline_entries(
    run: &TriageRunRecord,
    steps: &[TriageStepRecord],
    turns: &[TriageTurnRecord],
    retries: &[TriageRetryRecord],
    compactions: &[TriageCompactionRecord],
) -> Vec<TimelineEntry> {
    let turn_ordinals = turns
        .iter()
        .map(|turn| (turn.id, turn.ordinal))
        .collect::<HashMap<_, _>>();
    let terminal_end = run.outcome.as_ref().and(run.ended_at.clone());
    let mut entries = Vec::new();
    for turn in turns {
        entries.push(TimelineEntry::Turn {
            id: turn.id,
            ordinal: turn.ordinal,
            started_at: turn.started_at.clone(),
            ended_at: turn.ended_at.clone().or_else(|| terminal_end.clone()),
            state: if run.outcome.is_some() && turn.stop_reason.as_deref() == Some("aborted") {
                "interrupted"
            } else if turn.ended_at.is_some() {
                "completed"
            } else if run.outcome.is_some() {
                "interrupted"
            } else {
                "active"
            }
            .into(),
            stop_reason: turn.stop_reason.clone(),
            usage: turn_usage(turn),
            context_tokens: turn.context_tokens,
            context_window: turn.context_window,
        });
    }
    for step in steps.iter().filter(|step| step.kind != "compaction") {
        entries.push(TimelineEntry::Span {
            id: step.id,
            turn_ordinal: step.turn_ordinal,
            kind: step.kind.clone(),
            label: step.label.clone(),
            summary: step.summary.clone(),
            started_at: step.started_at.clone(),
            ended_at: step.ended_at.clone().or_else(|| terminal_end.clone()),
            state: step.outcome.clone().unwrap_or_else(|| {
                if run.outcome.is_some() {
                    "interrupted"
                } else {
                    "active"
                }
                .into()
            }),
        });
    }
    for retry in retries {
        entries.push(TimelineEntry::Retry {
            id: retry.id,
            turn_ordinal: retry
                .turn_id
                .and_then(|turn_id| turn_ordinals.get(&turn_id).copied()),
            attempt: retry.attempt,
            max_attempts: retry.max_attempts,
            delay_ms: retry.delay_ms,
            started_at: retry.started_at.clone(),
            wait_ended_at: retry.wait_ended_at.clone(),
            ended_at: retry.ended_at.clone().or_else(|| terminal_end.clone()),
            state: retry.outcome.clone().unwrap_or_else(|| {
                if run.outcome.is_some() {
                    "interrupted"
                } else {
                    "active"
                }
                .into()
            }),
            error_category: retry.error_category.clone(),
        });
    }
    for compaction in compactions {
        entries.push(TimelineEntry::Compaction {
            id: compaction.id,
            turn_ordinal: compaction
                .turn_id
                .and_then(|turn_id| turn_ordinals.get(&turn_id).copied()),
            reason: compaction.reason.clone(),
            started_at: compaction.started_at.clone(),
            ended_at: compaction.ended_at.clone().or_else(|| terminal_end.clone()),
            state: compaction.outcome.clone().unwrap_or_else(|| {
                if run.outcome.is_some() {
                    "interrupted"
                } else {
                    "active"
                }
                .into()
            }),
            aborted: compaction.aborted,
            will_retry: compaction.will_retry,
            tokens_before: compaction.tokens_before,
            estimated_tokens_after: compaction.estimated_tokens_after,
            total_tokens: compaction.total_tokens,
            total_cost: compaction.total_cost,
        });
    }
    entries.sort_by(|left, right| {
        entry_started_at(left)
            .cmp(entry_started_at(right))
            .then_with(|| entry_order(left).cmp(&entry_order(right)))
            .then_with(|| entry_id(left).cmp(&entry_id(right)))
    });
    entries
}

fn display_run(mut run: TriageRunRecord, event: &EventRecord) -> TriageRunRecord {
    if run.outcome.is_none() && event.status != EventStatus::Processing {
        run.ended_at = run.ended_at.or_else(|| Some(run.last_activity_at.clone()));
        run.outcome = Some("interrupted".into());
    }
    run
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

fn event_projection(event: &EventRecord) -> EventProjection {
    EventProjection {
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

fn effect_projection(effect: TriageEffectRecord) -> EffectProjection {
    EffectProjection {
        effect_type: effect.effect_type,
        value: effect.value,
        recorded_at: effect.recorded_at,
    }
}

fn prompt_projection(prompt: TriageRunPromptRecord) -> PromptProjection {
    PromptProjection {
        role: prompt.role,
        content: prompt.content,
        recorded_at: prompt.recorded_at,
    }
}

fn turn_usage(turn: &TriageTurnRecord) -> UsageMetrics {
    UsageMetrics {
        input_tokens: turn.input_tokens,
        output_tokens: turn.output_tokens,
        cache_read_tokens: turn.cache_read_tokens,
        cache_write_tokens: turn.cache_write_tokens,
        reasoning_tokens: turn.reasoning_tokens,
        total_tokens: turn.total_tokens,
        total_cost: turn.total_cost,
    }
}

fn add_interval(
    intervals: &mut Vec<Interval>,
    category: DurationCategory,
    interval_start: i64,
    interval_end: i64,
    run_start: i64,
    run_end: i64,
) {
    let start = run_start.max(interval_start);
    let end = run_end.min(interval_end);
    if end > start {
        intervals.push(Interval {
            start,
            end,
            category,
        });
    }
}

fn nullable_i64_sum(values: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    let present = values.flatten().collect::<Vec<_>>();
    (!present.is_empty()).then(|| present.into_iter().sum())
}

fn nullable_f64_sum(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let present = values.flatten().collect::<Vec<_>>();
    (!present.is_empty()).then(|| present.into_iter().sum())
}

fn elapsed(from: &str, to: &str) -> Option<i64> {
    Some((millis(to)? - millis(from)?).max(0))
}

fn millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn serialize_js_number_option<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) if value.fract() == 0.0 => serializer.serialize_i64(*value as i64),
        Some(value) => serializer.serialize_f64(*value),
        None => serializer.serialize_none(),
    }
}

fn entry_started_at(entry: &TimelineEntry) -> &str {
    match entry {
        TimelineEntry::Turn { started_at, .. }
        | TimelineEntry::Span { started_at, .. }
        | TimelineEntry::Retry { started_at, .. }
        | TimelineEntry::Compaction { started_at, .. } => started_at,
    }
}

fn entry_order(entry: &TimelineEntry) -> u8 {
    match entry {
        TimelineEntry::Turn { .. } => 0,
        TimelineEntry::Span { .. } => 1,
        TimelineEntry::Retry { .. } => 2,
        TimelineEntry::Compaction { .. } => 3,
    }
}

fn entry_id(entry: &TimelineEntry) -> i64 {
    match entry {
        TimelineEntry::Turn { id, .. }
        | TimelineEntry::Span { id, .. }
        | TimelineEntry::Retry { id, .. }
        | TimelineEntry::Compaction { id, .. } => *id,
    }
}
