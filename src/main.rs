use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use clap::Parser;
use futures::StreamExt;
use tracing::{debug, info, level_filters::LevelFilter, trace, warn};
use tracing_subscriber::EnvFilter;
use zombienet_sdk::{LocalFileSystem, Network, NetworkNode};

mod cli;
mod config;
mod doppelganger;
mod monit;
mod overrides;
mod sync;
mod utils;

use cli::{get_base_path, resolve_bite_config, resolve_spawn_config, Args, Commands};
use config::Relaychain;
use doppelganger::doppelganger_inner;
use monit::monit_progress;
use tokio::fs;

use crate::config::Step;

/// Signal for spawn to 'stop' and generate the artifacts
const STOP_FILE: &str = "stop.txt";

/// Helpers fns
async fn resolve_if_dir_exist(base_path: &Path, step: Step) {
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
        fs::rename(&path_to_use, &path_with_suffix)
            .await
            .expect("mv should work");
    }
}

async fn ensure_startup_producing_blocks(network: &Network<LocalFileSystem>) {
    // first wait until the collator replies with metrics
    let parachains = network.parachains();
    let para = parachains
        .first()
        .expect("At least one parachain should exist");
    let collators = para.collators();
    let collator = collators
        .first()
        .expect("At least one collator should exist");
    debug!("Waiting metrics for collator {}", collator.name());

    collator
        .wait_metric_with_timeout("node_roles", |x| x > 1.0, 300_u64)
        .await
        .unwrap();

    // ensure block production
    let client = network
        .get_node("alice")
        .unwrap()
        .wait_client::<zombienet_sdk::subxt::PolkadotConfig>()
        .await
        .unwrap();
    let mut blocks = client.blocks().subscribe_finalized().await.unwrap().take(3);

    while let Some(block) = blocks.next().await {
        info!("Block #{}", block.unwrap().header().number);
    }

    info!("🚀🚀🚀 network is up and running...");
}

async fn post_spawn_loop(
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

async fn tear_down_and_generate(
    stop_file: &str,
    step: Step,
    network: Network<LocalFileSystem>,
    base_path: PathBuf,
) -> Result<(), anyhow::Error> {
    let rc = Relaychain::new(network.relaychain().chain());
    let _ = network.destroy().await;
    let teardown_signal = fs::try_exists(&stop_file).await;

    if let Ok(true) = teardown_signal {
        // create the artifacts
        doppelganger::generate_artifacts(base_path.clone(), step, &rc)
            .await
            .expect("generate should works");
        doppelganger::clean_up_dir_for_step(base_path, step, &rc, &[])
            .await
            .expect("clean-up should works");
    }

    // signal that the teardown is completed
    _ = fs::remove_file(stop_file).await;

    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), anyhow::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let args = Args::parse();
    match args.cmd {
        Commands::Bite {
            config,
            relay,
            relay_runtime,
            relay_bite_at,
            ah_runtime,
            parachains,
            ah_bite_at,
            base_path,
            rc_sync_url,
            and_spawn,
            database,
        } => {
            let resolved_config = resolve_bite_config(
                config,
                relay,
                relay_runtime,
                relay_bite_at,
                ah_runtime,
                ah_bite_at,
                parachains,
                base_path,
                rc_sync_url,
                and_spawn,
            )?;

            debug!("{:?}", resolved_config.relaychain);
            doppelganger_inner(
                resolved_config.base_path.clone(),
                resolved_config.relaychain,
                resolved_config.parachains,
                &database,
            )
            .await
            .expect("bite should work");

            if resolved_config.and_spawn {
                let step = Step::Spawn;
                // STOP file
                let stop_file = format!(
                    "{}/{STOP_FILE}",
                    resolved_config.base_path.to_string_lossy()
                );

                resolve_if_dir_exist(&resolved_config.base_path, step).await;
                let network =
                    doppelganger::spawn(step, resolved_config.base_path.as_path(), None, None)
                        .await
                        .expect("spawn should works");

                ensure_startup_producing_blocks(&network).await;

                post_spawn_loop(&stop_file, &network, true).await?;

                tear_down_and_generate(&stop_file, step, network, resolved_config.base_path)
                    .await?;
            }
        }
        Commands::Spawn {
            config,
            base_path,
            with_monitor,
            step,
        } => {
            let resolved_config = resolve_spawn_config(config, base_path, with_monitor)?;
            let step: Step = step.into();
            let base_path_str = resolved_config.base_path.to_string_lossy();

            if !fs::try_exists(format!("{base_path_str}/{}", step.dir_from()))
                .await
                .expect("try_exist should work")
            {
                println!("\t\x1b[91mThe 'bite' dir doesn't exist, please run the bite subcommand first.\x1b[0m");
                println!("\tHelp: zombie-bite bite --help");

                std::process::exit(1);
            }

            resolve_if_dir_exist(&resolved_config.base_path, step).await;

            let network =
                doppelganger::spawn(step, resolved_config.base_path.as_path(), None, None)
                    .await
                    .expect("spawn should works");

            ensure_startup_producing_blocks(&network).await;

            // STOP file
            let stop_file = format!("{base_path_str}/{STOP_FILE}");

            post_spawn_loop(&stop_file, &network, resolved_config.with_monitor).await?;

            tear_down_and_generate(&stop_file, step, network, resolved_config.base_path).await?;
        }
        Commands::GenerateArtifacts {
            relay,
            base_path,
            step,
        } => {
            let rc = Relaychain::new(&relay);
            let step: Step = step.into();
            let base_path = get_base_path(base_path);
            doppelganger::generate_artifacts(base_path, step, &rc)
                .await
                .expect("generate artifacts should work")
        }
        Commands::CleanUpDir {
            relay,
            base_path,
            step,
        } => {
            let rc = Relaychain::new(&relay);
            let step: Step = step.into();
            let base_path = get_base_path(base_path);
            doppelganger::clean_up_dir_for_step(base_path, step, &rc, &[])
                .await
                .expect("clean-up should works");
        }
    };
    Ok(())
}
