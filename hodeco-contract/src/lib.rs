//! Wire types shared by the hodeco backend and the Rust/WASM frontend.
//!
//! This crate contains only serializable DTOs, enums, error codes, parameter
//! metadata, and the static protocol/transport catalog. It has no server,
//! tokio, or transport dependencies.

pub mod api;
pub mod catalog;
pub mod config;
pub mod defaults;
pub mod domain;
pub mod utils;
