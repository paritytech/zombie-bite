#![allow(dead_code)]
// TODO: don't allow dead_code

use std::env;

use zombienet_configuration::{NetworkConfig, NetworkConfigBuilder};
const BITE: &str = "bite";
const SPAWN: &str = "spawn";
const POST: &str = "post";
const AFTER: &str = "after";
const DEBUG: &str = "debug";

// `--state-pruning` config flag (two days +1 by default)
pub const STATE_PRUNING: &str = "28801";
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

#[derive(Debug, PartialEq)]
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
            _ => Self::Polkadot {
                maybe_override: None,
                maybe_sync_url: None,
                maybe_bite_at: None,
            },
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
            _ => Self::Polkadot {
                maybe_override,
                maybe_sync_url,
                maybe_bite_at,
            },
        }
    }

    pub fn as_local_chain_string(&self) -> String {
        String::from(match self {
            Relaychain::Polkadot { .. } => "polkadot-local",
            Relaychain::Kusama { .. } => "kusama-local",
            Relaychain::Paseo { .. } => "paseo-local",
        })
    }

    pub fn as_chain_string(&self) -> String {
        String::from(match self {
            Relaychain::Polkadot { .. } => "polkadot",
            Relaychain::Kusama { .. } => "kusama",
            Relaychain::Paseo { .. } => "paseo",
        })
    }

    // TODO: make this endpoints configurables
    pub fn sync_endpoint(&self) -> String {
        String::from(match self {
            Relaychain::Polkadot { .. } => "wss://polkadot-rpc.n.dwellir.com",
            Relaychain::Kusama { .. } => "wss://kusama-rpc.n.dwellir.com",
            Relaychain::Paseo { .. } => "wss://paseo-rpc.dwellir.com",
        })
    }

    pub fn rpc_endpoint(&self) -> String {
        String::from(match self {
            Relaychain::Polkadot { .. } => "wss://polkadot-rpc.n.dwellir.com",
            Relaychain::Kusama { .. } => "wss://kusama-rpc.n.dwellir.com",
            Relaychain::Paseo { .. } => "wss://paseo-rpc.dwellir.com",
        })
    }

    pub fn context(&self) -> Context {
        Context::Relaychain
    }

    pub fn wasm_overrides(&self) -> Option<&str> {
        match self {
            Relaychain::Kusama { maybe_override, .. }
            | Relaychain::Polkadot { maybe_override, .. }
            | Relaychain::Paseo { maybe_override, .. } => maybe_override.as_deref(),
        }
    }

    pub fn epoch_duration(&self) -> u64 {
        match self {
            Relaychain::Paseo { .. } => 600,
            Relaychain::Kusama { .. } => 600,
            _ => 2400,
        }
    }

    pub fn at_block(&self) -> Option<u32> {
        match self {
            Relaychain::Kusama { maybe_bite_at, .. }
            | Relaychain::Polkadot { maybe_bite_at, .. }
            | Relaychain::Paseo { maybe_bite_at, .. } => maybe_bite_at.clone(),
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
    }, // Bridge
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
            _ => Parachain::AssetHub {
                maybe_override: None,
                maybe_bite_at: None,
                maybe_rpc_endpoint: None,
            },
        }
    }

    pub fn as_local_chain_string(&self, relay_part: &str) -> String {
        let para_part = match self {
            Parachain::AssetHub { .. } => "asset-hub",
            Parachain::Coretime { .. } => "coretime",
            Parachain::People { .. } => "people",
        };

        format!("{para_part}-{relay_part}-local")
    }

    pub fn as_chain_string(&self, relay_part: &str) -> String {
        let para_part = match self {
            Parachain::AssetHub { .. } => "asset-hub",
            Parachain::Coretime { .. } => "coretime",
            Parachain::People { .. } => "people",
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
            Parachain::People { .. } => 1001,
        }
    }

    pub fn wasm_overrides(&self) -> Option<&str> {
        match self {
            Parachain::AssetHub { maybe_override, .. }
            | Parachain::Coretime { maybe_override, .. }
            | Parachain::People { maybe_override, .. } => maybe_override.as_deref(),
        }
    }

    pub fn at_block(&self) -> Option<u32> {
        match self {
            Parachain::AssetHub { maybe_bite_at, .. }
            | Parachain::Coretime { maybe_bite_at, .. }
            | Parachain::People { maybe_bite_at, .. } => maybe_bite_at.clone(),
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
            } => maybe_rpc_endpoint.as_deref(),
        }
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
        Relaychain::Polkadot { .. } | Relaychain::Kusama { .. } => CMD_TPL,
        Relaychain::Paseo { .. } => DEFAULT_CHAIN_SPEC_TPL_COMMAND,
    };

    let network_builder = NetworkConfigBuilder::new().with_relaychain(|r| {
        let relaychain_builder = r
            .with_chain(relay_chain.as_str())
            .with_default_command(relay_context.cmd().as_str())
            .with_chain_spec_command(chain_spec_cmd)
            .chain_spec_command_is_local(true)
            // .with_default_args(vec![("-l", "babe=debug,grandpa=debug,runtime=debug,parachain::=debug,sub-authority-discovery=trace").into()])
            .with_default_args(vec![("-l", "runtime=trace").into()]);

        let relaychain_builder = if let Ok(port) = env::var("ZOMBIE_BITE_RC_PORT") {
            let rpc_port = port
                .parse()
                .expect("env var ZOMBIE_BITE_RC_PORT must be a valid u16");
            relaychain_builder.with_validator(|node| node.with_name(ALICE).with_rpc_port(rpc_port))
        } else {
            relaychain_builder.with_validator(|node| node.with_name(ALICE))
        };

        // .with_node(|node| node.with_name(ALICE))
        relaychain_builder.with_validator(|node| node.with_name(BOB))
        // .with_node(|node| node.with_name(CHARLIE))
        // .with_node(|node| node.with_name(DAVE))
    });

    let network_builder = paras.iter().fold(network_builder, |builder, para| {
        println!("para: {:?}", para);
        let (chain_part, id) = match para {
            Parachain::AssetHub { .. } => ("asset-hub", para.id()),
            Parachain::Coretime{ .. } => ("coretime", para.id()),
            Parachain::People { .. } => ("people", para.id()),
        };
        let chain = format!("{}-{}",chain_part, relay_chain);

        builder.with_parachain(|p| {
            p.with_id(id)
                .with_default_command(para_context.cmd().as_str())
                .with_chain(chain.as_str())
                .with_chain_spec_command(chain_spec_cmd)
                .with_collator(|c| {
                    // TODO: use single collator for now
                    // c.with_name(&format!("col-{}",id))
                    let col_builder = c.with_name("collator")
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
}
