//! Post-spawn verification that the spawned network is a healthy, *diverged*
//! fork of the source network (see #119).

use std::{path::Path, time::Duration};

use anyhow::{anyhow, bail};
use serde_json::Value;
use tokio::fs;
use tracing::{info, warn};
use zombienet_sdk::{
    subxt::ext::subxt_rpcs::{client::RpcParams, RpcClient},
    LocalFileSystem, Network, NetworkNode,
};

const FINALIZED_METRIC: &str = "block_height{status=\"finalized\"}";
/// Blocks past the bite block at which fork and source hashes are compared.
const DIVERGENCE_OFFSET: u64 = 5;
/// A parachain block is only final once the relay block carrying its candidate
/// is finalized by GRANDPA; the first step takes ~68-120s after spawn.
const FINALITY_PRIME_TIMEOUT_SECS: u64 = 300;
/// How long to wait for the fork to produce the block used for the divergence
/// comparison.
const DIVERGENCE_TIMEOUT_SECS: u64 = 600;

/// Wait until the node's finalized height advances past its value at spawn
/// (the bitten snapshot already carries the source's finalized height, so an
/// absolute threshold would pass without any new finality).
async fn wait_first_finality_step(node: &NetworkNode) -> Result<(), anyhow::Error> {
    let initial = node.reports(FINALIZED_METRIC).await?;
    node.wait_metric_with_timeout(FINALIZED_METRIC, |x| x > initial, FINALITY_PRIME_TIMEOUT_SECS)
        .await
        .map_err(|e| {
            anyhow!(
                "{}: finalized height did not advance past {initial} within {FINALITY_PRIME_TIMEOUT_SECS}s: {e}",
                node.name()
            )
        })?;
    info!("✅ {} finality is advancing", node.name());
    Ok(())
}

/// Wait for the first finality step on the relaychain and on every parachain.
pub async fn wait_finality_primed(network: &Network<LocalFileSystem>) -> Result<(), anyhow::Error> {
    let alice = network.get_node("alice").map_err(|e| anyhow!("{e}"))?;
    wait_first_finality_step(alice).await?;

    for para in network.parachains() {
        if let Some(collator) = para.collators().first() {
            wait_first_finality_step(collator).await?;
        }
    }

    Ok(())
}

async fn block_hash(client: &RpcClient, number: u64) -> Result<Option<String>, anyhow::Error> {
    let mut params = RpcParams::new();
    params.push(number)?;
    let hash = client
        .request::<Option<String>>("chain_getBlockHash", params)
        .await?;
    Ok(hash)
}

/// Assert the fork diverged from the network it was bitten from: the block
/// hash at `bite_block + DIVERGENCE_OFFSET` must differ between fork and
/// source. Equal hashes mean the fork reached the source network and is
/// following *its* chain (e.g. a production bootnode was reintroduced).
///
/// An unreachable source is a skip (with a warning), not a failure: the spawn
/// may legitimately run on a machine without access to the source network.
pub async fn assert_diverged(
    chain: &str,
    fork_ws_uri: &str,
    source_rpc_url: &str,
    bite_block: u64,
) -> Result<(), anyhow::Error> {
    let target_block = bite_block + DIVERGENCE_OFFSET;

    let fork = RpcClient::from_insecure_url(fork_ws_uri)
        .await
        .map_err(|e| anyhow!("{chain}: can't connect to fork at {fork_ws_uri}: {e}"))?;

    // Wait until the fork produced the comparison block.
    let started = std::time::Instant::now();
    let fork_hash = loop {
        if let Some(hash) = block_hash(&fork, target_block).await? {
            break hash;
        }
        if started.elapsed().as_secs() > DIVERGENCE_TIMEOUT_SECS {
            bail!(
                "{chain}: fork did not reach block {target_block} (bite block {bite_block} + {DIVERGENCE_OFFSET}) within {DIVERGENCE_TIMEOUT_SECS}s"
            );
        }
        tokio::time::sleep(Duration::from_secs(6)).await;
    };

    let source = match RpcClient::from_url(source_rpc_url).await {
        Ok(client) => client,
        Err(e) => {
            warn!("{chain}: source {source_rpc_url} unreachable, skipping divergence check: {e}");
            return Ok(());
        }
    };
    let source_hash = match block_hash(&source, target_block).await {
        Ok(Some(hash)) => hash,
        Ok(None) => {
            warn!("{chain}: source has no block {target_block} yet, skipping divergence check");
            return Ok(());
        }
        Err(e) => {
            warn!("{chain}: error querying source, skipping divergence check: {e}");
            return Ok(());
        }
    };

    if fork_hash == source_hash {
        bail!(
            "{chain}: NOT a fork - block {target_block} hash {fork_hash} matches the source network; the fork is following production (a source bootnode was likely reintroduced)"
        );
    }

    info!("✅ {chain} diverged from source at block {target_block} (fork {fork_hash}, source {source_hash})");
    Ok(())
}

/// Post-spawn fork verification: first finality step on every chain, then
/// divergence from the source keyed on the bite block recorded in ready.json.
pub async fn verify_fork(
    network: &Network<LocalFileSystem>,
    base_path: &Path,
) -> Result<(), anyhow::Error> {
    wait_finality_primed(network).await?;

    let ready_path = base_path.join(crate::doppelganger::READY_FILE);
    let ready: Value = match fs::read_to_string(&ready_path).await {
        Ok(content) => serde_json::from_str(&content)?,
        Err(_) => {
            warn!(
                "{} not found, skipping divergence checks",
                ready_path.to_string_lossy()
            );
            return Ok(());
        }
    };

    match (
        ready["rc_start_block"].as_u64(),
        ready["rc_source_rpc"].as_str(),
    ) {
        (Some(bite_block), Some(source_rpc)) => {
            let alice = network.get_node("alice").map_err(|e| anyhow!("{e}"))?;
            assert_diverged("relaychain", alice.ws_uri(), source_rpc, bite_block).await?;
        }
        _ => warn!("no rc bite block/source rpc recorded, skipping relay divergence check"),
    }

    for para in network.parachains() {
        let id = para.para_id();
        let (bite_block, source_rpc) = (
            ready[format!("para_{id}_start_block")].as_u64(),
            ready[format!("para_{id}_source_rpc")].as_str(),
        );
        match (bite_block, source_rpc, para.collators().first()) {
            (Some(bite_block), Some(source_rpc), Some(collator)) => {
                assert_diverged(
                    &format!("para {id}"),
                    collator.ws_uri(),
                    source_rpc,
                    bite_block,
                )
                .await?;
            }
            _ => warn!("para {id}: no bite block/source rpc recorded, skipping divergence check"),
        }
    }

    Ok(())
}
