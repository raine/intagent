PRAGMA foreign_keys = OFF;
BEGIN TRANSACTION;
CREATE TABLE command_events (
    id INTEGER PRIMARY KEY,
    event_id INTEGER NOT NULL REFERENCES events(id),
    command TEXT NOT NULL,
    exit_code INTEGER NOT NULL,
    output_summary TEXT NOT NULL,
    created_at TEXT NOT NULL
  );
CREATE TABLE entities (
    id INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    external_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    aven_ref TEXT,
    investigation_handle TEXT,
    last_event_at TEXT NOT NULL,
    handling_status TEXT NOT NULL DEFAULT 'pending',
    operational_metadata TEXT NOT NULL DEFAULT '{}',
    UNIQUE(source, external_id)
  );
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    revision_id TEXT NOT NULL,
    payload TEXT,
    occurred_at TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL, source TEXT,
    UNIQUE(entity_id, revision_id)
  );
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
CREATE TABLE source_state (
    source TEXT PRIMARY KEY,
    checkpoint TEXT,
    last_success_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL
  );
CREATE TABLE triage_run_compactions (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    turn_id INTEGER REFERENCES triage_run_turns(id),
    reason TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    aborted INTEGER,
    will_retry INTEGER,
    tokens_before INTEGER,
    estimated_tokens_after INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    total_cost REAL
  );
CREATE TABLE triage_run_effects (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    type TEXT NOT NULL,
    value TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(run_id, type, value)
  );
CREATE TABLE triage_run_prompts (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(run_id, role)
  );
CREATE TABLE triage_run_retries (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    turn_id INTEGER REFERENCES triage_run_turns(id),
    attempt INTEGER NOT NULL,
    max_attempts INTEGER NOT NULL,
    delay_ms INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    wait_ended_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    error_category TEXT
  );
CREATE TABLE triage_run_steps (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    step_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT, turn_id INTEGER REFERENCES triage_run_turns(id), summary TEXT,
    UNIQUE(run_id, step_key)
  );
CREATE TABLE triage_run_turns (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    ordinal INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    stop_reason TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    total_tokens INTEGER,
    input_cost REAL,
    output_cost REAL,
    cache_read_cost REAL,
    cache_write_cost REAL,
    total_cost REAL,
    context_tokens INTEGER,
    context_window INTEGER,
    UNIQUE(run_id, ordinal)
  );
CREATE TABLE triage_runs (
    id INTEGER PRIMARY KEY,
    event_id INTEGER NOT NULL REFERENCES events(id),
    attempt INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    last_activity_at TEXT NOT NULL,
    outcome TEXT,
    model_id TEXT,
    model_provider TEXT,
    thinking_level TEXT,
    turn_count INTEGER NOT NULL DEFAULT 0,
    retry_count INTEGER NOT NULL DEFAULT 0,
    compaction_count INTEGER NOT NULL DEFAULT 0
  , termination_reason TEXT, failure_category TEXT, context_window INTEGER, max_tokens INTEGER, telemetry_version INTEGER, telemetry_completeness TEXT NOT NULL DEFAULT 'legacy', dispatch_reason TEXT, conclusion_json TEXT);
CREATE UNIQUE INDEX entities_external_id_idx ON entities(external_id);
CREATE INDEX events_queue_idx ON events(status, next_attempt_at, observed_at);
CREATE INDEX triage_run_compactions_run_idx ON triage_run_compactions(run_id, started_at);
CREATE INDEX triage_run_effects_run_idx ON triage_run_effects(run_id, recorded_at);
CREATE INDEX triage_run_prompts_run_idx ON triage_run_prompts(run_id, id);
CREATE INDEX triage_run_retries_run_idx ON triage_run_retries(run_id, started_at);
CREATE INDEX triage_run_steps_run_idx ON triage_run_steps(run_id, started_at);
CREATE INDEX triage_run_turns_run_idx ON triage_run_turns(run_id, ordinal);
CREATE INDEX triage_runs_event_idx ON triage_runs(event_id, started_at DESC);
CREATE INDEX triage_runs_recent_idx ON triage_runs(started_at DESC);
INSERT INTO "command_events" ("id", "event_id", "command", "exit_code", "output_summary", "created_at") VALUES (1, 1, 'tool=legacy', 0, 'unavailable', '2026-08-07T10:00:02.000Z');
INSERT INTO "entities" ("id", "source", "external_id", "kind", "title", "aven_ref", "investigation_handle", "last_event_at", "handling_status", "operational_metadata") VALUES (1, 'fastmail', 'github:example/intagent#42', 'github-issue', 'Investigate delayed notifications', 'APP-7KQ9', 'inspect-notifications', '2026-08-07T09:59:59.999Z', 'retryable', '{"url":"https://github.example/example/intagent/issues/42?token=removed#activity","kind":"github-issue"}');
INSERT INTO "entities" ("id", "source", "external_id", "kind", "title", "aven_ref", "investigation_handle", "last_event_at", "handling_status", "operational_metadata") VALUES (3, 'fastmail', 'mail:thread-7', 'email', 'Follow up on the release', NULL, NULL, '2026-08-07T09:58:00.000Z', 'succeeded', '{"url":null,"kind":"email"}');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (1, 1, 'shared-revision', '{"private":"older"}', '2026-08-07T09:57:00.000Z', '2026-08-07T09:58:00.000Z', 'pending', 0, NULL, NULL, '2026-08-07T09:58:00.000Z', 'fastmail');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (3, 1, 'issue-update-2', '{"private":"retained retry payload"}', '2026-08-07T09:59:59.999Z', '2026-08-07T10:00:00.000Z', 'retryable', 2, '2026-08-07T10:10:00.000Z', 'rate limit token=private', '2026-08-07T10:00:10.000Z', 'github');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (4, 3, 'message-9', NULL, '2026-08-07T09:58:00.000Z', '2026-08-07T09:58:30.000Z', 'succeeded', 1, NULL, NULL, '2026-08-07T10:03:00.000Z', 'fastmail');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (1, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (2, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (3, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (4, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (5, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (6, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (7, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (8, '2026-08-07T00:00:00.000Z');
INSERT INTO "source_state" ("source", "checkpoint", "last_success_at", "last_error", "updated_at") VALUES ('fastmail', '{"state":"mail-2"}', '2026-08-07T09:59:00.000Z', NULL, '2026-08-07T09:59:00.000Z');
INSERT INTO "source_state" ("source", "checkpoint", "last_success_at", "last_error", "updated_at") VALUES ('github', '{"cursor":"issue-42"}', '2026-08-07T10:00:00.000Z', NULL, '2026-08-07T10:00:00.000Z');
INSERT INTO "triage_run_compactions" ("id", "run_id", "turn_id", "reason", "started_at", "ended_at", "outcome", "aborted", "will_retry", "tokens_before", "estimated_tokens_after", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "total_tokens", "total_cost") VALUES (1, 1, 1, 'threshold', '2026-08-07T10:01:20.000Z', '2026-08-07T10:01:50.000Z', 'succeeded', 0, 0, 180000, 80000, 900, 100, 300, 0, 50, 1350, 0.01);
INSERT INTO "triage_run_effects" ("id", "run_id", "type", "value", "recorded_at") VALUES (1, 1, 'aven_reference', 'APP-7KQ9', '2026-08-07T10:02:50.000Z');
INSERT INTO "triage_run_effects" ("id", "run_id", "type", "value", "recorded_at") VALUES (2, 1, 'investigation_handle', 'inspect-notifications', '2026-08-07T10:02:55.000Z');
INSERT INTO "triage_run_prompts" ("id", "run_id", "role", "content", "recorded_at") VALUES (1, 1, 'system', 'Triage intake events using restricted tools.', '2026-08-07T10:00:00.000Z');
INSERT INTO "triage_run_prompts" ("id", "run_id", "role", "content", "recorded_at") VALUES (2, 1, 'user', 'Triage the fixture event.', '2026-08-07T10:00:00.000Z');
INSERT INTO "triage_run_retries" ("id", "run_id", "turn_id", "attempt", "max_attempts", "delay_ms", "started_at", "wait_ended_at", "ended_at", "outcome", "error_category") VALUES (1, 1, 1, 1, 3, 1000, '2026-08-07T10:01:00.000Z', '2026-08-07T10:01:01.000Z', '2026-08-07T10:01:01.000Z', 'succeeded', 'rate_limit');
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome", "turn_id", "summary") VALUES (1, 1, 'fixture-step-1', 'tool', 'bash', '2026-08-07T10:00:30.000Z', '2026-08-07T10:00:40.000Z', 'succeeded', 1, 'rg -n compatibility tests');
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome", "turn_id", "summary") VALUES (2, 2, 'fixture-step-2', 'thinking', 'thinking', '2026-08-07T10:00:21.000Z', '2026-08-07T10:00:25.000Z', 'interrupted', NULL, NULL);
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome", "turn_id", "summary") VALUES (3, 1, 'fixture-step-3', 'thinking', 'thinking', '2026-08-07T10:00:10.000Z', '2026-08-07T10:00:25.000Z', 'succeeded', 1, NULL);
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome", "turn_id", "summary") VALUES (4, 1, 'fixture-step-4', 'compaction', 'compaction', '2026-08-07T10:01:20.000Z', '2026-08-07T10:01:50.000Z', 'succeeded', 1, NULL);
INSERT INTO "triage_run_turns" ("id", "run_id", "ordinal", "started_at", "ended_at", "stop_reason", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "total_tokens", "input_cost", "output_cost", "cache_read_cost", "cache_write_cost", "total_cost", "context_tokens", "context_window") VALUES (1, 1, 1, '2026-08-07T10:00:10.000Z', '2026-08-07T10:01:00.000Z', 'toolUse', 800, 120, 400, 20, 80, 1420, 0.008, 0.006, 0.002, 0.001, 0.017, 45000, 200000);
INSERT INTO "triage_run_turns" ("id", "run_id", "ordinal", "started_at", "ended_at", "stop_reason", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "total_tokens", "input_cost", "output_cost", "cache_read_cost", "cache_write_cost", "total_cost", "context_tokens", "context_window") VALUES (2, 1, 2, '2026-08-07T10:02:00.000Z', '2026-08-07T10:02:40.000Z', 'stop', 500, 60, 200, 0, 30, 790, 0.005, 0.003, 0.001, 0, 0.009, 80000, 200000);
INSERT INTO "triage_runs" ("id", "event_id", "attempt", "started_at", "ended_at", "last_activity_at", "outcome", "model_id", "model_provider", "thinking_level", "turn_count", "retry_count", "compaction_count", "termination_reason", "failure_category", "context_window", "max_tokens", "telemetry_version", "telemetry_completeness", "dispatch_reason", "conclusion_json") VALUES (1, 4, 1, '2026-08-07T10:00:00.000Z', '2026-08-07T10:03:00.000Z', '2026-08-07T10:03:00.000Z', 'succeeded', 'gpt-5.6-luna', 'openai-codex', 'max', 1, 1, 1, 'completed', NULL, 200000, 16000, 1, 'complete', NULL, NULL);
INSERT INTO "triage_runs" ("id", "event_id", "attempt", "started_at", "ended_at", "last_activity_at", "outcome", "model_id", "model_provider", "thinking_level", "turn_count", "retry_count", "compaction_count", "termination_reason", "failure_category", "context_window", "max_tokens", "telemetry_version", "telemetry_completeness", "dispatch_reason", "conclusion_json") VALUES (2, 3, 2, '2026-08-07T10:00:20.000Z', '2026-08-07T10:00:25.000Z', '2026-08-07T10:00:25.000Z', 'interrupted', 'gpt-5.6-luna', 'openai-codex', 'high', 1, 0, 0, 'legacy_interruption', 'interrupted', NULL, NULL, NULL, 'legacy', NULL, NULL);
INSERT INTO "triage_runs" ("id", "event_id", "attempt", "started_at", "ended_at", "last_activity_at", "outcome", "model_id", "model_provider", "thinking_level", "turn_count", "retry_count", "compaction_count", "termination_reason", "failure_category", "context_window", "max_tokens", "telemetry_version", "telemetry_completeness", "dispatch_reason", "conclusion_json") VALUES (3, 3, 3, '2026-08-07T10:04:00.000Z', '2026-08-07T10:04:30.000Z', '2026-08-07T10:04:30.000Z', 'failed', 'gpt-5.6-luna', 'openai-codex', 'high', 1, 0, 0, 'model_error', 'model_unavailable', 200000, 16000, 1, 'complete', NULL, NULL);
COMMIT;
PRAGMA foreign_keys = ON;
