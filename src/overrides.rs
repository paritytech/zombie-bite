use codec::Encode;
use serde_json::{json, Value};
use std::{env, path::PathBuf};
use tokio::fs;
use tracing::{info, warn};

use crate::{
    config::{get_assigned_cores, CoresOverride, Parachain, Relaychain},
    metadata::{nested_mut, set_field, storage_key, ChainMetadata, RuntimeCheck},
    utils::{
        generate_collator_key_from_seed, generate_collator_next_keys_injects, get_validator_keys,
        ParaId, ValidationCode,
    },
};

use zombienet_sdk::generators::core_assignment;
use zombienet_sdk::subxt::ext::scale_value::Value as ScaleValue;

/// Storage overrides and injects for one chain, keyed by pallet and item name.
///
/// Every entry is checked against the chain's own metadata when it is
/// available: an item the runtime does not have is skipped, and a value that
/// does not survive a decode/encode round-trip against its real on-chain type
/// fails the bite.
struct OverrideSet<'a> {
    meta: Option<&'a dyn RuntimeCheck>,
    overrides: Value,
    injects: Value,
    skipped: Vec<String>,
    errors: Vec<String>,
}

impl<'a> OverrideSet<'a> {
    fn new(meta: Option<&'a dyn RuntimeCheck>) -> Self {
        Self {
            meta,
            overrides: json!({}),
            injects: json!({}),
            skipped: vec![],
            errors: vec![],
        }
    }

    /// `None` when the entry should be dropped (item absent, or value invalid).
    ///
    /// `required` entries are ones the user explicitly asked for (a carried
    /// upgrade, a wasm override, a sudo key): an item the runtime does not have
    /// is an error there, not something to quietly drop.
    fn checked_key(
        &mut self,
        pallet: &str,
        item: &str,
        value: &str,
        required: bool,
    ) -> Option<String> {
        if let Some(meta) = self.meta {
            if !meta.has_item(pallet, item) {
                if required {
                    self.errors.push(format!(
                        "{pallet}::{item} is not in the runtime, so it can't be set"
                    ));
                } else {
                    self.skipped.push(format!("{pallet}::{item}"));
                }
                return None;
            }
            if let Err(e) = meta.verify_value(pallet, item, value) {
                self.errors.push(e.to_string());
                return None;
            }
        }
        Some(storage_key(pallet, item))
    }

    fn set(&mut self, pallet: &str, item: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, false) {
            self.overrides[key] = json!(value);
        }
    }

    /// Like `set`, for an entry the user asked for explicitly.
    fn set_required(&mut self, pallet: &str, item: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, true) {
            self.overrides[key] = json!(value);
        }
    }

    /// Map entry; `key_suffix` is the already-hashed map key.
    fn set_map(&mut self, pallet: &str, item: &str, key_suffix: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, false) {
            self.overrides[format!("{key}{key_suffix}")] = json!(value);
        }
    }

    fn set_map_required(
        &mut self,
        pallet: &str,
        item: &str,
        key_suffix: &str,
        value: impl AsRef<str>,
    ) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, true) {
            self.overrides[format!("{key}{key_suffix}")] = json!(value);
        }
    }

    fn inject(&mut self, pallet: &str, item: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, false) {
            self.injects[key] = json!(value);
        }
    }

    fn inject_required(&mut self, pallet: &str, item: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, true) {
            self.injects[key] = json!(value);
        }
    }

    fn inject_map(&mut self, pallet: &str, item: &str, key_suffix: &str, value: impl AsRef<str>) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, false) {
            self.injects[format!("{key}{key_suffix}")] = json!(value);
        }
    }

    fn inject_map_required(
        &mut self,
        pallet: &str,
        item: &str,
        key_suffix: &str,
        value: impl AsRef<str>,
    ) {
        let value = value.as_ref();
        if let Some(key) = self.checked_key(pallet, item, value, true) {
            self.injects[format!("{key}{key_suffix}")] = json!(value);
        }
    }

    /// Item the runtime must have, whose value is too large to be worth
    /// verifying (a multi-MB runtime blob decodes into millions of `Value`
    /// nodes and the value is built by us from `Encode` anyway).
    fn inject_map_unverified(
        &mut self,
        pallet: &str,
        item: &str,
        key_suffix: &str,
        value: impl AsRef<str>,
    ) {
        if let Some(meta) = self.meta {
            if !meta.has_item(pallet, item) {
                self.errors.push(format!(
                    "{pallet}::{item} is not in the runtime, so it can't be set"
                ));
                return;
            }
        }
        let key = storage_key(pallet, item);
        self.injects[format!("{key}{key_suffix}")] = json!(value.as_ref());
    }

    /// Well-known keys that are not pallet storage items (`:code`,
    /// `:UsePreviousValidators:`), so there is no type to check them against.
    fn set_raw(&mut self, key: &str, value: impl AsRef<str>) {
        self.overrides[key] = json!(value.as_ref());
    }

    fn inject_raw(&mut self, key: &str, value: impl AsRef<str>) {
        self.injects[key] = json!(value.as_ref());
    }

    fn finish(self, chain: &str) -> Result<(Value, Value), anyhow::Error> {
        if !self.errors.is_empty() {
            anyhow::bail!(
                "{chain}: {} override(s) do not match the runtime:\n  - {}",
                self.errors.len(),
                self.errors.join("\n  - ")
            );
        }
        if !self.skipped.is_empty() {
            info!(
                "{chain}: skipped {} override(s) the runtime does not have: {}",
                self.skipped.len(),
                self.skipped.join(", ")
            );
        }
        Ok((self.overrides, self.injects))
    }
}

/// Seed `System::AuthorizedUpgrade` with the blob's hash, the state a passed
/// `authorize_upgrade(hash)` referendum leaves behind. The permissionless
/// `apply_authorized_upgrade(blob)` can then enact the upgrade through the
/// production path, which needs no sudo (usable on Kusama/Polkadot forks).
async fn inject_authorized_upgrade(set: &mut OverrideSet<'_>, upgrade_wasm: &str) {
    let wasm_content = fs::read(upgrade_wasm)
        .await
        .unwrap_or_else(|_| panic!("Error reading upgrade wasm from path {}", upgrade_wasm));
    // CodeUpgradeAuthorization { code_hash, check_version: true }
    let value = format!(
        "{}01",
        hex::encode(subhasher::blake2_256(&wasm_content[..]))
    );
    set.inject_required("System", "AuthorizedUpgrade", value);
}

/// Patch `num_cores` into the live `HostConfiguration`, leaving every other
/// field the production chain configured (executor params, async backing,
/// max_pov_size) untouched.
///
/// The built-in per-relay blob is only used when the source can't be reached at
/// all: it is a snapshot of a past runtime, so it drops whatever the live chain
/// has configured since.
async fn host_config(
    relay: &Relaychain,
    num_cores: u32,
    meta: Option<&ChainMetadata>,
) -> Result<String, anyhow::Error> {
    if let Some(meta) = meta {
        let key = storage_key("Configuration", "ActiveConfig");
        let live = meta
            .storage_value(&key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Configuration::ActiveConfig is empty on the source chain, refusing to replace it with a built-in blob"))?;
        let patched = patch_num_cores(meta, &live, num_cores)?;
        info!("Configuration::ActiveConfig patched from the live value (num_cores -> {num_cores})");
        return Ok(patched);
    }

    if relay.is_custom() {
        anyhow::bail!(
            "{}: there is no built-in host config for a custom relay, so the bite needs to read Configuration::ActiveConfig from it - check the endpoint",
            relay.as_chain_string()
        );
    }

    warn!("using the built-in host config: it is a snapshot of a past runtime, so executor params, async backing settings and max_pov_size of the live chain are lost");

    let cores = array_bytes::bytes2hex("", num_cores.encode());
    Ok(match relay {
        Relaychain::Westend { .. } => {
            format!("00003000005000005555150000008000fbff0100000200000a000000c80000006400000006000000020000000000a00000c800000a00000000c0220fca950300000000000000000000c0220fca9503000000000000000000e8030000009001000a000000009001000c01002000000600c4090000000000000601983a0000000000008070000001c800000006000000580200000200000028000000000000000200000001000000020000000f00000002000000100a010000000a00000005000000010500000005000000{}1027000080b2e60e80c3c9018096980000000000000000000000000000000000", cores)
        }
        Relaychain::Polkadot { .. } => {
            format!("0000300000500000aaaa020000001000fbff0000100000000a000000403800005802000003000000020000000000a00000c800008000000000e8764817000000000000000000000000e87648170000000000000000000000190000000090010080000000009001000c01002000000600c4090000000000000601983a0000000000004038000000060000005802000003000000d5000000000000001e00000006000000020000001400000002000000100b060000000a0000000a000000010500000005000000{}f401000080b2e60e80c3c90180b2e60e00000000000000000000000000000000", cores)
        }
        Relaychain::Kusama { .. } => {
            format!("0000300000500000aaaa0a0000004000fbff0000800000000a000000100e00005802000006000000020000000000a00000c800001e000000005039278c0400000000000000000000005039278c040000000000000000000019000000009001001e000000009001000c01002000000600c4090000000000000601983a0000000000008070000001bc0200000600000058020000030000002b010000000000001e00000006000000020000001400000002000000100b060000000a0000000a000000010500000005000000{}f401000080b2e60e80c3c90100f2052a01000000000000000000000000000000", cores)
        }
        Relaychain::Custom { .. } => unreachable!("custom relays bail above"),
        Relaychain::Paseo { .. } => {
            format!("e067350000800000aaaa020000001000fbff0000100000000a0000003c0000003c00000003000000020000000000a00000c800001e0000000000000000000000000000000000000000000000000000000000000000000000e8030000009001001e000000009001000c01002000000600c4090000000000000601983a000000000000b00400000006000000640000000200000019000000000000000200000002000000020000000500000001000000100b010000000a00000004000000010300000005000000{}6400000080b2e60e80c3c9018096980000000000000000000000000000000000", cores)
        }
    })
}

/// `num_cores` lives in `scheduler_params` on current runtimes and at the top
/// level on older ones.
fn patch_num_cores(
    meta: &ChainMetadata,
    live: &str,
    num_cores: u32,
) -> Result<String, anyhow::Error> {
    meta.patch_value("Configuration", "ActiveConfig", live, |value| {
        // context is only used for decode diagnostics, encoding ignores it
        let cores = ScaleValue::u128(num_cores as u128).map_context(|_| 0_u32);
        if let Some(params) = nested_mut(value, "scheduler_params") {
            set_field(params, "num_cores", cores)
        } else {
            set_field(value, "num_cores", cores)
        }
    })
}

/// Generate the injects for Session.NextKeys storage overrides for validators
fn generate_next_keys_injects(
    set: &mut OverrideSet<'_>,
    validator_keys: &[&crate::utils::ValidatorKeys],
) {
    for keys in validator_keys {
        let stash_bytes = hex::decode(keys.stash).expect("stash should be valid hex");
        let stash_hash = array_bytes::bytes2hex("", &subhasher::twox64_concat(&stash_bytes)[..8]);
        set.inject_map(
            "Session",
            "NextKeys",
            &format!("{stash_hash}{}", keys.stash),
            keys.session_keys_encoded(),
        );
    }
}

/// Generate the storage overrides for relay chain validators
fn generate_rc_overrides(
    set: &mut OverrideSet<'_>,
    validator_keys: &[&crate::utils::ValidatorKeys],
) {
    let num_validators = validator_keys.len();

    // Build stash list for validators (concatenated hex)
    let stash_list: String = validator_keys
        .iter()
        .map(|v| v.stash)
        .collect::<Vec<_>>()
        .join("");

    // Build QueuedKeys (stash + session_keys for each validator)
    let queued_keys: String = validator_keys
        .iter()
        .map(|v| v.session_keys_queuedkeys_format())
        .collect::<Vec<_>>()
        .join("");

    // Build Babe Authorities (babe_key + weight for each)
    let babe_authorities: String = validator_keys
        .iter()
        .map(|v| format!("{}0100000000000000", v.babe))
        .collect::<Vec<_>>()
        .join("");

    // Build Grandpa Authorities (grandpa_key + weight for each)
    let grandpa_authorities: String = validator_keys
        .iter()
        .map(|v| format!("{}0100000000000000", v.grandpa))
        .collect::<Vec<_>>()
        .join("");

    // Build authority discovery keys
    let authority_discovery_keys: String = validator_keys
        .iter()
        .map(|v| v.authority_discovery)
        .collect::<Vec<_>>()
        .join("");

    // Build validator indices for parachain shared
    let validator_indices: String = (0..num_validators)
        .map(|i| format!("{:02x}000000", i))
        .collect::<Vec<_>>()
        .join("");

    // ValidatorGroups is Vec<Vec<ValidatorIndex>>, so each single-validator group
    // carries its own compact length prefix.
    let validator_groups: Vec<Vec<u32>> = (0..num_validators as u32).map(|i| vec![i]).collect();
    let validator_groups = array_bytes::bytes2hex("", validator_groups.encode());

    // Build para validator keys (same as authority discovery for our purposes)
    let para_validator_keys: String = validator_keys
        .iter()
        .map(|v| v.para_validator)
        .collect::<Vec<_>>()
        .join("");

    // Format validator count as compact encoded
    let validator_count_hex = format!("{:02x}", num_validators * 4); // *4 because we encode each as 4 bytes

    let stashes = format!("{validator_count_hex}{stash_list}");
    // Only present on chains using the validator-set pallet.
    set.set("ValidatorSet", "Validators", &stashes);
    set.set("Session", "Validators", &stashes);
    set.set("Staking", "Invulnerables", &stashes);
    set.set(
        "Session",
        "QueuedKeys",
        format!("{validator_count_hex}{queued_keys}"),
    );
    set.set(
        "Babe",
        "Authorities",
        format!("{validator_count_hex}{babe_authorities}"),
    );
    set.set(
        "Babe",
        "NextAuthorities",
        format!("{validator_count_hex}{babe_authorities}"),
    );
    set.set(
        "Grandpa",
        "Authorities",
        format!("{validator_count_hex}{grandpa_authorities}"),
    );
    set.set("ParaScheduler", "ValidatorGroups", &validator_groups);
    set.set(
        "ParasShared",
        "ActiveValidatorIndices",
        format!("{validator_count_hex}{validator_indices}"),
    );
    set.set(
        "ParasShared",
        "ActiveValidatorKeys",
        format!("{validator_count_hex}{para_validator_keys}"),
    );
    set.set(
        "AuthorityDiscovery",
        "Keys",
        format!("{validator_count_hex}{authority_discovery_keys}"),
    );
    set.set(
        "AuthorityDiscovery",
        "NextKeys",
        format!("{validator_count_hex}{authority_discovery_keys}"),
    );
    set.set(
        "Sudo",
        "Key",
        "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    );
}

fn augment_overrides_for_paras(
    set: &mut OverrideSet<'_>,
    relay: &Relaychain,
    paras: &[&Parachain],
    cores_override: &CoresOverride,
    keep_messaging_state: bool,
) {
    // Generate paras_parachains
    let para_ids: Vec<u32> = paras.iter().map(|para| para.id()).collect();
    set.set(
        "Paras",
        "Parachains",
        generate_paras_parachains_value(para_ids),
    );

    // used to assign cores
    let mut core_index = 0_u32;
    let mut para_scheduler_value_parts: Vec<String> = vec![];

    for para in paras.iter() {
        let para_id = ParaId(para.id());

        let para_twox64 = array_bytes::bytes2hex("", subhasher::twox64(para_id.encode()));
        let para_hex = array_bytes::bytes2hex("", para_id.encode());
        let para_key_part = format!("{para_twox64}{para_hex}");

        if keep_messaging_state {
            // The relay and parachain snapshots agree on channel heads (a
            // self-owned relay), so inherited HRMP/DMP channels stay usable.
        } else {
            set.set_map(
                "Dmp",
                "DownwardMessageQueueHeads",
                &para_key_part,
                "0000000000000000000000000000000000000000000000000000000000000000",
            );
            set.set_map("Hrmp", "HrmpIngressChannelsIndex", &para_key_part, "00");
        }

        // ParaScheduler
        let para_cores = get_assigned_cores(relay, para, cores_override);
        for _ in 0..para_cores {
            para_scheduler_value_parts.push(core_assignment::generate(core_index, para.id()));
            core_index += 1;
        }
    }

    let count_prefix = format!("{:02x}", para_scheduler_value_parts.len() * 4);
    let core_assign_value = format!("{count_prefix}{}", para_scheduler_value_parts.join(""));
    // key is generated with prefix (`0x`), and the item is not in metadata on
    // every runtime, so it goes in raw.
    let scheduler_key = core_assignment::get_parascheduler_storage_key();
    set.set_raw(&scheduler_key[2..], core_assign_value);
}

#[allow(clippy::too_many_arguments)]
pub async fn generate_default_overrides_for_rc(
    base_dir: &str,
    relay: &Relaychain,
    paras: &Vec<Parachain>,
    req_cores: u32,
    maybe_upgrade: Option<&str>,
    meta: Option<&ChainMetadata>,
    cores_override: &CoresOverride,
    keep_messaging_state: bool,
) -> Result<PathBuf, anyhow::Error> {
    let num_validators = crate::config::num_validators_for_cores(req_cores);
    let validator_keys = get_validator_keys(num_validators as usize);

    let mut set = OverrideSet::new(meta.map(|m| m as &dyn RuntimeCheck));

    generate_rc_overrides(&mut set, &validator_keys);

    // add the paras related keys to override
    let paras_refs: Vec<&Parachain> = paras.iter().collect();
    augment_overrides_for_paras(
        &mut set,
        relay,
        &paras_refs,
        cores_override,
        keep_messaging_state,
    );

    set.set(
        "Configuration",
        "ActiveConfig",
        host_config(relay, req_cores, meta).await?,
    );

    generate_next_keys_injects(&mut set, &validator_keys);

    // set `UsePreviousValidators` to true to keep using the same validator set.
    set.inject_raw("c57d82d01f0fc18afc048ca20ac460dd", "01");

    // RcMigrator Manager (set //Alice by default)
    set.inject(
        "RcMigrator",
        "Manager",
        "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    );

    // update the overrides / injects map to use IFF the key is provided
    if let Ok(sudo_key) = env::var("ZOMBIE_SUDO") {
        set.set_required("Sudo", "Key", &sudo_key);
        set.inject("RcMigrator", "Manager", &sudo_key);
    }

    if let Some(override_wasm) = relay.wasm_overrides() {
        let wasm_content = fs::read(override_wasm)
            .await
            .unwrap_or_else(|_| panic!("Error reading override_wasm from path {}", override_wasm));
        set.set_raw("3a636f6465", hex::encode(wasm_content));
    }

    if let Some(upgrade_wasm) = maybe_upgrade {
        inject_authorized_upgrade(&mut set, upgrade_wasm).await;
    }

    // also check if any parachain includes a wasm override but we can't doit in the
    // augmented logic since we need to _inject_ keys
    for para in paras {
        if let Some(override_wasm) = para.wasm_overrides() {
            let wasm_content = fs::read(override_wasm).await.unwrap_or_else(|_| {
                panic!("Error reading override_wasm from path {}", override_wasm)
            });
            let code_hash = hex::encode(subhasher::blake2_256(&wasm_content[..]));
            let para_id_map_key = crate::utils::para_id_for_map_hash(para.id());

            set.set_map_required("Paras", "CurrentCodeHash", &para_id_map_key, &code_hash);

            // CodeByHash / CodeByHashRefs are injected since the map key is the
            // hash of the code itself, so they are never in the imported state.
            let validation_code: ValidationCode = ValidationCode(wasm_content);
            set.inject_map_unverified(
                "Paras",
                "CodeByHash",
                &code_hash,
                hex::encode(validation_code.encode()),
            );
            set.inject_map_required("Paras", "CodeByHashRefs", &code_hash, "01000000");
        }
    }

    let (overrides, injects) = set.finish(&relay.as_chain_string())?;
    let full_content = json!({
        "overrides": overrides,
        "injects": injects
    });

    let file_path = PathBuf::from(format!("{base_dir}/rc_overrides.json"));
    let contents = serde_json::to_string_pretty(&full_content).expect("Overrides should be valid.");
    fs::write(&file_path, contents)
        .await
        .expect("write file should works.");
    Ok(file_path)
}

pub async fn generate_default_overrides_for_para(
    base_dir: &str,
    para: &Parachain,
    relay: &Relaychain,
    maybe_upgrade: Option<&str>,
    meta: Option<&ChainMetadata>,
    keep_messaging_state: bool,
) -> Result<PathBuf, anyhow::Error> {
    // For AH determine key type based on relay chain: ed25519 for Polkadot, sr25519 for others
    let key_type = match (relay, para) {
        (Relaychain::Polkadot { .. }, Parachain::AssetHub { .. }) => "ed",
        _ => "sr",
    };

    // Generate collator key using "Collator-{para_id}" as seed
    let seed = format!("Collator-{}", para.id());
    let key_to_use = generate_collator_key_from_seed(&seed, key_type);

    let mut set = OverrideSet::new(meta.map(|m| m as &dyn RuntimeCheck));

    set.set("Session", "Validators", format!("04{key_to_use}"));
    set.set(
        "Session",
        "QueuedKeys",
        format!("04{key_to_use}{key_to_use}"),
    );
    set.set(
        "CollatorSelection",
        "Invulnerables",
        format!("04{key_to_use}"),
    );
    set.set("Aura", "Authorities", format!("04{key_to_use}"));
    set.set("AuraExt", "Authorities", format!("04{key_to_use}"));
    set.set("CollatorSelection", "DesiredCandidates", "01000000");
    set.set(
        "Sudo",
        "Key",
        "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    );

    if !keep_messaging_state {
        set.set(
            "ParachainSystem",
            "LastDmqMqcHead",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    // Session.NextKeys for the collator
    for (key, value) in generate_collator_next_keys_injects(&key_to_use)
        .as_object()
        .expect("collator injects should be a map")
    {
        set.inject_raw(
            key,
            value.as_str().expect("collator inject should be a string"),
        );
    }

    if let Some(override_wasm) = para.wasm_overrides() {
        let wasm_content = fs::read(override_wasm)
            .await
            .unwrap_or_else(|_| panic!("Error reading override_wasm from path {}", override_wasm));
        set.set_raw("3a636f6465", hex::encode(wasm_content));
    }

    if let Some(upgrade_wasm) = maybe_upgrade {
        inject_authorized_upgrade(&mut set, upgrade_wasm).await;
    }

    let (overrides, injects) = set.finish(&format!("para {}", para.id()))?;
    let full_content = json!({
        "overrides": overrides,
        "injects": injects
    });

    let file_path = PathBuf::from(format!("{base_dir}/{}_overrides.json", para.id()));
    let contents = serde_json::to_string_pretty(&full_content).expect("Overrides should be valid.");
    fs::write(&file_path, contents)
        .await
        .expect("write file should works.");
    Ok(file_path)
}

fn generate_paras_parachains_value(ids: impl Into<Vec<u32>>) -> String {
    let para_ids = ids.into();
    let para_ids: Vec<ParaId> = para_ids.iter().map(|id| ParaId(*id)).collect();

    array_bytes::bytes2hex("", para_ids.encode())
}

#[cfg(test)]
mod test {
    use codec::Encode;
    use serde_json::json;

    use crate::utils::{get_validator_keys, ParaId};

    use super::*;

    #[test]
    fn generate_paras_parachains_value_works() {
        let value = generate_paras_parachains_value([1000_u32]);
        println!("{value}");
        assert_eq!(value, "04e8030000");
    }

    #[tokio::test]
    async fn overrides_rc() {
        let paras = vec![];
        let _path = generate_default_overrides_for_rc(
            "/tmp",
            &crate::config::Relaychain::new("polkadot"),
            &paras,
            2,
            None,
            None,
            &CoresOverride::new(),
            false,
        )
        .await
        .unwrap();
    }

    #[test]
    fn test_generate_next_keys_injects() {
        let validator_keys = get_validator_keys(2);
        let mut set = OverrideSet::new(None);
        generate_next_keys_injects(&mut set, &validator_keys);

        let expected = json!({
            // Session NextKeys (alice)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb3e535263148daaf49be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25f": "88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0eed43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d020a1091341fe5664bfa1782d5e04779689068c916b04cb365ec3153755684d9a1",
            // Session NextKeys (bob)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb30e5be00fbc2e15b5fe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e": "d17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae698eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a480390084fdbf27d2b79d26a4f13f0ccd982cb755a661969143c37cbc49ef5b91f27",
        });

        assert_eq!(set.injects, expected);
    }

    #[test]
    fn test_generate_validator_overrides() {
        // Just 2 is alice and bob
        let validator_keys = get_validator_keys(2);
        let para = crate::config::Parachain::new("asset-hub");
        let paras = vec![&para];
        let rc = Relaychain::new("polkadot");

        let mut set = OverrideSet::new(None);
        generate_rc_overrides(&mut set, &validator_keys);
        augment_overrides_for_paras(&mut set, &rc, &paras, &CoresOverride::new(), false);
        let overrides = set.overrides;

        // ValidatorSet Validators
        assert_eq!(
            overrides["7d9fe37370ac390779f35763d98106e888dcde934c658227ee1dfafcd6e16903"],
            "08be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25ffe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e"
        );

        // Session Validators
        assert_eq!(
            overrides["cec5070d609dd3497f72bde07fc96ba088dcde934c658227ee1dfafcd6e16903"],
            "08be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25ffe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e"
        );

        // Session QueuedKeys
        assert_eq!(
            overrides["cec5070d609dd3497f72bde07fc96ba0e0cdd062e6eaf24295ad4ccfc41d4609"],
            "08be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25f88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0eed43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d020a1091341fe5664bfa1782d5e04779689068c916b04cb365ec3153755684d9a1fe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860ed17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae698eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a480390084fdbf27d2b79d26a4f13f0ccd982cb755a661969143c37cbc49ef5b91f27"
        );

        // Babe Authorities
        assert_eq!(
            overrides["1cb6f36e027abb2091cfb5110ab5087f5e0621c4869aa60c02be9adcc98a0d1d"],
            "08d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d01000000000000008eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a480100000000000000"
        );

        // Grandpa Authorities
        assert_eq!(
            overrides["5f9cc45b7a00c5899361e1c6099678dc5e0621c4869aa60c02be9adcc98a0d1d"],
            "0888dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0ee0100000000000000d17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae690100000000000000"
        );

        // Staking Invulnerables
        assert_eq!(
            overrides["5f3e4907f716ac89b6347d15ececedca5579297f4dfb9609e7e4c2ebab9ce40a"],
            "08be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25ffe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e"
        );

        // ParaScheduler ValidatorGroups: Vec<Vec<ValidatorIndex>>, two groups of one
        let expected_groups: Vec<Vec<u32>> = vec![vec![0], vec![1]];
        assert_eq!(
            overrides["94eadf0156a8ad5156507773d0471e4a16973e1142f5bd30d9464076794007db"],
            array_bytes::bytes2hex("", expected_groups.encode())
        );
        // pin the wire bytes too: compact(2) then each group with its own compact len
        assert_eq!(
            overrides["94eadf0156a8ad5156507773d0471e4a16973e1142f5bd30d9464076794007db"],
            "0804000000000401000000"
        );

        // Para Id Parachains
        assert_eq!(
            overrides["cd710b30bd2eab0352ddcc26417aa1940b76934f4cc08dee01012d059e1b83ee"],
            "04e8030000"
        );

        // Authority Discovery Keys
        assert_eq!(
            overrides["2099d7f109d6e535fb000bba623fd4409f99a2ce711f3a31b2fc05604c93f179"],
            "08d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48"
        );

        // Sudo Key (Alice)
        assert_eq!(
            overrides["5c0d1176a568c1f92944340dbfed9e9c530ebca703c85910e7164cb7d1c9e47b"],
            "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
        );

        // Dmp / Hrmp cleared for the para
        let para_id = ParaId(1000);
        let para_key = format!(
            "{}{}",
            array_bytes::bytes2hex("", subhasher::twox64(para_id.encode())),
            array_bytes::bytes2hex("", para_id.encode())
        );
        assert_eq!(
            overrides[format!(
                "{}{para_key}",
                storage_key("Hrmp", "HrmpIngressChannelsIndex")
            )],
            "00"
        );
    }

    #[test]
    fn keep_messaging_state_leaves_channels_alone() {
        let validator_keys = get_validator_keys(2);
        let para = crate::config::Parachain::new("asset-hub");
        let paras = vec![&para];
        let rc = Relaychain::new("polkadot");

        let mut set = OverrideSet::new(None);
        generate_rc_overrides(&mut set, &validator_keys);
        augment_overrides_for_paras(&mut set, &rc, &paras, &CoresOverride::new(), true);

        let keys: Vec<&String> = set
            .overrides
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| {
                k.starts_with(&storage_key("Hrmp", "HrmpIngressChannelsIndex"))
                    || k.starts_with(&storage_key("Dmp", "DownwardMessageQueueHeads"))
            })
            .collect();
        assert!(
            keys.is_empty(),
            "should not touch messaging state: {keys:?}"
        );
    }

    #[test]
    fn cores_override_changes_assignment() {
        let para = crate::config::Parachain::new("asset-hub");
        let rc = Relaychain::new("polkadot");
        let mut cores = CoresOverride::new();
        cores.insert(1000, 1);

        let scheduler_key = core_assignment::get_parascheduler_storage_key();
        let assignment = |cores: &CoresOverride| {
            let mut set = OverrideSet::new(None);
            augment_overrides_for_paras(&mut set, &rc, &[&para], cores, false);
            set.overrides[&scheduler_key[2..]]
                .as_str()
                .unwrap()
                .to_string()
        };

        // asset-hub defaults to three cores, the override brings it down to one
        let default = assignment(&CoresOverride::new());
        let overridden = assignment(&cores);
        assert!(default.starts_with("0c"), "expected 3 cores, got {default}");
        assert!(
            overridden.starts_with("04"),
            "expected 1 core, got {overridden}"
        );
    }

    #[tokio::test]
    async fn inject_authorized_upgrade_seeds_hash_and_check_version() {
        let wasm_path = "/tmp/zombie-bite-test-upgrade.wasm";
        let wasm = b"not-a-real-runtime";
        tokio::fs::write(wasm_path, wasm).await.unwrap();

        let mut set = OverrideSet::new(None);
        inject_authorized_upgrade(&mut set, wasm_path).await;

        let expected = format!("{}01", hex::encode(subhasher::blake2_256(&wasm[..])));
        assert_eq!(
            set.injects[storage_key("System", "AuthorizedUpgrade")],
            json!(expected)
        );
    }

    /// Runtime check stub: `present` lists the items the runtime has, and any
    /// value equal to `bad_value` fails verification.
    struct FakeRuntime {
        present: Vec<(&'static str, &'static str)>,
        bad_value: &'static str,
    }

    impl RuntimeCheck for FakeRuntime {
        fn has_item(&self, pallet: &str, item: &str) -> bool {
            self.present.iter().any(|(p, i)| *p == pallet && *i == item)
        }

        fn verify_value(
            &self,
            pallet: &str,
            item: &str,
            value_hex: &str,
        ) -> Result<(), anyhow::Error> {
            if value_hex == self.bad_value {
                anyhow::bail!("{pallet}::{item}: bad value");
            }
            Ok(())
        }
    }

    #[test]
    fn missing_item_is_skipped_but_required_one_is_an_error() {
        let runtime = FakeRuntime {
            present: vec![("Session", "Validators")],
            bad_value: "",
        };

        let mut set = OverrideSet::new(Some(&runtime));
        set.set("Session", "Validators", "04ff");
        // absent from the runtime: dropped quietly
        set.set("ValidatorSet", "Validators", "04ff");
        // absent but explicitly requested: must fail the bite
        set.inject_required("System", "AuthorizedUpgrade", "04ff");

        assert_eq!(set.overrides[storage_key("Session", "Validators")], "04ff");
        assert_eq!(set.skipped, vec!["ValidatorSet::Validators"]);
        let err = set.finish("test").unwrap_err().to_string();
        assert!(err.contains("System::AuthorizedUpgrade"), "got: {err}");
    }

    #[test]
    fn value_that_fails_verification_fails_the_bite() {
        let runtime = FakeRuntime {
            present: vec![("Session", "Validators")],
            bad_value: "deadbeef",
        };

        let mut set = OverrideSet::new(Some(&runtime));
        set.set("Session", "Validators", "deadbeef");

        assert!(set.overrides.as_object().unwrap().is_empty());
        let err = set.finish("test").unwrap_err().to_string();
        assert!(err.contains("bad value"), "got: {err}");
    }

    #[test]
    fn unverified_map_inject_still_requires_the_item() {
        let runtime = FakeRuntime {
            present: vec![],
            bad_value: "",
        };

        let mut set = OverrideSet::new(Some(&runtime));
        // a huge wasm blob is not verified, but the item must exist
        set.inject_map_unverified("Paras", "CodeByHash", "aa", "00ff");

        let err = set.finish("test").unwrap_err().to_string();
        assert!(err.contains("Paras::CodeByHash"), "got: {err}");
    }
}
