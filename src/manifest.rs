//! Bundle manifest.
//!
//! A bite is often produced in CI and restored hours later on another machine,
//! so the artifacts have to describe themselves: which block each chain was
//! bitten at, where the state came from, and what the files are.

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::info;

pub const MANIFEST_FILE: &str = "manifest.json";
/// Bumped when the shape changes, so an older bundle is reported as such
/// instead of silently failing to parse.
pub const VERSION: u32 = 1;

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
        };
        manifest.write(&dir).await.unwrap();

        let content = fs::read_to_string(dir.join(MANIFEST_FILE)).await.unwrap();
        let read: Manifest = serde_json::from_str(&content).unwrap();
        assert_eq!(read.version, VERSION);
        assert_eq!(read.bundle, "bite");
        assert_eq!(read.relay.bite_block, Some(42));
        assert_eq!(read.parachains[0].para_id, Some(1000));

        fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn missing_manifest_reads_as_none() {
        let dir = std::env::temp_dir().join("zb-manifest-absent");
        fs::create_dir_all(&dir).await.unwrap();
        let _ = fs::remove_file(dir.join(MANIFEST_FILE)).await;

        assert!(fs::read_to_string(dir.join(MANIFEST_FILE)).await.is_err());
    }
}
