//! Metadata-checked storage overrides.
//!
//! Storage keys are derived from pallet/item names and every candidate value is
//! decoded against the item's real on-chain type, so a runtime that renames an
//! item, changes a type or reorders a struct fails the bite loudly instead of
//! producing a network that silently never builds blocks.

use anyhow::{anyhow, bail};
use tracing::{debug, warn};
use zombienet_sdk::subxt::{
    ext::{
        scale_value,
        subxt_rpcs::{client::RpcParams, RpcClient},
    },
    Metadata,
};

use crate::rpc::rpc_client;

/// Cores per para id from the relay's `ParachainHost_claim_queue` (a SCALE
/// `BTreeMap<CoreIndex, VecDeque<ParaId>>`). Each core goes to the para that
/// holds most of its queue (the first one to appear on a tie), so a core shared
/// through interlaced or on-demand assignments is counted once.
pub(crate) fn claim_queue_cores(
    bytes: &[u8],
) -> Result<std::collections::BTreeMap<u32, u32>, anyhow::Error> {
    use std::collections::{BTreeMap, VecDeque};
    use zombienet_sdk::subxt::ext::codec::Decode;

    let mut input = bytes;
    let queue = BTreeMap::<u32, VecDeque<u32>>::decode(&mut input)
        .map_err(|e| anyhow!("can't decode the claim queue: {e}"))?;
    if !input.is_empty() {
        bail!("{} trailing byte(s) after the claim queue", input.len());
    }

    let mut cores = BTreeMap::new();
    for paras in queue.values() {
        // (para, entries), in order of first appearance
        let mut counts: Vec<(u32, usize)> = vec![];
        for para in paras {
            match counts.iter_mut().find(|(p, _)| p == para) {
                Some((_, n)) => *n += 1,
                None => counts.push((*para, 1)),
            }
        }
        let mut owner: Option<(u32, usize)> = None;
        for (para, n) in counts {
            if owner.is_none_or(|(_, best)| n > best) {
                owner = Some((para, n));
            }
        }
        if let Some((para, _)) = owner {
            *cores.entry(para).or_insert(0) += 1;
        }
    }
    Ok(cores)
}

pub fn storage_key(pallet: &str, item: &str) -> String {
    array_bytes::bytes2hex(
        "",
        substorager::storage_value_key(pallet.as_bytes(), item.as_bytes()),
    )
}

/// What an override is checked against. `ChainMetadata` is the real
/// implementation; tests use a double.
///
/// `Sync`, so that the `&dyn RuntimeCheck` an override set holds across an
/// `.await` is `Send`, and so is the `bite` future holding it.
pub trait RuntimeCheck: Sync {
    fn has_item(&self, pallet: &str, item: &str) -> bool;
    fn verify_value(&self, pallet: &str, item: &str, value_hex: &str) -> Result<(), anyhow::Error>;
}

pub struct ChainMetadata {
    rpc: RpcClient,
    metadata: Metadata,
    /// Block the state is read at, so metadata and storage come from the same
    /// runtime as the state being imported.
    at: Option<String>,
}

impl ChainMetadata {
    /// Fetch metadata from the chain being bitten, at `at_block` when the bite
    /// is pinned to a block (otherwise at head). Returns `None` (with a
    /// warning) when the endpoint can't be reached, so a bite without network
    /// access to the source falls back to unverified overrides rather than
    /// failing outright.
    pub async fn fetch(chain: &str, url: &str, at_block: Option<u32>) -> Option<Self> {
        let rpc = match rpc_client(url).await {
            Ok(rpc) => rpc,
            Err(e) => {
                warn!("{chain}: can't reach {url}, overrides will not be verified against the runtime: {e}");
                return None;
            }
        };

        let at = match at_block {
            Some(block) => match block_hash(&rpc, block).await {
                Ok(Some(hash)) => Some(hash),
                _ => {
                    warn!("{chain}: can't resolve the hash of block {block}, overrides will not be verified against the runtime");
                    return None;
                }
            },
            None => None,
        };

        match fetch_metadata(&rpc, at.as_deref()).await {
            Ok(metadata) => {
                debug!(
                    "{chain}: metadata fetched from {url} at {}",
                    at.as_deref().unwrap_or("head")
                );
                Some(Self { rpc, metadata, at })
            }
            Err(e) => {
                warn!("{chain}: can't fetch metadata from {url}, overrides will not be verified against the runtime: {e}");
                None
            }
        }
    }

    /// Type id of the item's value, or `None` when the runtime has no such
    /// pallet or item.
    fn value_ty(&self, pallet: &str, item: &str) -> Option<u32> {
        Some(
            self.metadata
                .pallet_by_name(pallet)?
                .storage()?
                .entry_by_name(item)?
                .entry_type()
                .value_ty(),
        )
    }

    fn verify(&self, pallet: &str, item: &str, value_hex: &str) -> Result<(), anyhow::Error> {
        let ty = self
            .value_ty(pallet, item)
            .ok_or_else(|| anyhow!("{pallet}::{item} not in metadata"))?;
        let bytes = hex::decode(value_hex)
            .map_err(|e| anyhow!("{pallet}::{item}: value is not valid hex: {e}"))?;

        let mut cursor = &bytes[..];
        let value = scale_value::scale::decode_as_type(&mut cursor, ty, self.metadata.types())
            .map_err(|e| {
                anyhow!("{pallet}::{item}: value does not decode as its on-chain type: {e}")
            })?;
        if !cursor.is_empty() {
            bail!(
                "{pallet}::{item}: {} trailing byte(s) after decoding, the value is malformed (wrong length prefix?)",
                cursor.len()
            );
        }

        let mut re_encoded = vec![];
        scale_value::scale::encode_as_type(&value, ty, self.metadata.types(), &mut re_encoded)
            .map_err(|e| anyhow!("{pallet}::{item}: value does not re-encode: {e}"))?;
        if re_encoded != bytes {
            bail!(
                "{pallet}::{item}: value is not byte-identical after a decode/encode round-trip (got 0x{}, expected 0x{value_hex})",
                hex::encode(&re_encoded)
            );
        }
        Ok(())
    }

    /// Field names of the runtime's `SessionKeys`, in encoding order (e.g.
    /// `["aura", "authority_discovery"]`), read from the value type of
    /// `Session::NextKeys`. `None` when the runtime has no session pallet.
    pub fn session_key_fields(&self) -> Result<Option<Vec<String>>, anyhow::Error> {
        let Some(ty) = self.value_ty("Session", "NextKeys") else {
            return Ok(None);
        };
        let resolved = self
            .metadata
            .types()
            .resolve(ty)
            .ok_or_else(|| anyhow!("Session::NextKeys: type {ty} is not in the registry"))?;
        let scale_info::TypeDef::Composite(composite) = &resolved.type_def else {
            bail!("Session::NextKeys: SessionKeys is not a struct, can't read its fields");
        };
        composite
            .fields
            .iter()
            .map(|f| {
                f.name
                    .clone()
                    .ok_or_else(|| anyhow!("Session::NextKeys: SessionKeys has unnamed fields"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Ask the runtime (`SessionKeys_decode_session_keys`, at the same block
    /// the metadata came from) how it decodes `encoded` session keys: the raw
    /// public keys with their `KeyTypeId`, or `None` when they don't decode.
    pub async fn decode_session_keys(
        &self,
        encoded: &[u8],
    ) -> Result<Option<Vec<(Vec<u8>, [u8; 4])>>, anyhow::Error> {
        use zombienet_sdk::subxt::ext::codec::{Decode, Encode};

        let mut params = RpcParams::new();
        params.push("SessionKeys_decode_session_keys")?;
        params.push(format!("0x{}", hex::encode(encoded.to_vec().encode())))?;
        if let Some(at) = &self.at {
            params.push(at)?;
        }
        let raw: String = self.rpc.request("state_call", params).await?;
        let bytes = hex::decode(raw.trim_start_matches("0x"))?;
        Option::<Vec<(Vec<u8>, [u8; 4])>>::decode(&mut &bytes[..])
            .map_err(|e| anyhow!("SessionKeys_decode_session_keys: can't decode the result: {e}"))
    }

    /// A `u64` (or narrower unsigned) pallet constant, e.g.
    /// `Babe::EpochDuration`; `None` when the runtime has no such constant.
    pub fn constant_u64(&self, pallet: &str, name: &str) -> Option<u64> {
        let bytes = self
            .metadata
            .pallet_by_name(pallet)?
            .constant_by_name(name)?
            .value();
        match bytes.len() {
            8 => Some(u64::from_le_bytes(bytes.try_into().ok()?)),
            4 => Some(u32::from_le_bytes(bytes.try_into().ok()?).into()),
            _ => None,
        }
    }

    /// Read a live storage value as hex (no `0x` prefix), at the same block the
    /// metadata came from.
    pub async fn storage_value(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let mut params = RpcParams::new();
        params.push(format!("0x{key}"))?;
        if let Some(at) = &self.at {
            params.push(at)?;
        }
        let raw: Option<String> = self.rpc.request("state_getStorage", params).await?;
        Ok(raw.map(|v| v.trim_start_matches("0x").to_string()))
    }

    /// Encode a value against the item's on-chain type, so its shape is the
    /// runtime's rather than a hand-rolled guess.
    pub fn encode_value(
        &self,
        pallet: &str,
        item: &str,
        value: &scale_value::Value<u32>,
    ) -> Result<String, anyhow::Error> {
        let ty = self
            .value_ty(pallet, item)
            .ok_or_else(|| anyhow!("{pallet}::{item} not in metadata"))?;
        let mut out = vec![];
        scale_value::scale::encode_as_type(value, ty, self.metadata.types(), &mut out).map_err(
            |e| anyhow!("{pallet}::{item}: value does not encode against the runtime's type: {e}"),
        )?;
        Ok(hex::encode(out))
    }

    /// Decode a live value, hand it to `patch`, and re-encode it. Only the
    /// fields `patch` touches change - everything else the live runtime
    /// configured is preserved byte for byte.
    pub fn patch_value(
        &self,
        pallet: &str,
        item: &str,
        value_hex: &str,
        patch: impl FnOnce(&mut scale_value::Value<u32>) -> Result<(), anyhow::Error>,
    ) -> Result<String, anyhow::Error> {
        let ty = self
            .value_ty(pallet, item)
            .ok_or_else(|| anyhow!("{pallet}::{item} not in metadata"))?;
        let bytes = hex::decode(value_hex)?;
        let mut cursor = &bytes[..];
        let mut value = scale_value::scale::decode_as_type(&mut cursor, ty, self.metadata.types())
            .map_err(|e| anyhow!("{pallet}::{item}: live value does not decode: {e}"))?;
        // Trailing bytes mean the metadata and the value disagree (e.g. the
        // runtime upgraded between the two reads); patching would silently
        // truncate the tail and still round-trip cleanly.
        if !cursor.is_empty() {
            bail!(
                "{pallet}::{item}: live value has {} trailing byte(s) against this runtime's type, refusing to patch it",
                cursor.len()
            );
        }

        patch(&mut value)?;

        let mut out = vec![];
        scale_value::scale::encode_as_type(&value, ty, self.metadata.types(), &mut out)
            .map_err(|e| anyhow!("{pallet}::{item}: patched value does not re-encode: {e}"))?;
        Ok(hex::encode(out))
    }
}

impl RuntimeCheck for ChainMetadata {
    fn has_item(&self, pallet: &str, item: &str) -> bool {
        self.value_ty(pallet, item).is_some()
    }

    /// Require `value_hex` to decode against the item's on-chain type and
    /// re-encode to the same bytes. Trailing bytes are an error too - that is
    /// what a wrong length prefix looks like.
    fn verify_value(&self, pallet: &str, item: &str, value_hex: &str) -> Result<(), anyhow::Error> {
        self.verify(pallet, item, value_hex)
    }
}

async fn block_hash(rpc: &RpcClient, block: u32) -> Result<Option<String>, anyhow::Error> {
    let mut params = RpcParams::new();
    params.push(block)?;
    Ok(rpc.request("chain_getBlockHash", params).await?)
}

async fn fetch_metadata(rpc: &RpcClient, at: Option<&str>) -> Result<Metadata, anyhow::Error> {
    use zombienet_sdk::subxt::ext::{
        codec::Decode,
        frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed},
    };

    let mut params = RpcParams::new();
    if let Some(at) = at {
        params.push(at)?;
    }
    let raw: String = rpc.request("state_getMetadata", params).await?;
    let bytes = hex::decode(raw.trim_start_matches("0x"))?;
    let prefixed = RuntimeMetadataPrefixed::decode(&mut &bytes[..])?;
    if !matches!(
        prefixed.1,
        RuntimeMetadata::V14(_) | RuntimeMetadata::V15(_)
    ) {
        bail!("unsupported metadata version, expected v14 or v15");
    }
    Metadata::try_from(prefixed).map_err(|e| anyhow!("can't read metadata: {e}"))
}

/// Get a mutable handle on a named inner struct.
pub fn nested_mut<'v>(
    value: &'v mut scale_value::Value<u32>,
    field: &str,
) -> Option<&'v mut scale_value::Value<u32>> {
    let scale_value::ValueDef::Composite(scale_value::Composite::Named(fields)) = &mut value.value
    else {
        return None;
    };
    fields
        .iter_mut()
        .find(|(name, _)| name == field)
        .map(|(_, v)| v)
}

/// Set a named field on a composite `Value`, keeping every other field as the
/// live runtime had it.
pub fn set_field(
    value: &mut scale_value::Value<u32>,
    field: &str,
    to: scale_value::Value<u32>,
) -> Result<(), anyhow::Error> {
    let scale_value::ValueDef::Composite(scale_value::Composite::Named(fields)) = &mut value.value
    else {
        bail!("expected a struct with named fields to set '{field}' on");
    };
    let entry = fields
        .iter_mut()
        .find(|(name, _)| name == field)
        .ok_or_else(|| anyhow!("no field '{field}' in value"))?;
    entry.1 = to;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use zombienet_sdk::subxt::ext::scale_value::{value, Value as ScaleValue};

    fn cores(n: u128) -> ScaleValue<u32> {
        ScaleValue::u128(n).map_context(|_| 0_u32)
    }

    #[test]
    fn set_field_replaces_only_that_field() {
        let mut v = value!({ num_cores: 18u32, max_pov_size: 5u32 }).map_context(|_| 0_u32);
        set_field(&mut v, "num_cores", cores(5)).unwrap();

        let expected = value!({ num_cores: 5u32, max_pov_size: 5u32 }).map_context(|_| 0_u32);
        assert_eq!(v, expected);
    }

    #[test]
    fn set_field_errors_on_unknown_field_and_wrong_shape() {
        let mut named = value!({ num_cores: 1u32 }).map_context(|_| 0_u32);
        assert!(set_field(&mut named, "nope", cores(5)).is_err());

        let mut unnamed = ScaleValue::u128(1).map_context(|_| 0_u32);
        assert!(set_field(&mut unnamed, "num_cores", cores(5)).is_err());
    }

    #[test]
    fn nested_mut_finds_the_inner_struct() {
        let mut v = value!({ scheduler_params: { num_cores: 18u32 } }).map_context(|_| 0_u32);

        let params = nested_mut(&mut v, "scheduler_params").expect("nested struct");
        set_field(params, "num_cores", cores(5)).unwrap();

        let expected = value!({ scheduler_params: { num_cores: 5u32 } }).map_context(|_| 0_u32);
        assert_eq!(v, expected);
        assert!(nested_mut(&mut v, "missing").is_none());
    }

    #[test]
    fn claim_queue_cores_from_previewnet() {
        // live `ParachainHost_claim_queue` of previewnet's relay: cores 0-2 to
        // 1500, 3 to 1502, 4 to 1501, 5 to 1600
        let raw = "18000000000cdc050000dc050000dc050000010000000cdc050000dc050000dc050000020000000cdc050000dc050000dc050000030000000cde050000de050000de050000040000000cdd050000dd050000dd050000050000000c400600004006000040060000";
        let cores = claim_queue_cores(&hex::decode(raw).unwrap()).unwrap();
        assert_eq!(
            cores.into_iter().collect::<Vec<_>>(),
            vec![(1500, 3), (1501, 1), (1502, 1), (1600, 1)]
        );
    }

    #[test]
    fn claim_queue_shared_core_goes_to_the_majority() {
        use std::collections::{BTreeMap, VecDeque};
        use zombienet_sdk::subxt::ext::codec::Encode;
        let mut queue: BTreeMap<u32, VecDeque<u32>> = BTreeMap::new();
        queue.insert(0, VecDeque::from([2000, 2001, 2001]));
        queue.insert(1, VecDeque::from([2002, 2003])); // tie: first wins
        queue.insert(2, VecDeque::new()); // idle core
        let cores = claim_queue_cores(&queue.encode()).unwrap();
        assert_eq!(
            cores.into_iter().collect::<Vec<_>>(),
            vec![(2001, 1), (2002, 1)]
        );
        assert!(claim_queue_cores(&[0x04]).is_err());
    }
}
