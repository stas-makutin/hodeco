//! Protocol-agnostic task automation: catalogs, history, scheduler, engine.

pub mod command;
pub mod engine;
pub mod history;
pub mod runtime;
pub mod schedule;
pub mod scheduler;
pub mod store;

pub use history::{HistoryError, HistoryLoadReport, HistoryStore};
pub use runtime::{AdmitError, TaskRuntime};
pub use store::{AutomationSnapshot, AutomationStore};
