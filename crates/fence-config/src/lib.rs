//! # fence-config
//!
//! Configuration file parsing and validation for Fence.
//!
//! Reads TOML configuration from:
//! 1. CLI argument `--config <path>` or `-c <path>` (highest priority)
//! 2. Environment variable `FENCE_CONFIG`
//! 3. Default path `/etc/fence/conf.toml`
//!
//! The config is used only at pool creation. After that, the pool header
//! is the source of truth for geometry (capacity, record_size, max_hosts).

pub mod config;
pub mod defaults;

pub use config::{FenceConfig, PoolSection, HostSection, OverflowSection, RecoverySection};
pub use defaults::DEFAULT_CONFIG_PATH;
