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
    Metadata, OnlineClient, PolkadotConfig,
};

pub fn storage_key(pallet: &str, item: &str) -> String {
    array_bytes::bytes2hex(
        "",
        substorager::storage_value_key(pallet.as_bytes(), item.as_bytes()),
    )
}

pub struct ChainMetadata {
    rpc: RpcClient,
    metadata: Metadata,
}

impl ChainMetadata {
    /// Fetch metadata from the chain being bitten. Returns `None` (with a
    /// warning) when the endpoint can't be reached, so a bite without network
    /// access to the source falls back to unverified overrides rather than
    /// failing outright.
    pub async fn fetch(chain: &str, url: &str) -> Option<Self> {
        match OnlineClient::<PolkadotConfig>::from_url(url).await {
            Ok(client) => {
                let metadata = client.metadata();
                let rpc = match RpcClient::from_url(url).await {
                    Ok(rpc) => rpc,
                    Err(e) => {
                        warn!("{chain}: can't open rpc client to {url}: {e}");
                        return None;
                    }
                };
                debug!("{chain}: metadata fetched from {url}");
                Some(Self { rpc, metadata })
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

    pub fn has_item(&self, pallet: &str, item: &str) -> bool {
        self.value_ty(pallet, item).is_some()
    }

    /// Require `value_hex` to decode against the item's on-chain type and
    /// re-encode to the same bytes. Trailing bytes are an error too - that is
    /// what a wrong length prefix looks like.
    pub fn verify_value(
        &self,
        pallet: &str,
        item: &str,
        value_hex: &str,
    ) -> Result<(), anyhow::Error> {
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

    /// Read a live storage value as hex (no `0x` prefix).
    pub async fn storage_value(&self, key: &str) -> Result<Option<String>, anyhow::Error> {
        let mut params = RpcParams::new();
        params.push(format!("0x{key}"))?;
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
        let mut value =
            scale_value::scale::decode_as_type(&mut &bytes[..], ty, self.metadata.types())
                .map_err(|e| anyhow!("{pallet}::{item}: live value does not decode: {e}"))?;

        patch(&mut value)?;

        let mut out = vec![];
        scale_value::scale::encode_as_type(&value, ty, self.metadata.types(), &mut out)
            .map_err(|e| anyhow!("{pallet}::{item}: patched value does not re-encode: {e}"))?;
        Ok(hex::encode(out))
    }
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
