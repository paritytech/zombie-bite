#![allow(dead_code)]
// TODO: don't allow dead_code

use serde::{Deserialize, Serialize};
// use zombienet_orchestrator::generators::chain_spec;
use std::env;

use zombienet_configuration::{NetworkConfig, NetworkConfigBuilder};
const BITE: &str = "bite";
const SPAWN: &str = "spawn";
const POST: &str = "post";
const AFTER: &str = "after";
const DEBUG: &str = "debug";

// `--state-pruning` config flag (two days +1 by default)
pub const STATE_PRUNING: &str = "256";
pub fn get_state_pruning_config() -> String {
    env::var("ZOMBIE_BITE_STATE_PRUNING").unwrap_or_else(|_| STATE_PRUNING.to_string())
}

pub const AH_POLKADOT_RCP: &str = "https://asset-hub-polkadot-rpc.n.dwellir.com";
pub const AH_KUSAMA_RCP: &str = "https://asset-hub-kusama-rpc.n.dwellir.com";

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Step {
    /// Initial step
    Bite,
    /// Spawn from `bite` directory
    Spawn,
    /// Spawn from `spawn` directory
    Post,
    /// Spawn from `post` directory
    After,
}

impl Step {
    pub fn dir(&self) -> String {
        match self {
            Step::Bite => String::from(BITE),
            Step::Spawn => String::from(SPAWN),
            Step::Post => String::from(POST),
            Step::After => String::from(AFTER),
        }
    }

    pub fn dir_debug(&self) -> String {
        match self {
            Step::Bite => format!("{BITE}-{DEBUG}"),
            Step::Spawn => format!("{SPAWN}-{DEBUG}"),
            Step::Post => format!("{POST}-{DEBUG}"),
            Step::After => String::from("{AFTER}-{DEBUG}"),
        }
    }

    pub fn dir_from(&self) -> String {
        match self {
            Step::Bite => String::from(""), // emtpy since is initial step
            Step::Spawn => String::from(BITE),
            Step::Post => String::from(SPAWN),
            Step::After => String::from(POST),
        }
    }

    pub fn next(&self) -> Option<String> {
        match self {
            Step::Bite => Some(String::from(SPAWN)),
            Step::Spawn => Some(String::from(POST)),
            Step::Post => Some(String::from(AFTER)),
            Step::After => None, // emtpy since is the last step
        }
    }
}

impl From<String> for Step {
    fn from(value: String) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "post" => Step::Post,
            "spawn" => Step::Spawn,
            "after" => Step::After,
            _ => Step::Bite,
        }
    }
}
#[derive(Debug, PartialEq)]
pub enum BiteMethod {
    DoppelGanger,
    Fork,
}

impl<T> From<T> for BiteMethod
where
    T: AsRef<str>,
{
    fn from(s: T) -> Self {
        if s.as_ref() == "fork-off" {
            BiteMethod::Fork
        } else {
            BiteMethod::DoppelGanger
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Context {
    Relaychain,
    Parachain,
}

impl Context {
    pub fn cmd(&self) -> String {
        String::from(if *self == Context::Relaychain {
            "polkadot"
        } else {
            "polkadot-parachain"
        })
    }

    pub fn doppelganger_cmd(&self) -> String {
        String::from(if *self == Context::Relaychain {
            "doppelganger"
        } else {
            "doppelganger-parachain"
        })
    }
}

type MaybeWasmOverridePath = Option<String>;
type MaybeSyncUrl = Option<String>;
type MaybeByteAt = Option<u32>;
type MaybeChainSpec = Option<String>;

// Get the current core assignment (data from May 2026)
// polkadot
// AH : 3
// coretime: 1
// people: 3
// bridge: 1
// collectives: 1

// Westend
// AH (1000): 3
// coretime (1005): 1
// people (1004): 3
// bridge (1002): 1
// collectives (1001): 1

// Kusama
// AH (1000): 3
// coretim (1005): 1
// people (1004): 1
// bridge (1002): 1
// collectives (1001): 1

// Paseo
// AH (1000): 3
// coretime (1005): 1
// people (1004): 1
// bridge (1002): 1
// collectives (1001): 1

/// Dev validators for a bitten relay: one per requested core plus one spare,
/// clamped between the two nodes the spawner always starts (alice, bob) and
/// the seven well-known dev accounts. Used by both the state overrides and the
/// spawner so the validator set in state always matches the nodes running.
// TODO: the upper bound is only there because we reuse the well-known dev
// accounts; generating keys would let a fork scale past 7 validators.
pub fn num_validators_for_cores(req_cores: u32) -> u32 {
    (1 + req_cores).clamp(2, 7)
}

/// Runtimes under test, carried into the fork as an *authorized* upgrade
/// (`System::AuthorizedUpgrade` seeded at bite time) instead of installed,
/// so the upgrade can be enacted through the production path via the
/// permissionless `apply_authorized_upgrade`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Upgrades {
    pub relay: Option<String>,
    pub paras: std::collections::HashMap<u32, String>,
}

impl Upgrades {
    pub fn is_empty(&self) -> bool {
        self.relay.is_none() && self.paras.is_empty()
    }
}

/// Per-parachain core counts from configuration, keyed by para id. Overrides
/// the built-in defaults below, which mirror the live networks.
pub type CoresOverride = std::collections::HashMap<u32, u32>;

/// Everything a bite needs beyond the chains themselves.
#[derive(Debug, Default, Clone)]
pub struct BiteOptions {
    pub upgrades: Upgrades,
    pub cores: CoresOverride,
    pub keep_messaging_state: bool,
}

pub fn get_assigned_cores(relay: &Relaychain, para: &Parachain, override_: &CoresOverride) -> u32 {
    if let Some(cores) = override_.get(&para.id()) {
        return *cores;
    }
    match para {
        Parachain::AssetHub { .. } => 3,
        Parachain::People { .. } => match relay {
            Relaychain::Polkadot { .. } | Relaychain::Westend { .. } => 3,
            _ => 1,
        },
        Parachain::Custom { cores, .. } => *cores,
        _ => 1,
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum Relaychain {
    Polkadot {
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    },
    Kusama {
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    },

    Paseo {
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    },
    Westend {
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    },
    /// A relay chain that is not one of the public networks: its name is used
    /// for the artifact file names, and the chain-spec and endpoint have to be
    /// supplied because there is nothing to look them up from.
    Custom {
        name: String,
        chain_spec: MaybeChainSpec,
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    },
}

impl Relaychain {
    pub fn new(network: impl AsRef<str>) -> Self {
        match network.as_ref() {
            "kusama" => Self::Kusama {
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
            "paseo" => Self::Paseo {
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
            "westend" => Self::Westend {
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
            "polkadot" => Self::Polkadot {
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
            // Keeps a custom relay's artifact names working in the helper
            // subcommands, which only get the name back as a string.
            other => Self::Custom {
                name: other.to_string(),
                chain_spec: None,
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
        }
    }

    pub fn new_custom(
        name: impl Into<String>,
        chain_spec: impl Into<String>,
        rpc: impl Into<String>,
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
    ) -> Self {
        Self::Custom {
            name: name.into(),
            chain_spec: Some(chain_spec.into()),
            maybe_override,
            maybe_sync_url: Some(rpc.into()),
            maybe_bite_at,
        }
    }

    pub fn new_with_values(
        network: impl AsRef<str>,
        maybe_override: MaybeWasmOverridePath,
        maybe_sync_url: MaybeSyncUrl,
        maybe_bite_at: MaybeByteAt,
    ) -> Self {
        match network.as_ref() {
            "kusama" => Self::Kusama {
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
            "paseo" => Self::Paseo {
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
            "westend" => Self::Westend {
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
            "polkadot" => Self::Polkadot {
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
            other => Self::Custom {
                name: other.to_string(),
                chain_spec: None,
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
        }
    }

    pub fn as_local_chain_string(&self) -> String {
        format!("{}-local", self.as_chain_string())
    }

    pub fn as_chain_string(&self) -> String {
        String::from(match self {
            Relaychain::Polkadot { .. } => "polkadot",
            Relaychain::Kusama { .. } => "kusama",
            Relaychain::Paseo { .. } => "paseo",
            Relaychain::Westend { .. } => "westend",
            Relaychain::Custom { name, .. } => name,
        })
    }

    /// Chain-spec of a custom relay; the public networks are known to the node
    /// by name.
    pub fn chain_spec(&self) -> Option<&str> {
        match self {
            Relaychain::Custom { chain_spec, .. } => chain_spec.as_deref(),
            _ => None,
        }
    }

    /// Value for the node's `--chain`: a spec path for a custom relay, the
    /// network name otherwise.
    pub fn chain_arg(&self) -> String {
        self.chain_spec()
            .map(str::to_string)
            .unwrap_or_else(|| self.as_chain_string())
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Relaychain::Custom { .. })
    }

    /// Endpoint supplied with `--rc-sync-url` / the config's `sync_url`, used
    /// instead of the public default. Both the parachain sync and the reads the
    /// bite does against the source go through it: the reason to pass it is that
    /// the public endpoint is unusable (rate limited, down, or not reachable
    /// from where the bite runs).
    pub fn sync_url(&self) -> Option<&str> {
        match self {
            Relaychain::Polkadot { maybe_sync_url, .. }
            | Relaychain::Kusama { maybe_sync_url, .. }
            | Relaychain::Paseo { maybe_sync_url, .. }
            | Relaychain::Westend { maybe_sync_url, .. }
            | Relaychain::Custom { maybe_sync_url, .. } => maybe_sync_url.as_deref(),
        }
    }

    fn default_endpoint(&self) -> &'static str {
        match self {
            Relaychain::Polkadot { .. } => "wss://rpc.polkadot.io",
            Relaychain::Kusama { .. } => "wss://kusama-rpc.polkadot.io",
            Relaychain::Paseo { .. } => "wss://paseo-rpc.dwellir.com",
            Relaychain::Westend { .. } => "wss://westend-rpc.n.dwellir.com",
            // A custom relay has no public endpoint to fall back to.
            Relaychain::Custom { .. } => "",
        }
    }

    pub fn sync_endpoint(&self) -> String {
        self.sync_url()
            .unwrap_or_else(|| self.default_endpoint())
            .to_string()
    }

    pub fn rpc_endpoint(&self) -> String {
        self.sync_url()
            .unwrap_or_else(|| self.default_endpoint())
            .to_string()
    }

    pub fn context(&self) -> Context {
        Context::Relaychain
    }

    pub fn wasm_overrides(&self) -> Option<&str> {
        match self {
            Relaychain::Kusama { maybe_override, .. }
            | Relaychain::Polkadot { maybe_override, .. }
            | Relaychain::Westend { maybe_override, .. }
            | Relaychain::Paseo { maybe_override, .. }
            | Relaychain::Custom { maybe_override, .. } => maybe_override.as_deref(),
        }
    }

    pub fn epoch_duration(&self) -> u64 {
        match self {
            Relaychain::Paseo { .. } => 600,
            Relaychain::Kusama { .. } => 600,
            Relaychain::Westend { .. } => 600,
            // TODO: read it from the chain instead of assuming a testnet-sized
            // epoch for a custom relay.
            Relaychain::Custom { .. } => 600,
            _ => 2400,
        }
    }

    pub fn at_block(&self) -> Option<u32> {
        match self {
            Relaychain::Kusama { maybe_bite_at, .. }
            | Relaychain::Polkadot { maybe_bite_at, .. }
            | Relaychain::Westend { maybe_bite_at, .. }
            | Relaychain::Paseo { maybe_bite_at, .. }
            | Relaychain::Custom { maybe_bite_at, .. } => *maybe_bite_at,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum Parachain {
    AssetHub {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
    },
    Coretime {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
    },
    People {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
    },
    BridgeHub {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
    },
    Collectives {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
    },
    Custom {
        maybe_override: MaybeWasmOverridePath,
        maybe_bite_at: MaybeByteAt,
        maybe_rpc_endpoint: MaybeSyncUrl,
        chain_spec: String,
        id: u32,
        cores: u32,
        name: String,
    },
}

impl Parachain {
    pub fn new(chain: &str) -> Self {
        match chain {
            "coretime" => Parachain::Coretime {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            "people" => Parachain::People {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            "collectives" => Parachain::Collectives {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            "asset-hub" => Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            // custom parachain
            _ => Parachain::Custom {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
                name: "custom".to_string(), // placeholder
                chain_spec: chain.to_string(),
                id: 2000, // placeholder
                cores: 1, // placeholder
            },
        }
    }

    pub fn as_local_chain_string(&self, relay_part: &str) -> String {
        let para_part = match self {
            Parachain::AssetHub { .. } => "asset-hub",
            Parachain::Coretime { .. } => "coretime",
            Parachain::People { .. } => "people",
            Parachain::BridgeHub { .. } => "bridge-hub",
            Parachain::Collectives { .. } => "collectives",
            Parachain::Custom { id, .. } => &id.to_string(),
        };

        format!("{para_part}-{relay_part}-local")
    }

    pub fn as_chain_string(&self, relay_part: &str) -> String {
        let para_part = match self {
            Parachain::AssetHub { .. } => "asset-hub",
            Parachain::Coretime { .. } => "coretime",
            Parachain::People { .. } => "people",
            Parachain::BridgeHub { .. } => "bridge-hub",
            Parachain::Collectives { .. } => "collectives",
            Parachain::Custom { id, .. } => &id.to_string(),
        };

        format!("{para_part}-{relay_part}")
    }

    pub fn context(&self) -> Context {
        Context::Parachain
    }

    pub fn id(&self) -> u32 {
        match self {
            Parachain::AssetHub { .. } => 1000,
            Parachain::Coretime { .. } => 1005,
            Parachain::People { .. } => 1004,
            Parachain::BridgeHub { .. } => 1002,
            Parachain::Collectives { .. } => 1001,
            Parachain::Custom { id, .. } => *id,
        }
    }

    pub fn wasm_overrides(&self) -> Option<&str> {
        match self {
            Parachain::AssetHub { maybe_override, .. }
            | Parachain::Coretime { maybe_override, .. }
            | Parachain::People { maybe_override, .. }
            | Parachain::BridgeHub { maybe_override, .. }
            | Parachain::Collectives { maybe_override, .. }
            | Parachain::Custom { maybe_override, .. } => maybe_override.as_deref(),
        }
    }

    pub fn at_block(&self) -> Option<u32> {
        match self {
            Parachain::AssetHub { maybe_bite_at, .. }
            | Parachain::Coretime { maybe_bite_at, .. }
            | Parachain::People { maybe_bite_at, .. }
            | Parachain::BridgeHub { maybe_bite_at, .. }
            | Parachain::Collectives { maybe_bite_at, .. }
            | Parachain::Custom { maybe_bite_at, .. } => *maybe_bite_at,
        }
    }

    pub fn chain_spec(&self) -> Option<&str> {
        match self {
            Parachain::Custom { chain_spec, .. } => Some(chain_spec),
            _ => None,
        }
    }

    pub fn req_cores(&self) -> Option<u32> {
        match self {
            Parachain::Custom { cores, .. } => Some(*cores),
            _ => None,
        }
    }

    pub fn rpc_endpoint(&self) -> Option<&str> {
        match self {
            Parachain::AssetHub {
                maybe_rpc_endpoint, ..
            }
            | Parachain::Coretime {
                maybe_rpc_endpoint, ..
            }
            | Parachain::People {
                maybe_rpc_endpoint, ..
            }
            | Parachain::BridgeHub {
                maybe_rpc_endpoint, ..
            }
            | Parachain::Collectives {
                maybe_rpc_endpoint, ..
            }
            | Parachain::Custom {
                maybe_rpc_endpoint, ..
            } => maybe_rpc_endpoint.as_deref(),
        }
    }

    /// Endpoint used to read the parachain's metadata when none is configured,
    /// so overrides are checked against the runtime by default. A wrong or
    /// unreachable guess only costs the verification (with a warning), never the
    /// bite itself.
    // TODO: same as the relay endpoints, these should be configurable.
    pub fn default_rpc_endpoint(&self, relay: &Relaychain) -> Option<String> {
        let prefix = match self {
            Parachain::AssetHub { .. } => "asset-hub",
            Parachain::Coretime { .. } => "coretime",
            Parachain::People { .. } => "people",
            Parachain::BridgeHub { .. } => "bridge-hub",
            Parachain::Collectives { .. } => "collectives",
            // A custom para is only reachable through the endpoint its config
            // supplies.
            Parachain::Custom { .. } => return None,
        };
        Some(format!(
            "wss://{prefix}-{}-rpc.n.dwellir.com",
            relay.as_chain_string()
        ))
    }

    pub fn chain_spec_path(&self) -> Option<&str> {
        match self {
            Parachain::Custom { chain_spec, .. } => Some(chain_spec.as_str()),
            _ => None,
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Parachain::Custom { .. })
    }
}

// Chain generator command template
const CMD_TPL: &str = "chain-spec-generator {{chainName}}";

pub const DEFAULT_CHAIN_SPEC_TPL_COMMAND: &str =
    "{{mainCommand}} build-spec --chain {{chainName}} {{disableBootnodes}}";

// Relaychain nodes
const ALICE: &str = "alice";
const BOB: &str = "bob";
const CHARLIE: &str = "charlie";
const DAVE: &str = "dave";
const EVE: &str = "eve";
const FERDIE: &str = "ferdie";
const ONE: &str = "one";

pub fn generate_network_config(
    network: &Relaychain,
    paras: Vec<Parachain>,
) -> Result<NetworkConfig, anyhow::Error> {
    println!("paras: {:?}", paras);
    // TODO: integrate k8s/docker
    // let images = environment::get_images_from_env();
    let relay_chain = network.as_local_chain_string();
    let relay_context = Context::Relaychain;
    let para_context = Context::Parachain;

    let chain_spec_cmd = match network {
        Relaychain::Polkadot { .. } | Relaychain::Kusama { .. } | Relaychain::Westend { .. } => {
            CMD_TPL
        }
        Relaychain::Paseo { .. } | Relaychain::Custom { .. } => DEFAULT_CHAIN_SPEC_TPL_COMMAND,
    };

    // Calculate required validators based on parachain count
    // Base: 2 validators (Alice, Bob) + 1 per parachain
    // Max supported: 7 validators for up to 5 parachains
    let num_parachains = paras.len();
    let required_validators = 2 + num_parachains;

    let network_builder = NetworkConfigBuilder::new().with_relaychain(|r| {
        let relaychain_builder = r
            .with_chain(relay_chain.as_str())
            .with_default_command(relay_context.cmd().as_str())
            .with_chain_spec_command(chain_spec_cmd)
            .chain_spec_command_is_local(true)
            // .with_default_args(vec![("-l", "babe=debug,grandpa=debug,runtime=debug,parachain::=debug,sub-authority-discovery=trace").into()])
            .with_default_args(vec![("-l", "runtime=trace").into()]);

        // Always add Alice (with optional custom RPC port)
        let relaychain_builder = if let Ok(port) = env::var("ZOMBIE_BITE_RC_PORT") {
            let rpc_port = port
                .parse()
                .expect("env var ZOMBIE_BITE_RC_PORT must be a valid u16");
            relaychain_builder.with_validator(|node| node.with_name(ALICE).with_rpc_port(rpc_port))
        } else {
            relaychain_builder.with_validator(|node| node.with_name(ALICE))
        };

        // Always add Bob
        let relaychain_builder = relaychain_builder.with_validator(|node| node.with_name(BOB));
        // Add additional validators based on parachain count
        let validator_names = [CHARLIE, DAVE, FERDIE, EVE, ONE];
        let additional_validators_needed = required_validators.saturating_sub(2);

        validator_names
            .iter()
            .take(additional_validators_needed)
            .fold(relaychain_builder, |builder, &name| {
                builder.with_validator(|node| node.with_name(name))
            })
    });

    let network_builder = paras.iter().fold(network_builder, |builder, para| {
        println!("para: {:?}", para);
        // let (chain_part, id) = match para {
        //     Parachain::AssetHub { .. } => ("asset-hub", para.id()),
        //     Parachain::Coretime{ .. } => ("coretime", para.id()),
        //     Parachain::People { .. } => ("people", para.id()),
        //     Parachain::BridgeHub { .. } => ("bridge-hub", para.id()),
        //     Parachain::Collectives { .. } => ("collectives", para.id()),
        //     Parachain::Custom { chain_spec,.. } => (chain_spec.as_str(), para.id()),
        // };

        // let chain = if let Parachain::Custom { .. } = para {
        //     chain_part.to_string()
        // } else {
        //     format!("{}-{}",chain_part, relay_chain)
        // };

        let collator_name = format!("Collator-{}", para.id());

        builder.with_parachain(|p| {
            let p = p
                .with_id(para.id())
                .with_default_command(para_context.cmd().as_str());

            // Custom paras use chain_spec_path directly; system paras use chain name + spec command
            let p = if let Some(spec_path) = para.chain_spec_path() {
                p.with_chain_spec_path(spec_path)
            } else {
                let chain_part = match para {
                    Parachain::AssetHub { .. } => "asset-hub",
                    Parachain::Coretime { .. } => "coretime",
                    Parachain::People { .. } => "people",
                    Parachain::BridgeHub { .. } => "bridge-hub",
                    Parachain::Collectives { .. } => "collectives",
                    Parachain::Custom { .. } => unreachable!(),
                };
                let chain = format!("{}-{}", chain_part, relay_chain);
                p.with_chain(chain.as_str())
                    .with_chain_spec_command(chain_spec_cmd)
            };

            p.with_collator(|c| {
                let col_builder = c.with_name(&collator_name)
                .with_args(vec![
                    ("-l", "aura=debug,runtime=trace,cumulus-consensus=trace,consensus::common=trace,parachain::collation-generation=trace,parachain::collator-protocol=trace,parachain=debug,basic-authorship=trace").into(),
                    "--force-authoring".into()
                ]);
                if let Ok(port) = env::var("ZOMBIE_BITE_AH_PORT") {
                    let rpc_port = port.parse().expect("env var ZOMBIE_BITE_AH_PORT must be a valid u16");
                    col_builder.with_rpc_port(rpc_port)
                } else {
                    col_builder
                }
            })
        })
    });

    let config = network_builder.build().map_err(|errs| {
        let e = errs
            .iter()
            .fold("".to_string(), |memo, err| format!("{memo} \n {err}"));
        anyhow::anyhow!(e)
    })?;

    Ok(config)
}

// Configuration file structures
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ZombieBiteConfig {
    pub relaychain: RelaychainConfig,
    pub parachains: Option<Vec<ParachainConfig>>,
    pub base_path: Option<String>,
    pub and_spawn: Option<bool>,
    pub with_monitor: Option<bool>,
    pub apply_upgrade: Option<bool>,
    /// Keep inherited HRMP/DMP state instead of clearing it. Correct when the
    /// relay and parachain snapshots agree on channel heads (a relay whose only
    /// parachains are the ones being bitten); wrong for a shared relay, where
    /// the mismatch makes cumulus panic with `HRMP head mismatch`.
    pub keep_messaging_state: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct RelaychainConfig {
    pub network: String, // polkadot, kusama, paseo
    pub runtime_override: Option<String>,
    pub sync_url: Option<String>,
    pub bite_at: Option<u32>,
    pub upgrade: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct ParachainConfig {
    #[serde(rename = "type")]
    pub parachain_type: String, // asset-hub, coretime, people, bridge-hub, collectives, custom
    pub runtime_override: Option<String>,
    pub upgrade: Option<String>,
    pub enabled: Option<bool>, // default true
    pub bite_at: Option<u32>,
    pub rpc_endpoint: Option<String>,
    // Parachain id. Used for custom
    pub id: Option<u32>,
    // Parachain chain-spec (or full chain name). Used for custom.
    pub chain_spec: Option<String>,
    /// Number of cores to assign, NOTE: only used in `custom` paras
    pub cores: Option<u32>,
}

impl ParachainConfig {
    pub fn to_parachain(&self) -> Option<Parachain> {
        if self.enabled.unwrap_or(true) {
            match self.parachain_type.as_str() {
                "asset-hub" => Some(Parachain::AssetHub {
                    maybe_override: self.runtime_override.clone(),
                    maybe_bite_at: self.bite_at,
                    maybe_rpc_endpoint: self.rpc_endpoint.clone(),
                }),
                "coretime" => Some(Parachain::Coretime {
                    maybe_override: self.runtime_override.clone(),
                    maybe_bite_at: self.bite_at,
                    maybe_rpc_endpoint: self.rpc_endpoint.clone(),
                }),
                "people" => Some(Parachain::People {
                    maybe_override: self.runtime_override.clone(),
                    maybe_bite_at: self.bite_at,
                    maybe_rpc_endpoint: self.rpc_endpoint.clone(),
                }),
                "bridge-hub" => Some(Parachain::BridgeHub {
                    maybe_override: self.runtime_override.clone(),
                    maybe_bite_at: self.bite_at,
                    maybe_rpc_endpoint: self.rpc_endpoint.clone(),
                }),
                "collectives" => Some(Parachain::Collectives {
                    maybe_override: self.runtime_override.clone(),
                    maybe_bite_at: self.bite_at,
                    maybe_rpc_endpoint: self.rpc_endpoint.clone(),
                }),
                "custom" => {
                    // validate chain / id
                    let (Some(id), Some(chain_spec), Some(rpc_endpoint)) =
                        (self.id, self.chain_spec.clone(), self.rpc_endpoint.clone())
                    else {
                        panic!("Invalid custom parachain config, 'id', 'chain_spec' and 'rpc_endpoint' are required");
                    };

                    Some(Parachain::Custom {
                        maybe_override: self.runtime_override.clone(),
                        maybe_bite_at: self.bite_at,
                        maybe_rpc_endpoint: Some(rpc_endpoint),
                        name: format!("custom-{}", id),
                        chain_spec,
                        id,
                        cores: self.cores.unwrap_or(1),
                    })
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

impl ZombieBiteConfig {
    pub fn from_file(path: &str) -> Result<Self, anyhow::Error> {
        let contents = std::fs::read_to_string(path)?;
        let config: ZombieBiteConfig = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn get_relaychain(&self) -> Relaychain {
        Relaychain::new_with_values(
            &self.relaychain.network,
            self.relaychain.runtime_override.clone(),
            self.relaychain.sync_url.clone(),
            self.relaychain.bite_at,
        )
    }

    pub fn get_parachains(&self) -> Vec<Parachain> {
        self.parachains
            .as_ref()
            .map(|paras| paras.iter().filter_map(|p| p.to_parachain()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn config_ok() {
        let config = generate_network_config(&Relaychain::new("kusama"), vec![]).unwrap();
        assert_eq!(0, config.parachains().len());
    }

    #[test]
    fn config_with_para_ok() {
        let config = generate_network_config(
            &Relaychain::new("kusama"),
            vec![Parachain::new("asset-hub")],
        )
        .unwrap();
        let parachain = config.parachains().first().unwrap().chain().unwrap();
        assert_eq!(parachain.as_str(), "asset-hub-kusama-local");
    }

    #[tokio::test]
    async fn spec() {
        let config = generate_network_config(
            &Relaychain::new("kusama"),
            vec![Parachain::new("asset-hub")],
        )
        .unwrap();
        println!("config: {:#?}", config);
        let spec = zombienet_orchestrator::NetworkSpec::from_config(&config)
            .await
            .unwrap();

        println!("{:#?}", spec);
    }

    #[test]
    fn parachain_config_enabled_defaults_to_true() {
        let config = ParachainConfig {
            parachain_type: "asset-hub".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: None, // Not specified
            bite_at: None,
            rpc_endpoint: None,
            id: None,
            chain_spec: None,
            cores: None,
        };

        assert!(config.to_parachain().is_some());
        match config.to_parachain().unwrap() {
            Parachain::AssetHub { .. } => {}
            _ => panic!("Expected AssetHub parachain"),
        }
    }

    #[test]
    fn parachain_config_explicitly_enabled() {
        let config = ParachainConfig {
            parachain_type: "coretime".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: None,
            id: None,
            chain_spec: None,
            cores: None,
        };

        assert!(config.to_parachain().is_some());
        match config.to_parachain().unwrap() {
            Parachain::Coretime { .. } => {}
            _ => panic!("Expected Coretime parachain"),
        }
    }

    #[test]
    fn parachain_config_explicitly_disabled() {
        let config = ParachainConfig {
            parachain_type: "people".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(false),
            bite_at: None,
            rpc_endpoint: None,
            id: None,
            chain_spec: None,
            cores: None,
        };

        assert!(config.to_parachain().is_none());
    }

    #[test]
    fn parachain_config_with_runtime_override() {
        let override_path = "/path/to/runtime.wasm".to_string();
        let config = ParachainConfig {
            parachain_type: "bridge-hub".to_string(),
            runtime_override: Some(override_path.clone()),
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: None,
            id: None,
            chain_spec: None,
            cores: None,
        };

        let parachain = config.to_parachain().unwrap();
        match parachain {
            Parachain::BridgeHub {
                maybe_override: Some(path),
                ..
            } => assert_eq!(path, override_path),
            _ => panic!("Expected BridgeHub with runtime override"),
        }
    }

    #[test]
    fn parachain_config_invalid_type() {
        let config = ParachainConfig {
            parachain_type: "invalid-chain".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: None,
            id: None,
            chain_spec: None,
            cores: None,
        };

        assert!(config.to_parachain().is_none());
    }

    #[test]
    fn all_parachain_types_supported() {
        let types = vec!["asset-hub", "coretime", "people", "bridge-hub"];

        for parachain_type in types {
            let config = ParachainConfig {
                parachain_type: parachain_type.to_string(),
                runtime_override: None,
                upgrade: None,
                enabled: Some(true),
                bite_at: None,
                rpc_endpoint: None,
                id: None,
                chain_spec: None,
                cores: None,
            };

            assert!(
                config.to_parachain().is_some(),
                "Failed for type: {}",
                parachain_type
            );
        }
    }

    #[test]
    fn parachain_ids_are_correct() {
        assert_eq!(
            Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .id(),
            1000
        );
        assert_eq!(
            Parachain::Coretime {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .id(),
            1005
        );
        assert_eq!(
            Parachain::People {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .id(),
            1004
        );
        assert_eq!(
            Parachain::BridgeHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .id(),
            1002
        );
    }

    #[test]
    fn parachain_chain_strings() {
        let relay = "polkadot";

        assert_eq!(
            Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_chain_string(relay),
            "asset-hub-polkadot"
        );
        assert_eq!(
            Parachain::Coretime {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_chain_string(relay),
            "coretime-polkadot"
        );
        assert_eq!(
            Parachain::People {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_chain_string(relay),
            "people-polkadot"
        );
        assert_eq!(
            Parachain::BridgeHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_chain_string(relay),
            "bridge-hub-polkadot"
        );
    }

    #[test]
    fn parachain_local_chain_strings() {
        let relay = "kusama";

        assert_eq!(
            Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_local_chain_string(relay),
            "asset-hub-kusama-local"
        );
        assert_eq!(
            Parachain::Coretime {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_local_chain_string(relay),
            "coretime-kusama-local"
        );
        assert_eq!(
            Parachain::People {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_local_chain_string(relay),
            "people-kusama-local"
        );
        assert_eq!(
            Parachain::BridgeHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None
            }
            .as_local_chain_string(relay),
            "bridge-hub-kusama-local"
        );
    }

    #[test]
    fn relaychain_creation() {
        let polkadot = Relaychain::new("polkadot");
        assert_eq!(polkadot.as_chain_string(), "polkadot");

        let kusama = Relaychain::new("kusama");
        assert_eq!(kusama.as_chain_string(), "kusama");

        let paseo = Relaychain::new("paseo");
        assert_eq!(paseo.as_chain_string(), "paseo");

        // An unknown name is a custom relay keeping its name, not a silent
        // fallback to polkadot: the helper subcommands name artifacts after it,
        // and a typo now fails instead of biting the wrong chain.
        let unknown = Relaychain::new("unknown");
        assert_eq!(unknown.as_chain_string(), "unknown");
        assert!(unknown.is_custom());
    }

    #[test]
    fn relaychain_with_overrides() {
        let runtime_path = Some("/path/to/runtime.wasm".to_string());
        let sync_url = Some("wss://custom-rpc.example.com".to_string());

        let relaychain =
            Relaychain::new_with_values("kusama", runtime_path.clone(), sync_url.clone(), None);

        assert_eq!(relaychain.wasm_overrides(), runtime_path.as_deref());
        match relaychain {
            Relaychain::Kusama { maybe_sync_url, .. } => assert_eq!(maybe_sync_url, sync_url),
            _ => panic!("Expected Kusama relaychain"),
        }
    }

    #[test]
    fn relaychain_epoch_durations() {
        assert_eq!(Relaychain::new("polkadot").epoch_duration(), 2400);
        assert_eq!(Relaychain::new("kusama").epoch_duration(), 600);
        assert_eq!(Relaychain::new("paseo").epoch_duration(), 600);
    }

    #[test]
    fn generate_config_with_all_parachains() {
        let relaychain = Relaychain::new("polkadot");
        let parachains = vec![
            Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            Parachain::Coretime {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            Parachain::People {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
            Parachain::BridgeHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
        ];

        let config = generate_network_config(&relaychain, parachains).unwrap();
        assert_eq!(config.parachains().len(), 4);
    }

    #[test]
    fn generate_config_with_runtime_overrides() {
        let relaychain = Relaychain::new_with_values(
            "kusama",
            Some("/path/to/relay_runtime.wasm".to_string()),
            None,
            None,
        );
        let parachains = vec![Parachain::AssetHub {
            maybe_override: Some("/path/to/ah_runtime.wasm".to_string()),
            maybe_bite_at: None,
            maybe_rpc_endpoint: None,
        }];

        let config = generate_network_config(&relaychain, parachains).unwrap();
        assert_eq!(config.parachains().len(), 1);
    }

    #[test]
    fn zombie_bite_config_get_parachains_empty() {
        let config = ZombieBiteConfig {
            relaychain: RelaychainConfig {
                network: "polkadot".to_string(),
                runtime_override: None,
                upgrade: None,
                sync_url: None,
                bite_at: None,
            },
            parachains: None,
            base_path: None,
            and_spawn: None,
            with_monitor: None,
            apply_upgrade: None,
            keep_messaging_state: None,
        };

        assert_eq!(config.get_parachains().len(), 0);
    }

    #[test]
    fn zombie_bite_config_get_parachains_with_enabled_disabled_mix() {
        let config = ZombieBiteConfig {
            relaychain: RelaychainConfig {
                network: "kusama".to_string(),
                runtime_override: None,
                upgrade: None,
                sync_url: None,
                bite_at: None,
            },
            parachains: Some(vec![
                ParachainConfig {
                    parachain_type: "asset-hub".to_string(),
                    runtime_override: None,
                    upgrade: None,
                    enabled: Some(true),
                    bite_at: None,
                    rpc_endpoint: None,
                    id: None,
                    chain_spec: None,
                    cores: None,
                },
                ParachainConfig {
                    parachain_type: "coretime".to_string(),
                    runtime_override: None,
                    upgrade: None,
                    enabled: Some(false), // Disabled
                    bite_at: None,
                    rpc_endpoint: None,
                    id: None,
                    chain_spec: None,
                    cores: None,
                },
                ParachainConfig {
                    parachain_type: "people".to_string(),
                    runtime_override: None,
                    upgrade: None,
                    enabled: None, // Defaults to true
                    bite_at: None,
                    rpc_endpoint: None,
                    id: None,
                    chain_spec: None,
                    cores: None,
                },
            ]),
            base_path: None,
            and_spawn: None,
            with_monitor: None,
            apply_upgrade: None,
            keep_messaging_state: None,
        };

        let parachains = config.get_parachains();
        assert_eq!(parachains.len(), 2); // Only asset-hub and people should be enabled

        // Check that the right parachains are included
        let para_ids: Vec<u32> = parachains.iter().map(|p| p.id()).collect();
        assert!(para_ids.contains(&1000)); // asset-hub
        assert!(para_ids.contains(&1004)); // people
        assert!(!para_ids.contains(&1005)); // coretime (disabled)
    }

    #[test]
    fn step_enum_conversion() {
        assert_eq!(Step::from("bite".to_string()), Step::Bite);
        assert_eq!(Step::from("spawn".to_string()), Step::Spawn);
        assert_eq!(Step::from("post".to_string()), Step::Post);
        assert_eq!(Step::from("after".to_string()), Step::After);
        assert_eq!(Step::from("SPAWN".to_string()), Step::Spawn); // Case insensitive
        assert_eq!(Step::from("unknown".to_string()), Step::Bite); // Unknown defaults to Bite
    }

    #[test]
    fn step_directories() {
        assert_eq!(Step::Bite.dir(), "bite");
        assert_eq!(Step::Spawn.dir(), "spawn");
        assert_eq!(Step::Post.dir(), "post");
        assert_eq!(Step::After.dir(), "after");
    }

    #[test]
    fn step_next() {
        assert_eq!(Step::Bite.next(), Some("spawn".to_string()));
        assert_eq!(Step::Spawn.next(), Some("post".to_string()));
        assert_eq!(Step::Post.next(), Some("after".to_string()));
        assert_eq!(Step::After.next(), None);
    }

    #[test]
    fn step_dir_from() {
        assert_eq!(Step::Bite.dir_from(), "");
        assert_eq!(Step::Spawn.dir_from(), "bite");
        assert_eq!(Step::Post.dir_from(), "spawn");
        assert_eq!(Step::After.dir_from(), "post");
    }

    // Test TOML parsing directly without file I/O
    #[test]
    fn zombie_bite_config_from_toml_string() {
        let toml_content = r#"
            base_path = "/custom/path"
            and_spawn = true
            with_monitor = false

            [relaychain]
            network = "kusama"
            runtime_override = "/path/to/runtime.wasm"

            [[parachains]]
            type = "asset-hub"
            enabled = true

            [[parachains]]
            type = "coretime"
            enabled = false
        "#;

        let config: ZombieBiteConfig = toml::from_str(toml_content).unwrap();

        assert_eq!(config.relaychain.network, "kusama");
        assert_eq!(
            config.relaychain.runtime_override,
            Some("/path/to/runtime.wasm".to_string())
        );
        assert_eq!(config.base_path, Some("/custom/path".to_string()));
        assert_eq!(config.and_spawn, Some(true));
        assert_eq!(config.with_monitor, Some(false));

        let parachains = config.get_parachains();
        assert_eq!(parachains.len(), 1); // Only asset-hub enabled
        assert_eq!(parachains[0].id(), 1000); // asset-hub ID
    }

    #[test]
    fn zombie_bite_config_minimal_toml() {
        let toml_content = r#"
[relaychain]
network = "polkadot"
        "#;

        let config: ZombieBiteConfig = toml::from_str(toml_content).unwrap();

        assert_eq!(config.relaychain.network, "polkadot");
        assert_eq!(config.relaychain.runtime_override, None);
        assert_eq!(config.parachains, None);
        assert_eq!(config.base_path, None);
        assert_eq!(config.and_spawn, None);
        assert_eq!(config.with_monitor, None);

        let parachains = config.get_parachains();
        assert_eq!(parachains.len(), 0); // No parachains specified
    }

    #[test]
    fn custom_parachain_id() {
        let para = Parachain::Custom {
            id: 3392,
            name: "yap-3392".to_string(),
            chain_spec: "/path/to/spec.json".to_string(),
            maybe_override: None,
            maybe_bite_at: None,
            maybe_rpc_endpoint: Some("wss://example.com".to_string()),
            cores: 0,
        };
        assert_eq!(para.id(), 3392);
    }

    #[test]
    fn custom_parachain_chain_strings() {
        let para = Parachain::Custom {
            id: 3392,
            name: "yap-3392".to_string(),
            chain_spec: "/path/to/spec.json".to_string(),
            maybe_override: None,
            maybe_bite_at: None,
            maybe_rpc_endpoint: Some("wss://example.com".to_string()),
            cores: 1,
        };
        assert_eq!(para.as_chain_string("kusama"), "3392-kusama");
        assert_eq!(para.as_local_chain_string("kusama"), "3392-kusama-local");
    }

    #[test]
    fn custom_parachain_chain_spec_path() {
        let para = Parachain::Custom {
            id: 3392,
            name: "yap-3392".to_string(),
            chain_spec: "/path/to/spec.json".to_string(),
            maybe_override: None,
            maybe_bite_at: None,
            maybe_rpc_endpoint: Some("wss://example.com".to_string()),
            cores: 1,
        };
        assert_eq!(para.chain_spec_path(), Some("/path/to/spec.json"));
        assert!(para.is_custom());

        // Non-custom paras return None for chain_spec_path
        let ah = Parachain::new("asset-hub");
        assert_eq!(ah.chain_spec_path(), None);
        assert!(!ah.is_custom());
    }

    #[test]
    fn custom_parachain_config_from_toml() {
        let toml_content = r#"
[relaychain]
network = "kusama"

[[parachains]]
type = "custom"
id = 3392
name = "yap-3392"
rpc_endpoint = "wss://kusama-yap-3392.parity-chains.parity.io"
chain_spec = "/path/to/yap-3392-raw-chain-spec.json"
        "#;

        let config: ZombieBiteConfig = toml::from_str(toml_content).unwrap();
        let parachains = config.get_parachains();
        assert_eq!(parachains.len(), 1);

        let para = &parachains[0];
        assert_eq!(para.id(), 3392);
        assert!(para.is_custom());
        assert_eq!(
            para.chain_spec_path(),
            Some("/path/to/yap-3392-raw-chain-spec.json")
        );
        assert_eq!(
            para.rpc_endpoint(),
            Some("wss://kusama-yap-3392.parity-chains.parity.io")
        );
        assert_eq!(para.as_chain_string("kusama"), "3392-kusama");
    }

    #[test]
    fn custom_parachain_config_default_name() {
        let config = ParachainConfig {
            parachain_type: "custom".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: Some("wss://example.com".to_string()),
            id: Some(3392),
            chain_spec: Some("/path/to/spec.json".to_string()),
            cores: None,
        };

        let para = config.to_parachain().unwrap();
        assert_eq!(para.as_chain_string("kusama"), "3392-kusama");
    }

    #[test]
    #[should_panic(
        expected = "Invalid custom parachain config, 'id', 'chain_spec' and 'rpc_endpoint' are required"
    )]
    fn custom_parachain_config_missing_para_id() {
        let config = ParachainConfig {
            parachain_type: "custom".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: Some("wss://example.com".to_string()),
            id: None,
            chain_spec: Some("/path/to/spec.json".to_string()),
            cores: None,
        };
        config.to_parachain();
    }

    #[test]
    #[should_panic(
        expected = "Invalid custom parachain config, 'id', 'chain_spec' and 'rpc_endpoint' are required"
    )]
    fn custom_parachain_config_missing_chain_spec() {
        let config = ParachainConfig {
            parachain_type: "custom".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: Some("wss://example.com".to_string()),
            id: Some(3392),
            chain_spec: None,
            cores: None,
        };
        config.to_parachain();
    }

    #[test]
    #[should_panic(
        expected = "Invalid custom parachain config, 'id', 'chain_spec' and 'rpc_endpoint' are required"
    )]
    fn custom_parachain_config_missing_rpc_endpoint() {
        let config = ParachainConfig {
            parachain_type: "custom".to_string(),
            runtime_override: None,
            upgrade: None,
            enabled: Some(true),
            bite_at: None,
            rpc_endpoint: None,
            id: Some(3392),
            chain_spec: Some("/path/to/spec.json".to_string()),
            cores: None,
        };
        config.to_parachain();
    }

    #[test]
    fn generate_config_with_custom_parachain() {
        let relaychain = Relaychain::new("kusama");
        // Create a temp chain spec file for the test
        let spec_path = "/tmp/test-custom-para-spec.json";
        std::fs::write(spec_path, r#"{"bootNodes": []}"#).unwrap();

        let parachains = vec![Parachain::Custom {
            id: 3392,
            name: "yap-3392".to_string(),
            chain_spec: spec_path.to_string(),
            maybe_override: None,
            maybe_bite_at: None,
            maybe_rpc_endpoint: Some("wss://example.com".to_string()),
            cores: 1,
        }];

        let config = generate_network_config(&relaychain, parachains).unwrap();
        let parachains = config.parachains();
        assert_eq!(parachains.len(), 1);
        let para_config = parachains.first().unwrap();
        assert_eq!(para_config.id(), 3392);
    }
    #[test]
    fn sync_url_overrides_the_public_endpoint() {
        let default = Relaychain::new("kusama");
        assert_eq!(default.sync_endpoint(), "wss://kusama-rpc.polkadot.io");
        assert_eq!(default.rpc_endpoint(), "wss://kusama-rpc.polkadot.io");

        let custom = Relaychain::new_with_values(
            "kusama",
            None,
            Some("wss://my-own-kusama.example.com".to_string()),
            None,
        );
        assert_eq!(custom.sync_endpoint(), "wss://my-own-kusama.example.com");
        assert_eq!(custom.rpc_endpoint(), "wss://my-own-kusama.example.com");
    }
}
