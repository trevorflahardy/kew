//! Execution layer: workers that take tasks, call LLMs, store results.
//!
//! This is the part everyone else skips. A Worker takes a Task, loads context,
//! builds messages, calls the LLM, and stores the result. A Pool manages N
//! concurrent workers as tokio tasks.

pub mod chain;
pub mod pool;
#[allow(clippy::module_inception)]
pub mod worker;
