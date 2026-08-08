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
CREATE TABLE triage_run_steps (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    step_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    label TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    outcome TEXT,
    UNIQUE(run_id, step_key)
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
  );
CREATE UNIQUE INDEX entities_external_id_idx ON entities(external_id);
CREATE INDEX events_queue_idx ON events(status, next_attempt_at, observed_at);
CREATE INDEX triage_run_steps_run_idx ON triage_run_steps(run_id, started_at);
CREATE INDEX triage_runs_event_idx ON triage_runs(event_id, started_at DESC);
CREATE INDEX triage_runs_recent_idx ON triage_runs(started_at DESC);
INSERT INTO "command_events" ("id", "event_id", "command", "exit_code", "output_summary", "created_at") VALUES (1, 1, 'aven add private title', 0, 'private command output', '2026-08-07T10:00:02.000Z');
INSERT INTO "entities" ("id", "source", "external_id", "kind", "title", "aven_ref", "investigation_handle", "last_event_at", "handling_status", "operational_metadata") VALUES (1, 'fastmail', 'github:example/intagent#42', 'github-issue', 'Investigate delayed notifications', 'APP-7KQ9', 'inspect-notifications', '2026-08-07T09:59:59.999Z', 'retryable', '{"url":"https://github.example/example/intagent/issues/42?token=removed#activity","kind":"github-issue"}');
INSERT INTO "entities" ("id", "source", "external_id", "kind", "title", "aven_ref", "investigation_handle", "last_event_at", "handling_status", "operational_metadata") VALUES (3, 'fastmail', 'mail:thread-7', 'email', 'Follow up on the release', NULL, NULL, '2026-08-07T09:58:00.000Z', 'succeeded', '{"url":null,"kind":"email"}');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (1, 1, 'shared-revision', '{"private":"older"}', '2026-08-07T09:57:00.000Z', '2026-08-07T09:58:00.000Z', 'pending', 0, NULL, NULL, '2026-08-07T09:58:00.000Z', 'fastmail');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (3, 1, 'issue-update-2', '{"private":"retained retry payload"}', '2026-08-07T09:59:59.999Z', '2026-08-07T10:00:00.000Z', 'retryable', 2, '2026-08-07T10:10:00.000Z', 'rate limit token=private', '2026-08-07T10:00:10.000Z', 'github');
INSERT INTO "events" ("id", "entity_id", "revision_id", "payload", "occurred_at", "observed_at", "status", "attempt_count", "next_attempt_at", "last_error", "updated_at", "source") VALUES (4, 3, 'message-9', NULL, '2026-08-07T09:58:00.000Z', '2026-08-07T09:58:30.000Z', 'succeeded', 1, NULL, NULL, '2026-08-07T10:03:00.000Z', 'fastmail');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (1, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (2, '2026-08-07T00:00:00.000Z');
INSERT INTO "schema_migrations" ("version", "applied_at") VALUES (3, '2026-08-07T00:00:00.000Z');
INSERT INTO "source_state" ("source", "checkpoint", "last_success_at", "last_error", "updated_at") VALUES ('fastmail', '{"state":"mail-2"}', '2026-08-07T09:59:00.000Z', NULL, '2026-08-07T09:59:00.000Z');
INSERT INTO "source_state" ("source", "checkpoint", "last_success_at", "last_error", "updated_at") VALUES ('github', '{"cursor":"issue-42"}', '2026-08-07T10:00:00.000Z', NULL, '2026-08-07T10:00:00.000Z');
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome") VALUES (1, 1, 'tool-1', 'tool', 'bash', '2026-08-07T10:00:30.000Z', '2026-08-07T10:00:40.000Z', 'succeeded');
INSERT INTO "triage_run_steps" ("id", "run_id", "step_key", "kind", "label", "started_at", "ended_at", "outcome") VALUES (2, 2, 'thinking-1', 'thinking', 'thinking', '2026-08-07T10:00:21.000Z', NULL, NULL);
INSERT INTO "triage_runs" ("id", "event_id", "attempt", "started_at", "ended_at", "last_activity_at", "outcome", "model_id", "model_provider", "thinking_level", "turn_count", "retry_count", "compaction_count") VALUES (1, 4, 1, '2026-08-07T10:00:00.000Z', '2026-08-07T10:03:00.000Z', '2026-08-07T10:03:00.000Z', 'succeeded', 'gpt-5.6-luna', 'openai-codex', 'max', 1, 1, 1);
INSERT INTO "triage_runs" ("id", "event_id", "attempt", "started_at", "ended_at", "last_activity_at", "outcome", "model_id", "model_provider", "thinking_level", "turn_count", "retry_count", "compaction_count") VALUES (2, 3, 2, '2026-08-07T10:00:20.000Z', NULL, '2026-08-07T10:00:25.000Z', NULL, 'gpt-5.6-luna', 'openai-codex', 'high', 1, 0, 0);
COMMIT;
PRAGMA foreign_keys = ON;
