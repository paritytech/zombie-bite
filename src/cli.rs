use anyhow::{anyhow, bail};
use clap::{Parser, Subcommand};
use std::{
    env,
    path::PathBuf,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{trace, warn};

use crate::config::{
    BiteOptions, CoresOverride, Parachain, Relaychain, Upgrades, ZombieBiteConfig,
};

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
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusama", "paseo", "westend"]))]
        relay: Option<String>,
        /// If provided we will override the runtime as part of the process of 'bite'
        /// The resulting network will be running with this runtime.
        #[arg(long = "rc-override", verbatim_doc_comment)]
        relay_runtime: Option<String>,
        /// Runtime to carry as an *authorized* upgrade for the relay chain.
        /// Unlike --rc-override it is NOT installed: the fork spawns on the live
        /// runtime with System::AuthorizedUpgrade seeded, so the upgrade can be
        /// enacted through the production path (apply_authorized_upgrade is
        /// permissionless), either manually or with --apply-upgrade.
        #[arg(long = "rc-upgrade", verbatim_doc_comment)]
        relay_upgrade: Option<String>,
        /// Same as --rc-upgrade but for a parachain, format: <para_id>=<wasm_path>
        /// Can be set multiple times, once per para to upgrade.
        #[arg(long = "para-upgrade", verbatim_doc_comment)]
        para_upgrade: Vec<String>,
        /// After spawn (requires --and-spawn), submit apply_authorized_upgrade
        /// for every carried upgrade and wait until it enacts.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        apply_upgrade: bool,
        /// Keep the inherited HRMP/DMP state instead of clearing it. Only correct
        /// when the relay's parachains are exactly the ones being bitten, so the
        /// two snapshots agree on channel heads.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        keep_messaging_state: bool,
        /// Override the cores assigned to a parachain, format: <para_id>=<cores>
        /// Can be set multiple times, once per para.
        #[arg(long = "para-cores", verbatim_doc_comment)]
        para_cores: Vec<String>,
        /// If provided we will _bite_ the live network at the supplied block hieght
        #[arg(long = "rc-bite-at", verbatim_doc_comment)]
        relay_bite_at: Option<u32>,
        /// Parachains to include: asset-hub, coretime, people, bridge-hub, collectives (comma-separated)
        /// For custom parachains use: custom%<para_id>%<rpc_endpoint>%<chain_spec_path>%[req_cores]
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
        /// Submit apply_authorized_upgrade for every upgrade carried by the bite
        /// and wait until it enacts.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        apply_upgrade: bool,
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
    pub apply_upgrade: bool,
    pub opts: BiteOptions,
}

#[derive(Debug)]
pub struct ResolvedSpawnConfig {
    pub base_path: PathBuf,
    pub with_monitor: bool,
    pub apply_upgrade: bool,
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
    relay_upgrade: Option<String>,
    para_upgrade: Vec<String>,
    apply_upgrade: bool,
    keep_messaging_state: bool,
    para_cores: Vec<String>,
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
                s if s.starts_with("custom%") => {
                    Some(resolve_custom_parachain(s))
                }
                unknown => {
                    warn!(
                        "⚠️  Warning: Unknown parachain '{}' will be ignored.
                     Valid options are: asset-hub, coretime, people, bridge-hub, collectives, custom%<para_id>%<rpc>%<chain_spec>%[req_cores]",
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

    // Resolve upgrades (CLI overrides config file)
    let mut para_upgrades = std::collections::HashMap::new();
    for entry in &para_upgrade {
        let (id, path) = entry.split_once('=').ok_or_else(|| {
            anyhow!("--para-upgrade must be <para_id>=<wasm_path>, got '{entry}'")
        })?;
        let id: u32 = id
            .parse()
            .map_err(|_| anyhow!("invalid para_id '{id}' in --para-upgrade"))?;
        para_upgrades.insert(id, path.to_string());
    }
    let mut upgrades = Upgrades {
        relay: relay_upgrade,
        paras: para_upgrades,
    };
    if let Some(ref config) = config_file {
        if upgrades.relay.is_none() {
            upgrades.relay = config.relaychain.upgrade.clone();
        }
        for para_cfg in config.parachains.as_deref().unwrap_or_default() {
            if let (Some(upgrade), Some(para)) = (&para_cfg.upgrade, para_cfg.to_parachain()) {
                upgrades.paras.entry(para.id()).or_insert(upgrade.clone());
            }
        }
    }

    let resolved_apply_upgrade = if apply_upgrade {
        true
    } else if let Some(ref config) = config_file {
        config.apply_upgrade.unwrap_or(false)
    } else {
        false
    };

    // Per-para cores: CLI entries win over the config file's `cores`.
    let mut cores: CoresOverride = CoresOverride::new();
    if let Some(ref config) = config_file {
        for para_cfg in config.parachains.as_deref().unwrap_or_default() {
            if let (Some(c), Some(para)) = (para_cfg.cores, para_cfg.to_parachain()) {
                cores.insert(para.id(), c);
            }
        }
    }
    for entry in &para_cores {
        let (id, c) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("--para-cores must be <para_id>=<cores>, got '{entry}'"))?;
        let id: u32 = id
            .parse()
            .map_err(|_| anyhow!("invalid para_id '{id}' in --para-cores"))?;
        let c: u32 = c
            .parse()
            .map_err(|_| anyhow!("invalid cores '{c}' in --para-cores"))?;
        if c == 0 {
            bail!("--para-cores {id}=0: a parachain with no cores can't have blocks backed");
        }
        cores.insert(id, c);
    }
    // A core count for a para that is not part of the bite is a typo, not a
    // silently ignorable no-op.
    for id in cores.keys() {
        if !resolved_parachains.iter().any(|para| para.id() == *id) {
            bail!("--para-cores/config sets cores for para {id}, which is not part of this bite");
        }
    }

    let resolved_keep_messaging = if keep_messaging_state {
        true
    } else if let Some(ref config) = config_file {
        config.keep_messaging_state.unwrap_or(false)
    } else {
        false
    };

    Ok(ResolvedBiteConfig {
        relaychain,
        parachains: resolved_parachains,
        base_path: resolved_base_path,
        and_spawn: resolved_and_spawn,
        apply_upgrade: resolved_apply_upgrade,
        opts: BiteOptions {
            upgrades,
            cores,
            keep_messaging_state: resolved_keep_messaging,
        },
    })
}

pub fn resolve_spawn_config(
    config_path: Option<String>,
    base_path: Option<String>,
    with_monitor: bool,
    apply_upgrade: bool,
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

    let resolved_apply_upgrade = if apply_upgrade {
        true
    } else if let Some(ref config) = config_file {
        config.apply_upgrade.unwrap_or(false)
    } else {
        false
    };

    Ok(ResolvedSpawnConfig {
        base_path: resolved_base_path,
        with_monitor: resolved_with_monitor,
        apply_upgrade: resolved_apply_upgrade,
    })
}

fn resolve_custom_parachain(s: &str) -> Parachain {
    let parts: Vec<&str> = s.splitn(5, '%').collect();
    trace!("custom parts: {parts:?}");
    if parts.len() < 4 || parts.len() > 5 {
        panic!(
            "Custom parachain format must be custom%<para_id>%<rpc_endpoint>%<chain_spec_path>%[req_cores], got: {}",
            s
        );
    }
    let para_id: u32 = parts[1]
        .parse()
        .unwrap_or_else(|_| panic!("Invalid para_id '{}' in custom parachain", parts[1]));
    let rpc_endpoint = parts[2].to_string();
    let chain_spec = parts[3].to_string();
    let name = format!("custom-{}", para_id);
    let req_cores = if parts.len() == 5 {
        parts[4]
            .parse()
            .unwrap_or_else(|_| panic!("Invalid req_cores '{}' in custom parachain", parts[4]))
    } else {
        // default to 1 core
        1
    };
    Parachain::Custom {
        id: para_id,
        name,
        chain_spec,
        maybe_override: None,
        maybe_bite_at: None,
        maybe_rpc_endpoint: Some(rpc_endpoint),
        cores: req_cores,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn custom_para_works() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com%/path/to/chain-spec.json";
        let para = resolve_custom_parachain(s);
        assert_eq!(para.id(), 3392, "para id should be valid");
        assert_eq!(
            para.rpc_endpoint(),
            Some("wss://kusama-yap-3392.example.com"),
            "rpc should match"
        );
        assert_eq!(
            para.chain_spec(),
            Some("/path/to/chain-spec.json"),
            "chain-spec should match"
        );
    }

    #[test]
    fn custom_para_works_with_port_number() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json";
        let para = resolve_custom_parachain(s);
        assert_eq!(
            para.rpc_endpoint(),
            Some("wss://kusama-yap-3392.example.com:1234"),
            "rpc should match"
        );
    }

    #[test]
    fn custom_para_works_with_cores() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%3";
        let para = resolve_custom_parachain(s);
        assert_eq!(para.req_cores(), Some(3), "cores should match");
    }

    #[test]
    fn custom_para_works_with_default_cores() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json";
        let para = resolve_custom_parachain(s);
        assert_eq!(para.req_cores(), Some(1), "cores should match");
    }

    #[test]
    #[should_panic(expected = "Invalid para_id 'abc3392' in custom parachain")]
    fn custom_para_id_parse_err() {
        let s = "custom%abc3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%3";
        let _para = resolve_custom_parachain(s);
    }

    #[test]
    #[should_panic(expected = "Invalid req_cores 'abc' in custom parachain")]
    fn custom_para_cores_parse_err() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%abc";
        let _para = resolve_custom_parachain(s);
    }
}
