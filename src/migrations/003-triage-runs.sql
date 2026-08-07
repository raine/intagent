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
  CREATE INDEX triage_runs_event_idx ON triage_runs(event_id, started_at DESC);
  CREATE INDEX triage_runs_recent_idx ON triage_runs(started_at DESC);
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
  CREATE INDEX triage_run_steps_run_idx ON triage_run_steps(run_id, started_at);
