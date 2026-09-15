//! flint — a minimal cross-platform rescue agent.
//!
//! The library holds all the logic so it can be tested end-to-end without a
//! real provider; `main.rs` is a thin CLI shell over it.

pub mod agent;
pub mod config;
pub mod context;
pub mod ndjson;
pub mod patch;
pub mod display;
pub mod engine;
pub mod event;
pub mod fetch;
pub mod provider;
pub mod schema;
pub mod search;
pub mod session;
pub mod sink;
pub mod term;
pub mod tools;
pub mod util;
pub mod web;
