//! Bite a Trinity stack from its descriptor.
//!
//! A Trinity stack descriptor (see the `trinity-stack-descriptor` RFC in
//! `paritytech/trinity-user-agents`) lists every chain of a stack with its
//! role, para id, genesis hash, RPC endpoints and full chain-spec, optionally
//! signed by the stack's operator. This module reads one from a pointer (a
//! descriptor URL, a local file or a `polkadotapp://stack?url=...&key=...`
//! link), checks it, and turns it into the [`ZombieBiteConfig`] a user would
//! otherwise write by hand: a custom relay plus custom parachains, with the
//! chain-specs downloaded and the core count of each para read from the relay.
//! From there the bite is resolved and run as any other.

use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context as _};
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize,
};
use sha2::{Digest, Sha256};
use tokio::fs;
use tracing::{info, warn};
use zombienet_sdk::subxt::ext::subxt_rpcs::{client::RpcParams, RpcClient};

use crate::{
    config::{ParachainConfig, RelaychainConfig, StackConfig, ZombieBiteConfig},
    metadata::claim_queue_cores,
};

/// Descriptor format version this reader understands.
pub const DESCRIPTOR_VERSION: u32 = 1;
/// Role of the relay chain in a descriptor.
pub const ROLE_RELAY: &str = "Relay";
/// Where, under the base path, the stack's chain-specs are downloaded.
pub const STACK_DIR: &str = "stack";
/// The config derived from the stack (plus the user's overrides), written to
/// the base path so the bite can be re-run with `-c` and no stack.
pub const STACK_CONFIG_FILE: &str = "stack-config.toml";

const KNOWN_RELAYS: [&str; 4] = ["polkadot", "kusama", "paseo", "westend"];

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
/// operator's key.
#[derive(Debug, Clone, PartialEq)]
pub struct Pointer {
    pub source: Source,
    pub key: Option<[u8; 32]>,
}

impl Pointer {
    /// Parse a pointer from a link (`<any scheme or domain>?url=...&key=...`),
    /// a descriptor URL, a `file://` URL or a local path. `extra_key` is a key
    /// given apart from the link (`--stack-key`); it has to agree with the
    /// link's own key when both are present.
    pub fn parse(s: &str, extra_key: Option<&str>) -> Result<Self, anyhow::Error> {
        let s = s.trim();
        let (source, link_key) = match url::Url::parse(s) {
            Ok(link) if link.query_pairs().any(|(k, _)| k == "url") => {
                // Readers take the pointer from the query, whatever the scheme
                // or domain the link arrives on, and ignore other parameters.
                let mut url = None;
                let mut key = None;
                for (k, v) in link.query_pairs() {
                    match k.as_ref() {
                        "url" => url = Some(v.into_owned()),
                        "key" => key = Some(v.into_owned()),
                        _ => {}
                    }
                }
                let url = url.expect("checked above");
                check_url(&url, &["https"])
                    .with_context(|| format!("stack link '{s}' has an invalid url"))?;
                (Source::Url(url), key)
            }
            Ok(url) if url.scheme() == "file" => {
                let path = url
                    .to_file_path()
                    .map_err(|_| anyhow!("invalid file url '{s}'"))?;
                (Source::File(path), None)
            }
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                check_url(s, &["https"])?;
                (Source::Url(s.to_string()), None)
            }
            Ok(url) if url.scheme().len() > 1 => {
                bail!("'{s}' is neither a stack link (no 'url' parameter), a descriptor url nor a file")
            }
            // No scheme, or a Windows drive letter: a local path.
            _ => (Source::File(PathBuf::from(s)), None),
        };

        let key = match (link_key.as_deref(), extra_key) {
            (Some(a), Some(b)) if a != b => {
                bail!("the stack link carries key {a}, which differs from the given key {b}")
            }
            (Some(k), _) | (None, Some(k)) => Some(parse_hex::<32>(k, "key")?),
            (None, None) => None,
        };

        Ok(Self { source, key })
    }
}

// ---------------------------------------------------------------------------
// Descriptor
// ---------------------------------------------------------------------------

/// The parts of a descriptor zombie-bite reads; unknown fields are ignored, as
/// the RFC requires.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    #[serde(rename = "$v")]
    pub v: u32,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub generation: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub network: String,
    /// Unix seconds at which the stack is shut down.
    pub expires_at: Option<u64>,
    pub chains: Vec<Chain>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chain {
    pub role: String,
    pub genesis_hash: String,
    pub para_id: Option<u32>,
    pub rpc: Vec<String>,
    pub full_spec: Option<SpecFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpecFile {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "$v")]
    v: u32,
    payload: String,
    signature: String,
}

impl Descriptor {
    pub fn relay(&self) -> &Chain {
        self.chains
            .iter()
            .find(|c| c.role == ROLE_RELAY)
            .expect("validated: exactly one relay")
    }

    pub fn parachains(&self) -> impl Iterator<Item = &Chain> {
        self.chains.iter().filter(|c| c.role != ROLE_RELAY)
    }

    /// Check the format rules zombie-bite depends on.
    pub fn validate(&self, now_unix: u64) -> Result<(), anyhow::Error> {
        if self.v != DESCRIPTOR_VERSION {
            bail!(
                "descriptor version {} is not supported (expected {DESCRIPTOR_VERSION})",
                self.v
            );
        }
        if self.id.trim().is_empty() {
            bail!("descriptor has an empty 'id'");
        }
        if let Some(expires_at) = self.expires_at {
            if expires_at <= now_unix {
                bail!(
                    "stack '{}' expired at {expires_at} (now {now_unix}), there is nothing left to bite",
                    self.id
                );
            }
        }

        let relays = self.chains.iter().filter(|c| c.role == ROLE_RELAY).count();
        if relays != 1 {
            bail!("a descriptor must have exactly one '{ROLE_RELAY}' chain, found {relays}");
        }

        let mut roles = HashSet::new();
        let mut para_ids = HashSet::new();
        for chain in &self.chains {
            if !roles.insert(chain.role.as_str()) {
                bail!("chain role '{}' appears more than once", chain.role);
            }
            match (chain.role == ROLE_RELAY, chain.para_id) {
                (true, Some(_)) => bail!("the relay chain must not have a 'paraId'"),
                (false, None) => bail!("chain '{}' has no 'paraId'", chain.role),
                (false, Some(id)) if !para_ids.insert(id) => {
                    bail!("paraId {id} appears more than once")
                }
                _ => {}
            }
            if chain.rpc.is_empty() {
                bail!("chain '{}' has no rpc endpoint", chain.role);
            }
            for rpc in &chain.rpc {
                check_url(rpc, &["wss"])
                    .with_context(|| format!("chain '{}' has an invalid rpc", chain.role))?;
            }
            parse_hex::<32>(&chain.genesis_hash, "genesisHash")
                .with_context(|| format!("chain '{}'", chain.role))?;
            if let Some(spec) = &chain.full_spec {
                check_url(&spec.url, &["https"]).with_context(|| {
                    format!("chain '{}' has an invalid fullSpec url", chain.role)
                })?;
                parse_hex::<32>(&spec.sha256, "fullSpec.sha256")
                    .with_context(|| format!("chain '{}'", chain.role))?;
            }
        }
        Ok(())
    }
}

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

/// Decode a fetched document: an envelope (signed) or a plain descriptor.
/// With a key only an envelope verifying against it is accepted.
pub fn decode_document(
    bytes: &[u8],
    key: Option<&[u8; 32]>,
) -> Result<(Descriptor, bool), anyhow::Error> {
    reject_duplicate_members(bytes).context("the descriptor is not valid I-JSON")?;
    let doc: serde_json::Value =
        serde_json::from_slice(bytes).context("the descriptor is not valid json")?;

    let is_envelope = doc.get("payload").is_some();
    let (descriptor, verified) = if is_envelope {
        let envelope: Envelope =
            serde_json::from_value(doc).context("invalid descriptor envelope")?;
        let payload = base64::engine::general_purpose::STANDARD
            .decode(envelope.payload.as_bytes())
            .context("the envelope payload is not standard base64")?;
        let verified = match key {
            Some(key) => {
                verify_signature(envelope.v, &payload, &envelope.signature, key)?;
                true
            }
            None => {
                warn!("the stack descriptor is signed but no key was given, its signature is NOT checked (pass the stack link or --stack-key to check it)");
                false
            }
        };
        reject_duplicate_members(&payload).context("the descriptor payload is not valid I-JSON")?;
        let descriptor: Descriptor =
            serde_json::from_slice(&payload).context("invalid descriptor payload")?;
        if descriptor.v != envelope.v {
            bail!(
                "the envelope is version {} but its payload is version {}",
                envelope.v,
                descriptor.v
            );
        }
        (descriptor, verified)
    } else {
        if key.is_some() {
            bail!("the pointer carries a key but the descriptor is not signed; refusing it");
        }
        (
            serde_json::from_value(doc).context("invalid descriptor")?,
            false,
        )
    };
    Ok((descriptor, verified))
}

fn verify_signature(
    version: u32,
    payload: &[u8],
    signature: &str,
    key: &[u8; 32],
) -> Result<(), anyhow::Error> {
    let key = VerifyingKey::from_bytes(key).map_err(|e| anyhow!("invalid operator key: {e}"))?;
    let signature = Signature::from_bytes(&parse_hex::<64>(signature, "signature")?);
    let mut message = format!("trinity-stack/descriptor/v{version}").into_bytes();
    message.extend_from_slice(payload);
    key.verify_strict(&message, &signature)
        .map_err(|_| anyhow!("the descriptor signature does not verify against the stack key"))
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
/// `<base_path>/stack` and checked against their digests, and each para's
/// core count read from the relay (at `relay_bite_at` when set).
pub async fn materialize(
    stack: &StackConfig,
    base_path: &Path,
    relay_bite_at: Option<u32>,
) -> Result<StackBite, anyhow::Error> {
    let pointer = Pointer::parse(&stack.pointer, stack.key.as_deref())?;
    info!("📚 reading stack descriptor from {}", pointer.source);
    let bytes = fetch(&pointer.source).await?;
    let (descriptor, verified) = decode_document(&bytes, pointer.key.as_ref())?;
    descriptor.validate(crate::manifest::now_unix())?;
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
    let relay = descriptor.relay();
    let relay_rpc = pick_endpoint(relay).await?;
    let relay_spec = download_spec(&http, relay, &spec_dir, &relay_name).await?;
    let relay_spec_id = spec_id(&relay_spec).await?;

    let cores = match relay_core_counts(&relay_rpc, relay_bite_at).await {
        Ok(cores) => Some(cores),
        Err(e) => {
            warn!("can't read the core assignments of '{relay_name}' ({e}); every para gets 1 core unless set with --para-cores or `cores`");
            None
        }
    };

    let mut parachains = vec![];
    let mut total_cores = 0;
    for chain in chains {
        let para_id = chain.para_id.expect("validated");
        let rpc = pick_endpoint(chain).await?;
        let spec = download_spec(&http, chain, &spec_dir, &relay_name).await?;
        check_para_spec(&spec, para_id, &relay_spec_id).await?;

        let para_cores = match cores.as_ref().map(|c| c.get(&para_id).copied()) {
            Some(Some(n)) => n,
            Some(None) => {
                warn!(
                    "para {para_id} ({}) has no core on the relay, giving it 1",
                    chain.role
                );
                1
            }
            None => 1,
        };
        info!(
            "🧩 {} (para {para_id}): {para_cores} core(s), rpc {rpc}",
            chain.role
        );
        total_cores += para_cores;

        parachains.push(ParachainConfig {
            parachain_type: "custom".into(),
            runtime_override: None,
            upgrade: None,
            enabled: None,
            bite_at: None,
            rpc_endpoint: Some(rpc),
            id: Some(para_id),
            chain_spec: Some(spec.to_string_lossy().to_string()),
            cores: Some(para_cores),
            role: Some(chain.role.clone()),
            command: None,
            image: None,
        });
    }

    info!("🧩 the stack's paras use {total_cores} core(s) on the relay");

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
        key: pointer.key.map(|k| format!("0x{}", hex::encode(k))),
        sequence: descriptor.sequence,
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
    let relay = descriptor.relay();
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
                let chain = descriptor
                    .parachains()
                    .find(|c| c.role.eq_ignore_ascii_case(role))
                    .ok_or_else(|| {
                        anyhow!(
                            "stack '{}' has no '{role}' chain (it has: {})",
                            descriptor.id,
                            descriptor
                                .parachains()
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
        None => Ok(descriptor
            .parachains()
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

async fn rpc_client(url: &str) -> Result<RpcClient, anyhow::Error> {
    let client = if url.starts_with("ws://") {
        RpcClient::from_insecure_url(url).await?
    } else {
        RpcClient::from_url(url).await?
    };
    Ok(client)
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

/// Cores per para id on the relay, from `ParachainHost_claim_queue`.
async fn relay_core_counts(
    url: &str,
    at_block: Option<u32>,
) -> Result<BTreeMap<u32, u32>, anyhow::Error> {
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
    let expected = spec.sha256.trim_start_matches("0x").to_ascii_lowercase();

    if let Ok(bytes) = fs::read(&path).await {
        if hex::encode(Sha256::digest(&bytes)) == expected {
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
    let got = hex::encode(Sha256::digest(&bytes));
    if got != expected {
        bail!(
            "the {} chain-spec at {} has sha256 0x{got}, the descriptor says 0x{expected}",
            chain.role,
            spec.url
        );
    }
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

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// 0x-prefixed lowercase hex of exactly `N` bytes.
fn parse_hex<const N: usize>(s: &str, what: &str) -> Result<[u8; N], anyhow::Error> {
    let hex_part = s
        .strip_prefix("0x")
        .ok_or_else(|| anyhow!("{what} '{s}' is not 0x-prefixed"))?;
    if hex_part.chars().any(|c| c.is_ascii_uppercase()) {
        bail!("{what} '{s}' must be lowercase hex");
    }
    let bytes = hex::decode(hex_part).map_err(|e| anyhow!("{what} '{s}' is not hex: {e}"))?;
    bytes
        .try_into()
        .map_err(|b: Vec<u8>| anyhow!("{what} must be {N} bytes, got {}", b.len()))
}

/// The RFC's URL rules: one of the `secure` schemes, or its insecure
/// counterpart for a literal private address; never credentials.
fn check_url(s: &str, secure: &[&str]) -> Result<(), anyhow::Error> {
    if s.contains('\\') {
        bail!("url '{s}' contains '\\'");
    }
    let url = url::Url::parse(s).map_err(|e| anyhow!("invalid url '{s}': {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("url '{s}' must not carry credentials");
    }
    let scheme = url.scheme();
    if secure.contains(&scheme) {
        return Ok(());
    }
    let insecure_ok = matches!(scheme, "http" | "ws")
        && secure
            .iter()
            .any(|s| (*s == "https" && scheme == "http") || (*s == "wss" && scheme == "ws"))
        && match url.host() {
            Some(url::Host::Ipv4(ip)) => is_private(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => is_private(IpAddr::V6(ip)),
            _ => false,
        };
    if insecure_ok {
        Ok(())
    } else {
        bail!(
            "url '{s}' must use {} ({scheme} is only allowed for a literal private address)",
            secure.join("/")
        )
    }
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Refuse duplicate member names anywhere in the document: two parsers that
/// resolve them differently would read one signed descriptor two ways.
fn reject_duplicate_members(bytes: &[u8]) -> Result<(), anyhow::Error> {
    serde_json::from_slice::<NoDuplicates>(bytes)?;
    Ok(())
}

struct NoDuplicates;

impl<'de> Deserialize<'de> for NoDuplicates {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NoDuplicatesVisitor)
    }
}

struct NoDuplicatesVisitor;

impl<'de> Visitor<'de> for NoDuplicatesVisitor {
    type Value = NoDuplicates;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any json value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_i64<E>(self, _: i64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_u64<E>(self, _: u64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_f64<E>(self, _: f64) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_str<E>(self, _: &str) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_unit<E>(self) -> Result<NoDuplicates, E> {
        Ok(NoDuplicates)
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<NoDuplicates, A::Error> {
        while seq.next_element::<NoDuplicates>()?.is_some() {}
        Ok(NoDuplicates)
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<NoDuplicates, A::Error> {
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!("duplicate member '{key}'")));
            }
            map.next_value::<NoDuplicates>()?;
        }
        Ok(NoDuplicates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

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

    fn envelope(payload: &[u8], signing: &SigningKey, version: u32) -> Vec<u8> {
        let mut msg = format!("trinity-stack/descriptor/v{version}").into_bytes();
        msg.extend_from_slice(payload);
        let sig = signing.sign(&msg);
        serde_json::json!({
            "$v": version,
            "payload": base64::engine::general_purpose::STANDARD.encode(payload),
            "signature": format!("0x{}", hex::encode(sig.to_bytes())),
        })
        .to_string()
        .into_bytes()
    }

    fn key() -> (SigningKey, [u8; 32]) {
        let signing = SigningKey::from_bytes(&KEY_SEED);
        let public = signing.verifying_key().to_bytes();
        (signing, public)
    }

    #[test]
    fn parses_links_urls_and_files() {
        let k = format!("0x{}", "ab".repeat(32));
        let link = format!(
            "polkadotapp://stack?key={k}&url=https%3A%2F%2Fpreviewnet.substrate.dev%2F.well-known%2Ftrinity-stack.json&foo=bar"
        );
        let p = Pointer::parse(&link, None).unwrap();
        assert_eq!(
            p.source,
            Source::Url("https://previewnet.substrate.dev/.well-known/trinity-stack.json".into())
        );
        assert_eq!(p.key, Some([0xab; 32]));

        // any scheme/domain carries the same pointer
        let p = Pointer::parse(
            "https://app.example/open?url=https%3A%2F%2Fa.example%2Fs.json",
            None,
        )
        .unwrap();
        assert_eq!(p.source, Source::Url("https://a.example/s.json".into()));
        assert_eq!(p.key, None);

        let p = Pointer::parse("https://a.example/s.json", Some(&k)).unwrap();
        assert_eq!(p.source, Source::Url("https://a.example/s.json".into()));
        assert_eq!(p.key, Some([0xab; 32]));

        assert_eq!(
            Pointer::parse("./stack.json", None).unwrap().source,
            Source::File("./stack.json".into())
        );
        assert_eq!(
            Pointer::parse("file:///tmp/stack.json", None)
                .unwrap()
                .source,
            Source::File("/tmp/stack.json".into())
        );
    }

    #[test]
    fn rejects_bad_pointers() {
        let k = format!("0x{}", "ab".repeat(32));
        let other = format!("0x{}", "cd".repeat(32));
        let link = format!("polkadotapp://stack?key={k}&url=https%3A%2F%2Fa.example%2Fs.json");
        assert!(
            Pointer::parse(&link, Some(&other)).is_err(),
            "conflicting keys"
        );
        assert!(
            Pointer::parse("http://a.example/s.json", None).is_err(),
            "plain http"
        );
        assert!(
            Pointer::parse("http://127.0.0.1:8080/s.json", None).is_ok(),
            "private literal"
        );
        assert!(
            Pointer::parse("https://u:p@a.example/s.json", None).is_err(),
            "credentials"
        );
        assert!(
            Pointer::parse("https://a.example/s.json", Some("0xABCD")).is_err(),
            "bad key"
        );
    }

    #[test]
    fn decodes_a_plain_descriptor() {
        let (d, verified) = decode_document(descriptor_json().as_bytes(), None).unwrap();
        assert!(!verified);
        assert_eq!(d.id, "previewnet");
        assert_eq!(d.relay().role, "Relay");
        assert_eq!(d.parachains().count(), 2);
        d.validate(0).unwrap();
    }

    #[test]
    fn verifies_a_signed_descriptor() {
        let (signing, public) = key();
        let doc = envelope(descriptor_json().as_bytes(), &signing, 1);
        let (d, verified) = decode_document(&doc, Some(&public)).unwrap();
        assert!(verified);
        assert_eq!(d.sequence, 42);

        // without a key the envelope is read as unsigned
        let (_, verified) = decode_document(&doc, None).unwrap();
        assert!(!verified);
    }

    #[test]
    fn rejects_a_wrong_key_or_a_tampered_payload() {
        let (signing, _) = key();
        let doc = envelope(descriptor_json().as_bytes(), &signing, 1);
        let other = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        assert!(decode_document(&doc, Some(&other)).is_err());

        let (_, public) = key();
        let mut env: serde_json::Value = serde_json::from_slice(&doc).unwrap();
        let tampered = descriptor_json().replace("previewnet", "evilnet");
        env["payload"] = base64::engine::general_purpose::STANDARD
            .encode(tampered.as_bytes())
            .into();
        let doc = serde_json::to_vec(&env).unwrap();
        assert!(decode_document(&doc, Some(&public)).is_err());
    }

    #[test]
    fn a_keyed_pointer_refuses_a_plain_descriptor() {
        let (_, public) = key();
        let err = decode_document(descriptor_json().as_bytes(), Some(&public)).unwrap_err();
        assert!(err.to_string().contains("not signed"), "{err}");
    }

    #[test]
    fn rejects_an_envelope_version_mismatch() {
        let (signing, public) = key();
        let doc = envelope(descriptor_json().as_bytes(), &signing, 2);
        // the signature is over the v2 prefix, so it verifies, but the payload is v1
        let err = decode_document(&doc, Some(&public)).unwrap_err();
        assert!(err.to_string().contains("version"), "{err}");
    }

    #[test]
    fn rejects_duplicate_members() {
        let doc = r#"{"$v":1,"id":"a","id":"b","chains":[]}"#;
        assert!(decode_document(doc.as_bytes(), None).is_err());
        let nested = r#"{"$v":1,"id":"a","chains":[{"role":"Relay","role":"X"}]}"#;
        assert!(decode_document(nested.as_bytes(), None).is_err());
    }

    fn descriptor_with(f: impl FnOnce(&mut serde_json::Value)) -> Descriptor {
        let mut v: serde_json::Value = serde_json::from_str(&descriptor_json()).unwrap();
        f(&mut v);
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn validation_rules() {
        let d = descriptor_with(|v| v["chains"][2]["role"] = "AssetHub".into());
        assert!(d.validate(0).is_err(), "duplicate role");
        let d = descriptor_with(|v| v["chains"][2]["paraId"] = 1500.into());
        assert!(d.validate(0).is_err(), "duplicate para id");
        let d = descriptor_with(|v| v["chains"][1]["role"] = "Relay".into());
        assert!(d.validate(0).is_err(), "two relays");
        let d = descriptor_with(|v| {
            v["chains"][1].as_object_mut().unwrap().remove("paraId");
        });
        assert!(d.validate(0).is_err(), "para without id");
        let d = descriptor_with(|v| v["chains"][1]["rpc"] = serde_json::json!([]));
        assert!(d.validate(0).is_err(), "no rpc");
        let d = descriptor_with(|v| v["expiresAt"] = 100.into());
        assert!(d.validate(100).is_err(), "expired");
        assert!(d.validate(99).is_ok());
        let d = descriptor_with(|v| v["$v"] = 2.into());
        assert!(d.validate(0).is_err(), "version");
    }

    #[test]
    fn selects_chains() {
        let d = descriptor_with(|_| {});
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
        Pointer::parse(&stack.pointer, stack.key.as_deref()).unwrap();
        let merged = merge_user_config(derived(), Some(&user)).unwrap();
        assert_eq!(merged.parachains.unwrap()[0].cores, Some(3));
    }
}
