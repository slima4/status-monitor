//! Background jobs that aren't part of the check pipeline: self-contained
//! tick functions the runtime schedules (daily scheduler, manual invocation
//! in tests) and the shared `periodic` purge-loop runner.

pub mod periodic;
pub mod purge_deleted;
pub mod retention;
