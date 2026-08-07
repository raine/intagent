PRAGMA foreign_keys = OFF;
BEGIN TRANSACTION;
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
COMMIT;
PRAGMA foreign_keys = ON;
