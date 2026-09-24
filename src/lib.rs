//! Fork a live relay chain (and its parachains) into a local network.
//!
//! The `zombie-bite` binary is a thin CLI over this crate: every step it runs
//! (bite, spawn, pack, generate artifacts, clean up) is available here.

pub mod bootnodes;
pub mod bundle;
pub mod config;
pub mod doppelganger;
pub mod manifest;
pub mod metadata;
pub mod monit;
pub mod network;
pub mod overrides;
pub mod resolve;
pub mod sync;
pub mod upgrade;
pub mod utils;
pub mod verify;
