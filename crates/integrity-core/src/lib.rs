//! Core, UI-independent engine for Integrity Browser.
//!
//! This crate deliberately has no GUI dependencies so its logic can be tested
//! on any platform and reused independently of the desktop shell.

pub mod filters;
pub mod proxy;
pub mod settings;
pub mod urlutil;

pub use filters::{Category, FilterEngine, FilterParseStats};
pub use proxy::{BlockedRequest, ProxyServer, ProxyStats, ProxyStatsSnapshot};
pub use settings::{FilterListConfig, FilterListKind, Sensitivity, Settings};
