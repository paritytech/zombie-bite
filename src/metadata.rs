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

pub fn storage_key(pallet: &str, item: &str) -> String {
    array_bytes::bytes2hex(
        "",
        substorager::storage_value_key(pallet.as_bytes(), item.as_bytes()),
    )
}

/// What an override is checked against. `ChainMetadata` is the real
/// implementation; tests use a double.
pub trait RuntimeCheck {
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
        let rpc = match RpcClient::from_url(url).await {
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
}
