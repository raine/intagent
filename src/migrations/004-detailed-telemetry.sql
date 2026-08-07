  ALTER TABLE triage_runs ADD COLUMN termination_reason TEXT;
  ALTER TABLE triage_runs ADD COLUMN failure_category TEXT;
  ALTER TABLE triage_runs ADD COLUMN context_window INTEGER;
  ALTER TABLE triage_runs ADD COLUMN max_tokens INTEGER;
  ALTER TABLE triage_runs ADD COLUMN telemetry_version INTEGER;
  ALTER TABLE triage_runs ADD COLUMN telemetry_completeness TEXT NOT NULL DEFAULT 'legacy';
  
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
  CREATE INDEX triage_run_turns_run_idx ON triage_run_turns(run_id, ordinal);
  ALTER TABLE triage_run_steps ADD COLUMN turn_id INTEGER REFERENCES triage_run_turns(id);
  
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
  CREATE INDEX triage_run_retries_run_idx ON triage_run_retries(run_id, started_at);
  
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
  CREATE INDEX triage_run_compactions_run_idx ON triage_run_compactions(run_id, started_at);
  
  CREATE TABLE triage_run_effects (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    type TEXT NOT NULL,
    value TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(run_id, type, value)
  );
  CREATE INDEX triage_run_effects_run_idx ON triage_run_effects(run_id, recorded_at);
  
  UPDATE triage_runs
  SET ended_at = last_activity_at,
      outcome = COALESCE(outcome, 'interrupted'),
      termination_reason = COALESCE(termination_reason, 'legacy_interruption'),
      failure_category = CASE
        WHEN outcome IS NULL THEN 'interrupted'
        ELSE failure_category
      END,
      telemetry_completeness = 'legacy'
  WHERE ended_at IS NULL
    AND (outcome IS NOT NULL OR event_id IN (
      SELECT id FROM events WHERE status != 'processing'
    ));
  
  UPDATE triage_run_steps SET step_key = lower(hex(randomblob(16)));
  
  UPDATE triage_run_steps
  SET ended_at = (SELECT run.ended_at FROM triage_runs run WHERE run.id = run_id),
      outcome = 'interrupted'
  WHERE ended_at IS NULL
    AND run_id IN (SELECT id FROM triage_runs WHERE ended_at IS NOT NULL);
