//! Fork a live relay chain (and its parachains) into a local network.
//!
//! The `zombie-bite` cli is a thin wrapper over this crate: every step it runs
//! (bite, spawn, pack, generate artifacts, clean up) is available here.
//!
//! A typical run resolves the settings with [`resolve::resolve_bite_config`],
//! bites the live network with [`bite`], then [`spawn`]s the fork and drives
//! it with the helpers in [`network`].

pub mod bundle;
pub mod config;
pub mod manifest;
pub mod network;
pub mod resolve;
pub mod upgrade;
pub mod verify;

mod bootnodes;
mod doppelganger;
mod metadata;
mod monit;
mod overrides;
mod sync;
mod utils;

pub use doppelganger::{bite, clean_up_dir_for_step, generate_artifacts, spawn};
