//! Pack a step's artifacts into one file and restore it elsewhere.
//!
//! A bite is often produced in CI and consumed hours later on another machine,
//! so everything needed to spawn has to travel together: the chain-specs, the
//! db snapshots, the config, the overrides that were applied, the manifest, and
//! any runtime carried as an authorized upgrade. `spawn` re-points the spec and
//! snapshot paths at wherever the bundle was unpacked, so the artifacts do not
//! have to land in the same directory they were produced in.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use tar::Archive;
use tokio::fs;
use tracing::info;

use crate::{config::Step, manifest::MANIFEST_FILE};

/// Files that live in the base dir rather than the step dir, and are part of the
/// bundle when present.
const BASE_FILES: [&str; 3] = [MANIFEST_FILE, "ready.json", "ports.json"];

fn default_bundle_name(step: Step) -> String {
    format!("{}-bundle.tgz", step.dir())
}

/// Pack `<base>/<step>` plus the base-level files into a single `.tgz`.
pub async fn pack(
    base_path: &Path,
    step: Step,
    out: Option<PathBuf>,
) -> Result<PathBuf, anyhow::Error> {
    let step_dir = base_path.join(step.dir());
    if !fs::try_exists(&step_dir).await? {
        bail!(
            "nothing to pack: {} does not exist",
            step_dir.to_string_lossy()
        );
    }

    let out = out.unwrap_or_else(|| base_path.join(default_bundle_name(step)));
    let file = std::fs::File::create(&out)
        .map_err(|e| anyhow!("can't create {}: {e}", out.to_string_lossy()))?;
    let mut encoder = GzEncoder::new(file, Compression::fast());
    {
        let mut archive = tar::Builder::new(&mut encoder);
        // Paths inside the archive are relative to the base dir, so unpacking
        // into any directory reproduces the same layout.
        archive.append_dir_all(step.dir(), &step_dir)?;
        for name in BASE_FILES {
            let path = base_path.join(name);
            if fs::try_exists(&path).await? {
                archive.append_path_with_name(&path, name)?;
            }
        }
        // Runtimes carried as an authorized upgrade.
        let mut entries = fs::read_dir(base_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with("-upgrade.wasm") {
                archive.append_path_with_name(entry.path(), &name)?;
            }
        }
        archive.finish()?;
    }
    encoder.finish()?;

    info!("📦 bundle written to {}", out.to_string_lossy());
    Ok(out)
}

/// Unpack a bundle into `base_path`.
pub async fn unpack(bundle: &Path, base_path: &Path) -> Result<(), anyhow::Error> {
    if !fs::try_exists(bundle).await? {
        bail!("bundle {} does not exist", bundle.to_string_lossy());
    }
    fs::create_dir_all(base_path).await?;

    let file = std::fs::File::open(bundle)
        .map_err(|e| anyhow!("can't open {}: {e}", bundle.to_string_lossy()))?;
    Archive::new(GzDecoder::new(file)).unpack(base_path)?;

    info!(
        "📦 bundle {} unpacked into {}",
        bundle.to_string_lossy(),
        base_path.to_string_lossy()
    );
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn pack_then_unpack_reproduces_the_layout() {
        let root = std::env::temp_dir().join("zb-bundle-test");
        let (from, to) = (root.join("from"), root.join("to"));
        let _ = fs::remove_dir_all(&root).await;
        fs::create_dir_all(from.join("bite")).await.unwrap();
        fs::create_dir_all(&to).await.unwrap();

        // step dir: spec, snapshot, config and the overrides that were applied
        for (name, content) in [
            ("kusama-spec.json", "{}"),
            ("kusama-snap.tgz", "snap"),
            ("config.toml", "[relaychain]"),
            ("rc_overrides.json", r#"{"overrides":{}}"#),
        ] {
            fs::write(from.join("bite").join(name), content)
                .await
                .unwrap();
        }
        // base dir files
        fs::write(from.join(MANIFEST_FILE), r#"{"created_at":1}"#)
            .await
            .unwrap();
        fs::write(from.join("ready.json"), r#"{"rc_start_block":7}"#)
            .await
            .unwrap();
        fs::write(from.join("kusama-upgrade.wasm"), "wasm")
            .await
            .unwrap();
        // not part of the bundle
        fs::write(from.join("unrelated.log"), "noise")
            .await
            .unwrap();

        let bundle = pack(&from, Step::Bite, None).await.unwrap();
        unpack(&bundle, &to).await.unwrap();

        for name in [
            "kusama-spec.json",
            "kusama-snap.tgz",
            "config.toml",
            "rc_overrides.json",
        ] {
            assert!(
                fs::try_exists(to.join("bite").join(name)).await.unwrap(),
                "missing {name}"
            );
        }
        assert_eq!(
            fs::read_to_string(to.join("ready.json")).await.unwrap(),
            r#"{"rc_start_block":7}"#
        );
        assert!(fs::try_exists(to.join(MANIFEST_FILE)).await.unwrap());
        assert!(fs::try_exists(to.join("kusama-upgrade.wasm"))
            .await
            .unwrap());
        assert!(!fs::try_exists(to.join("unrelated.log")).await.unwrap());

        fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn packing_a_missing_step_dir_fails() {
        let dir = std::env::temp_dir().join("zb-bundle-empty");
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let err = pack(&dir, Step::Bite, None).await.unwrap_err().to_string();
        assert!(err.contains("nothing to pack"), "got: {err}");

        fs::remove_dir_all(&dir).await.unwrap();
    }
}
