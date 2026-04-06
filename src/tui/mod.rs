//! TUI dashboard using ratatui.
//!
//! `kew status` shows a live terminal dashboard with task counts,
//! recent tasks, context entries, and embedding stats.

#[cfg(feature = "tui")]
pub mod dashboard;
