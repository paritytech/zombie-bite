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
zombie-bite bite --config examples/kusama-network.toml --relay polkadot

# Mix config file with CLI overrides
zombie-bite bite --config examples/asset-hub-only.toml --parachains asset-hub,coretime,people
```

### Basic Structure

```toml
[relaychain]
network = "polkadot"  # polkadot, kusama, or paseo
runtime_override = "/path/to/runtime.wasm"  # optional
sync_url = "wss://custom-rpc.example.com"   # optional

[[parachains]]
type = "asset-hub"     # asset-hub, coretime, people, or bridge-hub
enabled = true         # optional, defaults to true
runtime_override = "/path/to/runtime.wasm"  # optional

# Global settings (optional)
base_path = "/custom/path"
and_spawn = true
with_monitor = true
```