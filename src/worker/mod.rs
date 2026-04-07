//! Execution layer: workers that take tasks, call LLMs, store results.
//!
//! This is the part everyone else skips. A Worker takes a Task, loads context,
//! builds messages, calls the LLM, and stores the result. A Pool manages N
//! concurrent workers as tokio tasks.
//!
//! ## Agentic tool loop
//!
//! When tool definitions are provided, workers run an agentic loop: the LLM
//! can call tools (read_file, list_dir, grep, write_file) mid-generation to
//! explore the codebase. The loop continues until the model produces a final
//! text response or hits the iteration cap.

pub mod chain;
pub mod pool;
pub mod tools;
#[allow(clippy::module_inception)]
pub mod worker;
