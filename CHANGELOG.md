# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The repository ships two crates with the same version: `zombie-bite` (the cli)
and, starting with 0.5.0, `zombie-bite-core` (the library it is built on).

## [Unreleased]

### Added

- `zombie-bite-core` library crate. Everything the cli does can now be
  driven from Rust: resolve the settings (`resolve::resolve_bite_config` /
  `resolve_spawn_config`, with `BiteOverrides` / `SpawnOverrides`), `bite`,
  `spawn`, `generate_artifacts`, `clean_up_dir_for_step`, the `network`,
  `verify` and `upgrade` helpers for a spawned fork, and `bundle` to pack and
  unpack artifacts. See `crates/zombie-bite-core/README.md` ([#144]).
- `bite --doppelganger <path>` and `--doppelganger-parachain <path>`, and a
  `[doppelganger]` table (`relay`, `parachain`) in the config file, to run a
  specific doppelganger build instead of the one on `PATH`. A value
  containing `/` must be an executable file, checked before any sync starts.
  From the library, the same is set with `BiteOptions::doppelganger`. See
  `examples/doppelganger.toml` ([#151]).
- `--help` now opens with a one-line description of the tool ([#144]).

### Changed

- **Breaking:** the repository is a cargo workspace with the cli in
  `crates/zombie-bite` and the library in `crates/zombie-bite-core`.
  `cargo build` and `cargo run` from the repository root work as before, and
  the binary is still `target/<profile>/zombie-bite` ([#144]).
- **Breaking:** building from source needs Rust 1.90 or newer, the minimum
  required by `zombienet-sdk` 0.5 ([#144]).
- Invalid input is reported as an error, and the cli exits with code 1,
  instead of panicking: a malformed `-p custom%...` value, a `custom`
  parachain in the config file without `id`, `chain_spec` or
  `rpc_endpoint`, an unreadable override or upgrade wasm, and an unreadable
  or invalid custom chain-spec ([#144]).
- A spawned network that doesn't start (a node never reports metrics, the
  rpc connection drops) or whose artifacts can't be generated at teardown
  now ends with an error instead of a panic ([#144]).
- Bump `keccak` to 0.1.6 ([#109]) and `tar` to 0.4.46 ([#146]).

## [0.4.0] - 2026-09-24

### Added

- Custom parachains with `-p custom%<para_id>%<rpc_endpoint>%<chain_spec_path>%[req_cores]`,
  or a `type = "custom"` entry in the config file ([#110]).
- Fork a relay chain that is not a public network with
  `-r custom%<name>%<rpc_endpoint>%<chain_spec_path>` ([#137]).
- Carry a runtime as an *authorized* upgrade with `--rc-upgrade` and
  `--para-upgrade <para_id>=<wasm_path>`, and enact it after spawn with
  `--apply-upgrade` ([#128]).
- The spawned fork is verified: finality has to progress and the fork has to
  diverge from the live chain ([#128]).
- Overrides are checked against the chain metadata, and the live
  `HostConfiguration` is patched instead of replaced ([#129]).
- `--para-cores <para_id>=<cores>` to override the cores assigned to a
  parachain, and `--keep-messaging-state` to keep the inherited HRMP/DMP
  state ([#129]).
- A `manifest.json` describing the bite artifacts ([#131]).
- `pack` subcommand, and `spawn --bundle`, to move a step's artifacts to
  another machine as a single file ([#131]).
- `--publish-bootnodes [host]` to advertise the fork's own nodes as
  bootNodes in the published chain-specs ([#131]).
- `command` and `image` for the relay chain and each parachain in the config
  file, to choose what the spawned network runs ([#140]).

### Changed

- Relay validators are spawned with the doppelganger binary so finality
  resumes, and parachains with more than one core use slot-based
  authoring ([#137]).
- The state is synced without warp-sync proofs: chains whose staking lives
  on Asset Hub froze at ~37% with them. Requires doppelganger v0.2.3 or
  newer ([#137]).
- The spawned network runs stock `polkadot` v1.22.1 or newer ([#137]).
- Bump zombienet ([#141]).

### Fixed

- Relay chain overrides: `ValidatorGroups` encoding, no-op keys and a
  validator count mismatch ([#128]).
- `CoreDescriptors` are encoded against the runtime's own type ([#137]).
- `--rc-sync-url` is honoured instead of ignored ([#137]).

## [0.3.0] - 2026-06-02

### Added

- A TOML config file (`-c`) to describe the relay chain and any number of
  parachains, with examples in `examples/` ([#84]).
- Support for all system chains (asset-hub, coretime, people, bridge-hub,
  collectives), several parachains at once, with the relay validators scaled
  to the number of parachains ([#84], [#97]).
- Westend in the multi-chain flow ([#117]).

### Changed

- The monitor is optional and disabled by default ([#99]).
- New default sync and rpc endpoints ([#90]).
- Release binaries for macOS arm64 ([#99]).

### Removed

- **Breaking:** the Asset Hub specific flags `--ah-override` and
  `--ah-bite-at`. Parachains are configured with `-p` or in the config
  file ([#101]).

## [0.2.26] - 2025-11-03

### Added

- Select the database to use ([#87]).

### Fixed

- Parachain database path to remove ([#89]).

## [0.2.24] - 2025-11-01

### Added

- Spawn with no parachain ([#86]).

### Changed

- Update zombienet to v0.4.2 ([#85]).

## [0.2.23] - 2025-10-30

### Added

- Bite the relay chain and Asset Hub at a target block ([#76]).

## [0.2.22] - 2025-10-24

### Fixed

- Extra args ([#81]).

## [0.2.21] - 2025-10-24

### Added

- Extra args for the nodes, with `ZOMBIE_BITE_RC_EXTRA_ARGS` and
  `ZOMBIE_BITE_AH_EXTRA_ARGS` ([#79]).

## [0.2.20] - 2025-10-24

### Changed

- The default log level is debug ([#77]).

## [0.2.19] - 2025-10-20

### Added

- `after` step ([#75]).

## [0.2.18] - 2025-10-17

### Added

- Release binaries built by CI ([#73]).

## [0.2.17] - 2025-10-15

### Changed

- Bump zombienet ([#71]).

### Fixed

- Keys for Polkadot ([#69], [#71]).

## [0.2.15] - 2025-10-09

### Added

- Set `--state-pruning` from the environment with
  `ZOMBIE_BITE_STATE_PRUNING` ([#67]).

## [0.2.14] - 2025-09-25

### Fixed

- Keys mixed up between Polkadot and Kusama Asset Hub ([#66]).

## [0.2.13] - 2025-09-06

### Fixed

- Kusama fixes ([#63]).

## [0.2.12] - 2025-09-05

### Added

- Kusama support, with its epoch duration.

## [0.2.10] - 2025-09-03

### Fixed

- Typo ([#62]).

## [0.2.9] - 2025-08-19

### Fixed

- Bob's port ([#61]).

## [0.2.8] - 2025-08-13

### Fixed

- State pruning ([#60]).

## [0.2.7] - 2025-08-11

### Changed

- `--state-pruning` is set when syncing ([#59]).

## [0.2.6] - 2025-08-11

### Added

- Localize config files downloaded from CI ([#57]).

### Fixed

- Localize ([#58]).

## [0.2.4] - 2025-08-06

### Fixed

- Paseo snapshot ([#56]).

## [0.2.3] - 2025-08-06

### Changed

- More log information ([#52]).

### Fixed

- Teardown ([#53]).
- Paseo ([#54]).

## [0.2.1] - 2025-08-03

### Fixed

- Missing file ([#51]).

## [0.2.0] - 2025-08-03

### Changed

- Refactor for the Asset Hub Migration (AHM) flow ([#50]).

## [0.1.14] - 2025-07-29

### Changed

- Use only the doppelganger binaries ([#49]).
- Log by default ([#47]).

## [0.1.13] - 2025-07-25

First tagged release.

### Added

- `bite` a live network with doppelganger nodes and `spawn` a local fork from
  the artifacts, with state overrides and key injection applied during block
  import.
- Polkadot, Kusama, Westend and Paseo, with their Asset Hub.
- A runtime override (wasm) for each network ([#15]).
- Monitor the spawned network and restart stalled nodes ([#36]).
- Ready info for other tools ([#37], [#38]).
- Relay chain and Asset Hub ports from the environment ([#34]).
- Collator log levels ([#45]).
- `ZOMBIE_SUDO` to set the sudo key and the `RcMigrator` manager ([#44]).

[Unreleased]: https://github.com/pepoviola/zombie-bite/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/pepoviola/zombie-bite/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/pepoviola/zombie-bite/compare/v0.2.26...v0.3.0
[0.2.26]: https://github.com/pepoviola/zombie-bite/compare/v0.2.24...v0.2.26
[0.2.24]: https://github.com/pepoviola/zombie-bite/compare/v0.2.23...v0.2.24
[0.2.23]: https://github.com/pepoviola/zombie-bite/compare/v0.2.22...v0.2.23
[0.2.22]: https://github.com/pepoviola/zombie-bite/compare/v0.2.21...v0.2.22
[0.2.21]: https://github.com/pepoviola/zombie-bite/compare/v0.2.20...v0.2.21
[0.2.20]: https://github.com/pepoviola/zombie-bite/compare/v0.2.19...v0.2.20
[0.2.19]: https://github.com/pepoviola/zombie-bite/compare/v0.2.18...v0.2.19
[0.2.18]: https://github.com/pepoviola/zombie-bite/compare/v0.2.17...v0.2.18
[0.2.17]: https://github.com/pepoviola/zombie-bite/compare/v0.2.15...v0.2.17
[0.2.15]: https://github.com/pepoviola/zombie-bite/compare/v0.2.14...v0.2.15
[0.2.14]: https://github.com/pepoviola/zombie-bite/compare/v0.2.13...v0.2.14
[0.2.13]: https://github.com/pepoviola/zombie-bite/compare/v0.2.12...v0.2.13
[0.2.12]: https://github.com/pepoviola/zombie-bite/compare/v0.2.10...v0.2.12
[0.2.10]: https://github.com/pepoviola/zombie-bite/compare/v0.2.9...v0.2.10
[0.2.9]: https://github.com/pepoviola/zombie-bite/compare/v0.2.8...v0.2.9
[0.2.8]: https://github.com/pepoviola/zombie-bite/compare/v0.2.7...v0.2.8
[0.2.7]: https://github.com/pepoviola/zombie-bite/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/pepoviola/zombie-bite/compare/v0.2.4...v0.2.6
[0.2.4]: https://github.com/pepoviola/zombie-bite/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/pepoviola/zombie-bite/compare/v0.2.1...v0.2.3
[0.2.1]: https://github.com/pepoviola/zombie-bite/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/pepoviola/zombie-bite/compare/v0.1.14...v0.2.0
[0.1.14]: https://github.com/pepoviola/zombie-bite/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/pepoviola/zombie-bite/releases/tag/v0.1.13

[#15]: https://github.com/pepoviola/zombie-bite/pull/15
[#34]: https://github.com/pepoviola/zombie-bite/pull/34
[#36]: https://github.com/pepoviola/zombie-bite/pull/36
[#37]: https://github.com/pepoviola/zombie-bite/pull/37
[#38]: https://github.com/pepoviola/zombie-bite/pull/38
[#44]: https://github.com/pepoviola/zombie-bite/pull/44
[#45]: https://github.com/pepoviola/zombie-bite/pull/45
[#47]: https://github.com/pepoviola/zombie-bite/pull/47
[#49]: https://github.com/pepoviola/zombie-bite/pull/49
[#50]: https://github.com/pepoviola/zombie-bite/pull/50
[#51]: https://github.com/pepoviola/zombie-bite/pull/51
[#52]: https://github.com/pepoviola/zombie-bite/pull/52
[#53]: https://github.com/pepoviola/zombie-bite/pull/53
[#54]: https://github.com/pepoviola/zombie-bite/pull/54
[#56]: https://github.com/pepoviola/zombie-bite/pull/56
[#57]: https://github.com/pepoviola/zombie-bite/pull/57
[#58]: https://github.com/pepoviola/zombie-bite/pull/58
[#59]: https://github.com/pepoviola/zombie-bite/pull/59
[#60]: https://github.com/pepoviola/zombie-bite/pull/60
[#61]: https://github.com/pepoviola/zombie-bite/pull/61
[#62]: https://github.com/pepoviola/zombie-bite/pull/62
[#63]: https://github.com/pepoviola/zombie-bite/pull/63
[#66]: https://github.com/pepoviola/zombie-bite/pull/66
[#67]: https://github.com/pepoviola/zombie-bite/pull/67
[#69]: https://github.com/pepoviola/zombie-bite/pull/69
[#71]: https://github.com/pepoviola/zombie-bite/pull/71
[#73]: https://github.com/pepoviola/zombie-bite/pull/73
[#75]: https://github.com/pepoviola/zombie-bite/pull/75
[#76]: https://github.com/pepoviola/zombie-bite/pull/76
[#77]: https://github.com/pepoviola/zombie-bite/pull/77
[#79]: https://github.com/pepoviola/zombie-bite/pull/79
[#81]: https://github.com/pepoviola/zombie-bite/pull/81
[#84]: https://github.com/pepoviola/zombie-bite/pull/84
[#85]: https://github.com/pepoviola/zombie-bite/pull/85
[#86]: https://github.com/pepoviola/zombie-bite/pull/86
[#87]: https://github.com/pepoviola/zombie-bite/pull/87
[#89]: https://github.com/pepoviola/zombie-bite/pull/89
[#90]: https://github.com/pepoviola/zombie-bite/pull/90
[#97]: https://github.com/pepoviola/zombie-bite/pull/97
[#99]: https://github.com/pepoviola/zombie-bite/pull/99
[#101]: https://github.com/pepoviola/zombie-bite/pull/101
[#109]: https://github.com/pepoviola/zombie-bite/pull/109
[#110]: https://github.com/pepoviola/zombie-bite/pull/110
[#117]: https://github.com/pepoviola/zombie-bite/pull/117
[#128]: https://github.com/pepoviola/zombie-bite/pull/128
[#129]: https://github.com/pepoviola/zombie-bite/pull/129
[#131]: https://github.com/pepoviola/zombie-bite/pull/131
[#137]: https://github.com/pepoviola/zombie-bite/pull/137
[#140]: https://github.com/pepoviola/zombie-bite/pull/140
[#141]: https://github.com/pepoviola/zombie-bite/pull/141
[#144]: https://github.com/pepoviola/zombie-bite/pull/144
[#146]: https://github.com/pepoviola/zombie-bite/pull/146
[#151]: https://github.com/pepoviola/zombie-bite/pull/151
