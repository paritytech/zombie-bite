#![allow(dead_code)]
// TODO: don't allow dead_code

use std::{
    io::{self, Cursor, Write},
    path::PathBuf,
    time::Duration,
};

use crate::config::get_state_pruning_config;
use crate::utils::get_random_port;

use reqwest::Url;
use tracing::{debug, error, info, trace};
use zombienet_orchestrator::metrics::{Metrics, MetricsHelper};
use zombienet_provider::{types::SpawnNodeOptions, DynNamespace, DynNode};
use zombienet_support::net::wait_ws_ready;

const PASEO_ASSET_HUB_SPEC_URL: &str =
    "https://paseo-r2.zondax.ch/chain-specs/paseo-asset-hub.json";

#[allow(clippy::too_many_arguments)]
pub async fn sync_relay_only(
    ns: DynNamespace,
    cmd: impl AsRef<str>,
    chain: impl AsRef<str>,
    para_heads_env: Vec<(String, String)>,
    overrides_path: PathBuf,
    info_path: impl AsRef<str>,
    maybe_target_header: Option<String>,
    database: &str,
) -> Result<(DynNode, String, String), ()> {
    debug!("paras: \n {:?}", para_heads_env);
    let sync_db_path = format!("{}/sync-db", ns.base_dir().to_string_lossy());

    let mut env = if std::env::var("ZOMBIE_DUMP").is_ok() {
        [
            para_heads_env,
            vec![("ZOMBIE_DUMP".to_string(), "1".to_string())],
        ]
        .concat()
    } else {
        para_heads_env
    };

    if let Some(target_header_path) = maybe_target_header {
        env.push(("ZOMBIE_TARGET_HEADER_PATH".into(), target_header_path));
    }

    let rc_overrides_path = overrides_path.to_string_lossy().to_string();
    env.push(("ZOMBIE_RC_OVERRIDES_PATH".to_string(), rc_overrides_path));
    env.push(("RUST_LOG".into(), "doppelganger=debug".into()));
    env.push(("ZOMBIE_INFO_PATH".into(), info_path.as_ref().into()));
    if chain.as_ref() == "paseo" || chain.as_ref() == "kusama" {
        env.push(("ZOMBIE_RC_EPOCH_DURATION".into(), "600".into()));
    }

    trace!("env: {env:?}");

    let metrics_random_port = get_random_port().await;
    let opts = SpawnNodeOptions::new("sync-node", cmd.as_ref())
        .args(vec![
            "--chain",
            chain.as_ref(),
            "--sync",
            "warp",
            "-d",
            &sync_db_path,
            "--prometheus-port",
            &metrics_random_port.to_string(),
            "--no-hardware-benchmarks",
            // needed to not drop the pre-migration state
            "--state-pruning",
            get_state_pruning_config().as_str(),
            "--database",
            database,
        ])
        .env(env);

    info!("🔎 sync node opts: {:?}", opts);
    let sync_node = ns.spawn_node(&opts).await.unwrap();
    let metrics_url = format!("http://127.0.0.1:{metrics_random_port}/metrics");

    debug!("prometheus link http://127.0.0.1:{metrics_random_port}/metrics");
    info!("📓 sync node logs: {}", sync_node.log_cmd());

    wait_ws_ready(&metrics_url).await.unwrap();
    let url = reqwest::Url::try_from(metrics_url.as_str()).unwrap();
    wait_sync(url).await.unwrap();
    info!("✅ Synced (chain: {})", chain.as_ref());
    // we should just paused
    Ok((sync_node, sync_db_path, chain.as_ref().to_string()))
}

#[allow(clippy::too_many_arguments)]
pub async fn sync_para(
    ns: DynNamespace,
    cmd: impl AsRef<str>,
    chain: impl AsRef<str>,
    relaychain: impl AsRef<str>,
    relaychain_endpoint: impl AsRef<str>,
    overrides_path: PathBuf,
    info_path: impl AsRef<str>,
    maybe_target_header: Option<String>,
    database: &str,
) -> Result<(DynNode, String, String, String), ()> {
    let sync_db_path = format!(
        "{}/paras/{}/sync-db",
        ns.base_dir().to_string_lossy(),
        chain.as_ref()
    );

    let para_head_path = format!(
        "{}/paras/{}/head.txt",
        ns.base_dir().to_string_lossy(),
        chain.as_ref()
    );

    let rpc_random_port = get_random_port().await;
    let metrics_random_port = get_random_port().await;
    let mut env = if std::env::var("ZOMBIE_DUMP").is_ok() {
        vec![("ZOMBIE_DUMP", "1")]
    } else {
        vec![]
    };

    if let Some(target_header_path) = maybe_target_header.as_ref() {
        env.push(("ZOMBIE_TARGET_HEADER_PATH", target_header_path));
    }

    let para_overrides_path = overrides_path.to_string_lossy().to_string();
    env.push(("ZOMBIE_PARA_OVERRIDES_PATH", &para_overrides_path));
    env.push(("ZOMBIE_PARA_HEAD_PATH", &para_head_path));
    env.push(("RUST_LOG", "doppelganger=debug"));
    env.push(("ZOMBIE_INFO_PATH", info_path.as_ref()));

    trace!("env: {env:?}");

    let dest_for_paseo = format!("{}/asset-hub-paseo.json", ns.base_dir().to_string_lossy(),);
    let chain_arg = if chain.as_ref() == "asset-hub-paseo" {
        // get chain spec from https://paseo-r2.zondax.ch/chain-specs/paseo-asset-hub.json
        let response = reqwest::get(PASEO_ASSET_HUB_SPEC_URL)
            .await
            .unwrap_or_else(|_| panic!("Create file {dest_for_paseo} should work"));
        let mut file = std::fs::File::create(&dest_for_paseo)
            .unwrap_or_else(|_| panic!("Create file {dest_for_paseo} should work"));
        let mut content = Cursor::new(response.bytes().await.expect("Create cursor should works."));
        std::io::copy(&mut content, &mut file).expect("Copy bytes should works.");
        dest_for_paseo.as_str()
    } else {
        chain.as_ref()
    };

    let unique_node_name = format!("sync-node-para-{}", chain.as_ref().replace("-", "_"));
    info!(
        "🔄 Starting sync for parachain: {} with unique node name: {}",
        chain.as_ref(),
        unique_node_name
    );

    let opts = SpawnNodeOptions::new(unique_node_name.as_str(), cmd.as_ref())
        .args(vec![
            "--chain",
            chain_arg,
            "--sync",
            "warp",
            "-d",
            &sync_db_path,
            "--rpc-port",
            &rpc_random_port.to_string(),
            "--prometheus-port",
            &metrics_random_port.to_string(),
            "--relay-chain-rpc-url",
            relaychain_endpoint.as_ref(),
            "--no-hardware-benchmarks",
            // needed to not drop the pre-migration state
            "--state-pruning",
            get_state_pruning_config().as_str(),
            "--database",
            database,
            "--",
            "--chain",
            relaychain.as_ref(),
        ])
        .env(env);

    info!("🔎 sync para opts: {:?}", opts);
    let sync_node = ns.spawn_node(&opts).await.unwrap();
    let metrics_url = format!("http://127.0.0.1:{metrics_random_port}/metrics");

    debug!("prometheus link http://127.0.0.1:{metrics_random_port}/metrics");
    info!("📓 sync para logs: {}", sync_node.log_cmd());

    wait_ws_ready(&metrics_url).await.unwrap();
    let url = reqwest::Url::try_from(metrics_url.as_str()).unwrap();

    match wait_sync(url).await {
        Ok(_) => info!("✅ Synced (chain: {}), stopping node.", chain.as_ref()),
        Err(e) => {
            error!("❌ Sync failed for parachain {}: {}", chain.as_ref(), e);
            return Err(());
        }
    }
    // we should just paused
    // sync_node.destroy().await.unwrap();
    Ok((
        sync_node,
        sync_db_path,
        //chain.as_ref().to_string(),
        chain_arg.to_string(),
        para_head_path,
    ))
}

// TODO: FIX terminal output on multiple tasks
async fn wait_sync(url: impl Into<Url>) -> Result<(), anyhow::Error> {
    const TERMINAL_WIDTH: u32 = 80;
    let url = url.into();

    print!("Syncing");
    let mut q = TERMINAL_WIDTH;
    // remove the first message
    q -= 7;

    while is_syncing(url.clone()).await {
        if q == 0 {
            print!("\x1b[2K"); // Clear the whole line
            print!("\x1b[80D"); // Move to the start of the line
            print!("Syncing");
            io::stdout().flush().unwrap();
            q = TERMINAL_WIDTH - 7;
        }
        print!(".");
        io::stdout().flush().unwrap();
        q -= 1;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // ensure new line
    println!();

    Ok(())
}

async fn is_syncing(url: Url) -> bool {
    let metric = Metrics::metric_with_url("substrate_sub_libp2p_is_major_syncing", url).await;
    if let Ok(m) = metric {
        m == 1_f64
    } else {
        // Error getting metric
        false
    }
}
