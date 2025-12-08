use std::time::Duration;

use tokio::fs;
use tracing::{debug, trace, warn};
use zombienet_sdk::NetworkNode;

const CHECK_TIMEOUT_SECS: u64 = 600; // 10 mins

async fn restart(node: &NetworkNode, checkpoint: impl Into<f64>) {
    if (node.restart(None).await).is_ok() {
        warn!(
            "{} was restarted at block {}",
            node.name(),
            checkpoint.into()
        );
    } else {
        warn!("Error restarting {}", node.name());
    }
}

async fn progress(node: &NetworkNode, checkpoint: impl Into<f64>) -> Result<f64, anyhow::Error> {
    let metric = node.reports("block_height{status=\"best\"}").await?;
    let checkpoint = checkpoint.into();
    if metric > checkpoint {
        debug!(
            "{} is making progress, checkpoint {} - current {}",
            node.name(),
            checkpoint,
            metric
        );

        Ok(metric)
    } else {
        Err(anyhow::anyhow!(
            "node don't progress, current {metric} - checkpoint {checkpoint}"
        ))
    }
}

pub async fn monit_progress(
    alice: &NetworkNode,
    bob: &NetworkNode,
    collator: Option<&NetworkNode>,
    stop_file: Option<&str>,
) {
    // monitoring block production every 15 mins
    let mut alice_block = progress(alice, 0).await.expect("first check should works");
    let mut bob_block = progress(bob, 0).await.expect("first check should works");

    let mut collator_block = if let Some(collator) = collator {
        progress(collator, 0)
            .await
            .expect("first check should works")
    } else {
        // no collator deployed.
        -1_f64
    };

    let mut check_progress = async || {
        // check the progress
        // alice
        let mut alice_was_restarted = false;
        if let Ok(block) = progress(alice, alice_block).await {
            alice_block = block;
        } else {
            // restart alice / collator
            restart(alice, alice_block).await;
            if let Some(collator) = collator {
                restart(collator, collator_block).await;
            }
            alice_was_restarted = true;
        }

        // bob
        if let Ok(block) = progress(bob, bob_block).await {
            bob_block = block;
        } else {
            // restart alice / collator
            restart(bob, bob_block).await;
        }

        if !alice_was_restarted {
            if let Some(collator) = collator {
                if let Ok(block) = progress(collator, collator_block).await {
                    collator_block = block;
                } else {
                    // restart alice / collator
                    restart(collator, collator_block).await;
                }
            }
        }
    };

    if let Some(stop_file) = stop_file {
        let mut counter = 0;
        while let Ok(false) = fs::try_exists(&stop_file).await {
            trace!("monit counter: {counter}");
            tokio::time::sleep(Duration::from_secs(60)).await;
            if counter >= 15 {
                check_progress().await;
                counter = 0;
            } else {
                counter += 1;
            }
        }
    } else {
        loop {
            tokio::time::sleep(Duration::from_secs(CHECK_TIMEOUT_SECS)).await;
            check_progress().await
        }
    }
}
