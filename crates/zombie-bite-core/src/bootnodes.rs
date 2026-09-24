//! Publish the fork's own node addresses into the chain-specs it ships.
//!
//! `generate_chain_spec` clears `bootNodes` so a fork can never dial the network
//! it was forked from. That is the right default, but it also means a published
//! spec is unusable to anything that was not started by this process: the peer
//! wiring only exists in the spawned nodes' arguments. Filling the list with the
//! fork's own nodes - optionally advertised under a routable host - makes the
//! artifacts usable without every consumer patching the specs itself.

use std::path::Path;

use anyhow::anyhow;
use serde_json::Value;
use tokio::fs;
use tracing::{info, warn};
use zombienet_sdk::{LocalFileSystem, Network};

/// Addresses of a spawned chain's nodes, captured while the network is still up.
#[derive(Debug, Clone)]
pub struct ChainBootnodes {
    /// `None` for the relay chain.
    pub para_id: Option<u32>,
    pub addresses: Vec<String>,
}

/// Collect the running nodes' addresses. Has to happen before teardown, while
/// the network object still describes live nodes.
pub fn collect(network: &Network<LocalFileSystem>) -> Vec<ChainBootnodes> {
    let mut chains = vec![ChainBootnodes {
        para_id: None,
        addresses: network
            .relaychain()
            .nodes()
            .iter()
            .map(|node| node.multiaddr().to_string())
            .collect(),
    }];

    for para in network.parachains() {
        chains.push(ChainBootnodes {
            para_id: Some(para.para_id()),
            addresses: para
                .collators()
                .iter()
                .map(|node| node.multiaddr().to_string())
                .collect(),
        });
    }

    chains
}

/// Rewrite the host of a multiaddr, keeping port, transport and peer id.
///
/// The addresses zombienet reports are always loopback (the native provider
/// hands out `127.0.0.1`), which is fine on the same box and useless anywhere
/// else - so a deployment advertises its own hostname instead.
fn advertise(addr: &str, host: &str) -> String {
    let protocol = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        "ip6"
    } else if host.parse::<std::net::Ipv4Addr>().is_ok() {
        "ip4"
    } else {
        "dns4"
    };

    let mut parts: Vec<&str> = addr.split('/').collect();
    // "/ip4/127.0.0.1/tcp/30333/ws/p2p/<peer>" -> ["", "ip4", "127.0.0.1", ...]
    if parts.len() < 3 {
        return addr.to_string();
    }
    parts[1] = protocol;
    parts[2] = host;
    parts.join("/")
}

/// Write the collected addresses into the chain-specs of `spec_dir`.
///
/// Specs are matched by their own contents, not by file name: a fork carries the
/// source chain's spec id, which does not have to match the file the bite wrote
/// (`collectives-polkadot` vs an id of `collectives_polkadot`), and a custom
/// parachain's spec id is whatever its author chose. A raw spec with a `para_id`
/// belongs to that parachain; one without is the relay chain.
pub async fn publish(
    chains: &[ChainBootnodes],
    spec_dir: &Path,
    host: &str,
) -> Result<(), anyhow::Error> {
    let mut entries = fs::read_dir(spec_dir)
        .await
        .map_err(|e| anyhow!("can't read {}: {e}", spec_dir.to_string_lossy()))?;

    let mut patched = 0_usize;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let Ok(content) = fs::read_to_string(&path).await else {
            continue;
        };
        let Ok(mut spec) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        // A raw chain-spec has an id and a bootNodes list; config.toml,
        // ready.json and friends do not.
        if spec.get("id").is_none() || !spec["bootNodes"].is_array() {
            continue;
        }

        let para_id = spec["para_id"].as_u64().map(|id| id as u32);
        let Some(chain) = chains.iter().find(|c| c.para_id == para_id) else {
            warn!(
                "{}: no spawned chain matches this spec, leaving bootNodes empty",
                path.to_string_lossy()
            );
            continue;
        };
        if chain.addresses.is_empty() {
            warn!("{}: no running nodes to advertise", path.to_string_lossy());
            continue;
        }

        let addresses: Vec<String> = chain
            .addresses
            .iter()
            .map(|addr| advertise(addr, host))
            .collect();
        spec["bootNodes"] = serde_json::to_value(&addresses)?;
        // to_string, not to_string_pretty: a raw spec is tens of MB and
        // consumers checksum it.
        fs::write(&path, serde_json::to_string(&spec)?).await?;
        info!(
            "{}: {} bootNode(s) advertised as {host}",
            path.to_string_lossy(),
            addresses.len()
        );
        patched += 1;
    }

    if patched == 0 {
        warn!(
            "--publish-bootnodes: no chain-spec in {} was updated",
            spec_dir.to_string_lossy()
        );
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    const ADDR: &str =
        "/ip4/127.0.0.1/tcp/30333/ws/p2p/12D3KooWQCkBm1BYtkHpocxCwMgR8yjitEeHGx8spzcDLGt2gkBm";

    #[test]
    fn advertise_keeps_port_transport_and_peer() {
        assert_eq!(
            advertise(ADDR, "fork.example.com"),
            "/dns4/fork.example.com/tcp/30333/ws/p2p/12D3KooWQCkBm1BYtkHpocxCwMgR8yjitEeHGx8spzcDLGt2gkBm"
        );
        assert_eq!(
            advertise(ADDR, "10.0.0.7"),
            "/ip4/10.0.0.7/tcp/30333/ws/p2p/12D3KooWQCkBm1BYtkHpocxCwMgR8yjitEeHGx8spzcDLGt2gkBm"
        );
        assert_eq!(advertise(ADDR, "::1").split('/').nth(1), Some("ip6"));
        // same host: unchanged
        assert_eq!(advertise(ADDR, "127.0.0.1"), ADDR);
    }

    #[tokio::test]
    async fn publish_matches_specs_by_para_id_not_file_name() {
        let dir = std::env::temp_dir().join("zb-bootnodes-publish");
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        // file names deliberately unrelated to the spec ids
        fs::write(
            dir.join("relay-spec.json"),
            r#"{"id":"kusama","bootNodes":[]}"#,
        )
        .await
        .unwrap();
        fs::write(
            dir.join("collectives-kusama-spec.json"),
            r#"{"id":"collectives_kusama","para_id":1001,"bootNodes":[]}"#,
        )
        .await
        .unwrap();
        // not a chain-spec: must be left alone
        fs::write(dir.join("ready.json"), r#"{"rc_start_block":10}"#)
            .await
            .unwrap();

        let chains = vec![
            ChainBootnodes {
                para_id: None,
                addresses: vec![ADDR.to_string()],
            },
            ChainBootnodes {
                para_id: Some(1001),
                addresses: vec![ADDR.to_string()],
            },
        ];
        publish(&chains, &dir, "fork.example.com").await.unwrap();

        let relay: Value = serde_json::from_str(
            &fs::read_to_string(dir.join("relay-spec.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            relay["bootNodes"][0].as_str().unwrap(),
            advertise(ADDR, "fork.example.com")
        );

        let para: Value = serde_json::from_str(
            &fs::read_to_string(dir.join("collectives-kusama-spec.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(para["bootNodes"].as_array().unwrap().len(), 1);

        let ready = fs::read_to_string(dir.join("ready.json")).await.unwrap();
        assert_eq!(ready, r#"{"rc_start_block":10}"#);

        fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn publish_warns_when_no_chain_matches() {
        let dir = std::env::temp_dir().join("zb-bootnodes-nomatch");
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();
        fs::write(
            dir.join("a-spec.json"),
            r#"{"id":"x","para_id":9999,"bootNodes":[]}"#,
        )
        .await
        .unwrap();

        let chains = vec![ChainBootnodes {
            para_id: None,
            addresses: vec![ADDR.to_string()],
        }];
        // unmatched spec is left untouched rather than failing the run
        publish(&chains, &dir, "127.0.0.1").await.unwrap();
        let spec: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("a-spec.json")).await.unwrap())
                .unwrap();
        assert!(spec["bootNodes"].as_array().unwrap().is_empty());

        fs::remove_dir_all(&dir).await.unwrap();
    }
}
