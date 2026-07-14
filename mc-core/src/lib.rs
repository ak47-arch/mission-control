//! Mission Control core library.
//!
//! Collectors, reducer, rules, state store — everything the CLI and daemon need.

pub mod collector;
pub mod config;
pub mod reducer;
pub mod state;
pub mod transport;