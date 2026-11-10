//! Bite a Trinity stack from its descriptor.
//!
//! A Trinity stack descriptor (see the `trinity-stack-descriptor` RFC in
//! `paritytech/trinity-user-agents`) lists every chain of a stack with its
//! role, para id, genesis hash, RPC endpoints and full chain-spec, optionally
//! signed by the stack's operator. This module reads one from a pointer (a
//! descriptor URL, a local file or a `polkadotapp://stack?url=...&key=...`
//! link) and turns it into the [`ZombieBiteConfig`] a user would otherwise
//! write by hand: a custom relay plus custom parachains, with the chain-specs
//! downloaded and the core count of each para read from the relay. From there
//! the bite is resolved and run as any other.
//!
//! Every rule of the RFC (pointers, envelopes and their signatures, I-JSON,
//! the descriptor's format and the spec digests) is applied by the
//! [`trinity_stack`] crate, so zombie-bite accepts and refuses the same
//! descriptors as every other reader.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context as _};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{info, warn};
use trinity_stack::{accept, Chain, Descriptor, Document, Fetched, Pointer};
use zombienet_sdk::subxt::ext::subxt_rpcs::client::RpcParams;

use crate::{
    config::{ParachainConfig, RelaychainConfig, StackConfig, ZombieBiteConfig},
    rpc::rpc_client,
};

/// Role of the relay chain in a descriptor.
pub const ROLE_RELAY: &str = "Relay";
/// Where, under the base path, the stack's chain-specs are downloaded.
pub const STACK_DIR: &str = "stack";
/// The config derived from the stack (plus the user's overrides), written to
/// the base path so the bite can be re-run with `-c` and no stack.
pub const STACK_CONFIG_FILE: &str = "stack-config.toml";

const KNOWN_RELAYS: [&str; 4] = ["polkadot", "kusama", "paseo", "westend"];

/// Stand-in URL for the pointer of a descriptor read from a local file. A
/// [`Pointer`] only takes web URLs, and reading a document applies only its
/// key, so the URL is never used.
const LOCAL_FILE_URL: &str = "https://localhost/trinity-stack.json";

// ---------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------

/// Where a descriptor lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Url(String),
    File(PathBuf),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Url(url) => write!(f, "{url}"),
            Source::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// A stack pointer: where its descriptor lives and, for a signed stack, the
/// operator's key. A descriptor URL or link is read with the crate's
/// [`Pointer`]; a local file has no pointer of its own, only a key.
#[derive(Debug, Clone, PartialEq)]
pub struct StackPointer {
    pub source: Source,
    /// The pointer whose rules a fetched document is read with.
    rules: Pointer,
}

impl StackPointer {
    /// Parse a pointer from a link (`<any scheme or domain>?url=...&key=...`),
    /// a descriptor URL, a `file://` URL or a local path. `extra_key` is a key
    /// given apart from the link (`--stack-key`); it has to agree with the
    /// link's own key when both are present.
    pub fn parse(s: &str, extra_key: Option<&str>) -> Result<Self, anyhow::Error> {
        let s = s.trim();
        let refused = |e: trinity_stack::Error| anyhow!("invalid stack pointer '{s}': {e}");
        let (source, rules) = match url::Url::parse(s) {
            Ok(link) if link.query_pairs().any(|(k, _)| k == "url") => {
                let link = Pointer::from_link(s).map_err(refused)?;
                let rules = match extra_key {
                    None => link,
                    Some(extra) => {
                        let with_extra =
                            Pointer::new(link.url().as_str(), Some(extra)).map_err(refused)?;
                        if let Some(key) = link.key() {
                            if Some(key) != with_extra.key() {
                                bail!(
                                    "the stack link carries key 0x{}, which differs from the given key {extra}",
                                    hex::encode(key)
                                );
                            }
                        }
                        with_extra
                    }
                };
                (Source::Url(rules.url().to_string()), rules)
            }
            Ok(url) if url.scheme() == "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| anyhow!("invalid file url '{s}'"))?;
                (Source::File(path), local_rules(extra_key).map_err(refused)?)
            }
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                let rules = Pointer::new(s, extra_key).map_err(refused)?;
                (Source::Url(s.to_string()), rules)
            }
            Ok(url) if url.scheme().len() > 1 => {
                bail!("'{s}' is neither a stack link (no 'url' parameter), a descriptor url nor a file")
            }
            // No scheme, or a Windows drive letter: a local path.
            _ => (
                Source::File(PathBuf::from(s)),
                local_rules(extra_key).map_err(refused)?,
            ),
        };
        Ok(Self { source, rules })
    }

    /// The operator's key, when the stack is signed.
    pub fn key(&self) -> Option<&[u8; 32]> {
        self.rules.key()
    }
}

fn local_rules(key: Option<&str>) -> Result<Pointer, trinity_stack::Error> {
    Pointer::new(LOCAL_FILE_URL, key)
}

// ---------------------------------------------------------------------------
// Descriptor
// ---------------------------------------------------------------------------

/// How a descriptor was read, recorded in the bite's manifest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StackSource {
    /// The descriptor's `id`.
    pub id: String,
    /// Where the descriptor was read from.
    pub source: String,
    /// Operator key the signature was checked against.
    pub key: Option<String>,
    pub sequence: u64,
    pub generation: String,
    /// Whether the descriptor's signature was checked.
    pub signature_verified: bool,
}

/// Read a fetched document with the pointer's rules: with a key only an
/// envelope verifying against it is accepted, without one an envelope is read
/// as unsigned. A stack past its `expiresAt` is refused, as there is nothing
/// left to bite. Returns the descriptor and whether its signature was checked.
pub fn decode_document(
    bytes: &[u8],
    pointer: &StackPointer,
    now_unix: i64,
) -> Result<(Descriptor, bool), anyhow::Error> {
    let accepted = accept(&pointer.rules, None, Fetched::Document(bytes), now_unix)
        .map_err(|e| anyhow!("the stack descriptor is refused: {e} ({})", e.code()))?;
    let descriptor = accepted.stack.descriptor;
    if accepted.expired {
        bail!(
            "stack '{}' expired at {} (now {now_unix}), there is nothing left to bite",
            descriptor.id,
            descriptor.expires_at.unwrap_or_default()
        );
    }
    if accepted.stale {
        warn!(
            "stack '{}' is past its validUntil ({}), the operator may have published a newer descriptor",
            descriptor.id,
            descriptor.valid_until.unwrap_or_default()
        );
    }
    if !accepted.stack.signed && matches!(Document::parse(bytes), Ok(Document::Envelope { .. })) {
        warn!("the stack descriptor is signed but no key was given, its signature is NOT checked (pass the stack link or --stack-key to check it)");
    }
    Ok((descriptor, accepted.stack.signed))
}

/// The descriptor's relay; validation guarantees exactly one.
fn relay_chain(descriptor: &Descriptor) -> &Chain {
    descriptor
        .chain(ROLE_RELAY)
        .expect("validated: exactly one relay")
}

fn parachains(descriptor: &Descriptor) -> impl Iterator<Item = &Chain> {
    descriptor.chains.iter().filter(|c| c.role != ROLE_RELAY)
}

// ---------------------------------------------------------------------------
// Turning a descriptor into a bite config
// ---------------------------------------------------------------------------

/// A stack resolved into a bite config.
#[derive(Debug, Clone)]
pub struct StackBite {
    /// The derived config, with the user's overrides merged in.
    pub config: ZombieBiteConfig,
    pub source: StackSource,
}

/// Read the stack `stack` points to and derive the bite config: endpoints
/// checked against each chain's genesis hash, chain-specs downloaded to
/// `<base_path>/stack` and checked against their digests. Core counts are left
/// unset, to be read from the relay when the bite is resolved.
pub async fn materialize(
    stack: &StackConfig,
    base_path: &Path,
) -> Result<StackBite, anyhow::Error> {
    let pointer = StackPointer::parse(&stack.pointer, stack.key.as_deref())?;
    info!("📚 reading stack descriptor from {}", pointer.source);
    let bytes = fetch(&pointer.source).await?;
    let (descriptor, verified) =
        decode_document(&bytes, &pointer, crate::manifest::now_unix() as i64)?;
    info!(
        "📚 stack '{}' ({}), sequence {}, {} chain(s){}",
        descriptor.id,
        descriptor.name,
        descriptor.sequence,
        descriptor.chains.len(),
        if verified { ", signature verified" } else { "" }
    );

    let chains = select_chains(&descriptor, stack.chains.as_deref())?;

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;
    let spec_dir = base_path.join(STACK_DIR);
    fs::create_dir_all(&spec_dir).await?;

    let relay_name = relay_name(&descriptor.id);
    let relay = relay_chain(&descriptor);
    let relay_rpc = pick_endpoint(relay).await?;
    let relay_spec = download_spec(&http, relay, &spec_dir, &relay_name).await?;
    let relay_spec_id = spec_id(&relay_spec).await?;

    let mut parachains = vec![];
    for chain in chains {
        let para_id = chain.para_id.expect("validated");
        let rpc = pick_endpoint(chain).await?;
        let spec = download_spec(&http, chain, &spec_dir, &relay_name).await?;
        check_para_spec(&spec, para_id, &relay_spec_id).await?;

        info!("🧩 {} (para {para_id}): rpc {rpc}", chain.role);

        parachains.push(ParachainConfig {
            parachain_type: "custom".into(),
            runtime_override: None,
            upgrade: None,
            enabled: None,
            bite_at: None,
            rpc_endpoint: Some(rpc),
            id: Some(para_id),
            chain_spec: Some(spec.to_string_lossy().to_string()),
            // read from the relay when the bite is resolved
            cores: None,
            role: Some(chain.role.clone()),
            command: None,
            image: None,
        });
    }

    let config = ZombieBiteConfig {
        relaychain: RelaychainConfig {
            network: relay_name,
            runtime_override: None,
            sync_url: Some(relay_rpc),
            chain_spec: Some(relay_spec.to_string_lossy().to_string()),
            bite_at: None,
            upgrade: None,
            command: None,
            image: None,
        },
        parachains: Some(parachains),
        base_path: Some(base_path.to_string_lossy().to_string()),
        and_spawn: None,
        with_monitor: None,
        apply_upgrade: None,
        keep_messaging_state: None,
        publish_bootnodes: None,
        doppelganger: None,
        stack: None,
    };

    let source = StackSource {
        id: descriptor.id.clone(),
        source: pointer.source.to_string(),
        key: pointer.key().map(|k| format!("0x{}", hex::encode(k))),
        sequence: descriptor.sequence.into(),
        generation: descriptor.generation.clone(),
        signature_verified: verified,
    };
    Ok(StackBite { config, source })
}

/// Merge the user's config onto the one derived from the stack: `[relaychain]`
/// and `[[parachains]]` (matched by `role`, or by `id`) override the derived
/// chains, the other settings are taken as they are.
pub fn merge_user_config(
    mut derived: ZombieBiteConfig,
    user: Option<&ZombieBiteConfig>,
) -> Result<ZombieBiteConfig, anyhow::Error> {
    let Some(user) = user else {
        return Ok(derived);
    };

    let relay = &mut derived.relaychain;
    let u = &user.relaychain;
    if !u.network.is_empty() && u.network != relay.network {
        warn!(
            "[relaychain] network = '{}' is ignored, the relay comes from the stack",
            u.network
        );
    }
    override_opt(&mut relay.runtime_override, &u.runtime_override);
    override_opt(&mut relay.sync_url, &u.sync_url);
    override_opt(&mut relay.chain_spec, &u.chain_spec);
    override_opt(&mut relay.bite_at, &u.bite_at);
    override_opt(&mut relay.upgrade, &u.upgrade);
    override_opt(&mut relay.command, &u.command);
    override_opt(&mut relay.image, &u.image);

    let paras = derived.parachains.get_or_insert_with(Vec::new);
    for u in user.parachains.as_deref().unwrap_or_default() {
        let target = paras.iter_mut().find(|p| match (&u.role, u.id) {
            (Some(role), _) => p
                .role
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(role)),
            (None, Some(id)) => p.id == Some(id),
            (None, None) => false,
        });
        let Some(p) = target else {
            bail!(
                "[[parachains]] entry (role {:?}, id {:?}) matches no chain being bitten from the stack; select a chain with `role` or `id`",
                u.role,
                u.id
            );
        };
        override_opt(&mut p.runtime_override, &u.runtime_override);
        override_opt(&mut p.upgrade, &u.upgrade);
        override_opt(&mut p.enabled, &u.enabled);
        override_opt(&mut p.bite_at, &u.bite_at);
        override_opt(&mut p.rpc_endpoint, &u.rpc_endpoint);
        override_opt(&mut p.chain_spec, &u.chain_spec);
        override_opt(&mut p.cores, &u.cores);
        override_opt(&mut p.command, &u.command);
        override_opt(&mut p.image, &u.image);
    }

    override_opt(&mut derived.base_path, &user.base_path);
    override_opt(&mut derived.and_spawn, &user.and_spawn);
    override_opt(&mut derived.with_monitor, &user.with_monitor);
    override_opt(&mut derived.apply_upgrade, &user.apply_upgrade);
    override_opt(
        &mut derived.keep_messaging_state,
        &user.keep_messaging_state,
    );
    override_opt(&mut derived.publish_bootnodes, &user.publish_bootnodes);
    override_opt(&mut derived.doppelganger, &user.doppelganger);
    Ok(derived)
}

fn override_opt<T: Clone>(target: &mut Option<T>, value: &Option<T>) {
    if value.is_some() {
        target.clone_from(value);
    }
}

/// The derived config as TOML, with a header saying where it came from.
pub fn config_to_toml(config: &ZombieBiteConfig) -> Result<String, anyhow::Error> {
    let header = "# Derived from a Trinity stack descriptor by zombie-bite.\n# Re-run the same bite without the stack with: zombie-bite bite -c <this file>\n\n";
    Ok(format!("{header}{}", toml::to_string_pretty(config)?))
}

/// Write the derived config as TOML to `path`.
pub async fn write_config(config: &ZombieBiteConfig, path: &Path) -> Result<(), anyhow::Error> {
    fs::write(path, config_to_toml(config)?).await?;
    Ok(())
}

/// The parachains to bite: every chain but the relay, or the roles in
/// `wanted`. A chain without a full chain-spec can't be bitten: asking for it
/// is an error, otherwise it is skipped.
fn select_chains<'d>(
    descriptor: &'d Descriptor,
    wanted: Option<&[String]>,
) -> Result<Vec<&'d Chain>, anyhow::Error> {
    let relay = relay_chain(descriptor);
    if relay.full_spec.is_none() {
        bail!("the stack's relay has no 'fullSpec', it can't be bitten");
    }

    match wanted {
        Some(wanted) => {
            let mut chains = vec![];
            for role in wanted {
                if role.eq_ignore_ascii_case(ROLE_RELAY) {
                    continue; // always bitten
                }
                let chain = parachains(descriptor)
                    .find(|c| c.role.eq_ignore_ascii_case(role))
                    .ok_or_else(|| {
                        anyhow!(
                            "stack '{}' has no '{role}' chain (it has: {})",
                            descriptor.id,
                            parachains(descriptor)
                                .map(|c| c.role.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?;
                if chain.full_spec.is_none() {
                    bail!(
                        "the stack's '{}' chain has no 'fullSpec', it can't be bitten",
                        chain.role
                    );
                }
                if !chains.iter().any(|c: &&Chain| c.role == chain.role) {
                    chains.push(chain);
                }
            }
            Ok(chains)
        }
        None => Ok(parachains(descriptor)
            .filter(|c| {
                if c.full_spec.is_none() {
                    warn!(
                        "the stack's '{}' chain has no 'fullSpec', skipping it",
                        c.role
                    );
                }
                c.full_spec.is_some()
            })
            .collect()),
    }
}

/// Name of the custom relay, used for artifact file names. Kept apart from the
/// public networks' names, which select their built-in handling.
fn relay_name(id: &str) -> String {
    let mut name: String = id
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    name = name.trim_matches('-').to_string();
    if name.is_empty() {
        name = "stack".into();
    }
    if KNOWN_RELAYS.contains(&name.as_str()) {
        name.push_str("-stack");
    }
    name
}

async fn fetch(source: &Source) -> Result<Vec<u8>, anyhow::Error> {
    match source {
        Source::File(path) => fs::read(path)
            .await
            .with_context(|| format!("can't read the stack descriptor {}", path.display())),
        Source::Url(url) => {
            let response = reqwest::get(url)
                .await
                .with_context(|| format!("can't fetch the stack descriptor {url}"))?
                .error_for_status()?;
            Ok(response.bytes().await?.to_vec())
        }
    }
}

/// The first endpoint of `chain` that answers with the chain's genesis hash.
async fn pick_endpoint(chain: &Chain) -> Result<String, anyhow::Error> {
    let expected = chain.genesis_hash.to_ascii_lowercase();
    let mut errors = vec![];
    for url in &chain.rpc {
        match genesis_hash(url).await {
            Ok(hash) if hash.to_ascii_lowercase() == expected => return Ok(url.clone()),
            Ok(hash) => errors.push(format!("{url}: genesis {hash}")),
            Err(e) => errors.push(format!("{url}: {e}")),
        }
    }
    bail!(
        "no rpc endpoint of the stack's '{}' chain serves genesis {expected}: {}",
        chain.role,
        errors.join("; ")
    )
}

async fn genesis_hash(url: &str) -> Result<String, anyhow::Error> {
    let rpc = tokio::time::timeout(Duration::from_secs(30), rpc_client(url))
        .await
        .map_err(|_| anyhow!("timed out connecting"))??;
    let mut params = RpcParams::new();
    params.push(0u32)?;
    let hash: Option<String> = rpc.request("chain_getBlockHash", params).await?;
    hash.ok_or_else(|| anyhow!("no genesis block"))
}

/// Download `chain`'s full chain-spec into `dir`, reusing a copy already there
/// when its digest matches.
async fn download_spec(
    http: &reqwest::Client,
    chain: &Chain,
    dir: &Path,
    relay_name: &str,
) -> Result<PathBuf, anyhow::Error> {
    let spec = chain
        .full_spec
        .as_ref()
        .expect("selected chains have a spec");
    let path = dir.join(format!(
        "{relay_name}-{}.json",
        chain.role.to_ascii_lowercase()
    ));
    if let Ok(bytes) = fs::read(&path).await {
        if spec.check(&bytes).is_ok() {
            info!("📄 {} chain-spec already at {}", chain.role, path.display());
            return Ok(path);
        }
    }

    info!(
        "📥 downloading the {} chain-spec from {}",
        chain.role, spec.url
    );
    let bytes = http
        .get(&spec.url)
        .send()
        .await
        .with_context(|| format!("can't fetch {}", spec.url))?
        .error_for_status()?
        .bytes()
        .await?;
    spec.check(&bytes).map_err(|e| {
        anyhow!(
            "the {} chain-spec at {} doesn't match the descriptor: {e} (expected sha256 {})",
            chain.role,
            spec.url,
            spec.sha256
        )
    })?;
    fs::write(&path, &bytes).await?;
    Ok(path)
}

async fn read_spec(path: &Path) -> Result<serde_json::Value, anyhow::Error> {
    let bytes = fs::read(path).await?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a json chain-spec", path.display()))
}

async fn spec_id(path: &Path) -> Result<String, anyhow::Error> {
    read_spec(path).await?["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("chain-spec {} has no 'id'", path.display()))
}

/// A para's chain-spec has to name the descriptor's para id and the stack's
/// relay, or the fork would wire it to the wrong chain.
async fn check_para_spec(
    path: &Path,
    para_id: u32,
    relay_spec_id: &str,
) -> Result<(), anyhow::Error> {
    let spec = read_spec(path).await?;
    let spec_para_id = spec
        .get("para_id")
        .or_else(|| spec.get("paraId"))
        .and_then(|v| v.as_u64());
    if let Some(spec_para_id) = spec_para_id {
        if spec_para_id != u64::from(para_id) {
            bail!(
                "chain-spec {} is for para {spec_para_id}, the descriptor says {para_id}",
                path.display()
            );
        }
    }
    let relay_chain = spec
        .get("relay_chain")
        .or_else(|| spec.get("relayChain"))
        .and_then(|v| v.as_str());
    if let Some(relay_chain) = relay_chain {
        if relay_chain != relay_spec_id {
            bail!(
                "chain-spec {} belongs to relay '{relay_chain}', the stack's relay is '{relay_spec_id}'",
                path.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_SEED: [u8; 32] = [7; 32];

    fn descriptor_json() -> String {
        serde_json::json!({
            "$v": 1,
            "sequence": 42,
            "generation": "g1",
            "id": "previewnet",
            "name": "Previewnet",
            "network": "paseo",
            "tld": "testnet",
            "services": [],
            "chains": [
                {
                    "role": "Relay",
                    "genesisHash": format!("0x{}", "11".repeat(32)),
                    "rpc": ["wss://example.com/relay"],
                    "fullSpec": { "url": "https://example.com/relay.json", "sha256": format!("0x{}", "aa".repeat(32)) }
                },
                {
                    "role": "AssetHub",
                    "paraId": 1500,
                    "genesisHash": format!("0x{}", "22".repeat(32)),
                    "rpc": ["wss://example.com/ah"],
                    "statementStore": true,
                    "fullSpec": { "url": "https://example.com/ah.json", "sha256": format!("0x{}", "bb".repeat(32)) }
                },
                {
                    "role": "People",
                    "paraId": 1502,
                    "genesisHash": format!("0x{}", "33".repeat(32)),
                    "rpc": ["wss://example.com/people"]
                }
            ]
        })
        .to_string()
    }

    fn public_key() -> String {
        format!("0x{}", hex::encode(trinity_stack::public_key(&KEY_SEED)))
    }

    fn signed() -> Vec<u8> {
        trinity_stack::sign(descriptor_json().as_bytes(), &KEY_SEED).unwrap()
    }

    fn pointer(key: Option<&str>) -> StackPointer {
        StackPointer::parse("./stack.json", key).unwrap()
    }

    fn decode(bytes: &[u8], key: Option<&str>) -> Result<(Descriptor, bool), anyhow::Error> {
        decode_document(bytes, &pointer(key), 0)
    }

    #[test]
    fn parses_links_urls_and_files() {
        let k = public_key();
        let link = format!(
            "polkadotapp://stack?key={k}&url=https%3A%2F%2Fpreviewnet.substrate.dev%2F.well-known%2Ftrinity-stack.json&foo=bar"
        );
        let p = StackPointer::parse(&link, None).unwrap();
        assert_eq!(
            p.source,
            Source::Url("https://previewnet.substrate.dev/.well-known/trinity-stack.json".into())
        );
        assert_eq!(p.key(), Some(&trinity_stack::public_key(&KEY_SEED)));

        // any scheme/domain carries the same pointer
        let p = StackPointer::parse(
            "https://app.example/open?url=https%3A%2F%2Fa.example%2Fs.json",
            None,
        )
        .unwrap();
        assert_eq!(p.source, Source::Url("https://a.example/s.json".into()));
        assert_eq!(p.key(), None);

        // the same key given apart from the link agrees with it
        assert!(StackPointer::parse(&link, Some(&k)).is_ok());

        let p = StackPointer::parse("https://a.example/s.json", Some(&k)).unwrap();
        assert_eq!(p.source, Source::Url("https://a.example/s.json".into()));
        assert!(p.key().is_some());

        assert_eq!(
            StackPointer::parse("./stack.json", None).unwrap().source,
            Source::File("./stack.json".into())
        );
        let p = StackPointer::parse("file:///tmp/stack.json", Some(&k)).unwrap();
        assert_eq!(p.source, Source::File("/tmp/stack.json".into()));
        assert!(p.key().is_some());
    }

    #[test]
    fn rejects_bad_pointers() {
        let k = format!("0x{}", "ab".repeat(32));
        let other = format!("0x{}", "cd".repeat(32));
        let link = format!("polkadotapp://stack?key={k}&url=https%3A%2F%2Fa.example%2Fs.json");
        assert!(
            StackPointer::parse(&link, Some(&other)).is_err(),
            "conflicting keys"
        );
        assert!(
            StackPointer::parse("http://a.example/s.json", None).is_err(),
            "plain http"
        );
        assert!(
            StackPointer::parse("http://127.0.0.1:8080/s.json", None).is_ok(),
            "private literal"
        );
        assert!(
            StackPointer::parse("https://u:p@a.example/s.json", None).is_err(),
            "credentials"
        );
        assert!(
            StackPointer::parse("https://a.example/s.json", Some("0xABCD")).is_err(),
            "bad key"
        );
        assert!(
            StackPointer::parse("ftp://a.example/s.json", None).is_err(),
            "unknown scheme"
        );
    }

    #[test]
    fn decodes_a_plain_descriptor() {
        let (d, verified) = decode(descriptor_json().as_bytes(), None).unwrap();
        assert!(!verified);
        assert_eq!(d.id, "previewnet");
        assert_eq!(relay_chain(&d).role, "Relay");
        assert_eq!(parachains(&d).count(), 2);
    }

    #[test]
    fn verifies_a_signed_descriptor() {
        let (d, verified) = decode(&signed(), Some(&public_key())).unwrap();
        assert!(verified);
        assert_eq!(d.sequence, 42);

        // without a key the envelope is read as unsigned
        let (_, verified) = decode(&signed(), None).unwrap();
        assert!(!verified);
    }

    #[test]
    fn rejects_a_wrong_key_or_a_tampered_signature() {
        let other = format!("0x{}", hex::encode(trinity_stack::public_key(&[9; 32])));
        assert!(decode(&signed(), Some(&other)).is_err());

        let mut env: serde_json::Value = serde_json::from_slice(&signed()).unwrap();
        let sig = env["signature"].as_str().unwrap().to_string();
        let flipped = if sig.ends_with('0') { '1' } else { '0' };
        env["signature"] = format!("{}{flipped}", &sig[..sig.len() - 1]).into();
        let doc = serde_json::to_vec(&env).unwrap();
        assert!(decode(&doc, Some(&public_key())).is_err());
    }

    #[test]
    fn a_keyed_pointer_refuses_a_plain_descriptor() {
        let err = decode(descriptor_json().as_bytes(), Some(&public_key())).unwrap_err();
        assert!(err.to_string().contains("not signed"), "{err}");
    }

    #[test]
    fn rejects_an_unsupported_envelope_version() {
        let mut env: serde_json::Value = serde_json::from_slice(&signed()).unwrap();
        env["$v"] = 2.into();
        let doc = serde_json::to_vec(&env).unwrap();
        let err = decode(&doc, Some(&public_key())).unwrap_err();
        assert!(err.to_string().contains("version"), "{err}");
    }

    #[test]
    fn rejects_duplicate_members() {
        let doc = descriptor_json().replacen(r#""id":"previewnet""#, r#""id":"a","id":"b""#, 1);
        assert_ne!(doc, descriptor_json());
        let err = decode(doc.as_bytes(), None).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    fn descriptor_with(
        f: impl FnOnce(&mut serde_json::Value),
    ) -> Result<Descriptor, anyhow::Error> {
        let mut v: serde_json::Value = serde_json::from_str(&descriptor_json()).unwrap();
        f(&mut v);
        decode(v.to_string().as_bytes(), None).map(|(d, _)| d)
    }

    #[test]
    fn validation_rules() {
        assert!(
            descriptor_with(|v| v["chains"][2]["role"] = "AssetHub".into()).is_err(),
            "duplicate role"
        );
        assert!(
            descriptor_with(|v| v["chains"][2]["paraId"] = 1500.into()).is_err(),
            "duplicate para id"
        );
        assert!(
            descriptor_with(|v| v["chains"][1]["role"] = "Relay".into()).is_err(),
            "two relays"
        );
        assert!(
            descriptor_with(|v| {
                v["chains"][1].as_object_mut().unwrap().remove("paraId");
            })
            .is_err(),
            "para without id"
        );
        assert!(
            descriptor_with(|v| v["chains"][1]["rpc"] = serde_json::json!([])).is_err(),
            "no rpc"
        );
        assert!(
            descriptor_with(|v| v["chains"][1]["rpc"] = serde_json::json!(["ws://a.example"]))
                .is_err(),
            "insecure rpc"
        );
    }

    #[test]
    fn refuses_an_expired_stack() {
        let mut v: serde_json::Value = serde_json::from_str(&descriptor_json()).unwrap();
        v["expiresAt"] = 100.into();
        let bytes = v.to_string().into_bytes();
        let err = decode_document(&bytes, &pointer(None), 100).unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        assert!(decode_document(&bytes, &pointer(None), 99).is_ok());
    }

    #[test]
    fn selects_chains() {
        let d = descriptor_with(|_| {}).unwrap();
        // People has no fullSpec: skipped by default
        let all = select_chains(&d, None).unwrap();
        assert_eq!(
            all.iter().map(|c| c.role.as_str()).collect::<Vec<_>>(),
            ["AssetHub"]
        );
        // ...and an error when asked for
        assert!(select_chains(&d, Some(&["People".into()])).is_err());
        assert!(select_chains(&d, Some(&["Bridge".into()])).is_err());
        let some = select_chains(&d, Some(&["relay".into(), "assethub".into()])).unwrap();
        assert_eq!(some.len(), 1);
    }

    #[test]
    fn relay_names_stay_apart_from_public_networks() {
        assert_eq!(relay_name("previewnet"), "previewnet");
        assert_eq!(relay_name("Paseo Next v2"), "paseo-next-v2");
        assert_eq!(relay_name("polkadot"), "polkadot-stack");
    }

    fn derived() -> ZombieBiteConfig {
        ZombieBiteConfig {
            relaychain: RelaychainConfig {
                network: "previewnet".into(),
                runtime_override: None,
                sync_url: Some("wss://example.com/relay".into()),
                chain_spec: Some("/b/stack/relay.json".into()),
                bite_at: None,
                upgrade: None,
                command: None,
                image: None,
            },
            parachains: Some(vec![ParachainConfig {
                parachain_type: "custom".into(),
                runtime_override: None,
                upgrade: None,
                enabled: None,
                bite_at: None,
                rpc_endpoint: Some("wss://example.com/ah".into()),
                id: Some(1500),
                chain_spec: Some("/b/stack/ah.json".into()),
                cores: Some(3),
                role: Some("AssetHub".into()),
                command: None,
                image: None,
            }]),
            base_path: Some("/b".into()),
            and_spawn: None,
            with_monitor: None,
            apply_upgrade: None,
            keep_messaging_state: None,
            publish_bootnodes: None,
            doppelganger: None,
            stack: None,
        }
    }

    #[test]
    fn user_config_overrides_the_stack() {
        let user: ZombieBiteConfig = toml::from_str(
            r#"
            and_spawn = true
            [relaychain]
            command = "./bins/polkadot"
            [[parachains]]
            type = "custom"
            role = "assethub"
            cores = 2
            command = "polkadot-omni-node"
            "#,
        )
        .unwrap();
        let merged = merge_user_config(derived(), Some(&user)).unwrap();
        assert_eq!(merged.and_spawn, Some(true));
        assert_eq!(
            merged.relaychain.command.as_deref(),
            Some("./bins/polkadot")
        );
        assert_eq!(merged.relaychain.network, "previewnet");
        let ah = &merged.parachains.as_ref().unwrap()[0];
        assert_eq!(ah.cores, Some(2));
        assert_eq!(ah.command.as_deref(), Some("polkadot-omni-node"));
        assert_eq!(ah.rpc_endpoint.as_deref(), Some("wss://example.com/ah"));

        // the derived config resolves to a custom relay and an Asset Hub role
        let relay = merged.get_relaychain();
        assert!(relay.is_custom());
        assert_eq!(relay.chain_spec(), Some("/b/stack/relay.json"));
        let paras = merged.get_parachains().unwrap();
        assert_eq!(paras[0].id(), 1500);
        assert!(paras[0].is_asset_hub());
    }

    #[test]
    fn an_override_must_match_a_stack_chain() {
        let user: ZombieBiteConfig = toml::from_str(
            r#"
            [relaychain]
            [[parachains]]
            type = "custom"
            role = "People"
            cores = 2
            "#,
        )
        .unwrap();
        assert!(merge_user_config(derived(), Some(&user)).is_err());
    }

    #[test]
    fn derived_config_round_trips_through_toml() {
        let s = toml::to_string_pretty(&derived()).unwrap();
        let back: ZombieBiteConfig = toml::from_str(&s).unwrap();
        assert_eq!(back, derived());
    }

    #[test]
    fn example_stack_toml_is_valid() {
        let user = ZombieBiteConfig::from_file(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/stack.toml"
        ))
        .unwrap();
        let stack = user.stack.as_ref().unwrap();
        StackPointer::parse(&stack.pointer, stack.key.as_deref()).unwrap();
        let merged = merge_user_config(derived(), Some(&user)).unwrap();
        assert_eq!(merged.parachains.unwrap()[0].cores, Some(3));
    }
}
