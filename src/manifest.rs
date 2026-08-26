//! Bundle manifest.
//!
//! A bite is often produced in CI and restored hours later on another machine,
//! so the artifacts have to describe themselves: which block each chain was
//! bitten at, where the state came from, and which binaries produced it. The
//! last one matters because a snapshot written by a newer node fails to restore
//! in ways that look like corruption.

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::{fs, process::Command};
use tracing::{info, warn};

pub const MANIFEST_FILE: &str = "manifest.json";
/// Bumped when the shape changes, so an older bundle is reported as such
/// instead of silently failing to parse.
pub const VERSION: u32 = 1;

/// Binaries that produce the snapshots, and whose versions therefore have to
/// match on restore.
const SNAPSHOT_BINARIES: [&str; 2] = ["doppelganger", "doppelganger-parachain"];

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChainEntry {
    pub chain: String,
    pub para_id: Option<u32>,
    /// Block the state was captured at.
    pub bite_block: Option<u64>,
    /// Network the state came from.
    pub source_rpc: Option<String>,
    pub spec_file: Option<String>,
    pub snapshot_file: Option<String>,
    pub snapshot_bytes: Option<u64>,
    /// Runtime carried as an authorized upgrade, if any.
    pub upgrade_file: Option<String>,
    pub upgrade_hash: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub version: u32,
    /// Step directory this manifest describes (the bite bundle); later steps
    /// repack the snapshots under different names.
    #[serde(default)]
    pub bundle: String,
    pub created_at: u64,
    pub relay: ChainEntry,
    pub parachains: Vec<ChainEntry>,
    /// `--version` of the binaries that produced the snapshots.
    #[serde(default)]
    pub binaries: Vec<(String, String)>,
}

async fn binary_version(cmd: &str) -> Option<String> {
    let out = Command::new(cmd).arg("--version").output().await.ok()?;
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

pub async fn binary_versions() -> Vec<(String, String)> {
    let mut versions = vec![];
    for cmd in SNAPSHOT_BINARIES {
        if let Some(version) = binary_version(cmd).await {
            versions.push((cmd.to_string(), version));
        }
    }
    versions
}

pub async fn file_size(path: &str) -> Option<u64> {
    fs::metadata(path).await.ok().map(|m| m.len())
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl Manifest {
    pub async fn write(&self, base_path: &Path) -> Result<(), anyhow::Error> {
        let path = base_path.join(MANIFEST_FILE);
        fs::write(&path, serde_json::to_string_pretty(self)?).await?;
        info!("📄 manifest written to {}", path.to_string_lossy());
        Ok(())
    }

    /// `None` when there is no manifest; a manifest that exists but can't be
    /// parsed warns, since that means a shape change rather than an old bundle.
    pub async fn read(base_path: &Path) -> Option<Self> {
        let path = base_path.join(MANIFEST_FILE);
        let content = fs::read_to_string(&path).await.ok()?;
        match serde_json::from_str::<Self>(&content) {
            Ok(manifest) => Some(manifest),
            Err(e) => {
                warn!("{}: can't read manifest: {e}", path.to_string_lossy());
                None
            }
        }
    }
}

/// Compare the binaries that produced the bundle with the ones on this machine.
/// A mismatch is a warning, not an error: it usually still restores, and when it
/// does not the failure otherwise looks like a corrupt snapshot.
///
/// Both sides are the *doppelganger* binaries: the bite writes what produced the
/// snapshots, and a restore needs the same ones to import that state.
pub async fn warn_on_binary_mismatch(base_path: &Path) {
    let Some(manifest) = Manifest::read(base_path).await else {
        info!("no bundle manifest found, skipping the binary version check");
        return;
    };
    if manifest.version != VERSION {
        warn!(
            "bundle manifest is version {} but this build writes {VERSION}; some fields may be missing",
            manifest.version
        );
    }
    if manifest.binaries.is_empty() {
        info!("bundle manifest records no binary versions, skipping the check");
        return;
    }

    let local = binary_versions().await;
    for (cmd, bundled) in &manifest.binaries {
        match local.iter().find(|(name, _)| name == cmd) {
            Some((_, current)) if current == bundled => {}
            Some((_, current)) => warn!(
                "{cmd}: bundle was produced with '{bundled}' but this machine has '{current}'; a snapshot from a newer node can fail to restore in ways that look like corruption"
            ),
            None => warn!("{cmd}: not found locally, can't compare with the bundle's '{bundled}'"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[tokio::test]
    async fn manifest_round_trips() {
        let dir = std::env::temp_dir().join("zb-manifest-round-trip");
        fs::create_dir_all(&dir).await.unwrap();

        let manifest = Manifest {
            version: VERSION,
            bundle: "bite".into(),
            created_at: 1,
            relay: ChainEntry {
                chain: "kusama".into(),
                bite_block: Some(42),
                source_rpc: Some("wss://example".into()),
                snapshot_bytes: Some(123),
                ..Default::default()
            },
            parachains: vec![ChainEntry {
                chain: "asset-hub-kusama".into(),
                para_id: Some(1000),
                bite_block: Some(7),
                ..Default::default()
            }],
            binaries: vec![("doppelganger".into(), "1.2.3".into())],
        };
        manifest.write(&dir).await.unwrap();

        let read = Manifest::read(&dir)
            .await
            .expect("manifest should be there");
        assert_eq!(read.version, VERSION);
        assert_eq!(read.bundle, "bite");
        assert_eq!(read.relay.bite_block, Some(42));
        assert_eq!(read.parachains[0].para_id, Some(1000));
        assert_eq!(read.binaries[0].1, "1.2.3");

        fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn missing_manifest_reads_as_none() {
        let dir = std::env::temp_dir().join("zb-manifest-absent");
        fs::create_dir_all(&dir).await.unwrap();
        let _ = fs::remove_file(dir.join(MANIFEST_FILE)).await;

        assert!(Manifest::read(&dir).await.is_none());
        // must not panic when there is nothing to compare
        warn_on_binary_mismatch(&dir).await;
    }
}
