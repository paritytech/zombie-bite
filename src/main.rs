use std::path::{Path, PathBuf};

use anyhow::bail;
use clap::Parser;
use tokio::fs;
use tracing::{debug, level_filters::LevelFilter};
use tracing_subscriber::EnvFilter;

use zombie_bite::{
    bundle,
    config::{Relaychain, Step},
    doppelganger::{self, doppelganger_inner},
    network::{
        ensure_startup_producing_blocks, post_spawn_loop, resolve_if_dir_exist,
        tear_down_and_generate, STOP_FILE,
    },
    upgrade, verify,
};

mod cli;

use cli::{get_base_path, resolve_bite_config, resolve_spawn_config, Args, Commands};

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
            parachains,
            base_path,
            rc_sync_url,
            and_spawn,
            with_monitor,
            database,
            relay_upgrade,
            para_upgrade,
            apply_upgrade,
            keep_messaging_state,
            para_cores,
            publish_bootnodes,
        } => {
            if with_monitor && !and_spawn {
                bail!("--with-monitor can only be used with --and-spawn");
            }

            let resolved_config = resolve_bite_config(
                config,
                relay,
                relay_runtime,
                relay_bite_at,
                parachains,
                base_path,
                rc_sync_url,
                and_spawn,
                relay_upgrade,
                para_upgrade,
                apply_upgrade,
                keep_messaging_state,
                para_cores,
                publish_bootnodes,
            )?;

            if resolved_config.publish_bootnodes.is_some() && !resolved_config.and_spawn {
                bail!("--publish-bootnodes can only be used with --and-spawn");
            }
            if resolved_config.apply_upgrade && !resolved_config.and_spawn {
                bail!("--apply-upgrade can only be used with --and-spawn");
            }
            if resolved_config.apply_upgrade && resolved_config.opts.upgrades.is_empty() {
                bail!("--apply-upgrade needs an upgrade to carry (--rc-upgrade / --para-upgrade)");
            }

            if resolved_config.relaychain.is_custom() {
                if resolved_config.relaychain.chain_spec().is_none() {
                    bail!("a custom relay needs a chain-spec: use -r custom%<name>%<rpc>%<chain_spec>");
                }
                if resolved_config.relaychain.sync_url().is_none() {
                    bail!("a custom relay needs an rpc endpoint: use -r custom%<name>%<rpc>%<chain_spec>");
                }
            }

            debug!("{:?}", resolved_config.relaychain);
            doppelganger_inner(
                resolved_config.base_path.clone(),
                resolved_config.relaychain,
                resolved_config.parachains,
                &database,
                &resolved_config.spawn_setup,
                &resolved_config.opts,
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

                verify::verify_fork(&network, resolved_config.base_path.as_path()).await?;

                if resolved_config.apply_upgrade {
                    upgrade::apply_from_ready(&network, resolved_config.base_path.as_path())
                        .await?;
                }

                post_spawn_loop(&stop_file, &network, true).await?;

                tear_down_and_generate(
                    &stop_file,
                    step,
                    network,
                    resolved_config.base_path,
                    resolved_config.publish_bootnodes,
                )
                .await?;
            }
        }
        Commands::Spawn {
            config,
            base_path,
            with_monitor,
            step,
            apply_upgrade,
            publish_bootnodes,
            bundle,
        } => {
            let resolved_config = resolve_spawn_config(
                config,
                base_path,
                with_monitor,
                apply_upgrade,
                publish_bootnodes,
            )?;
            let step: Step = step.into();

            if let Some(bundle) = bundle {
                bundle::unpack(Path::new(&bundle), resolved_config.base_path.as_path()).await?;
            }

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

            verify::verify_fork(&network, resolved_config.base_path.as_path()).await?;

            if resolved_config.apply_upgrade {
                upgrade::apply_from_ready(&network, resolved_config.base_path.as_path()).await?;
            }

            // STOP file
            let stop_file = format!("{base_path_str}/{STOP_FILE}");

            post_spawn_loop(&stop_file, &network, resolved_config.with_monitor).await?;

            tear_down_and_generate(
                &stop_file,
                step,
                network,
                resolved_config.base_path,
                resolved_config.publish_bootnodes,
            )
            .await?;
        }
        Commands::Pack {
            base_path,
            step,
            out,
        } => {
            let base_path = get_base_path(base_path);
            let step: Step = step.into();
            bundle::pack(&base_path, step, out.map(PathBuf::from)).await?;
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
