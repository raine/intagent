ALTER TABLE events ADD COLUMN next_dispatch_trigger TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE events ADD COLUMN next_dispatch_prior_run_id INTEGER REFERENCES triage_runs(id);

ALTER TABLE triage_runs ADD COLUMN dispatch_sequence INTEGER;
ALTER TABLE triage_runs ADD COLUMN dispatch_trigger TEXT;
ALTER TABLE triage_runs ADD COLUMN dispatch_prior_run_id INTEGER REFERENCES triage_runs(id);
ALTER TABLE triage_runs ADD COLUMN dispatch_scheduled_for TEXT;
