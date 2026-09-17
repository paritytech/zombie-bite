# Zombie Bite Configuration Files

Zombie Bite now supports configuration files to simplify the management of system chains and reduce the need for many CLI arguments. This provides a better user experience for complex setups.

## How It Works

- Configuration files are written in TOML format
- CLI arguments always override config file values
- Config files can specify all system chains and their settings
- If no config file is provided, the tool uses CLI arguments

## Usage

### Using a Configuration File

```bash
# Bite with config file
zombie-bite bite --config examples/all-system-chains.toml

# Override specific values from CLI (CLI takes precedence)
zombie-bite bite --config examples/kusama-network.toml --rc polkadot
```

### Basic Structure

```toml
[relaychain]
network = "polkadot"  # polkadot, kusama, or paseo
runtime_override = "/path/to/runtime.wasm"  # optional
sync_url = "wss://custom-rpc.example.com"   # optional
command = "./bins/polkadot"                 # optional, defaults to `polkadot`
image = "docker.io/parity/polkadot:v1.19.0" # optional

[[parachains]]
type = "asset-hub"     # asset-hub, coretime, people, or bridge-hub
enabled = true         # optional, defaults to true
runtime_override = "/path/to/runtime.wasm"  # optional
command = "./bins/polkadot-parachain"       # optional, defaults to `polkadot-parachain`
image = "docker.io/parity/polkadot-parachain:v1.19.0"  # optional

# Global settings (optional)
base_path = "/custom/path"
and_spawn = true
with_monitor = true
```

### Command and image for the spawned network

`command` and `image` can be set per chain and land as `default_command` /
`default_image` in the `config.toml` that `bite` writes into
`<base_path>/bite/`, which is the file the network is spawned from. They can be
set independently and both are validated when the config file is read, so a
typo fails right away instead of after the sync.

- `command` is the binary the nodes of the *spawned* network run. It defaults to
  `polkadot` for the relaychain and `polkadot-parachain` for the collators, and
  is useful to point at a local build instead of relying on `PATH`. It cannot
  contain whitespace, extra args go in `ZOMBIE_BITE_RC_EXTRA_ARGS` /
  `ZOMBIE_BITE_AH_EXTRA_ARGS`.
- `image` is only honored by providers that use images (docker/k8s). Since
  `zombie-bite spawn` uses the native provider, `image` is a pass-through for
  consuming the generated `config.toml` with zombienet elsewhere.

Neither key affects the `bite` step itself, which always runs the
`doppelganger` / `doppelganger-parachain` binaries.

See [with-images.toml](./with-images.toml) for a full example.
