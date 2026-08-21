//! Enact a carried runtime upgrade through the production path.
//!
//! The bite seeds `System::AuthorizedUpgrade` with the blob's hash, so
//! `apply_authorized_upgrade(blob)` is a valid *unsigned* extrinsic (validated
//! by `frame_system`'s `ValidateUnsigned`) - no sudo or root origin needed,
//! which is what makes this work on Kusama/Polkadot forks. For parachains the
//! call goes through the full cumulus flow: relay-side PVF pre-checking,
//! go-ahead signal, and enactment.

use std::{path::Path, time::Duration};

use anyhow::{anyhow, bail};
use serde_json::Value;
use tokio::fs;
use tracing::{info, warn};
use zombienet_sdk::{
    subxt::{
        dynamic::{tx, Value as TxValue},
        ext::subxt_rpcs::{client::RpcParams, RpcClient},
        OnlineClient, PolkadotConfig,
    },
    LocalFileSystem, Network,
};

/// Parachain upgrades wait for relay-side PVF pre-checking plus the go-ahead
/// signal before enacting.
const UPGRADE_TIMEOUT_SECS: u64 = 900;

async fn spec_version(client: &RpcClient) -> Result<u64, anyhow::Error> {
    let version: Value = client
        .request("state_getRuntimeVersion", RpcParams::new())
        .await?;
    version["specVersion"]
        .as_u64()
        .ok_or_else(|| anyhow!("specVersion missing in state_getRuntimeVersion response"))
}

/// Submit `apply_authorized_upgrade(blob)` unsigned and wait until the chain
/// reports a higher spec version.
pub async fn apply_authorized_upgrade(
    chain: &str,
    ws_uri: &str,
    wasm_path: &Path,
) -> Result<(), anyhow::Error> {
    let code = fs::read(wasm_path)
        .await
        .map_err(|e| anyhow!("{chain}: can't read upgrade blob {wasm_path:?}: {e}"))?;

    let rpc = RpcClient::from_insecure_url(ws_uri).await?;
    let initial_version = spec_version(&rpc).await?;

    let client = OnlineClient::<PolkadotConfig>::from_insecure_url(ws_uri).await?;
    let call = tx(
        "System",
        "apply_authorized_upgrade",
        vec![TxValue::from_bytes(&code)],
    );
    let tx_hash = client
        .tx()
        .create_unsigned(&call)?
        .submit()
        .await
        .map_err(|e| {
            anyhow!(
                "{chain}: apply_authorized_upgrade rejected (blob hash not matching the seeded authorization, or spec version not higher?): {e}"
            )
        })?;
    info!("{chain}: apply_authorized_upgrade submitted ({tx_hash})");

    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let current = spec_version(&rpc).await?;
        if current > initial_version {
            info!("✅ {chain}: runtime upgraded, spec version {initial_version} -> {current}");
            return Ok(());
        }
        if started.elapsed().as_secs() > UPGRADE_TIMEOUT_SECS {
            bail!(
                "{chain}: spec version still {current} after {UPGRADE_TIMEOUT_SECS}s, the upgrade did not enact"
            );
        }
    }
}

/// Apply every upgrade recorded in ready.json: relay first, then parachains.
pub async fn apply_from_ready(
    network: &Network<LocalFileSystem>,
    base_path: &Path,
) -> Result<(), anyhow::Error> {
    let ready_path = base_path.join(crate::doppelganger::READY_FILE);
    let content = fs::read_to_string(&ready_path).await.map_err(|e| {
        anyhow!(
            "--apply-upgrade needs {}: {e}",
            ready_path.to_string_lossy()
        )
    })?;
    let ready: Value = serde_json::from_str(&content)?;

    let mut applied = false;
    if let Some(blob) = ready["rc_upgrade_wasm"].as_str() {
        let alice = network.get_node("alice").map_err(|e| anyhow!("{e}"))?;
        apply_authorized_upgrade("relaychain", alice.ws_uri(), &base_path.join(blob)).await?;
        applied = true;
    }

    for para in network.parachains() {
        let id = para.para_id();
        if let Some(blob) = ready[format!("para_{id}_upgrade_wasm")].as_str() {
            let collator = para
                .collators()
                .first()
                .copied()
                .ok_or_else(|| anyhow!("para {id} has no collator to submit the upgrade to"))?;
            apply_authorized_upgrade(
                &format!("para {id}"),
                collator.ws_uri(),
                &base_path.join(blob),
            )
            .await?;
            applied = true;
        }
    }

    if !applied {
        warn!("--apply-upgrade set but the bite carried no upgrade (use --rc-upgrade / --para-upgrade)");
    }
    Ok(())
}
