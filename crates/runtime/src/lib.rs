//! Application boundary for durable conversations and Agent execution.
//!
//! This crate owns domain types and semantic storage ports. It must not depend
//! on Axum, Toasty or Apalis. Infrastructure implements the ports, and the host
//! composes those implementations with the shared Agent phase engine.

pub mod execution;
pub mod model;
pub mod projection;
pub mod store;
