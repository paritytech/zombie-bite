//! Lifecycle of a spawned network: startup checks, the run loop and teardown.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use futures::StreamExt;
use tokio::fs;
use tracing::{debug, info, trace, warn};
use zombienet_sdk::{LocalFileSystem, Network, NetworkNode};

use crate::{
    bootnodes,
    config::{Relaychain, Step},
    doppelganger,
    monit::monit_progress,
};

/// Signal for spawn to 'stop' and generate the artifacts
pub const STOP_FILE: &str = "stop.txt";

pub async fn resolve_if_dir_exist(base_path: &Path, step: Step) -> Result<(), anyhow::Error> {
    let base_path_str = base_path.to_string_lossy();
    let path_to_use = format!("{base_path_str}/{}", step.dir());
    let mut path_with_suffix = format!("{base_path_str}/{}", step.dir());
    let mut suffix = 0;
    // check if the `spawn` fir exist and if exist mv to `.n` starting from 0
    info!("checking {path_with_suffix}");
    while let Ok(true) = fs::try_exists(&path_with_suffix).await {
        trace!("suffix {suffix}");
        path_with_suffix = format!("{base_path_str}/{}.{suffix}", step.dir());
        suffix += 1;
    }

    if path_to_use != path_with_suffix {
        // spawn exist and we need to move the content
        warn!("'{}' dir exist, moving to {path_with_suffix}", step.dir());
        fs::rename(&path_to_use, &path_with_suffix).await?;
    }

    Ok(())
}

pub async fn ensure_startup_producing_blocks(
    network: &Network<LocalFileSystem>,
) -> Result<(), anyhow::Error> {
    // Check metrics for all parachains and their collators
    let parachains = network.parachains();
    for para in parachains {
        for collator in para.collators() {
            debug!("Waiting metrics for collator {}", collator.name());
            collator
                .wait_metric_with_timeout("node_roles", |x| x > 1.0, 300_u64)
                .await?;
        }
    }

    // ensure block production
    let client = network
        .get_node("alice")?
        .wait_client::<zombienet_sdk::subxt::PolkadotConfig>()
        .await?;
    let mut blocks = client.blocks().subscribe_finalized().await?.take(3);

    while let Some(block) = blocks.next().await {
        info!("Block #{}", block?.header().number);
    }

    info!("🚀🚀🚀 network is up and running...");

    Ok(())
}

pub async fn post_spawn_loop(
    stop_file: &str,
    network: &Network<LocalFileSystem>,
    with_monitor: bool,
) -> Result<(), anyhow::Error> {
    if with_monitor {
        let alice = network.get_node("alice")?;
        let bob = network.get_node("bob")?;

        let parachains = network.parachains();
        let collator_opt: Option<&NetworkNode> = parachains
            .first()
            .and_then(|para| para.collators().first().copied());

        if let Some(col) = collator_opt {
            debug!("Will monitor collator {} for progress...", col.name());
        } else {
            debug!("No collator found, monitoring only validators");
        }

        monit_progress(alice, bob, collator_opt, Some(stop_file)).await;
    } else {
        while let Ok(false) = fs::try_exists(&stop_file).await {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    Ok(())
}

pub async fn tear_down_and_generate(
    stop_file: &str,
    step: Step,
    network: Network<LocalFileSystem>,
    base_path: PathBuf,
    publish_bootnodes: Option<String>,
) -> Result<(), anyhow::Error> {
    let rc = Relaychain::new(network.relaychain().chain());
    // Addresses have to be read while the nodes are still up, but they are
    // written after the artifacts are generated, so the bite bundle stays as it
    // was and only the published one advertises this run's nodes.
    let bootnodes = publish_bootnodes
        .as_ref()
        .map(|_| bootnodes::collect(&network));
    let _ = network.destroy().await;
    let teardown_signal = fs::try_exists(&stop_file).await;

    if let Ok(true) = teardown_signal {
        // create the artifacts
        doppelganger::generate_artifacts(base_path.clone(), step, &rc).await?;
        doppelganger::clean_up_dir_for_step(base_path.clone(), step, &rc, &[]).await?;

        if let (Some(host), Some(chains)) = (publish_bootnodes, bootnodes) {
            let spec_dir = base_path.join(step.dir());
            bootnodes::publish(&chains, &spec_dir, &host).await?;
        }
    } else if publish_bootnodes.is_some() {
        warn!("--publish-bootnodes: no teardown signal, so no artifacts were generated to publish into");
    }

    // signal that the teardown is completed
    _ = fs::remove_file(stop_file).await;

    Ok(())
}
