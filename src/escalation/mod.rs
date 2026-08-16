//! Incident paging: turns an incident's lifecycle into notifications on the
//! monitor's bound channels. Mirrors the alert engine's shape (an MPSC channel
//! plus a periodic sweep) but is driven by incidents, not raw check results —
//! so when it is active the alert engine suppresses its own dispatch and the
//! incident is the single source of down/up notification.

pub mod engine;

pub use engine::rules::is_damper_marker;
pub use engine::{EngineDeps, EscalationEngine, IncidentSignal};
