//! Fence configuration struct and parsing logic.

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::defaults::*;

/// Top-level Fence configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct FenceConfig {
    pub pool: PoolSection,
    #[serde(default)]
    pub host: HostSection,
    #[serde(default)]
    pub overflow: OverflowSection,
    #[serde(default)]
    pub recovery: RecoverySection,
}

/// Pool configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct PoolSection {
    /// Path to the backing file or device.
    pub path: PathBuf,

    /// Number of record slots.
    #[serde(default = "default_capacity")]
    pub capacity: u32,

    /// Payload bytes per record.
    #[serde(default = "default_payload_size")]
    pub payload_size: u32,

    /// Maximum concurrent writer hosts.
    #[serde(default = "default_max_hosts")]
    pub max_hosts: u16,
}

/// Host configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct HostSection {
    /// This host's unique ID (0-based, must be < max_hosts).
    #[serde(default = "default_host_id")]
    pub host_id: u16,

    /// Human-readable name (optional).
    #[serde(default)]
    pub name: Option<String>,
}

/// Overflow strategy configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct OverflowSection {
    /// Overflow strategy: "error", "segmented", "circular".
    #[serde(default = "default_overflow_strategy")]
    pub strategy: String,

    /// Max segments (for "segmented" strategy).
    #[serde(default)]
    pub max_segments: Option<u32>,

    /// Backpressure mode (for "circular" strategy): "block" or "drop".
    #[serde(default)]
    pub backpressure: Option<String>,
}

/// Recovery configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct RecoverySection {
    /// Run recovery automatically on open.
    #[serde(default = "default_auto_recover")]
    pub auto_recover: bool,

    /// Maximum time (ms) for recovery before timing out.
    #[serde(default = "default_recovery_timeout_ms")]
    pub recovery_timeout_ms: u64,
}

// Default value functions for serde.
fn default_capacity() -> u32 { DEFAULT_CAPACITY }
fn default_payload_size() -> u32 { DEFAULT_PAYLOAD_SIZE }
fn default_max_hosts() -> u16 { DEFAULT_MAX_HOSTS }
fn default_host_id() -> u16 { DEFAULT_HOST_ID }
fn default_overflow_strategy() -> String { DEFAULT_OVERFLOW_STRATEGY.to_string() }
fn default_auto_recover() -> bool { DEFAULT_AUTO_RECOVER }
fn default_recovery_timeout_ms() -> u64 { DEFAULT_RECOVERY_TIMEOUT_MS }

impl Default for HostSection {
    fn default() -> Self {
        Self {
            host_id: DEFAULT_HOST_ID,
            name: None,
        }
    }
}

impl Default for OverflowSection {
    fn default() -> Self {
        Self {
            strategy: DEFAULT_OVERFLOW_STRATEGY.to_string(),
            max_segments: None,
            backpressure: None,
        }
    }
}

impl Default for RecoverySection {
    fn default() -> Self {
        Self {
            auto_recover: DEFAULT_AUTO_RECOVER,
            recovery_timeout_ms: DEFAULT_RECOVERY_TIMEOUT_MS,
        }
    }
}

/// Configuration validation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// File could not be read.
    FileNotFound { path: PathBuf },
    /// TOML parsing failed.
    ParseError { detail: String },
    /// A field has an invalid value.
    ValidationError { field: String, detail: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound { path } => write!(f, "config file not found: {}", path.display()),
            Self::ParseError { detail } => write!(f, "config parse error: {detail}"),
            Self::ValidationError { field, detail } => {
                write!(f, "invalid config: {field}: {detail}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl FenceConfig {
    /// Parse a configuration from a TOML string.
    pub fn from_str(toml_str: &str) -> Result<Self, ConfigError> {
        let config: FenceConfig = toml::from_str(toml_str).map_err(|e| ConfigError::ParseError {
            detail: e.to_string(),
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Load configuration from a file path.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|_| ConfigError::FileNotFound {
            path: path.to_path_buf(),
        })?;
        Self::from_str(&content)
    }

    /// Resolve the config path from CLI arg, env var, or default.
    ///
    /// Priority:
    /// 1. `cli_path` (if Some)
    /// 2. `FENCE_CONFIG` environment variable
    /// 3. `/etc/fence/conf.toml`
    pub fn resolve_path(cli_path: Option<&Path>) -> PathBuf {
        if let Some(p) = cli_path {
            return p.to_path_buf();
        }
        if let Ok(env_path) = std::env::var("FENCE_CONFIG") {
            return PathBuf::from(env_path);
        }
        PathBuf::from(DEFAULT_CONFIG_PATH)
    }

    /// Validate all config fields.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.pool.capacity == 0 {
            return Err(ConfigError::ValidationError {
                field: "pool.capacity".into(),
                detail: "must be > 0".into(),
            });
        }

        if self.pool.payload_size == 0 {
            return Err(ConfigError::ValidationError {
                field: "pool.payload_size".into(),
                detail: "must be > 0".into(),
            });
        }

        if self.pool.payload_size > 65536 {
            return Err(ConfigError::ValidationError {
                field: "pool.payload_size".into(),
                detail: "must be <= 65536".into(),
            });
        }

        if self.pool.payload_size % 8 != 0 {
            return Err(ConfigError::ValidationError {
                field: "pool.payload_size".into(),
                detail: "must be a multiple of 8 (alignment)".into(),
            });
        }

        if self.pool.max_hosts == 0 || self.pool.max_hosts > 256 {
            return Err(ConfigError::ValidationError {
                field: "pool.max_hosts".into(),
                detail: "must be 1-256".into(),
            });
        }

        if self.host.host_id >= self.pool.max_hosts {
            return Err(ConfigError::ValidationError {
                field: "host.host_id".into(),
                detail: format!("must be < max_hosts ({})", self.pool.max_hosts),
            });
        }

        let valid_strategies = ["error", "segmented", "circular"];
        if !valid_strategies.contains(&self.overflow.strategy.as_str()) {
            return Err(ConfigError::ValidationError {
                field: "overflow.strategy".into(),
                detail: format!(
                    "must be one of: {}",
                    valid_strategies.join(", ")
                ),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[pool]
path = "/tmp/fence_pool.bin"
"#;

    const FULL_TOML: &str = r#"
[pool]
path = "/dev/dax0.0"
capacity = 500000
payload_size = 256
max_hosts = 8

[host]
host_id = 2
name = "worker-2"

[overflow]
strategy = "segmented"
max_segments = 4

[recovery]
auto_recover = true
recovery_timeout_ms = 3000
"#;

    #[test]
    fn parse_minimal_config() {
        let config = FenceConfig::from_str(MINIMAL_TOML).unwrap();
        assert_eq!(config.pool.path, PathBuf::from("/tmp/fence_pool.bin"));
        assert_eq!(config.pool.capacity, DEFAULT_CAPACITY);
        assert_eq!(config.pool.payload_size, DEFAULT_PAYLOAD_SIZE);
        assert_eq!(config.pool.max_hosts, DEFAULT_MAX_HOSTS);
        assert_eq!(config.host.host_id, DEFAULT_HOST_ID);
        assert_eq!(config.overflow.strategy, "error");
        assert!(config.recovery.auto_recover);
    }

    #[test]
    fn parse_full_config() {
        let config = FenceConfig::from_str(FULL_TOML).unwrap();
        assert_eq!(config.pool.path, PathBuf::from("/dev/dax0.0"));
        assert_eq!(config.pool.capacity, 500000);
        assert_eq!(config.pool.payload_size, 256);
        assert_eq!(config.pool.max_hosts, 8);
        assert_eq!(config.host.host_id, 2);
        assert_eq!(config.host.name, Some("worker-2".into()));
        assert_eq!(config.overflow.strategy, "segmented");
        assert_eq!(config.overflow.max_segments, Some(4));
        assert_eq!(config.recovery.recovery_timeout_ms, 3000);
    }

    #[test]
    fn validate_zero_capacity_rejected() {
        let toml = r#"
[pool]
path = "/tmp/test"
capacity = 0
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "pool.capacity"
        ));
    }

    #[test]
    fn validate_zero_payload_rejected() {
        let toml = r#"
[pool]
path = "/tmp/test"
payload_size = 0
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "pool.payload_size"
        ));
    }

    #[test]
    fn validate_payload_alignment_rejected() {
        let toml = r#"
[pool]
path = "/tmp/test"
payload_size = 100
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "pool.payload_size"
        ));
    }

    #[test]
    fn validate_host_id_exceeds_max_hosts() {
        let toml = r#"
[pool]
path = "/tmp/test"
max_hosts = 4

[host]
host_id = 4
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "host.host_id"
        ));
    }

    #[test]
    fn validate_invalid_overflow_strategy() {
        let toml = r#"
[pool]
path = "/tmp/test"

[overflow]
strategy = "invalid"
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "overflow.strategy"
        ));
    }

    #[test]
    fn validate_max_hosts_out_of_range() {
        let toml = r#"
[pool]
path = "/tmp/test"
max_hosts = 0
"#;
        let result = FenceConfig::from_str(toml);
        assert!(matches!(
            result,
            Err(ConfigError::ValidationError { field, .. }) if field == "pool.max_hosts"
        ));
    }

    #[test]
    fn invalid_toml_parse_error() {
        let result = FenceConfig::from_str("this is not valid toml [[[");
        assert!(matches!(result, Err(ConfigError::ParseError { .. })));
    }

    #[test]
    fn resolve_path_cli_wins() {
        let path = FenceConfig::resolve_path(Some(Path::new("/custom/path.toml")));
        assert_eq!(path, PathBuf::from("/custom/path.toml"));
    }

    #[test]
    fn resolve_path_default_fallback() {
        // Clear env var for this test.
        std::env::remove_var("FENCE_CONFIG");
        let path = FenceConfig::resolve_path(None);
        assert_eq!(path, PathBuf::from(DEFAULT_CONFIG_PATH));
    }

    #[test]
    fn from_file_not_found() {
        let result = FenceConfig::from_file(Path::new("/nonexistent/path/conf.toml"));
        assert!(matches!(result, Err(ConfigError::FileNotFound { .. })));
    }

    #[test]
    fn from_file_valid() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), FULL_TOML).unwrap();

        let config = FenceConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.pool.capacity, 500000);
    }
}
