#![allow(dead_code)]
// TODO: don't allow dead_code

use anyhow::anyhow;
use futures::future::try_join_all;
use futures::FutureExt;
use serde_json::json;
// use serde_json::json;
use std::fs::{read_to_string, File};
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::fs;
use zombienet_sdk::NetworkConfig;

use codec::Encode;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::Builder;

use tracing::debug;
use tracing::{info, trace};
use zombienet_configuration::NetworkConfigBuilder;
use zombienet_orchestrator::network::Network;
use zombienet_orchestrator::Orchestrator;
use zombienet_provider::types::RunCommandOptions;
use zombienet_provider::types::SpawnNodeOptions;
use zombienet_provider::DynNamespace;
use zombienet_provider::NativeProvider;
use zombienet_provider::Provider;
use zombienet_support::fs::local::LocalFileSystem;

use crate::utils::{
    get_header_from_block, get_random_port, localize_config, para_head_key, HeadData,
};

use crate::config::{get_state_pruning_config, Context, Parachain, Relaychain, Step};
use crate::overrides::{generate_default_overrides_for_para, generate_default_overrides_for_rc};
use crate::sync::{sync_para, sync_relay_only};

use std::env;

const PORTS_FILE: &str = "ports.json";
const READY_FILE: &str = "ready.json";

#[derive(Debug, Clone)]
struct ChainArtifact {
    cmd: String,
    chain: String,
    spec_path: String,
    snap_path: String,
    override_wasm: Option<String>,
    para_id: Option<u32>,
}

pub async fn doppelganger_inner(
    global_base_dir: PathBuf,
    relay_chain: Relaychain,
    paras_to: Vec<Parachain>,
    database: &str,
) -> Result<(), anyhow::Error> {
    // Star the node and wait until finish (with temp dir managed by us)
    info!(
        "🪞 Starting DoppelGanger process for {} and {:?}",
        relay_chain.as_chain_string(),
        paras_to
    );

    let filesystem = LocalFileSystem;
    let provider = NativeProvider::new(filesystem.clone());

    // ensure the base path exist
    fs::create_dir_all(&global_base_dir).await.unwrap();

    // add `/bite` to global base
    let fixed_base_dir = global_base_dir.canonicalize().unwrap().join("bite");

    let base_dir_str = fixed_base_dir.to_string_lossy();
    let ns = provider
        .create_namespace_with_base_dir(fixed_base_dir.as_path())
        .await
        .unwrap();

    let _relaychain_rpc_random_port = get_random_port().await;

    // Parachain sync
    let mut syncs = vec![];
    for para in &paras_to {
        let para_default_overrides_path =
            generate_default_overrides_for_para(&base_dir_str, para, &relay_chain).await;
        let info_path = format!("{base_dir_str}/para-{}.txt", para.id());

        let maybe_target_header_path = if let Some(at_block) = para.at_block() {
            let para_rpc = para
                .rpc_endpoint()
                .expect("rpc for parachain should be set. qed");
            let header = get_header_from_block(at_block, para_rpc).await?;

            let target_header_path = format!("{base_dir_str}/para-header.json");
            fs::write(&target_header_path, serde_json::to_string_pretty(&header)?)
                .await
                .expect("create target head json should works");
            Some(target_header_path)
        } else {
            None
        };

        syncs.push(
            sync_para(
                ns.clone(),
                "doppelganger-parachain",
                para.as_chain_string(&relay_chain.as_chain_string()),
                relay_chain.as_chain_string(),
                relay_chain.sync_endpoint(),
                para_default_overrides_path,
                info_path,
                maybe_target_header_path,
                database,
            )
            .boxed(),
        );
    }

    let res = try_join_all(syncs).await.unwrap();

    // loop over paras
    let mut para_artifacts = vec![];
    let mut para_heads_env = vec![];
    let context_para = Context::Parachain;
    for (para_index, (_sync_node, sync_db_path, sync_chain, sync_head_path)) in
        res.into_iter().enumerate()
    {
        let sync_chain_name = if sync_chain.contains('/') {
            let parts: Vec<&str> = sync_chain.split('/').collect();
            let name_parts: Vec<&str> = parts.last().unwrap().split('.').collect();
            name_parts.first().unwrap().to_string()
        } else {
            // is not a file
            sync_chain.clone()
        };

        let chain_spec_path = format!("{}/{}-spec.json", &base_dir_str, &sync_chain_name);
        generate_chain_spec(
            ns.clone(),
            &chain_spec_path,
            &context_para.doppelganger_cmd(),
            &sync_chain,
        )
        .await
        .unwrap();

        // generate the data.tgz to use as snapshot
        let snap_path = format!("{}/{}-snap.tgz", &base_dir_str, &sync_chain_name);
        trace!("snap_path: {snap_path}");
        generate_snap(&sync_db_path, &snap_path).await.unwrap();

        let para_head_str = read_to_string(&sync_head_path)
            .unwrap_or_else(|_| panic!("read para_head ({sync_head_path}) file should works."));
        let para_head_hex = if &para_head_str[..2] == "0x" {
            &para_head_str[2..]
        } else {
            &para_head_str
        };

        let para_head = array_bytes::bytes2hex(
            "0x",
            HeadData(hex::decode(para_head_hex).expect("para_head should be a valid hex. qed"))
                .encode(),
        );

        let para = paras_to
            .get(para_index)
            .expect("para_index should be valid. qed");
        para_heads_env.push((
            format!("ZOMBIE_{}", &para_head_key(para.id())[2..]),
            para_head[2..].to_string(),
        ));

        para_artifacts.push(ChainArtifact {
            cmd: context_para.doppelganger_cmd(),
            chain: if sync_chain.contains('/') {
                para.as_chain_string(&relay_chain.as_chain_string())
            } else {
                sync_chain
            },
            spec_path: chain_spec_path,
            snap_path,
            override_wasm: para.wasm_overrides().map(str::to_string),
            para_id: Some(para.id()),
        });
    }

    let rc_default_overrides_path =
        generate_default_overrides_for_rc(&base_dir_str, &relay_chain, &paras_to).await;
    let rc_info_path = format!("{base_dir_str}/rc_info.txt");
    // RELAYCHAIN sync

    let maybe_target_header_path = if let Some(at_block) = relay_chain.at_block() {
        let header = get_header_from_block(at_block, &relay_chain.rpc_endpoint()).await?;

        let target_header_path = format!("{base_dir_str}/rc-header.json");
        fs::write(&target_header_path, serde_json::to_string_pretty(&header)?)
            .await
            .expect("create target head json should works");
        Some(target_header_path)
    } else {
        None
    };

    let (sync_node, sync_db_path, sync_chain) = sync_relay_only(
        ns.clone(),
        "doppelganger",
        relay_chain.as_chain_string(),
        para_heads_env,
        rc_default_overrides_path,
        &rc_info_path,
        maybe_target_header_path,
        database,
    )
    .await
    .unwrap();

    // stop relay node
    sync_node.destroy().await.unwrap();

    // get the chain-spec (prod) and clean the bootnodes
    // relaychain
    let context_relay = Context::Relaychain;
    let r_chain_spec_path = format!("{}/{}-spec.json", &base_dir_str, &sync_chain);
    generate_chain_spec(
        ns.clone(),
        &r_chain_spec_path,
        &context_relay.doppelganger_cmd(),
        &sync_chain,
    )
    .await
    .unwrap();

    // remove `parachains` db
    let sync_chain_in_path = if sync_chain == "kusama" {
        "ksmcc3"
    } else {
        sync_chain.as_str()
    };

    let parachains_path = if database == "rocksdb" {
        format!("{sync_db_path}/chains/{sync_chain_in_path}/db/full/parachains")
    } else {
        format!("{sync_db_path}/chains/{sync_chain_in_path}/paritydb/parachains")
    };

    debug!("Deleting `parachains` db at {parachains_path}");
    tokio::fs::remove_dir_all(parachains_path)
        .await
        .expect("remove parachains db should work");

    // generate the data.tgz to use as snapshot
    let r_snap_path = format!("{}/{}-snap.tgz", &base_dir_str, &sync_chain);
    generate_snap(&sync_db_path, &r_snap_path).await.unwrap();

    let relay_artifacts = ChainArtifact {
        cmd: context_relay.doppelganger_cmd(),
        chain: sync_chain,
        spec_path: r_chain_spec_path,
        snap_path: r_snap_path,
        override_wasm: relay_chain.wasm_overrides().map(str::to_string),
        para_id: None,
    };

    let config = generate_config(
        relay_artifacts,
        para_artifacts,
        Some(global_base_dir.clone()),
        database,
    )
    .await
    .map_err(|e| anyhow!(e.to_string()))?;
    // write config in 'bite'
    let config_toml_path = format!("{}/bite/config.toml", global_base_dir.to_string_lossy());
    let toml_config = config.dump_to_toml()?;
    fs::write(config_toml_path, &toml_config)
        .await
        .expect("create config.toml should works");

    // create port and ready files
    let rc_start_block = fs::read_to_string(format!("{base_dir_str}/rc_info.txt"))
        .await
        .unwrap()
        .parse::<u64>()
        .expect("read bite rc block should works");

    // Collect start blocks for all parachains
    let mut para_start_blocks = serde_json::Map::new();
    for para in &paras_to {
        let para_start_block = fs::read_to_string(format!("{base_dir_str}/para-{}.txt", para.id()))
            .await
            .unwrap()
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("read bite para-{} block should works", para.id()));
        para_start_blocks.insert(
            format!("para_{}_start_block", para.id()),
            serde_json::Value::Number(para_start_block.into()),
        );
    }

    // ready to start
    let mut ready_content = json!({
        "rc_start_block": rc_start_block,
    });

    // Add all parachain start blocks
    for (key, value) in para_start_blocks {
        ready_content[key] = value;
    }

    let alice_config = config
        .relaychain()
        .nodes()
        .into_iter()
        .find(|node| node.name() == "alice")
        .expect("'alice' should exist");

    // Collect ports for all parachains
    let mut collator_ports = serde_json::Map::new();
    for para_config in config.parachains() {
        if let Some(collator) = para_config.collators().first() {
            collator_ports.insert(
                format!("para_{}_collator_port", para_config.id()),
                serde_json::Value::Number(collator.rpc_port().unwrap().into()),
            );
        }
    }

    // ports
    collator_ports.insert(
        "alice_port".to_string(),
        serde_json::Value::Number(alice_config.rpc_port().unwrap().into()),
    );
    let ports_content = serde_json::Value::Object(collator_ports);

    let _ = fs::write(
        format!("{}/{PORTS_FILE}", global_base_dir.to_string_lossy()),
        ports_content.to_string(),
    )
    .await;
    let _ = fs::write(
        format!("{}/{READY_FILE}", global_base_dir.to_string_lossy()),
        ready_content.to_string(),
    )
    .await;

    clean_up_dir_for_step(global_base_dir, Step::Bite, &relay_chain, &paras_to).await?;

    Ok(())
}

/// Create the needed artifats for the next step
pub async fn generate_artifacts(
    global_base_dir: PathBuf,
    step: Step,
    rc: &Relaychain,
) -> Result<(), anyhow::Error> {
    let global_base_dir_str = global_base_dir.to_string_lossy();

    // Load the config from the previous step to get parachain information
    let from_config_path = format!("{global_base_dir_str}/{}/config.toml", step.dir_from());

    // Parse config to get parachain information
    let network_config = zombienet_configuration::NetworkConfig::load_from_toml(&from_config_path)
        .expect("should be able to load config");

    // generate snapshot for alice (rc)
    let alice_data = format!("{global_base_dir_str}/{}/alice/data", step.dir());

    let alice_rc_snap_file = format!("alice-{}-snap.tgz", rc.as_chain_string());
    let alice_rc_snap_path = format!("{global_base_dir_str}/{}/{alice_rc_snap_file}", step.dir());
    generate_snap(&alice_data, &alice_rc_snap_path).await?;

    // generate snapshot for bob (rc)
    let bob_data = format!("{global_base_dir_str}/{}/bob/data", step.dir());
    let bob_rc_snap_file = format!("bob-{}-snap.tgz", rc.as_chain_string());
    let bob_rc_snap_path = format!("{global_base_dir_str}/{}/{bob_rc_snap_file}", step.dir());
    generate_snap(&bob_data, &bob_rc_snap_path).await?;

    let mut snaps = vec![alice_rc_snap_path, bob_rc_snap_path];
    let mut specs = vec![];

    // cp chain-spec for rc
    let rc_spec_file = format!("{}-spec.json", rc.as_chain_string());
    let rc_spec_from = format!("{global_base_dir_str}/{}/{rc_spec_file}", step.dir_from());
    let rc_spec_to = format!("{global_base_dir_str}/{}/{rc_spec_file}", step.dir());
    fs::copy(&rc_spec_from, &rc_spec_to)
        .await
        .expect("cp should work");
    specs.push(rc_spec_to);

    // Generate snapshots and copy chain-specs for all parachains
    for para_config in network_config.parachains() {
        let para_id = para_config.id();
        let para_chain = para_config.chain().expect("parachain should have a chain");
        let para_chain_str = para_chain.as_str();
        let collator_name = format!("Collator-{}", para_id);

        // generate snapshot for this parachain's collator
        let collator_data = format!(
            "{global_base_dir_str}/{}/{}/data",
            step.dir(),
            collator_name
        );
        let para_snap_file = format!("{}-snap.tgz", para_chain_str);
        let para_snap_path = format!("{global_base_dir_str}/{}/{}", step.dir(), para_snap_file);
        generate_snap(&collator_data, &para_snap_path).await?;
        snaps.push(para_snap_path);

        // cp chain-spec for this parachain
        let para_spec_file = format!("{}-spec.json", para_chain_str);
        let para_spec_from = format!(
            "{global_base_dir_str}/{}/{}",
            step.dir_from(),
            para_spec_file
        );
        let para_spec_to = format!("{global_base_dir_str}/{}/{}", step.dir(), para_spec_file);
        fs::copy(&para_spec_from, &para_spec_to)
            .await
            .expect("cp should work");
        specs.push(para_spec_to);
    }

    // generate custom config
    let from_config_path = format!("{global_base_dir_str}/{}/config.toml", step.dir_from());
    let config = fs::read_to_string(&from_config_path)
        .await
        .expect("read config file should work");
    let db_snaps_in_file: Vec<(usize, &str)> = config.match_indices("db_snapshot").collect();
    let needs_to_insert_db = db_snaps_in_file.len() != snaps.len();
    let toml_config = config
        .lines()
        .map(|l| {
            match l {
                l if l.starts_with("default_db_snapshot =") => {
                    String::from("") // emty to remove
                }
                l if l.starts_with("name =") => {
                    if needs_to_insert_db {
                        let snap_line = format!(r#"db_snapshot = "{}""#, snaps.remove(0));
                        trace!("setting {snap_line}");
                        format!("{l}\n{snap_line}")
                    } else {
                        l.to_string()
                    }
                }
                l if l.starts_with("chain_spec_path =") => {
                    let new_l = format!(r#"chain_spec_path = "{}""#, specs.remove(0));
                    trace!("setting {new_l}");
                    new_l
                }
                _ => l.to_string(),
            }
        })
        .collect::<Vec<String>>()
        .join("\n");

    // write config in 'dir'
    let config_toml_path = format!("{global_base_dir_str}/{}/config.toml", step.dir());
    fs::write(config_toml_path, &toml_config)
        .await
        .expect("create config.toml should works");

    Ok(())
}

pub async fn clean_up_dir_for_step(
    global_base_dir: PathBuf,
    step: Step,
    rc: &Relaychain,
    paras: &[Parachain],
) -> Result<(), anyhow::Error> {
    let global_base_dir_str = global_base_dir.to_string_lossy();
    // clean bite directory to leave only the needed artifacts
    let debug_path = format!("{global_base_dir_str}/{}", step.dir_debug());

    // if we already have a debug path, remove it
    if let Ok(true) = fs::try_exists(&debug_path).await {
        fs::remove_dir_all(&debug_path)
            .await
            .expect("remove debug dir should works");
    }

    let step_path = format!("{global_base_dir_str}/{}", step.dir());
    fs::rename(&step_path, &debug_path)
        .await
        .expect("rename dir should works");
    info!("renamed dir from {step_path} to {debug_path}");

    // create the step dir again
    fs::create_dir_all(&step_path)
        .await
        .expect("Create step dir should works");
    info!("created dir {step_path}");

    // Build list of needed files dynamically based on parachains
    let rc_spec = format!("{}-spec.json", rc.as_chain_string());
    let rc_snap = format!("{}-snap.tgz", rc.as_chain_string());
    let alice_snap = format!("alice-{}-snap.tgz", rc.as_chain_string());

    let mut needed_files: Vec<String> = vec!["config.toml".to_string(), rc_spec.clone()];

    // Add parachain files dynamically
    for para in paras {
        let para_chain_name = para.as_chain_string(&rc.as_chain_string());
        let para_spec = format!("{}-spec.json", para_chain_name);
        let para_snap = format!("{}-snap.tgz", para_chain_name);
        needed_files.push(para_spec);
        needed_files.push(para_snap);
    }

    if step == Step::Bite {
        needed_files.push(rc_snap);
    } else {
        needed_files.push(alice_snap);
    }

    for file in &needed_files {
        let from = format!("{debug_path}/{file}");
        let to = format!("{step_path}/{file}");
        info!("mv {from} {to}");
        fs::rename(&from, &to)
            .await
            .unwrap_or_else(|e| panic!("Failed to move {from} to {to}: {e}"));
    }

    Ok(())
}

async fn generate_config(
    relaychain: ChainArtifact,
    paras: Vec<ChainArtifact>,
    global_base_dir: Option<PathBuf>,
    database: &str,
) -> Result<NetworkConfig, String> {
    let leaked_rust_log = env::var("RUST_LOG_RC").unwrap_or_else(|_| {
        String::from(
            "babe=debug,grandpa=info,runtime=debug,consensus::common=debug,parachain=debug,parachain::gossip-support=info",
        )
    });

    let para_leaked_rust_log = env::var("RUST_LOG_COL").unwrap_or_else(|_| {
        String::from(
            "aura=debug,runtime=debug,cumulus-consensus=debug,consensus::common=debug,parachain::collation-generation=debug,parachain::collator-protocol=debug,parachain=debug,xcm=debug",
        )
    });

    let (chain_spec_path, db_path) = if let Ok(ci_path) = env::var("ZOMBIE_BITE_CI_PATH") {
        let chain_spec_path = PathBuf::from(relaychain.spec_path.as_str());
        let chain_spec_filename = chain_spec_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let db_path = PathBuf::from(relaychain.snap_path.as_str());
        let db_path_filename = db_path.file_name().unwrap().to_string_lossy().to_string();

        let new_chain_spec_path = PathBuf::from(&format!("{ci_path}/{}", chain_spec_filename));
        let new_db_path = PathBuf::from(&format!("{ci_path}/{}", db_path_filename));

        tokio::fs::rename(chain_spec_path, &new_chain_spec_path)
            .await
            .unwrap();
        tokio::fs::rename(db_path, &new_db_path).await.unwrap();

        (
            PathBuf::from(format!("./{}", chain_spec_filename)),
            PathBuf::from(format!("./{}", db_path_filename)),
        )
    } else {
        (
            PathBuf::from(relaychain.spec_path.as_str()),
            PathBuf::from(relaychain.snap_path.as_str()),
        )
    };

    // backward compatibility
    let rpc_alice_port: u16 = if let Ok(port) = env::var("ZOMBIE_BITE_RC_PORT") {
        port.parse()
            .expect("env var ZOMBIE_BITE_RC_PORT must be a valid u16")
    } else if let Ok(port) = env::var("ZOMBIE_BITE_ALICE_PORT") {
        port.parse()
            .expect("env var ZOMBIE_BITE_ALICE_PORT must be a valid u16")
    } else {
        get_random_port().await
    };

    let rpc_bob_port: u16 = if let Ok(port) = env::var("ZOMBIE_BITE_BOB_PORT") {
        port.parse()
            .expect("env var ZOMBIE_BITE_RC_PORT must be a valid u16")
    } else {
        get_random_port().await
    };

    // Alice + Bob + number of parachains
    let num_validators = (2 + paras.len()).min(7);

    // config a new network with dynamic validators
    let mut config = NetworkConfigBuilder::new().with_relaychain(|r| {
        let mut default_args = vec![
            ("-l", leaked_rust_log.as_str()).into(),
            "--discover-local".into(),
            "--allow-private-ip".into(),
            "--no-hardware-benchmarks".into(),
            ("--state-pruning", get_state_pruning_config().as_str()).into(),
            ("--database", database).into(),
        ];

        if let Ok(extra_args) = env::var("ZOMBIE_BITE_RC_EXTRA_ARGS") {
            for extra in extra_args.split(',') {
                default_args.push(extra.trim().into());
            }
        }

        let relay_builder = r
            .with_chain(relaychain.chain.as_str())
            .with_default_command(relaychain.cmd.as_str())
            .with_chain_spec_path(chain_spec_path)
            .with_default_db_snapshot(db_path)
            .with_default_args(default_args);

        {
            let mut relay_builder = relay_builder
                .with_validator(|node| node.with_name("alice").with_rpc_port(rpc_alice_port))
                .with_validator(|node| node.with_name("bob").with_rpc_port(rpc_bob_port));

            let additional_validators = ["charlie", "dave", "ferdie", "eve", "one"];
            // Spawn validators based on total count, accounting for alice and bob already added
            for name in additional_validators
                .iter()
                .take(num_validators.saturating_sub(2))
            {
                relay_builder = relay_builder.with_validator(|node| node.with_name(*name));
            }
            relay_builder
        }
    });

    if !paras.is_empty() {
        // TODO: enable for multiple paras
        // let validation_context = Rc::new(RefCell::new(ValidationContext::default()));
        for para in paras {
            // TODO: enable for multiple paras
            // let builder = ParachainConfigBuilder::new(validation_context);
            // let para_config = builder.with_id(1000)
            // .with_chain(para.chain.as_str())
            // .with_default_command(para.cmd.as_str())
            // .with_chain_spec_path(PathBuf::from(para.spec_path.as_str()))
            // .with_default_db_snapshot(PathBuf::from(para.snap_path.as_str()))
            // .with_collator(|c| c.with_name("col-1000"));

            let (chain_spec_path, db_path) = if let Ok(ci_path) = env::var("ZOMBIE_BITE_CI_PATH") {
                let chain_spec_path = PathBuf::from(para.spec_path.as_str());
                let chain_spec_filename = chain_spec_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();

                let db_path = PathBuf::from(para.snap_path.as_str());
                let db_path_filename = db_path.file_name().unwrap().to_string_lossy().to_string();

                let new_chain_spec_path =
                    PathBuf::from(&format!("{ci_path}/{}", chain_spec_filename));
                let new_db_path = PathBuf::from(&format!("{ci_path}/{}", db_path_filename));

                tokio::fs::rename(chain_spec_path, &new_chain_spec_path)
                    .await
                    .unwrap();
                tokio::fs::rename(db_path, &new_db_path).await.unwrap();

                (
                    PathBuf::from(format!("./{}", chain_spec_filename)),
                    PathBuf::from(format!("./{}", db_path_filename)),
                )
            } else {
                (
                    PathBuf::from(para.spec_path.as_str()),
                    PathBuf::from(para.snap_path.as_str()),
                )
            };

            let para_rpc_port: u16 = if let Ok(port) = env::var("ZOMBIE_BITE_AH_PORT") {
                port.parse()
                    .expect("env var ZOMBIE_BITE_AH_PORT must be a valid u16")
            } else {
                get_random_port().await
            };

            let mut para_default_args = vec![
                (
                    "--relay-chain-rpc-urls",
                    format!("ws://127.0.0.1:{rpc_alice_port}").as_str(),
                )
                    .into(),
                ("-l", para_leaked_rust_log.as_str()).into(),
                "--force-authoring".into(),
                "--discover-local".into(),
                "--allow-private-ip".into(),
                "--no-hardware-benchmarks".into(),
                ("--state-pruning", get_state_pruning_config().as_str()).into(),
                ("--database", database).into(),
            ];

            if let Ok(extra_args) = env::var("ZOMBIE_BITE_AH_EXTRA_ARGS") {
                for extra in extra_args.split(',') {
                    para_default_args.push(extra.trim().into());
                }
            }

            let para_id = para.para_id.expect("Para id should be available");
            let collator_name = format!("Collator-{}", para_id);

            config = config.with_parachain(|p| {
                let para_builder = p
                    .with_id(para_id)
                    .with_chain(para.chain.as_str())
                    .with_default_command(para.cmd.as_str())
                    .with_chain_spec_path(chain_spec_path)
                    .with_default_db_snapshot(db_path);

                para_builder.with_collator(|c| {
                    c.with_name(&collator_name)
                        .with_rpc_port(para_rpc_port)
                        .with_args(para_default_args)
                })
            })
        }
    }

    let config = if let Some(global_base_dir) = &global_base_dir {
        let fixed_base_dir = global_base_dir.canonicalize().unwrap().join("spawn");
        config.with_global_settings(|global_settings| {
            global_settings.with_base_dir(fixed_base_dir.to_string_lossy().to_string())
        })
    } else {
        config
    };

    let network_config = config.build().unwrap();
    Ok(network_config)
}

/// Spawn a new instance of the chain from a base_path and step.
pub async fn spawn(
    step: Step,
    base_path: &Path,
    maybe_custom_src_dir: Option<PathBuf>,
    _maybe_custom_dst_dir: Option<PathBuf>,
) -> Result<Network<LocalFileSystem>, anyhow::Error> {
    // spawn the network
    let filesystem = LocalFileSystem;
    let provider = NativeProvider::new(filesystem.clone());
    let orchestrator = Orchestrator::new(filesystem, provider);

    // by default spawn will always look at `bite` directory to spawn the new network
    // but this could be overriden with maybe_custom_src_dir
    let config_dir = if let Some(custom_dir) = maybe_custom_src_dir {
        custom_dir
    } else {
        PathBuf::from_str(&format!(
            "{}/{}",
            base_path.to_string_lossy(),
            step.dir_from()
        ))
        .expect("base_path should be valid")
    };

    let config_file = format!("{}/config.toml", config_dir.to_string_lossy());

    // localize if needed (change the content if needed)
    localize_config(&config_file).await?;
    info!("spawning from {config_file}");

    // ensure base_dir is correct in settings
    let base_dir = format!("{}/{}", base_path.to_string_lossy(), step.dir());
    let global_settings = zombienet_configuration::GlobalSettingsBuilder::new()
        .with_base_dir(&base_dir)
        .with_tear_down_on_failure(false)
        .build()
        .expect("global settings should work");

    let network_config = zombienet_configuration::NetworkConfig::load_from_toml_with_settings(
        &config_file,
        &global_settings,
    )
    .unwrap();

    orchestrator
        .spawn(network_config)
        .await
        .map_err(|e| anyhow!(e.to_string()))
}

async fn generate_snap(data_path: &str, snap_path: &str) -> Result<(), anyhow::Error> {
    info!("\n📝 Generating snapshot file {snap_path} with data_path {data_path}...");

    let compressed_file = File::create(snap_path).unwrap();
    let mut encoder = GzEncoder::new(compressed_file, Compression::fast());

    let mut archive = Builder::new(&mut encoder);
    archive.append_dir_all("data", data_path).unwrap();
    archive.finish().unwrap();

    info!("✅ generated with path {snap_path}");
    Ok(())
}

async fn generate_chain_spec(
    ns: DynNamespace,
    chain_spec_path: &str,
    cmd: &str,
    chain: &str,
) -> Result<(), String> {
    info!("\n📝 Generating chain-spec file {chain_spec_path} using cmd {cmd} with chain {chain} without bootnodes...");

    let temp_node = ns
        .spawn_node(
            &SpawnNodeOptions::new("temp-polkadot", "bash")
                .args(vec!["-c", "while :; do sleep 60; done"]),
        )
        .await
        .unwrap();

    let cmd_stdout = temp_node
        .run_command(RunCommandOptions::new(cmd).args(vec!["build-spec", "--chain", chain]))
        .await
        .unwrap()
        .unwrap();

    temp_node.destroy().await.unwrap();

    let mut chain_spec_json: serde_json::Value = serde_json::from_str(&cmd_stdout).unwrap();
    chain_spec_json["bootNodes"] = serde_json::Value::Array(vec![]);
    let contents = serde_json::to_string_pretty(&chain_spec_json).unwrap();

    tokio::fs::write(&chain_spec_path, contents).await.unwrap();
    info!("✅ generated with path {chain_spec_path}");

    Ok(())
}

async fn run_doppelganger_node(ns: DynNamespace, base_path: &Path) -> Result<(), String> {
    let data_path = format!("{}/sync_db", &base_path.to_string_lossy());
    let logs_path = format!("{}/sync.log", &base_path.to_string_lossy());
    info!(
        "⛓  Syncing using warp, this could take a while. You can follow the logs with: \n\t
    tail -f {}",
        &logs_path
    );

    let temp_node = ns
        .spawn_node(
            &SpawnNodeOptions::new("temp-doppelganger", "bash")
                .args(vec!["-c", "while :; do sleep 60; done"]),
        )
        .await
        .unwrap();

    let _stdout = temp_node
        .run_command(
            RunCommandOptions::new("bash")
                .args(vec![
                    "-c",
                    format!(
                        "doppelganger -l doppelganger=debug --chain kusama --sync warp -d {} > {} 2>&1",
                        &data_path, &logs_path
                    )
                    .as_str(),
                ])
                // Override rust log for sync
                .env(vec![("RUST_LOG", "")]),
        )
        .await
        .unwrap()
        .unwrap();

    temp_node.destroy().await.unwrap();

    info!("✅ Synced");

    Ok(())
}

fn get_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

#[cfg(test)]
mod test {
    use super::*;

    #[ignore = "Internal test, require some artifacts"]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_snap() {
        let snap_path = "/tmp/zombie-bite_1726677980197/snap.tgz";
        let demo = generate_snap("/tmp/zombie-bite_1726677980197", snap_path).await;
        // .unwrap();
        println!("{:?}", demo);
        // let _n = spawn(provider, chain_spec_path, snap_path).await.unwrap();
    }

    #[ignore = "Internal test, require some artifacts"]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_spawn() {
        // let filesystem = LocalFileSystem;
        // let provider = NativeProvider::new(filesystem.clone());
        // let r = ChainArtifact {
        //     cmd: "polkadot".into(),
        //     chain: "polkadot".into(),
        //     spec_path: "/tmp/zombie-bite_1730630215147/polkadot-spec.json".into(),
        //     snap_path: "/tmp/zombie-bite_1730630215147/polkadot-snap.tgz".into(),
        //     override_wasm: None,
        // };

        // let p = ChainArtifact {
        //     cmd: "polkadot-parachain".into(),
        //     chain: "asset-hub-polkadot".into(),
        //     spec_path: "/tmp/zombie-bite_1730630215147/asset-hub-polkadot-spec.json".into(),
        //     snap_path: "/tmp/zombie-bite_1730630215147/asset-hub-polkadot-snap.tgz".into(),
        //     override_wasm: None,
        // };

        let n = spawn(Step::Spawn, &PathBuf::new(), None, None)
            .await
            .unwrap();
        println!("{:?}", n);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    #[tokio::test]
    async fn test_generate_config() {
        std::env::set_var(
            "ZOMBIE_BITE_AH_EXTRA_ARGS",
            "--db-cache=24000, --trie-cache-size=24000, --runtime-cache-size=255",
        );

        // Create dummy chain spec and snapshot files
        let relay_spec_path = "/tmp/test-something.json";
        let relay_snap_path = "/tmp/test-something.tgz";
        let ah_spec_path = "/tmp/test-something-ah.json";
        let ah_snap_path = "/tmp/test-something-ah.tgz";

        // Minimal valid chain spec JSON
        let minimal_spec = r#"{ "genesis": { "runtime": { "session": { "keys": [] }, "babe": { "authorities": [] }, "grandpa": { "authorities": [] }, "aura": { "authorities": [] } } } }"#;
        std::fs::write(relay_spec_path, minimal_spec).unwrap();
        std::fs::write(ah_spec_path, minimal_spec).unwrap();
        std::fs::write(relay_snap_path, b"dummy").unwrap();
        std::fs::write(ah_snap_path, b"dummy").unwrap();

        let relay = ChainArtifact {
            cmd: "doppelganger".into(),
            chain: "polkadot".into(),
            spec_path: relay_spec_path.into(),
            snap_path: relay_snap_path.into(),
            override_wasm: None,
            para_id: None,
        };
        let ah = ChainArtifact {
            cmd: "doppelganger-parachain".into(),
            chain: "ah-polkadot".into(),
            spec_path: ah_spec_path.into(),
            snap_path: ah_snap_path.into(),
            override_wasm: None,
            para_id: Some(1000),
        };

        let network_config = generate_config(relay, vec![ah], None, "rocksdb")
            .await
            .unwrap();

        let toml = network_config.dump_to_toml().unwrap();
        assert!(toml.contains("--db-cache=24000"));
    }
}
