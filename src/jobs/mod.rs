//! Background jobs that aren't part of the check pipeline. Each module is a
//! self-contained tick function; the runtime decides when to call it (daily
//! scheduler, manual invocation in tests).

pub mod purge_deleted;
pub mod retention;
