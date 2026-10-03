#![doc = include_str!("../README.md")]

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

/// Every public async function's future is `Send`.
///
/// They are driven from services, which run them on multi-threaded runtimes:
/// spawned as tasks, or awaited inside `async_trait` methods. Both need the
/// future to be `Send`, and nothing else would notice when it stops being —
/// holding a zombienet builder (an `Rc<RefCell<…>>` inside) across an
/// `.await` is all it takes. Never called: if a future stops being `Send`,
/// this stops compiling, and the error names the value and the `.await`.
#[cfg(test)]
#[allow(dead_code, clippy::too_many_arguments)]
mod futures_are_send {
    use std::path::{Path, PathBuf};

    use zombienet_sdk::{LocalFileSystem, Network};

    use crate::config::{BiteOptions, Parachain, Relaychain, SpawnSetup, Step};

    fn assert_send<T: Send>(_: &T) {}

    fn bite_and_spawn(
        base: PathBuf,
        relay: Relaychain,
        paras: Vec<Parachain>,
        setup: &SpawnSetup,
        opts: &BiteOptions,
        path: &Path,
    ) {
        assert_send(&crate::bite(
            base.clone(),
            relay.clone(),
            paras.clone(),
            "rocksdb",
            setup,
            opts,
        ));
        assert_send(&crate::spawn(Step::Spawn, path, None, None));
        assert_send(&crate::generate_artifacts(
            base.clone(),
            Step::Spawn,
            &relay,
        ));
        assert_send(&crate::clean_up_dir_for_step(
            base,
            Step::Bite,
            &relay,
            &paras,
        ));
    }

    fn on_a_network(
        network: &Network<LocalFileSystem>,
        owned: Network<LocalFileSystem>,
        path: &Path,
    ) {
        assert_send(&crate::network::resolve_if_dir_exist(path, Step::Spawn));
        assert_send(&crate::network::ensure_startup_producing_blocks(network));
        assert_send(&crate::network::post_spawn_loop("stop", network, false));
        assert_send(&crate::network::tear_down_and_generate(
            "stop",
            Step::Spawn,
            owned,
            path.to_path_buf(),
            None,
        ));
        assert_send(&crate::verify::wait_finality_primed(network));
        assert_send(&crate::verify::assert_diverged(
            "relay", "ws://a", "ws://b", 1,
        ));
        assert_send(&crate::verify::verify_fork(network, path));
        assert_send(&crate::upgrade::apply_authorized_upgrade(
            "relay", "ws://a", path,
        ));
        assert_send(&crate::upgrade::apply_from_ready(network, path));
    }

    fn bundles(path: &Path) {
        assert_send(&crate::bundle::pack(path, Step::Bite, None));
        assert_send(&crate::bundle::unpack(path, path));
    }
}
