#![allow(dead_code)]
// TODO: don't allow dead_code

use anyhow::{anyhow, bail};
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
use tracing::{info, trace, warn};
use zombienet_configuration::shared::types::AssetLocation;
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

use crate::config::{
    get_assigned_cores, get_state_pruning_config, BiteOptions, Context, Parachain, Relaychain, Step,
};
use crate::manifest::{self, ChainEntry, Manifest};
use crate::metadata::ChainMetadata;
use crate::overrides::{generate_default_overrides_for_para, generate_default_overrides_for_rc};
use crate::sync::{sync_para, sync_relay_only};

use std::env;

const PORTS_FILE: &str = "ports.json";
pub const READY_FILE: &str = "ready.json";
const VALIDATOR_ENV: (&str, &str) = ("ZOMBIE_DISPUTE_CANDIDATE_LIFETIME_AFTER_FINALIZATION", "1");

#[derive(Debug, Clone)]
struct ChainArtifact {
    cmd: String,
    chain: String,
    spec_path: String,
    snap_path: String,
    /// Size of the snapshot, measured when it is written: later steps can move
    /// it (ZOMBIE_BITE_CI_PATH) and then it can no longer be stat'ed here.
    snap_bytes: Option<u64>,
    override_wasm: Option<String>,
    para_id: Option<u32>,
    /// Cores assigned to this parachain (0 for the relay).
    cores: u32,
}

pub async fn doppelganger_inner(
    global_base_dir: PathBuf,
    relay_chain: Relaychain,
    paras_to: Vec<Parachain>,
    database: &str,
    opts: &BiteOptions,
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
        let para_meta = match para
            .rpc_endpoint()
            .map(str::to_string)
            .or_else(|| para.default_rpc_endpoint(&relay_chain))
        {
            Some(url) => {
                ChainMetadata::fetch(&format!("para {}", para.id()), &url, para.at_block()).await
            }
            None => {
                warn!(
                    "para {}: no 'rpc_endpoint' configured, overrides will not be verified against the runtime",
                    para.id()
                );
                None
            }
        };
        let para_default_overrides_path = generate_default_overrides_for_para(
            &base_dir_str,
            para,
            &relay_chain,
            opts.upgrades.paras.get(&para.id()).map(String::as_str),
            para_meta.as_ref(),
            opts.keep_messaging_state,
        )
        .await?;
        let info_path = format!("{base_dir_str}/para-{}.txt", para.id());

        let maybe_target_header_path = if let Some(at_block) = para.at_block() {
            let para_rpc = para
                .rpc_endpoint()
                .expect("'rpc_endpoint' for parachain should be set when use 'bite_at' to get the target header. qed");
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
                para,
                &relay_chain,
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
        let para = paras_to
            .get(para_index)
            .expect("para_index should be valid. qed");

        // let sync_chain_name = if sync_chain.contains('/') {
        //     let parts: Vec<&str> = sync_chain.split('/').collect();
        //     let name_parts: Vec<&str> = parts.last().unwrap().split('.').collect();
        //     name_parts.first().unwrap().to_string()
        // } else {
        //     // is not a file
        //     sync_chain.clone()
        // };

        let sync_chain_name = para.as_chain_string(&relay_chain.as_chain_string());
        let chain_spec_path = format!("{}/{}-spec.json", base_dir_str, sync_chain_name);

        if para.is_custom() {
            // For custom paras, copy the user's chain spec and clear bootNodes
            // instead of running `doppelganger-parachain build-spec` which may not
            // understand arbitrary runtimes.
            let spec_content = tokio::fs::read_to_string(&sync_chain)
                .await
                .unwrap_or_else(|_| panic!("Failed to read custom chain spec: {}", sync_chain));
            let mut spec_json: serde_json::Value = serde_json::from_str(&spec_content)
                .unwrap_or_else(|_| {
                    panic!("Failed to parse custom chain spec JSON: {}", sync_chain)
                });
            spec_json["bootNodes"] = serde_json::Value::Array(vec![]);
            let contents = serde_json::to_string_pretty(&spec_json).unwrap();
            tokio::fs::write(&chain_spec_path, contents).await.unwrap();
        } else {
            generate_chain_spec(
                ns.clone(),
                &chain_spec_path,
                &context_para.doppelganger_cmd(),
                &sync_chain,
            )
            .await
            .unwrap();
        }

        // generate the data.tgz to use as snapshot
        let snap_path = format!("{}/{}-snap.tgz", base_dir_str, sync_chain_name);
        trace!("snap_path: {snap_path}");
        generate_snap(&sync_db_path, &snap_path).await.unwrap();
        let snap_bytes = fs::metadata(&snap_path).await.ok().map(|m| m.len());

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

        para_heads_env.push((
            format!("ZOMBIE_{}", &para_head_key(para.id())[2..]),
            para_head[2..].to_string(),
        ));

        para_artifacts.push(ChainArtifact {
            cmd: context_para.cmd(),
            chain: if sync_chain.contains('/') {
                para.as_chain_string(&relay_chain.as_chain_string())
            } else {
                sync_chain
            },
            spec_path: chain_spec_path,
            snap_path,
            snap_bytes,
            override_wasm: para.wasm_overrides().map(str::to_string),
            para_id: Some(para.id()),
            cores: get_assigned_cores(&relay_chain, para, &opts.cores),
        });
    }

    let req_cores: u32 = paras_to.iter().fold(0u32, |acc, para| {
        acc + get_assigned_cores(&relay_chain, para, &opts.cores)
    });
    let rc_meta = ChainMetadata::fetch(
        &relay_chain.as_chain_string(),
        &relay_chain.rpc_endpoint(),
        relay_chain.at_block(),
    )
    .await;
    let rc_default_overrides_path = generate_default_overrides_for_rc(
        &base_dir_str,
        &relay_chain,
        &paras_to,
        req_cores,
        opts.upgrades.relay.as_deref(),
        rc_meta.as_ref(),
        &opts.cores,
        opts.keep_messaging_state,
    )
    .await?;
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
        &relay_chain,
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
    let r_chain_spec_path = format!("{}/{}-spec.json", base_dir_str, sync_chain);
    generate_chain_spec(
        ns.clone(),
        &r_chain_spec_path,
        &context_relay.doppelganger_cmd(),
        &relay_chain.chain_arg(),
    )
    .await
    .unwrap();

    // remove `parachains` db
    // The node keeps its db under the chain-spec's own id, which is not always
    // the name we use for the artifacts.
    let sync_chain_in_path = if sync_chain == "kusama" {
        "ksmcc3".to_string()
    } else if sync_chain == "westend" {
        "westend2".to_string()
    } else {
        spec_chain_id(&r_chain_spec_path).await?
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
    let r_snap_path = format!("{}/{}-snap.tgz", base_dir_str, sync_chain);
    generate_snap(&sync_db_path, &r_snap_path).await.unwrap();
    let r_snap_bytes = fs::metadata(&r_snap_path).await.ok().map(|m| m.len());

    let relay_artifacts = ChainArtifact {
        // The relay validators must run the doppelganger binary: it honours
        // ZOMBIE_DISPUTE_CANDIDATE_LIFETIME_AFTER_FINALIZATION, without which
        // the stock dispute coordinator scans ancestor headers a warp-synced
        // bite does not have, never initializes, and caps finality at the bite
        // block forever while blocks keep being produced.
        cmd: context_relay.doppelganger_cmd(),
        chain: sync_chain,
        spec_path: r_chain_spec_path,
        snap_path: r_snap_path,
        snap_bytes: r_snap_bytes,
        override_wasm: relay_chain.wasm_overrides().map(str::to_string),
        para_id: None,
        cores: 0,
    };

    let config = generate_config(
        relay_artifacts.clone(),
        para_artifacts.clone(),
        Some(global_base_dir.clone()),
        database,
        req_cores,
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
    // The source rpc endpoints let the spawn step verify the fork actually
    // diverged from the network it was bitten from.
    let mut ready_content = json!({
        "rc_start_block": rc_start_block,
        "rc_source_rpc": relay_chain.rpc_endpoint(),
    });

    // Add all parachain start blocks
    for (key, value) in para_start_blocks {
        ready_content[key] = value;
    }
    for para in &paras_to {
        if let Some(source_rpc) = para.rpc_endpoint() {
            ready_content[format!("para_{}_source_rpc", para.id())] = json!(source_rpc);
        }
    }

    // Carried upgrade blobs live next to ready.json (outside the step dirs, so
    // they survive clean-up) and must match the seeded System::AuthorizedUpgrade.
    let global_base_dir_str = global_base_dir.to_string_lossy();
    if let Some(upgrade_wasm) = &opts.upgrades.relay {
        let blob_name = format!("{}-upgrade.wasm", relay_chain.as_chain_string());
        let (blob, hash) = copy_upgrade_blob(upgrade_wasm, &global_base_dir_str, &blob_name).await;
        ready_content["rc_upgrade_wasm"] = json!(blob);
        ready_content["rc_upgrade_hash"] = json!(hash);
    }
    for para in &paras_to {
        if let Some(upgrade_wasm) = opts.upgrades.paras.get(&para.id()) {
            let blob_name = format!(
                "{}-upgrade.wasm",
                para.as_chain_string(&relay_chain.as_chain_string())
            );
            let (blob, hash) =
                copy_upgrade_blob(upgrade_wasm, &global_base_dir_str, &blob_name).await;
            ready_content[format!("para_{}_upgrade_wasm", para.id())] = json!(blob);
            ready_content[format!("para_{}_upgrade_hash", para.id())] = json!(hash);
        }
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

    let manifest = build_manifest(
        &relay_chain,
        &paras_to,
        &ready_content,
        &relay_artifacts,
        &para_artifacts,
    );
    manifest.write(&global_base_dir).await?;

    clean_up_dir_for_step(global_base_dir, Step::Bite, &relay_chain, &paras_to).await?;

    Ok(())
}

/// `id` of a chain-spec, which is the directory the node stores its db under.
async fn spec_chain_id(spec_path: &str) -> Result<String, anyhow::Error> {
    let content = fs::read_to_string(spec_path)
        .await
        .map_err(|e| anyhow!("can't read chain-spec {spec_path}: {e}"))?;
    let spec: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow!("chain-spec {spec_path} is not valid json: {e}"))?;
    spec["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("chain-spec {spec_path} has no 'id'"))
}

fn build_manifest(
    relay_chain: &Relaychain,
    paras_to: &[Parachain],
    ready: &serde_json::Value,
    relay_artifacts: &ChainArtifact,
    para_artifacts: &[ChainArtifact],
) -> Manifest {
    let file_name = |path: &str| {
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    };

    let relay = ChainEntry {
        chain: relay_chain.as_chain_string(),
        para_id: None,
        bite_block: ready["rc_start_block"].as_u64(),
        source_rpc: ready["rc_source_rpc"].as_str().map(str::to_string),
        spec_file: file_name(&relay_artifacts.spec_path),
        snapshot_file: file_name(&relay_artifacts.snap_path),
        snapshot_bytes: relay_artifacts.snap_bytes,
        upgrade_file: ready["rc_upgrade_wasm"].as_str().map(str::to_string),
        upgrade_hash: ready["rc_upgrade_hash"].as_str().map(str::to_string),
    };

    // para_artifacts is built in paras_to order, so zip keeps them aligned.
    let parachains = paras_to
        .iter()
        .zip(para_artifacts)
        .map(|(para, artifact)| {
            let id = para.id();
            ChainEntry {
                chain: para.as_chain_string(&relay_chain.as_chain_string()),
                para_id: Some(id),
                bite_block: ready[format!("para_{id}_start_block")].as_u64(),
                source_rpc: ready[format!("para_{id}_source_rpc")]
                    .as_str()
                    .map(str::to_string),
                spec_file: file_name(&artifact.spec_path),
                snapshot_file: file_name(&artifact.snap_path),
                snapshot_bytes: artifact.snap_bytes,
                upgrade_file: ready[format!("para_{id}_upgrade_wasm")]
                    .as_str()
                    .map(str::to_string),
                upgrade_hash: ready[format!("para_{id}_upgrade_hash")]
                    .as_str()
                    .map(str::to_string),
            }
        })
        .collect();

    Manifest {
        version: manifest::VERSION,
        bundle: Step::Bite.dir(),
        created_at: manifest::now_unix(),
        relay,
        parachains,
    }
}

async fn copy_upgrade_blob(from: &str, base_dir: &str, blob_name: &str) -> (String, String) {
    let wasm = fs::read(from)
        .await
        .unwrap_or_else(|_| panic!("Error reading upgrade wasm from path {from}"));
    fs::write(format!("{base_dir}/{blob_name}"), &wasm)
        .await
        .expect("write upgrade blob should works");
    let hash = format!("0x{}", hex::encode(subhasher::blake2_256(&wasm[..])));
    (blob_name.to_string(), hash)
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

    // The overrides that were applied are part of the bundle: without them a
    // restored bite cannot show what was changed in the state it carries.
    if step == Step::Bite {
        needed_files.push("rc_overrides.json".to_string());
    }

    // Add parachain files dynamically
    for para in paras {
        let para_chain_name = para.as_chain_string(&rc.as_chain_string());
        let para_spec = format!("{}-spec.json", para_chain_name);
        let para_snap = format!("{}-snap.tgz", para_chain_name);
        needed_files.push(para_spec);
        needed_files.push(para_snap);
        if step == Step::Bite {
            needed_files.push(format!("{}_overrides.json", para.id()));
        }
    }

    if step == Step::Bite {
        needed_files.push(rc_snap);
    } else {
        needed_files.push(alice_snap);
    }

    // Overrides are only there when this step generated them; a missing
    // spec or snapshot below is still a hard error.
    let mut present = vec![];
    for file in needed_files {
        if file.ends_with("_overrides.json")
            && !fs::try_exists(format!("{debug_path}/{file}")).await?
        {
            warn!("{file} not found, it will not be part of the bundle");
            continue;
        }
        present.push(file);
    }
    let needed_files = present;

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
    req_cores: u32,
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

    // Must match the validator set installed by the state overrides.
    let num_validators = crate::config::num_validators_for_cores(req_cores) as usize;

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
                .with_validator(|node| {
                    node.with_name("alice")
                        .with_rpc_port(rpc_alice_port)
                        .with_env(vec![VALIDATOR_ENV])
                })
                .with_validator(|node| {
                    node.with_name("bob")
                        .with_rpc_port(rpc_bob_port)
                        .with_env(vec![VALIDATOR_ENV])
                });

            let additional_validators = ["charlie", "dave", "ferdie", "eve", "one"];
            // Spawn validators based on total count, accounting for alice and bob already added
            for name in additional_validators
                .iter()
                .take(num_validators.saturating_sub(2))
            {
                relay_builder = relay_builder
                    .with_validator(|node| node.with_name(*name).with_env(vec![VALIDATOR_ENV]));
            }
            relay_builder
        }
    });

    if !paras.is_empty() {
        for para in paras {
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

            // Elastic scaling (more than one core) requires slot-based
            // authoring, whatever the parachain is called.
            if para.chain.contains("asset-hub") || para.cores > 1 {
                para_default_args.push("--authoring=slot-based".into());
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

    validate_parachain_specs(&network_config).await?;

    orchestrator
        .spawn(network_config)
        .await
        .map_err(|e| anyhow!(e.to_string()))
}

/// Two parachains resolving to the same chain-spec identity share one spec
/// (last one wins) and every collator silently runs the same chain. The
/// identity is `chain` when set, else the `id` of the supplied chain-spec,
/// which is what zombienet uses when no `chain` is given.
async fn validate_parachain_specs(
    network_config: &zombienet_configuration::NetworkConfig,
) -> Result<(), anyhow::Error> {
    let mut seen: Vec<(String, u32)> = vec![];
    for para in network_config.parachains() {
        let identity = if let Some(chain) = para.chain() {
            chain.as_str().to_string()
        } else if let Some(AssetLocation::FilePath(path)) = para.chain_spec_path() {
            let spec = fs::read_to_string(path)
                .await
                .map_err(|e| anyhow!("parachain {}: can't read chain-spec: {e}", para.id()))?;
            let spec: serde_json::Value = serde_json::from_str(&spec)
                .map_err(|e| anyhow!("parachain {}: invalid chain-spec json: {e}", para.id()))?;
            spec["id"]
                .as_str()
                .ok_or_else(|| {
                    anyhow!(
                        "parachain {} has neither 'chain' nor an 'id' in its chain-spec",
                        para.id()
                    )
                })?
                .to_string()
        } else {
            continue;
        };

        if let Some((_, other)) = seen.iter().find(|(seen, _)| *seen == identity) {
            bail!(
                "parachains {other} and {} both resolve to chain '{identity}', so they would share one chain spec",
                para.id()
            );
        }
        seen.push((identity, para.id()));
    }
    Ok(())
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
    let data_path = format!("{}/sync_db", base_path.to_string_lossy());
    let logs_path = format!("{}/sync.log", base_path.to_string_lossy());
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
                        data_path, logs_path
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

    fn config_with_paras(
        paras: Vec<(u32, Option<&str>, Option<&str>)>, // (id, chain, chain_spec_path)
    ) -> NetworkConfig {
        let mut config = NetworkConfigBuilder::new().with_relaychain(|r| {
            r.with_chain("polkadot")
                .with_default_command("polkadot")
                .with_validator(|node| node.with_name("alice"))
                .with_validator(|node| node.with_name("bob"))
        });

        for (id, chain, spec_path) in paras {
            config = config.with_parachain(|p| {
                let mut p = p.with_id(id).with_default_command("polkadot-parachain");
                if let Some(chain) = chain {
                    p = p.with_chain(chain);
                }
                if let Some(spec_path) = spec_path {
                    p = p.with_chain_spec_path(spec_path);
                }
                p.with_collator(|c| c.with_name(format!("col-{id}").as_str()))
            });
        }

        config.build().unwrap()
    }

    async fn write_spec(path: &str, id: &str) {
        fs::write(path, format!(r#"{{"id":"{id}"}}"#))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn validate_rejects_duplicated_chain_names() {
        let config = config_with_paras(vec![
            (1000, Some("asset-hub"), Some("/tmp/a.json")),
            (1005, Some("asset-hub"), Some("/tmp/b.json")),
        ]);
        let err = validate_parachain_specs(&config).await.unwrap_err();
        assert!(
            err.to_string().contains("share one chain spec"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn validate_accepts_unique_chains() {
        let config = config_with_paras(vec![
            (1000, Some("asset-hub"), Some("/tmp/a.json")),
            (1005, Some("coretime"), Some("/tmp/b.json")),
        ]);
        assert!(validate_parachain_specs(&config).await.is_ok());
    }

    #[tokio::test]
    async fn validate_uses_spec_id_when_chain_is_absent() {
        let (dup_a, dup_b) = ("/tmp/zb-dup-a.json", "/tmp/zb-dup-b.json");
        write_spec(dup_a, "asset-hub-kusama").await;
        write_spec(dup_b, "asset-hub-kusama").await;

        let config = config_with_paras(vec![(1000, None, Some(dup_a)), (1005, None, Some(dup_b))]);
        let err = validate_parachain_specs(&config).await.unwrap_err();
        assert!(err.to_string().contains("asset-hub-kusama"), "got: {err}");

        write_spec(dup_b, "coretime-kusama").await;
        let config = config_with_paras(vec![(1000, None, Some(dup_a)), (1005, None, Some(dup_b))]);
        assert!(validate_parachain_specs(&config).await.is_ok());
    }

    #[tokio::test]
    async fn test_generate_config() {
        // test extra args in env
        unsafe {
            std::env::set_var(
                "ZOMBIE_BITE_AH_EXTRA_ARGS",
                "--db-cache=24000, --trie-cache-size=24000, --runtime-cache-size=255",
            );
        }

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
            snap_bytes: None,
            override_wasm: None,
            para_id: None,
            cores: 0,
        };
        let ah = ChainArtifact {
            cmd: "doppelganger-parachain".into(),
            chain: "ah-polkadot".into(),
            spec_path: ah_spec_path.into(),
            snap_path: ah_snap_path.into(),
            snap_bytes: None,
            override_wasm: None,
            para_id: Some(1000),
            cores: 3,
        };

        let network_config = generate_config(relay, vec![ah], None, "rocksdb", 3)
            .await
            .unwrap();

        let toml = network_config.dump_to_toml().unwrap();
        assert!(toml.contains("--db-cache=24000"));
    }
}
