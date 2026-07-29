//! Shared domain model and service traits for Flux.
//!
//! This crate is deliberately free of I/O, database, and HTTP concerns. It defines
//! the vocabulary (`types`), the boundaries between `fluxd`'s internal modules
//! (`engine`, `port`), and the declarative configuration documents that are stored
//! as JSONB in Postgres (`config`).
//!
//! Keeping the traits here means any module behind them (the packet engine, the
//! privileged port controller) could be lifted into its own service later without
//! touching call sites.

pub mod config;
pub mod engine;
pub mod flow;
pub mod frame;
pub mod port;
pub mod profile;
pub mod rate;
pub mod rfc2544;
pub mod types;

pub use types::*;
