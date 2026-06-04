//! Default values for Fence configuration.

/// Default config file path.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/fence/conf.toml";

/// Default number of record slots.
pub const DEFAULT_CAPACITY: u32 = 1_000_000;

/// Default payload size per record (bytes).
pub const DEFAULT_PAYLOAD_SIZE: u32 = 128;

/// Default maximum concurrent hosts.
pub const DEFAULT_MAX_HOSTS: u16 = 16;

/// Default host ID.
pub const DEFAULT_HOST_ID: u16 = 0;

/// Default overflow strategy.
pub const DEFAULT_OVERFLOW_STRATEGY: &str = "error";

/// Default auto-recover on open.
pub const DEFAULT_AUTO_RECOVER: bool = true;

/// Default recovery timeout (ms).
pub const DEFAULT_RECOVERY_TIMEOUT_MS: u64 = 5000;

/// Default log level.
pub const DEFAULT_LOG_LEVEL: &str = "info";
