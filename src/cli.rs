use clap::{Parser, Subcommand};
use std::{
    env,
    path::PathBuf,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::warn;

use crate::config::{Parachain, Relaychain, ZombieBiteConfig};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Bite the running network using 'doppelganger' binaries, and generate the artifacts for spawning.
    Bite {
        /// Configuration file path to use for the bite operation. CLI args override config file values.
        #[arg(long, short = 'c', verbatim_doc_comment)]
        config: Option<String>,
        /// The network will be using for bite
        /// If not specified, will use the value from config.
        /// If not in config, defaults to polkadot.
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusama", "paseo"]))]
        relay: Option<String>,
        /// If provided we will override the runtime as part of the process of 'bite'
        /// The resulting network will be running with this runtime.
        #[arg(long = "rc-override", verbatim_doc_comment)]
        relay_runtime: Option<String>,
        /// If provided we will _bite_ the live network at the supplied block hieght
        #[arg(long = "rc-bite-at", verbatim_doc_comment)]
        relay_bite_at: Option<u32>,
        /// Parachains to include: asset-hub, coretime, people, bridge-hub, collectives (comma-separated)
        /// For custom parachains use: custom:<para_id>:<rpc_endpoint>:<chain_spec_path>
        /// Example: custom:3392:wss://kusama-yap-3392.example.com:/path/to/chain-spec.json
        #[arg(long, short = 'p', value_delimiter = ',', verbatim_doc_comment)]
        parachains: Option<Vec<String>>,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// sync url to use when we bite the parachain.
        #[arg(long = "rc-sync-url", verbatim_doc_comment)]
        rc_sync_url: Option<String>,
        /// Automatically spawn the 'bited' network
        #[arg(long, short = 'm', default_value_t = false, verbatim_doc_comment)]
        and_spawn: bool,
        /// Monit the progress of the chains, and restart the nodes if the block production stall
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        with_monitor: bool,
        /// Db to use
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["rocksdb", "paritydb"]), default_value="rocksdb", verbatim_doc_comment)]
        database: String,
    },
    /// Spawn a new instance of the network from the bite step.
    Spawn {
        /// Configuration file path to use for the spawn operation. CLI args override config file values.
        #[arg(long, short = 'c', verbatim_doc_comment)]
        config: Option<String>,
        /// Base path where the 'bite' artifacts lives, we should use this base_path
        /// to find those artifacts and 'spawn' the network.
        /// if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// Monit the progress of the chains, and restart the nodes if the block prodution stall
        #[arg(long, short = 'm', default_value_t = false, verbatim_doc_comment)]
        with_monitor: bool,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["spawn", "post", "after"]), default_value="spawn")]
        step: String,
    },
    /// [Helper] Generate artifacts to be used by the next step (only 'spawn' and 'post' allowed)
    GenerateArtifacts {
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusame", "paseo"]), default_value="polkadot")]
        relay: String,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["spawn", "post"]), default_value="spawn")]
        step: String,
    },
    /// [Helper] Clean up directory to only include the needed artifacts
    CleanUpDir {
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusame", "paseo"]), default_value="polkadot")]
        relay: String,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["bite", "spawn", "post"]), default_value="bite")]
        step: String,
    },
}

/// base_path can be set from env with 'ZOMBIE_BITE_BASE_PATH'
/// or using the cli argument (take precedence).
/// And if not set we fallback to defaul `cwd_timestamp`
pub fn get_base_path(cli_base_path: Option<String>) -> PathBuf {
    let global_base_path = if let Some(base_path) = cli_base_path {
        PathBuf::from_str(&base_path).expect("Base path in cli args should be valid")
    } else if let Ok(base_path) = env::var("ZOMBIE_BITE_BASE_PATH") {
        PathBuf::from_str(&base_path)
            .expect("Base path in env 'ZOMBIE_BITE_BASE_PATH' should be valid")
    } else {
        // fallback
        let path = env::current_dir().expect("cwd should be valid");
        let now = SystemTime::now();
        let duration_since_epoch = now
            .duration_since(UNIX_EPOCH)
            .expect("Epoch ts show be valid");
        let fallback = format!(
            "{}_{}",
            path.to_string_lossy(),
            duration_since_epoch.as_secs()
        );
        PathBuf::from_str(&fallback).expect("Base path form fallback should be valid")
    };

    match global_base_path.canonicalize() {
        Ok(canonical_path) => canonical_path,
        Err(_) => global_base_path,
    }
}

#[derive(Debug)]
pub struct ResolvedBiteConfig {
    pub relaychain: Relaychain,
    pub parachains: Vec<Parachain>,
    pub base_path: PathBuf,
    pub and_spawn: bool,
}

#[derive(Debug)]
pub struct ResolvedSpawnConfig {
    pub base_path: PathBuf,
    pub with_monitor: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_bite_config(
    config_path: Option<String>,
    relay: Option<String>,
    relay_runtime: Option<String>,
    relay_bite_at: Option<u32>,
    parachains: Option<Vec<String>>,
    base_path: Option<String>,
    rc_sync_url: Option<String>,
    and_spawn: bool,
) -> Result<ResolvedBiteConfig, anyhow::Error> {
    // Load config file if provided
    let config_file = if let Some(path) = config_path {
        Some(ZombieBiteConfig::from_file(&path)?)
    } else {
        None
    };

    // Resolve relaychain (CLI always overrides config file)
    // Determine relay network: CLI > config > default
    let relay_network = if let Some(ref cli_relay) = relay {
        cli_relay.clone()
    } else if let Some(ref config) = config_file {
        config.relaychain.network.clone()
    } else {
        "polkadot".to_string()
    };

    let relaychain = if relay_runtime.is_some() || rc_sync_url.is_some() || relay_bite_at.is_some()
    {
        // CLI args provided, use them
        Relaychain::new_with_values(&relay_network, relay_runtime, rc_sync_url, relay_bite_at)
    } else if let Some(ref config) = config_file {
        Relaychain::new_with_values(
            &relay_network,
            config.relaychain.runtime_override.clone(),
            config.relaychain.sync_url.clone(),
            config.relaychain.bite_at,
        )
    } else {
        Relaychain::new_with_values(&relay_network, relay_runtime, rc_sync_url, relay_bite_at)
    };

    // Resolve parachains (CLI overrides config file)
    let resolved_parachains = if let Some(cli_paras) = parachains {
        // CLI specified parachains
        cli_paras
            .iter()
            .filter_map(|p| match p.as_str() {
                "asset-hub" => Some(Parachain::AssetHub {
                    maybe_override: None,
                    maybe_bite_at: None,
                    maybe_rpc_endpoint: None,
                }),
                "coretime" => Some(Parachain::Coretime {
                    maybe_override: None,
                    maybe_bite_at: None,
                    maybe_rpc_endpoint: None,
                }),
                "people" => Some(Parachain::People {
                    maybe_override: None,
                    maybe_bite_at: None,
                    maybe_rpc_endpoint: None,
                }),
                "bridge-hub" => Some(Parachain::BridgeHub {
                    maybe_override: None,
                    maybe_bite_at: None,
                    maybe_rpc_endpoint: None,
                }),
                "collectives" => Some(Parachain::Collectives {
                    maybe_override: None,
                    maybe_bite_at: None,
                    maybe_rpc_endpoint: None,
                }),
                s if s.starts_with("custom:") => {
                    let parts: Vec<&str> = s.splitn(4, ':').collect();
                    if parts.len() != 4 {
                        panic!(
                            "Custom parachain format must be custom:<para_id>:<rpc_endpoint>:<chain_spec_path>, got: {}",
                            s
                        );
                    }
                    let para_id: u32 = parts[1]
                        .parse()
                        .unwrap_or_else(|_| panic!("Invalid para_id '{}' in custom parachain", parts[1]));
                    let rpc_endpoint = parts[2].to_string();
                    let chain_spec = parts[3].to_string();
                    let name = format!("custom-{}", para_id);
                    Some(Parachain::Custom {
                        para_id,
                        name,
                        chain_spec,
                        maybe_override: None,
                        maybe_bite_at: None,
                        maybe_rpc_endpoint: Some(rpc_endpoint),
                    })
                }
                unknown => {
                    warn!(
                        "⚠️  Warning: Unknown parachain '{}' will be ignored.
                     Valid options are: asset-hub, coretime, people, bridge-hub, collectives, custom:<para_id>:<rpc>:<chain_spec>",
                        unknown
                    );
                    None
                }
            })
            .collect()
    } else if let Some(ref config) = config_file {
        // Use config file parachains
        config.get_parachains().to_vec()
    } else {
        vec![]
    };

    // Resolve base_path (CLI overrides config file)
    let resolved_base_path = if base_path.is_some() {
        get_base_path(base_path)
    } else if let Some(ref config) = config_file {
        get_base_path(config.base_path.clone())
    } else {
        get_base_path(None)
    };

    // Resolve and_spawn (CLI overrides config file)
    let resolved_and_spawn = if and_spawn {
        true
    } else if let Some(ref config) = config_file {
        config.and_spawn.unwrap_or(false)
    } else {
        and_spawn
    };

    Ok(ResolvedBiteConfig {
        relaychain,
        parachains: resolved_parachains,
        base_path: resolved_base_path,
        and_spawn: resolved_and_spawn,
    })
}

pub fn resolve_spawn_config(
    config_path: Option<String>,
    base_path: Option<String>,
    with_monitor: bool,
) -> Result<ResolvedSpawnConfig, anyhow::Error> {
    // Load config file if provided
    let config_file = if let Some(path) = config_path {
        Some(ZombieBiteConfig::from_file(&path)?)
    } else {
        None
    };

    // Resolve base_path (CLI overrides config file)
    let resolved_base_path = if base_path.is_some() {
        get_base_path(base_path)
    } else if let Some(ref config) = config_file {
        get_base_path(config.base_path.clone())
    } else {
        get_base_path(None)
    };

    // Resolve with_monitor (CLI overrides config file)
    let resolved_with_monitor = if let Some(ref config) = config_file {
        config.with_monitor.unwrap_or(with_monitor)
    } else {
        with_monitor
    };

    Ok(ResolvedSpawnConfig {
        base_path: resolved_base_path,
        with_monitor: resolved_with_monitor,
    })
}
