//! Resolve the effective bite/spawn settings from a config file plus
//! overrides (typically the cli flags).

use anyhow::{anyhow, bail};
use std::{
    collections::BTreeSet,
    env,
    path::PathBuf,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{info, trace, warn};

use crate::config::{
    BiteOptions, CoresOverride, DoppelgangerSetup, Parachain, Relaychain, SpawnSetup, StackConfig,
    Upgrades, ZombieBiteConfig,
};
use crate::{
    cores,
    stack::{self, StackSource},
};

const KNOWN_RELAYS: [&str; 4] = ["polkadot", "kusama", "paseo", "westend"];

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

/// Values that take precedence over the config file when resolving a bite.
///
/// Every field mirrors a `bite` cli flag; `None`/`false`/empty means "not
/// set", so the config file value (or the default) is used.
#[derive(Debug, Clone, Default)]
pub struct BiteOverrides {
    /// Relay to bite: polkadot, kusama, paseo, westend or
    /// `custom%<name>%<rpc_endpoint>%<chain_spec_path>`.
    pub relay: Option<String>,
    /// Runtime to install on the forked relay.
    pub relay_runtime: Option<String>,
    /// Block height to bite the relay at.
    pub relay_bite_at: Option<u32>,
    /// Parachains to include (asset-hub, coretime, people, bridge-hub,
    /// collectives or `custom%<para_id>%<rpc>%<chain_spec>%[req_cores]`).
    pub parachains: Option<Vec<String>>,
    pub base_path: Option<String>,
    pub rc_sync_url: Option<String>,
    pub and_spawn: bool,
    /// Runtime to carry as an authorized upgrade for the relay.
    pub relay_upgrade: Option<String>,
    /// Authorized upgrades for parachains, as `<para_id>=<wasm_path>`.
    pub para_upgrade: Vec<String>,
    pub apply_upgrade: bool,
    pub keep_messaging_state: bool,
    /// Cores per parachain, as `<para_id>=<cores>`.
    pub para_cores: Vec<String>,
    /// Host to advertise the spawned nodes under as bootNodes.
    pub publish_bootnodes: Option<String>,
    /// `doppelganger` binary to sync the relay chain with.
    pub doppelganger: Option<String>,
    /// `doppelganger-parachain` binary to sync the parachains with.
    pub doppelganger_parachain: Option<String>,
    /// Trinity stack to bite (descriptor url, file or `polkadotapp://stack`
    /// link), instead of `relay` / `parachains`.
    pub stack: Option<String>,
    /// Operator key to check the stack descriptor's signature with.
    pub stack_key: Option<String>,
    /// Roles of the stack's chains to bite; all of them when unset.
    pub stack_chains: Option<Vec<String>>,
}

/// Values that take precedence over the config file when resolving a spawn.
#[derive(Debug, Clone, Default)]
pub struct SpawnOverrides {
    pub base_path: Option<String>,
    pub with_monitor: bool,
    pub apply_upgrade: bool,
    /// Host to advertise the spawned nodes under as bootNodes.
    pub publish_bootnodes: Option<String>,
}

#[derive(Debug)]
pub struct ResolvedBiteConfig {
    pub relaychain: Relaychain,
    pub parachains: Vec<Parachain>,
    pub base_path: PathBuf,
    pub and_spawn: bool,
    pub apply_upgrade: bool,
    pub spawn_setup: SpawnSetup,
    pub publish_bootnodes: Option<String>,
    pub opts: BiteOptions,
    /// Para ids whose entry in `opts.cores` comes from `--para-cores` (the
    /// rest come from the config), to log where each count came from.
    pub cores_from_cli: BTreeSet<u32>,
}

impl ResolvedBiteConfig {
    /// Reject option combinations that can't be carried out.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if self.publish_bootnodes.is_some() && !self.and_spawn {
            bail!("--publish-bootnodes can only be used with --and-spawn");
        }
        if self.apply_upgrade && !self.and_spawn {
            bail!("--apply-upgrade can only be used with --and-spawn");
        }
        if self.apply_upgrade && self.opts.upgrades.is_empty() {
            bail!("--apply-upgrade needs an upgrade to carry (--rc-upgrade / --para-upgrade)");
        }

        if self.relaychain.is_custom() {
            if self.relaychain.chain_spec().is_none() {
                bail!("a custom relay needs a chain-spec: use -r custom%<name>%<rpc>%<chain_spec>");
            }
            if self.relaychain.sync_url().is_none() {
                bail!(
                    "a custom relay needs an rpc endpoint: use -r custom%<name>%<rpc>%<chain_spec>"
                );
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct ResolvedSpawnConfig {
    pub base_path: PathBuf,
    pub with_monitor: bool,
    pub apply_upgrade: bool,
    pub publish_bootnodes: Option<String>,
}

/// Resolve a bite from an optional config file plus overrides. A Trinity stack
/// needs network access to be read, use [`resolve_bite_config_with_stack`] for
/// it.
pub fn resolve_bite_config(
    config_path: Option<String>,
    overrides: BiteOverrides,
) -> Result<ResolvedBiteConfig, anyhow::Error> {
    let config_file = load_config(config_path)?;
    if overrides.stack.is_some() || config_file.as_ref().is_some_and(|c| c.stack.is_some()) {
        bail!("biting a stack needs `resolve_bite_config_with_stack`");
    }
    resolve_bite_config_from(config_file, overrides)
}

fn load_config(config_path: Option<String>) -> Result<Option<ZombieBiteConfig>, anyhow::Error> {
    config_path
        .map(|path| {
            ZombieBiteConfig::from_file(&path)
                .map_err(|e| anyhow!("can't read config file '{path}': {e}"))
        })
        .transpose()
}

/// The `[stack]` to bite, if any: the cli's `--stack` wins over the config
/// file's, and `--stack-key` / `--stack-chains` over its `key` / `chains`.
fn stack_to_bite(
    config_file: Option<&ZombieBiteConfig>,
    overrides: &BiteOverrides,
) -> Option<StackConfig> {
    let mut stack = match (&overrides.stack, config_file.and_then(|c| c.stack.clone())) {
        (Some(pointer), _) => StackConfig {
            pointer: pointer.clone(),
            ..Default::default()
        },
        (None, Some(stack)) => stack,
        (None, None) => return None,
    };
    if overrides.stack_key.is_some() {
        stack.key.clone_from(&overrides.stack_key);
    }
    if overrides.stack_chains.is_some() {
        stack.chains.clone_from(&overrides.stack_chains);
    }
    Some(stack)
}

/// Derive the bite config of a Trinity stack: read and check its descriptor,
/// download the chain-specs under the base path, merge the config file's
/// `[relaychain]` / `[[parachains]]` on top, and fill each para's core count
/// not set there (read from the relay, see [`crate::cores`]).
/// Returns `None` when no stack is set.
pub async fn derive_stack_config(
    config_path: Option<String>,
    overrides: &BiteOverrides,
) -> Result<Option<(ZombieBiteConfig, StackSource)>, anyhow::Error> {
    let Some((mut config, source)) =
        derive_stack_config_without_cores(config_path, overrides).await?
    else {
        return Ok(None);
    };

    let resolved = resolve_bite_config_from(
        Some(config.clone()),
        BiteOverrides {
            relay_bite_at: overrides.relay_bite_at,
            ..Default::default()
        },
    )?;
    let cores = cores::resolve_cores(
        &resolved.relaychain,
        &resolved.parachains,
        &resolved.opts.cores,
        &resolved.cores_from_cli,
    )
    .await?;
    fill_unset_cores(&mut config, &cores)?;
    Ok(Some((config, source)))
}

/// Record the resolved core counts in the `[[parachains]]` entries that set
/// none, so the derived config re-runs the same bite.
fn fill_unset_cores(
    config: &mut ZombieBiteConfig,
    cores: &CoresOverride,
) -> Result<(), anyhow::Error> {
    for para_cfg in config.parachains.iter_mut().flatten() {
        if para_cfg.cores.is_some() {
            continue;
        }
        if let Some(para) = para_cfg.to_parachain()? {
            para_cfg.cores = cores.get(&para.id()).copied();
        }
    }
    Ok(())
}

/// [`derive_stack_config`] leaving unset core counts unset, for the bite to
/// resolve along with every other para.
async fn derive_stack_config_without_cores(
    config_path: Option<String>,
    overrides: &BiteOverrides,
) -> Result<Option<(ZombieBiteConfig, StackSource)>, anyhow::Error> {
    let config_file = load_config(config_path)?;
    let Some(stack) = stack_to_bite(config_file.as_ref(), overrides) else {
        return Ok(None);
    };
    if overrides.relay.is_some()
        || overrides.parachains.is_some()
        || overrides.rc_sync_url.is_some()
    {
        bail!("a stack brings its own relay and parachains: --stack can't be combined with --rc, --parachains or --rc-sync-url");
    }

    let base_path = get_base_path(
        overrides
            .base_path
            .clone()
            .or_else(|| config_file.as_ref().and_then(|c| c.base_path.clone())),
    );
    let bite = stack::materialize(&stack, &base_path).await?;
    let mut merged = stack::merge_user_config(bite.config, config_file.as_ref())?;
    // the chain-specs were downloaded there
    merged.base_path = Some(base_path.to_string_lossy().to_string());
    Ok(Some((merged, bite.source)))
}

/// Resolve a bite like [`resolve_bite_config`], reading the Trinity stack set
/// with `--stack` (or the config file's `[stack]`) when there is one. The
/// derived config is written to `<base_path>/stack-config.toml`.
pub async fn resolve_bite_config_with_stack(
    config_path: Option<String>,
    overrides: BiteOverrides,
) -> Result<ResolvedBiteConfig, anyhow::Error> {
    let Some((mut config, source)) =
        derive_stack_config_without_cores(config_path.clone(), &overrides).await?
    else {
        let mut resolved = resolve_bite_config(config_path, overrides)?;
        resolve_cores(&mut resolved).await?;
        return Ok(resolved);
    };

    let mut resolved = resolve_bite_config_from(
        Some(config.clone()),
        BiteOverrides {
            stack: None,
            stack_key: None,
            stack_chains: None,
            ..overrides
        },
    )?;
    resolve_cores(&mut resolved).await?;

    fill_unset_cores(&mut config, &resolved.opts.cores)?;
    let base_path = PathBuf::from(
        config
            .base_path
            .clone()
            .expect("set by derive_stack_config"),
    );
    stack::write_config(&config, &base_path.join(stack::STACK_CONFIG_FILE)).await?;
    info!(
        "📝 config derived from stack '{}' written to {}",
        source.id,
        base_path.join(stack::STACK_CONFIG_FILE).display()
    );

    resolved.opts.stack = Some(source);
    Ok(resolved)
}

/// Fill `opts.cores` for every para (explicit counts, else the relay's live
/// assignment, else the built-in defaults) and refuse a bite the fork can't
/// back, before anything is synced.
async fn resolve_cores(resolved: &mut ResolvedBiteConfig) -> Result<(), anyhow::Error> {
    resolved.opts.cores = cores::resolve_cores(
        &resolved.relaychain,
        &resolved.parachains,
        &resolved.opts.cores,
        &resolved.cores_from_cli,
    )
    .await?;
    Ok(())
}

fn resolve_bite_config_from(
    config_file: Option<ZombieBiteConfig>,
    overrides: BiteOverrides,
) -> Result<ResolvedBiteConfig, anyhow::Error> {
    let BiteOverrides {
        relay,
        relay_runtime,
        relay_bite_at,
        parachains,
        base_path,
        rc_sync_url,
        and_spawn,
        relay_upgrade,
        para_upgrade,
        apply_upgrade,
        keep_messaging_state,
        para_cores,
        publish_bootnodes,
        doppelganger,
        doppelganger_parachain,
        stack: _,
        stack_key: _,
        stack_chains: _,
    } = overrides;

    // Resolve relaychain (CLI always overrides config file)
    // Determine relay network: CLI > config > default
    let relay_network = if let Some(ref cli_relay) = relay {
        cli_relay.clone()
    } else if let Some(ref config) = config_file {
        config.relaychain.network.clone()
    } else {
        "polkadot".to_string()
    };

    let custom_from_config = config_file
        .as_ref()
        .filter(|c| relay.is_none() && c.relaychain.chain_spec.is_some());
    let relaychain = if let Some(config) = custom_from_config {
        // A custom relay written in the config file (e.g. derived from a stack).
        let rc = &config.relaychain;
        let sync_url = rc_sync_url
            .clone()
            .or_else(|| rc.sync_url.clone())
            .ok_or_else(|| {
                anyhow!(
                    "[relaychain] '{}' has a chain_spec but no sync_url",
                    rc.network
                )
            })?;
        Relaychain::new_custom(
            &rc.network,
            rc.chain_spec.clone().expect("filtered above"),
            sync_url,
            relay_runtime
                .clone()
                .or_else(|| rc.runtime_override.clone()),
            relay_bite_at.or(rc.bite_at),
        )
    } else if relay_network.starts_with("custom%") {
        resolve_custom_relaychain(&relay_network, relay_runtime.clone(), relay_bite_at)?
    } else if !KNOWN_RELAYS.contains(&relay_network.as_str()) {
        // Anything else is a typo, not a chain to bite: a custom relay has to
        // come with its endpoint and chain-spec.
        bail!(
            "unknown relay '{relay_network}'; use one of {} or custom%<name>%<rpc_endpoint>%<chain_spec_path>",
            KNOWN_RELAYS.join(", ")
        );
    } else if relay_runtime.is_some() || rc_sync_url.is_some() || relay_bite_at.is_some() {
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
        let mut paras = vec![];
        for p in &cli_paras {
            let para = match p.as_str() {
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
                s if s.starts_with("custom%") => Some(resolve_custom_parachain(s)?),
                unknown => {
                    warn!(
                        "⚠️  Warning: Unknown parachain '{}' will be ignored.
                     Valid options are: asset-hub, coretime, people, bridge-hub, collectives, custom%<para_id>%<rpc>%<chain_spec>%[req_cores]",
                        unknown
                    );
                    None
                }
            };
            paras.extend(para);
        }
        paras
    } else if let Some(ref config) = config_file {
        // Use config file parachains
        config.get_parachains()?
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
            if let (Some(upgrade), Some(para)) = (&para_cfg.upgrade, para_cfg.to_parachain()?) {
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

    // `command` / `image` only come from the config file, there is no cli flag
    // for them. Validate here so a typo fails now and not after the sync.
    let spawn_setup = if let Some(ref config) = config_file {
        config.get_spawn_setup()
    } else {
        SpawnSetup::default()
    };
    spawn_setup.validate()?;

    // Doppelganger binaries: each cli flag wins over its config-file key.
    let mut doppelganger_setup = if let Some(ref config) = config_file {
        config.get_doppelganger_setup()
    } else {
        DoppelgangerSetup::default()
    };
    if doppelganger.is_some() {
        doppelganger_setup.relay = doppelganger;
    }
    if doppelganger_parachain.is_some() {
        doppelganger_setup.para = doppelganger_parachain;
    }
    doppelganger_setup.validate()?;
    // Per-para cores: CLI entries win over the config file's `cores`.
    let mut cores: CoresOverride = CoresOverride::new();
    let mut cores_from_cli = BTreeSet::new();
    if let Some(ref config) = config_file {
        for para_cfg in config.parachains.as_deref().unwrap_or_default() {
            if let (Some(c), Some(para)) = (para_cfg.cores, para_cfg.to_parachain()?) {
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
        cores_from_cli.insert(id);
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

    let resolved = ResolvedBiteConfig {
        relaychain,
        parachains: resolved_parachains,
        base_path: resolved_base_path,
        and_spawn: resolved_and_spawn,
        apply_upgrade: resolved_apply_upgrade,
        spawn_setup,
        publish_bootnodes: publish_bootnodes.or_else(|| {
            config_file
                .as_ref()
                .and_then(|c| c.publish_bootnodes.clone())
        }),
        opts: BiteOptions {
            upgrades,
            cores,
            keep_messaging_state: resolved_keep_messaging,
            doppelganger: doppelganger_setup,
            stack: None,
        },
        cores_from_cli,
    };
    resolved.validate()?;
    Ok(resolved)
}

pub fn resolve_spawn_config(
    config_path: Option<String>,
    overrides: SpawnOverrides,
) -> Result<ResolvedSpawnConfig, anyhow::Error> {
    let SpawnOverrides {
        base_path,
        with_monitor,
        apply_upgrade,
        publish_bootnodes,
    } = overrides;
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
        publish_bootnodes: publish_bootnodes.or_else(|| {
            config_file
                .as_ref()
                .and_then(|c| c.publish_bootnodes.clone())
        }),
    })
}

/// custom%<name>%<rpc_endpoint>%<chain_spec_path>
fn resolve_custom_relaychain(
    s: &str,
    maybe_override: Option<String>,
    maybe_bite_at: Option<u32>,
) -> Result<Relaychain, anyhow::Error> {
    let parts: Vec<&str> = s.splitn(4, '%').collect();
    if parts.len() != 4 {
        bail!("custom relay must be custom%<name>%<rpc_endpoint>%<chain_spec_path>, got '{s}'");
    }
    let (name, rpc, chain_spec) = (parts[1], parts[2], parts[3]);
    if name.is_empty() || rpc.is_empty() || chain_spec.is_empty() {
        bail!("custom relay needs a name, an rpc endpoint and a chain-spec path, got '{s}'");
    }
    Ok(Relaychain::new_custom(
        name,
        chain_spec,
        rpc,
        maybe_override,
        maybe_bite_at,
    ))
}

fn resolve_custom_parachain(s: &str) -> Result<Parachain, anyhow::Error> {
    let parts: Vec<&str> = s.splitn(5, '%').collect();
    trace!("custom parts: {parts:?}");
    if parts.len() < 4 || parts.len() > 5 {
        bail!(
            "Custom parachain format must be custom%<para_id>%<rpc_endpoint>%<chain_spec_path>%[req_cores], got: {}",
            s
        );
    }
    let para_id: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid para_id '{}' in custom parachain", parts[1]))?;
    let rpc_endpoint = parts[2].to_string();
    let chain_spec = parts[3].to_string();
    let name = format!("custom-{}", para_id);
    // Without `req_cores` the count is read from the relay.
    let req_cores = match parts.get(4) {
        Some(c) => Some(
            c.parse()
                .map_err(|_| anyhow!("Invalid req_cores '{}' in custom parachain", c))?,
        ),
        None => None,
    };
    Ok(Parachain::Custom {
        id: para_id,
        name,
        chain_spec,
        maybe_override: None,
        maybe_bite_at: None,
        maybe_rpc_endpoint: Some(rpc_endpoint),
        cores: req_cores,
        role: None,
    })
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn custom_para_works() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com%/path/to/chain-spec.json";
        let para = resolve_custom_parachain(s).unwrap();
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
        let para = resolve_custom_parachain(s).unwrap();
        assert_eq!(
            para.rpc_endpoint(),
            Some("wss://kusama-yap-3392.example.com:1234"),
            "rpc should match"
        );
    }

    #[test]
    fn custom_para_works_with_cores() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%3";
        let para = resolve_custom_parachain(s).unwrap();
        assert_eq!(para.req_cores(), Some(3), "cores should match");
    }

    #[test]
    fn custom_para_works_with_default_cores() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json";
        let para = resolve_custom_parachain(s).unwrap();
        assert_eq!(
            para.req_cores(),
            None,
            "cores should be read from the relay"
        );
    }

    #[test]
    fn custom_para_id_parse_err() {
        let s = "custom%abc3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%3";
        let err = resolve_custom_parachain(s).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Invalid para_id 'abc3392' in custom parachain"
        );
    }

    #[test]
    fn custom_para_cores_parse_err() {
        let s = "custom%3392%wss://kusama-yap-3392.example.com:1234%/path/to/chain-spec.json%abc";
        let err = resolve_custom_parachain(s).unwrap_err();
        assert_eq!(
            err.to_string(),
            "Invalid req_cores 'abc' in custom parachain"
        );
    }
    #[test]
    fn custom_relay_works() {
        let rc = resolve_custom_relaychain(
            "custom%previewnet%wss://previewnet.example.com%/path/to/previewnet.json",
            None,
            Some(42),
        )
        .unwrap();

        assert_eq!(rc.as_chain_string(), "previewnet");
        assert_eq!(rc.chain_spec(), Some("/path/to/previewnet.json"));
        // a custom relay is passed to the node as a spec path, not a name
        assert_eq!(rc.chain_arg(), "/path/to/previewnet.json");
        assert_eq!(rc.rpc_endpoint(), "wss://previewnet.example.com");
        assert_eq!(rc.sync_endpoint(), "wss://previewnet.example.com");
        assert_eq!(rc.at_block(), Some(42));
        assert!(rc.is_custom());
    }

    #[test]
    fn custom_relay_needs_every_part() {
        for bad in [
            "custom%previewnet%wss://previewnet.example.com",
            "custom%previewnet%%/path/to/spec.json",
            "custom%%wss://x%/path/to/spec.json",
        ] {
            assert!(
                resolve_custom_relaychain(bad, None, None).is_err(),
                "should reject '{bad}'"
            );
        }
    }

    #[test]
    fn unknown_relay_name_keeps_its_name() {
        // helper subcommands only get the name back as a string, and the
        // artifacts are named after it
        let rc = Relaychain::new("previewnet");
        assert_eq!(rc.as_chain_string(), "previewnet");
        assert!(rc.is_custom());
    }

    fn write_config(name: &str, contents: &str) -> String {
        let path = std::env::temp_dir().join(format!("zb-{name}-{}.toml", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn doppelganger_defaults_to_path_lookup() {
        let resolved = resolve_bite_config(
            None,
            BiteOverrides {
                base_path: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(resolved.opts.doppelganger, DoppelgangerSetup::default());
    }

    #[test]
    fn doppelganger_flags_win_over_the_config_file() {
        // Bare names, so validation has no file to look for.
        let config = write_config(
            "doppelganger-precedence",
            r#"
            [relaychain]
            network = "polkadot"

            [doppelganger]
            relay = "doppelganger-from-config"
            parachain = "doppelganger-parachain-from-config"
            "#,
        );

        let from_config = resolve_bite_config(
            Some(config.clone()),
            BiteOverrides {
                base_path: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            from_config.opts.doppelganger.relay_command(),
            "doppelganger-from-config"
        );
        assert_eq!(
            from_config.opts.doppelganger.para_command(),
            "doppelganger-parachain-from-config"
        );

        // Each flag overrides its own key only.
        let with_flag = resolve_bite_config(
            Some(config.clone()),
            BiteOverrides {
                base_path: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                doppelganger: Some("doppelganger-from-flag".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            with_flag.opts.doppelganger.relay_command(),
            "doppelganger-from-flag"
        );
        assert_eq!(
            with_flag.opts.doppelganger.para_command(),
            "doppelganger-parachain-from-config"
        );

        std::fs::remove_file(config).unwrap();
    }

    #[test]
    fn a_doppelganger_path_that_does_not_exist_fails_resolution() {
        let err = resolve_bite_config(
            None,
            BiteOverrides {
                base_path: Some(std::env::temp_dir().to_string_lossy().into_owned()),
                doppelganger_parachain: Some("/definitely/not/here".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("for parachain"), "{err}");
    }

    /// A config derived from a stack resolves to a custom relay started from
    /// the stack's chain-spec, and custom paras started from theirs.
    #[test]
    fn stack_derived_config_uses_the_stack_specs() {
        let config: ZombieBiteConfig = toml::from_str(
            r#"
            [relaychain]
            network = "previewnet"
            sync_url = "wss://previewnet.substrate.dev/relay/alice"
            chain_spec = "/b/stack/previewnet-relay.json"

            [[parachains]]
            type = "custom"
            id = 1500
            role = "AssetHub"
            rpc_endpoint = "wss://previewnet.substrate.dev/asset-hub"
            chain_spec = "/b/stack/previewnet-assethub.json"
            cores = 3
            "#,
        )
        .unwrap();
        let resolved = resolve_bite_config_from(
            Some(config),
            BiteOverrides {
                base_path: Some("/tmp/zb-stack-test".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let relay = &resolved.relaychain;
        assert!(relay.is_custom());
        assert_eq!(relay.as_chain_string(), "previewnet");
        // relay sync node and the relay side of the para sync nodes
        assert_eq!(relay.chain_arg(), "/b/stack/previewnet-relay.json");
        assert_eq!(
            relay.sync_endpoint(),
            "wss://previewnet.substrate.dev/relay/alice"
        );

        let ah = &resolved.parachains[0];
        assert_eq!(ah.id(), 1500);
        assert!(ah.is_asset_hub());
        assert_eq!(
            ah.chain_spec_path(),
            Some("/b/stack/previewnet-assethub.json")
        );
        assert_eq!(resolved.opts.cores.get(&1500), Some(&3));
    }
}
