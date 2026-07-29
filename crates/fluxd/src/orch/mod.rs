//! Test orchestration.
//!
//! The orchestrator owns what happens between "an operator pressed run" and "a
//! result exists": validating the configuration against current reality, taking
//! the ports, programming streams, driving the engine, and recording what came
//! back.
//!
//! The RFC 2544 search in `rfc2544` is deliberately a pure function of the trials
//! run so far, separated from the async execution loop in `run`, so it can be
//! table-tested exhaustively without an engine at all.

pub mod profile;
pub mod rfc2544;
pub mod statemachine;
pub mod run;
pub mod translate;

pub use run::RunSupervisor;
