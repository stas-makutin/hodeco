//! Protocol-agnostic task automation: catalogs, history, scheduler, engine.

pub mod command;
pub mod engine;
pub mod feed;
pub mod history;
mod run_list;
pub mod runtime;
pub mod scheduler;
pub mod store;

pub use feed::RunFeed;
pub use history::{HistoryError, HistoryLoadReport, HistoryStore};
pub use runtime::{AdmitError, TaskRuntime};
pub use store::{AutomationSnapshot, AutomationStore};
