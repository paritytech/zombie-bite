//! How many cores each bitten parachain gets on the fork.
//!
//! Precedence, per para:
//! 1. `--para-cores <id>=<n>`
//! 2. the config file's `cores` (or `req_cores` of a `custom%…` para)
//! 3. the live assignment, read once per bite from the relay's
//!    `ParachainHost_claim_queue` (at `--rc-bite-at` when set, head otherwise).
//!    A para with no core there gets 1.
//! 4. the built-in defaults of [`builtin_cores`], used when the relay can't be
//!    read (no runtime API, endpoint unreachable).
//!
//! The total is capped by the validators the fork can run, see
//! [`MAX_BACKED_CORES`].

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};

use anyhow::{anyhow, bail};
use tracing::{info, warn};
use zombienet_sdk::subxt::ext::subxt_rpcs::{client::RpcParams, RpcClient};

use crate::{
    config::{num_validators_for_cores, CoresOverride, Parachain, Relaychain},
    metadata::claim_queue_cores,
};

/// Cores the fork's validator set (capped at 7: one per core plus a spare)
/// can back.
pub(crate) const MAX_BACKED_CORES: u32 = 6;

const RELAY_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a para's core count came from, logged for every para.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreSource {
    /// `--para-cores`.
    Cli,
    /// The config file's `cores`, or `req_cores` of a `custom%…` para.
    Config,
    /// The relay's claim queue.
    Relay,
    /// The relay was read but has no core for the para (e.g. on-demand only).
    NoCoreOnRelay,
    /// The relay couldn't be read: built-in default.
    BuiltIn,
}

impl fmt::Display for CoreSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CoreSource::Cli => "--para-cores",
            CoreSource::Config => "config",
            CoreSource::Relay => "relay claim queue",
            CoreSource::NoCoreOnRelay => "no core on the relay, defaulting to 1",
            CoreSource::BuiltIn => "built-in default",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreAssignment {
    pub para_id: u32,
    pub cores: u32,
    pub source: CoreSource,
}

/// Core counts used when the relay can't be read. They mirror the live
/// networks at the time of writing and go stale, hence only a fallback.
pub(crate) fn builtin_cores(relay: &Relaychain, para: &Parachain) -> u32 {
    match para {
        Parachain::AssetHub { .. } => 3,
        Parachain::People { .. } => match relay {
            Relaychain::Polkadot { .. } | Relaychain::Westend { .. } => 3,
            _ => 1,
        },
        _ => 1,
    }
}

/// The count set explicitly for `para` (cli or config), if any.
fn explicit(
    para: &Parachain,
    explicit: &CoresOverride,
    from_cli: &BTreeSet<u32>,
) -> Option<(u32, CoreSource)> {
    let id = para.id();
    if let Some(n) = explicit.get(&id) {
        let source = if from_cli.contains(&id) {
            CoreSource::Cli
        } else {
            CoreSource::Config
        };
        return Some((*n, source));
    }
    para.req_cores().map(|n| (n, CoreSource::Config))
}

/// Whether any para still needs the relay's live assignment.
pub(crate) fn needs_relay(
    paras: &[Parachain],
    explicit_cores: &CoresOverride,
    from_cli: &BTreeSet<u32>,
) -> bool {
    paras
        .iter()
        .any(|p| explicit(p, explicit_cores, from_cli).is_none())
}

/// Pick each para's core count. `live` is the relay's assignment, `None` when
/// it couldn't be read.
pub(crate) fn assign(
    relay: &Relaychain,
    paras: &[Parachain],
    explicit_cores: &CoresOverride,
    from_cli: &BTreeSet<u32>,
    live: Option<&BTreeMap<u32, u32>>,
) -> Vec<CoreAssignment> {
    paras
        .iter()
        .map(|para| {
            let para_id = para.id();
            let (cores, source) =
                explicit(para, explicit_cores, from_cli).unwrap_or_else(|| match live {
                    Some(live) => match live.get(&para_id) {
                        Some(n) => (*n, CoreSource::Relay),
                        None => (1, CoreSource::NoCoreOnRelay),
                    },
                    None => (builtin_cores(relay, para), CoreSource::BuiltIn),
                });
            CoreAssignment {
                para_id,
                cores,
                source,
            }
        })
        .collect()
}

/// Refuse a bite whose cores can't all get a backing group on the fork.
pub(crate) fn check_cap(assignments: &[CoreAssignment]) -> Result<(), anyhow::Error> {
    let total: u32 = assignments.iter().map(|a| a.cores).sum();
    if total > MAX_BACKED_CORES {
        let detail = assignments
            .iter()
            .map(|a| format!("{}={} ({})", a.para_id, a.cores, a.source))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "the bite needs {total} cores but the fork runs at most {} validators, so it can back at most {MAX_BACKED_CORES}: [{detail}]. Lower them with --para-cores <para_id>=<cores> (or `cores` in the config), or bite fewer parachains",
            num_validators_for_cores(total)
        );
    }
    Ok(())
}

/// Resolve every para's core count, reading the relay when some para has no
/// explicit one. Never fails on the relay read (falls back with a warning),
/// only on the cap.
pub(crate) async fn resolve_cores(
    relay: &Relaychain,
    paras: &[Parachain],
    explicit_cores: &CoresOverride,
    from_cli: &BTreeSet<u32>,
) -> Result<CoresOverride, anyhow::Error> {
    let live = if needs_relay(paras, explicit_cores, from_cli) {
        let url = relay.rpc_endpoint();
        let at = relay.at_block();
        match tokio::time::timeout(RELAY_QUERY_TIMEOUT, relay_core_counts(&url, at)).await {
            Ok(Ok(cores)) => {
                info!(
                    "🧩 read core assignments from {url} at {}",
                    at.map_or("head".to_string(), |b| format!("block {b}"))
                );
                Some(cores)
            }
            Ok(Err(e)) => {
                warn!("can't read the core assignments from the relay ({e}); using the built-in core counts");
                None
            }
            Err(_) => {
                warn!("timed out reading the core assignments from the relay; using the built-in core counts");
                None
            }
        }
    } else {
        None
    };

    let assignments = assign(relay, paras, explicit_cores, from_cli, live.as_ref());
    for a in &assignments {
        if a.source == CoreSource::NoCoreOnRelay {
            warn!("🧩 para {}: {} core(s) ({})", a.para_id, a.cores, a.source);
        } else {
            info!("🧩 para {}: {} core(s) ({})", a.para_id, a.cores, a.source);
        }
    }
    check_cap(&assignments)?;

    Ok(assignments
        .into_iter()
        .map(|a| (a.para_id, a.cores))
        .collect())
}

async fn rpc_client(url: &str) -> Result<RpcClient, anyhow::Error> {
    let client = if url.starts_with("ws://") {
        RpcClient::from_insecure_url(url).await?
    } else {
        RpcClient::from_url(url).await?
    };
    Ok(client)
}

/// Cores per para id on the relay, from `ParachainHost_claim_queue`.
pub(crate) async fn relay_core_counts(
    url: &str,
    at_block: Option<u32>,
) -> Result<BTreeMap<u32, u32>, anyhow::Error> {
    if url.is_empty() {
        bail!("no rpc endpoint for the relay");
    }
    let rpc = rpc_client(url).await?;
    let mut params = RpcParams::new();
    params.push("ParachainHost_claim_queue")?;
    params.push("0x")?;
    if let Some(block) = at_block {
        let mut hash_params = RpcParams::new();
        hash_params.push(block)?;
        let hash: Option<String> = rpc.request("chain_getBlockHash", hash_params).await?;
        params.push(hash.ok_or_else(|| anyhow!("no block {block} on the relay"))?)?;
    }
    let raw: String = rpc.request("state_call", params).await?;
    let bytes = hex::decode(raw.trim_start_matches("0x"))?;
    claim_queue_cores(&bytes)
}

#[cfg(test)]
mod test {
    use super::*;

    fn polkadot() -> Relaychain {
        Relaychain::new("polkadot")
    }

    fn custom(id: u32, cores: Option<u32>) -> Parachain {
        Parachain::Custom {
            maybe_override: None,
            maybe_bite_at: None,
            maybe_rpc_endpoint: Some("wss://example.com".into()),
            name: format!("custom-{id}"),
            chain_spec: "spec.json".into(),
            id,
            cores,
            role: None,
        }
    }

    fn by_id(a: &[CoreAssignment], id: u32) -> (u32, CoreSource) {
        let a = a.iter().find(|a| a.para_id == id).unwrap();
        (a.cores, a.source)
    }

    #[test]
    fn precedence_cli_over_config_over_relay_over_builtin() {
        let paras = vec![
            Parachain::new("asset-hub"), // 1000
            Parachain::new("people"),    // 1004
            Parachain::new("coretime"),  // 1005
            custom(2000, Some(2)),
            custom(2001, None),
        ];
        let explicit: CoresOverride = [(1000, 1), (1004, 2)].into();
        let from_cli: BTreeSet<u32> = [1000].into();
        let live: BTreeMap<u32, u32> =
            [(1000, 4), (1004, 4), (1005, 2), (2000, 5), (2001, 3)].into();

        let a = assign(&polkadot(), &paras, &explicit, &from_cli, Some(&live));
        assert_eq!(by_id(&a, 1000), (1, CoreSource::Cli));
        assert_eq!(by_id(&a, 1004), (2, CoreSource::Config));
        assert_eq!(by_id(&a, 1005), (2, CoreSource::Relay));
        assert_eq!(by_id(&a, 2000), (2, CoreSource::Config));
        assert_eq!(by_id(&a, 2001), (3, CoreSource::Relay));
    }

    #[test]
    fn no_core_on_relay_gets_one() {
        let paras = vec![custom(2001, None)];
        let live: BTreeMap<u32, u32> = [(1000, 3)].into();
        let a = assign(
            &polkadot(),
            &paras,
            &Default::default(),
            &Default::default(),
            Some(&live),
        );
        assert_eq!(by_id(&a, 2001), (1, CoreSource::NoCoreOnRelay));
    }

    #[test]
    fn unreadable_relay_falls_back_to_builtin() {
        let paras = vec![
            Parachain::new("asset-hub"),
            Parachain::new("people"),
            custom(2001, None),
        ];
        let a = assign(
            &polkadot(),
            &paras,
            &Default::default(),
            &Default::default(),
            None,
        );
        assert_eq!(by_id(&a, 1000), (3, CoreSource::BuiltIn));
        assert_eq!(by_id(&a, 1004), (3, CoreSource::BuiltIn));
        assert_eq!(by_id(&a, 2001), (1, CoreSource::BuiltIn));

        let a = assign(
            &Relaychain::new("kusama"),
            &paras,
            &Default::default(),
            &Default::default(),
            None,
        );
        assert_eq!(by_id(&a, 1004), (1, CoreSource::BuiltIn));
    }

    #[test]
    fn relay_only_queried_when_needed() {
        let paras = vec![Parachain::new("asset-hub"), custom(2000, Some(1))];
        let mut explicit = CoresOverride::new();
        assert!(needs_relay(&paras, &explicit, &Default::default()));
        explicit.insert(1000, 2);
        assert!(!needs_relay(&paras, &explicit, &Default::default()));
    }

    #[test]
    fn cap_refuses_more_than_six_cores() {
        let a = |cores: &[u32]| {
            cores
                .iter()
                .enumerate()
                .map(|(i, c)| CoreAssignment {
                    para_id: 1000 + i as u32,
                    cores: *c,
                    source: CoreSource::Relay,
                })
                .collect::<Vec<_>>()
        };
        assert!(check_cap(&a(&[3, 3])).is_ok());
        let err = check_cap(&a(&[3, 3, 1])).unwrap_err().to_string();
        assert!(err.contains("needs 7 cores"), "{err}");
        assert!(err.contains("--para-cores"), "{err}");
    }

    #[tokio::test]
    #[ignore = "needs network access"]
    async fn reads_polkadot_core_assignments() {
        let cores = relay_core_counts("wss://rpc.polkadot.io", None)
            .await
            .unwrap();
        assert!(cores.get(&1000).is_some_and(|n| *n >= 1), "{cores:?}");
        println!("{cores:?}");
    }
}
