use std::time::Duration;

use tokio::fs;
use tracing::{debug, trace, warn};
use zombienet_sdk::NetworkNode;

const CHECK_TIMEOUT_SECS: u64 = 600; // 10 mins

/// Best/finalized heights of a node at the last successful check.
#[derive(Debug, Clone, Copy)]
struct Checkpoint {
    best: f64,
    finalized: f64,
}

/// Initial checkpoint: requires at least one best block on the first check but
/// only records the finalized height, since finality legitimately takes
/// ~68-120s after spawn to make its first step (the finalized comparison then
/// happens on the next check, several minutes later).
const INITIAL: Checkpoint = Checkpoint {
    best: 0_f64,
    finalized: -1_f64,
};

async fn restart(node: &NetworkNode, checkpoint: Checkpoint) {
    if (node.restart(None).await).is_ok() {
        warn!(
            "{} was restarted at block {} (finalized {})",
            node.name(),
            checkpoint.best,
            checkpoint.finalized
        );
    } else {
        warn!("Error restarting {}", node.name());
    }
}

async fn progress(node: &NetworkNode, checkpoint: Checkpoint) -> Result<Checkpoint, anyhow::Error> {
    let best = node.reports("block_height{status=\"best\"}").await?;
    let finalized = node.reports("block_height{status=\"finalized\"}").await?;
    if best > checkpoint.best && finalized > checkpoint.finalized {
        debug!(
            "{} is making progress, checkpoint best {}/finalized {} - current best {}/finalized {}",
            node.name(),
            checkpoint.best,
            checkpoint.finalized,
            best,
            finalized
        );

        Ok(Checkpoint { best, finalized })
    } else {
        Err(anyhow::anyhow!(
            "node don't progress, current best {best}/finalized {finalized} - checkpoint best {}/finalized {}",
            checkpoint.best,
            checkpoint.finalized
        ))
    }
}

pub async fn monit_progress(
    alice: &NetworkNode,
    bob: &NetworkNode,
    collator: Option<&NetworkNode>,
    stop_file: Option<&str>,
) {
    // monitoring block production and finality every 15 mins
    let mut alice_block = progress(alice, INITIAL)
        .await
        .expect("first check should works");
    let mut bob_block = progress(bob, INITIAL)
        .await
        .expect("first check should works");

    let mut collator_block = if let Some(collator) = collator {
        progress(collator, INITIAL)
            .await
            .expect("first check should works")
    } else {
        // no collator deployed.
        Checkpoint {
            best: -1_f64,
            finalized: -1_f64,
        }
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
