//! Transport-layer primitives that aren't tied to a specific protocol.
//! Currently just `happy_eyeballs`, used by both the check-path connector
//! and the outbound connector to race v6/v4 connects.

pub mod happy_eyeballs;
