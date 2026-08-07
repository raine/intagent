  CREATE TABLE triage_run_prompts (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES triage_runs(id),
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    UNIQUE(run_id, role)
  );
  CREATE INDEX triage_run_prompts_run_idx ON triage_run_prompts(run_id, id);
