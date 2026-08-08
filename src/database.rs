mod reader;
mod records;
mod schema;
mod writer;

pub const DATABASE_QUEUE_CAPACITY: usize = 64;
pub const DATABASE_READER_COUNT: usize = 2;

pub use reader::DatabaseReaders;
pub use records::*;
pub use schema::{MIGRATIONS, QueueOwnerLock, SCHEMA_VERSION};
pub use writer::IntakeDatabase;
