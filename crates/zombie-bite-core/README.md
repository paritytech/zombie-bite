# zombie-bite-core

Library behind the [`zombie-bite`](https://crates.io/crates/zombie-bite) cli:
fork a live relay chain (and its parachains) into a local network you can
spawn, test against and throw away.

The flow mirrors the cli subcommands:

1. **resolve** the settings from an optional config file plus overrides
   (`resolve::resolve_bite_config`), the same way the cli merges its flags.
2. **bite** (`bite`): sync the live network with `doppelganger` nodes, apply
   the state overrides and write the artifacts (chain-specs, snapshots,
   manifest) to a base path.
3. **spawn** (`spawn`): start a new network from those artifacts, then drive
   it with the `network`, `verify` and `upgrade` helpers: wait for block
   production, check it forked, enact carried runtime upgrades, and tear it
   down to generate the artifacts for the next step.
4. **pack** / **unpack** (`bundle`): move the artifacts to another machine as a
   single file.

The same external binaries as the cli are needed in `PATH` (doppelganger,
`polkadot`, `polkadot-parachain`), see the
[repository README](https://github.com/pepoviola/zombie-bite#requirements).

## Example

```rust,no_run
use zombie_bite_core::{
    bite,
    config::Step,
    network,
    resolve::{resolve_bite_config, BiteOverrides},
    spawn, verify,
};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // Resolve the settings as the cli does: an optional config file plus
    // overrides that take precedence over it.
    let config = resolve_bite_config(
        None,
        BiteOverrides {
            relay: Some("paseo".into()),
            parachains: Some(vec!["asset-hub".into()]),
            base_path: Some("/tmp/paseo-fork".into()),
            ..Default::default()
        },
    )?;

    // Sync and bite the live network, writing the artifacts to the base path.
    bite(
        config.base_path.clone(),
        config.relaychain,
        config.parachains,
        "rocksdb",
        &config.spawn_setup,
        &config.opts,
    )
    .await?;

    // Spawn the fork from those artifacts, wait until it produces blocks and
    // check it diverged from the live chain.
    let network = spawn(Step::Spawn, &config.base_path, None, None).await?;
    network::ensure_startup_producing_blocks(&network).await?;
    verify::verify_fork(&network, &config.base_path).await?;

    Ok(())
}
```

Nodes and ports can be tuned with the same `ZOMBIE_BITE_*` environment
variables the cli reads, see the
[repository README](https://github.com/pepoviola/zombie-bite#environment-variables).

## License

Apache-2.0
