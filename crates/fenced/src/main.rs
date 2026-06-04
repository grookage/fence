//! # fenced — The Fence daemon binary
//!
//! Entry point for the Fence shared-memory engine process.
//!
//! ## Usage
//!
//! ```bash
//! # Use default config (/etc/fence/conf.toml)
//! fenced
//!
//! # Specify config path
//! fenced --config /path/to/conf.toml
//! fenced -c /path/to/conf.toml
//!
//! # Initialize a new pool
//! fenced init --config /path/to/conf.toml
//!
//! # Print pool status
//! fenced status --config /path/to/conf.toml
//!
//! # Run recovery
//! fenced recover --config /path/to/conf.toml
//! ```

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process;

use fence_config::FenceConfig;
use fence_runtime::{FenceRuntime, PoolConfig};

#[derive(Parser)]
#[command(name = "fenced", version, about = "Fence shared-memory engine daemon")]
struct Cli {
    /// Path to configuration file.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new pool (writes header, zeroes data).
    Init,
    /// Print pool status (committed_tail, reserve_tail, metrics).
    Status,
    /// Run crash recovery on the pool.
    Recover,
    /// Validate the configuration file without starting.
    Validate,
}

fn main() {
    let cli = Cli::parse();

    // Resolve config path.
    let config_path = FenceConfig::resolve_path(cli.config.as_deref());

    match &cli.command {
        Some(Commands::Validate) => {
            cmd_validate(&config_path);
        }
        Some(Commands::Init) => {
            cmd_init(&config_path);
        }
        Some(Commands::Status) => {
            cmd_status(&config_path);
        }
        Some(Commands::Recover) => {
            cmd_recover(&config_path);
        }
        None => {
            // Default: open pool, print status, exit.
            // In the future this will be a long-running daemon.
            cmd_status(&config_path);
        }
    }
}

fn load_config(config_path: &PathBuf) -> FenceConfig {
    match FenceConfig::from_file(config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    }
}

fn cmd_validate(config_path: &PathBuf) {
    let _config = load_config(config_path);
    println!("configuration valid: {}", config_path.display());
}

fn cmd_init(config_path: &PathBuf) {
    let config = load_config(config_path);

    let pool_config = PoolConfig {
        path: config.pool.path.clone(),
        capacity: config.pool.capacity,
        payload_size: config.pool.payload_size,
        max_hosts: config.pool.max_hosts,
        host_id: config.host.host_id,
        create: true,
    };

    match FenceRuntime::open(pool_config) {
        Ok(rt) => {
            println!("pool initialized: {}", config.pool.path.display());
            println!("  capacity:     {}", config.pool.capacity);
            println!("  payload_size: {}", config.pool.payload_size);
            println!("  max_hosts:    {}", config.pool.max_hosts);
            println!("  record_size:  {}", rt.geometry().record_size);
            println!("  total_size:   {} bytes", rt.geometry().total_size);
        }
        Err(e) => {
            eprintln!("error initializing pool: {e}");
            process::exit(1);
        }
    }
}

fn cmd_status(config_path: &PathBuf) {
    let config = load_config(config_path);

    let pool_config = PoolConfig {
        path: config.pool.path.clone(),
        capacity: config.pool.capacity,
        payload_size: config.pool.payload_size,
        max_hosts: config.pool.max_hosts,
        host_id: config.host.host_id,
        create: false,
    };

    match FenceRuntime::open(pool_config) {
        Ok(rt) => {
            let stats = rt.stats();
            println!("pool: {}", config.pool.path.display());
            println!("  committed_tail: {}", rt.committed_tail());
            println!("  reserve_tail:   {}", rt.reserve_tail());
            println!("  capacity:       {}", rt.geometry().capacity);
            println!("  metrics:");
            println!("    appends_total:      {}", stats.appends_total);
            println!("    appends_failed:     {}", stats.appends_failed);
            println!("    bytes_written:      {}", stats.bytes_written);
            println!("    reads_total:        {}", stats.reads_total);
            println!("    checksum_failures:  {}", stats.checksum_failures);
            println!("    recovery_runs:      {}", stats.recovery_runs);
            println!("    abandoned_found:    {}", stats.abandoned_found);
        }
        Err(e) => {
            eprintln!("error opening pool: {e}");
            process::exit(1);
        }
    }
}

fn cmd_recover(config_path: &PathBuf) {
    let config = load_config(config_path);

    let pool_config = PoolConfig {
        path: config.pool.path.clone(),
        capacity: config.pool.capacity,
        payload_size: config.pool.payload_size,
        max_hosts: config.pool.max_hosts,
        host_id: config.host.host_id,
        create: false,
    };

    match FenceRuntime::open(pool_config) {
        Ok(rt) => {
            let report = rt.recover();
            println!("recovery complete:");
            println!("  slots_scanned:   {}", report.slots_scanned);
            println!("  abandoned_count: {}", report.abandoned_count);
            println!("  committed_tail:  {}", report.committed_tail);
        }
        Err(e) => {
            eprintln!("error opening pool: {e}");
            process::exit(1);
        }
    }
}
