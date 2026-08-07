  CREATE TABLE source_state (
    source TEXT PRIMARY KEY,
    checkpoint TEXT,
    last_success_at TEXT,
    last_error TEXT,
    updated_at TEXT NOT NULL
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
    updated_at TEXT NOT NULL,
    UNIQUE(entity_id, revision_id)
  );
  CREATE INDEX events_queue_idx ON events(status, next_attempt_at, observed_at);
  CREATE TABLE command_events (
    id INTEGER PRIMARY KEY,
    event_id INTEGER NOT NULL REFERENCES events(id),
    command TEXT NOT NULL,
    exit_code INTEGER NOT NULL,
    output_summary TEXT NOT NULL,
    created_at TEXT NOT NULL
  );
