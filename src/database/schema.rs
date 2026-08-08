use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OpenFlags, params};

use super::{DatabaseError, timestamp};

pub const MIGRATIONS: [&str; 9] = [
    include_str!("../migrations/001-initial.sql"),
    include_str!("../migrations/002-global-entity-identity.sql"),
    include_str!("../migrations/003-triage-runs.sql"),
    include_str!("../migrations/004-detailed-telemetry.sql"),
    include_str!("../migrations/005-redact-legacy-command-events.sql"),
    include_str!("../migrations/006-step-summaries.sql"),
    include_str!("../migrations/007-run-prompts.sql"),
    include_str!("../migrations/008-triage-conclusions.sql"),
    include_str!("../migrations/009-structured-dispatch.sql"),
];

pub const SCHEMA_VERSION: usize = MIGRATIONS.len();

static MEMORY_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct QueueOwnerLock {
    file: File,
    database: PathBuf,
}

impl QueueOwnerLock {
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let database = canonical_database_identity(path.as_ref())?;
        let file_name = database
            .file_name()
            .ok_or_else(|| DatabaseError::InvalidValue("database path has no file name".into()))?;
        let mut lock_name = file_name.to_os_string();
        lock_name.push(".queue-owner.lock");
        let lock_path = database.with_file_name(lock_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(lock_path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK)
                || error.raw_os_error() == Some(libc::EAGAIN)
            {
                return Err(DatabaseError::QueueOwnerBusy { database });
            }
            return Err(DatabaseError::Io(error));
        }
        Ok(Self { file, database })
    }

    pub fn database(&self) -> &Path {
        &self.database
    }
}

impl Drop for QueueOwnerLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn canonical_database_identity(path: &Path) -> Result<PathBuf, DatabaseError> {
    if path == Path::new(":memory:") {
        return Err(DatabaseError::InvalidValue(
            "queue ownership requires a file-backed database".into(),
        ));
    }
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }
    let parent = path
        .parent()
        .ok_or_else(|| DatabaseError::InvalidValue("database path has no parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let parent = std::fs::canonicalize(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| DatabaseError::InvalidValue("database path has no file name".into()))?;
    Ok(parent.join(name))
}

#[derive(Clone, Debug)]
pub(super) struct OpenTarget {
    value: String,
    flags: OpenFlags,
    pub(super) directory: Option<PathBuf>,
}

impl OpenTarget {
    pub(super) fn new(path: &Path) -> Self {
        if path == Path::new(":memory:") {
            let id = MEMORY_DATABASE_ID.fetch_add(1, Ordering::Relaxed);
            return Self {
                value: format!("file:intake-memory-{id}?mode=memory&cache=shared"),
                flags: OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                directory: None,
            };
        }
        Self {
            value: path.to_string_lossy().into_owned(),
            flags: OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            directory: path.parent().map(Path::to_path_buf),
        }
    }

    fn read_flags(&self) -> OpenFlags {
        if self.flags.contains(OpenFlags::SQLITE_OPEN_URI) {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
        }
    }
}

pub(super) fn open_connection(
    target: &OpenTarget,
    read_only: bool,
) -> Result<Connection, DatabaseError> {
    let flags = if read_only {
        target.read_flags()
    } else {
        target.flags
    };
    let connection = Connection::open_with_flags(&target.value, flags)?;
    connection.busy_timeout(Duration::from_millis(5_000))?;
    if read_only {
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA query_only = ON;")?;
    } else {
        connection.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
    }
    Ok(connection)
}

pub(super) fn migrate(connection: &Connection) -> Result<(), DatabaseError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)",
    )?;
    let mut statement =
        connection.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let applied = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(found) = applied.last().copied()
        && found > SCHEMA_VERSION as i64
    {
        return Err(DatabaseError::FutureSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, found) in applied.iter().copied().enumerate() {
        if found != index as i64 + 1 {
            return Err(DatabaseError::MigrationGap {
                found,
                position: index + 1,
            });
        }
    }
    drop(statement);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied.len()) {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![index + 1, timestamp(Utc::now())],
        )?;
        transaction.commit()?;
    }
    Ok(())
}
