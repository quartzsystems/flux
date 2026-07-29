//! Test orchestration.
//!
//! The orchestrator owns what happens between "an operator pressed run" and "a
//! result exists": validating the configuration against current reality, taking
//! the ports, programming streams, driving the engine, and recording what came
//! back.
//!
//! Milestone 2 implements the manual test type — start these flows, stop them
//! when asked. Milestone 3 adds the RFC 2544 state machine on top, whose search
//! logic is deliberately a pure function so it can be table-tested without an
//! engine at all.

pub mod run;
pub mod translate;

pub use run::RunSupervisor;
