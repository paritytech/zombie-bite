use codec::Encode;
use serde_json::{json, Value};
use std::{env, path::PathBuf};
use tokio::fs;

use crate::{
    config::{Parachain, Relaychain},
    utils::{
        generate_collator_key_from_seed, generate_collator_next_keys_injects, get_validator_keys,
        ValidationCode,
    },
};

/// Generate the injects for Session.NextKeys storage overrides for validators
pub fn generate_next_keys_injects(
    validator_keys: &[&crate::utils::ValidatorKeys],
) -> serde_json::Value {
    let mut next_keys_injects = serde_json::json!({});
    for keys in validator_keys {
        let stash_bytes = hex::decode(keys.stash).expect("stash should be valid hex");
        let stash_hash = array_bytes::bytes2hex("", &subhasher::twox64_concat(&stash_bytes)[..8]);
        let inject_key = format!(
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb3{}{}",
            stash_hash, keys.stash
        );
        next_keys_injects[inject_key] = serde_json::json!(keys.session_keys_encoded());
    }
    next_keys_injects
}

/// Generate the storage overrides for relay chain validators
pub fn generate_validator_overrides(
    validator_keys: &[&crate::utils::ValidatorKeys],
    paras: &[&Parachain],
) -> serde_json::Value {
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

    let validator_groups_count_hex = format!("{:02x}", num_validators * 4); // Each group entry is 4 bytes
    let validator_groups: String = (0..num_validators)
        .map(|i| format!("{:02x}000000", i))
        .collect::<Vec<_>>()
        .join("");

    // Build para validator keys (same as authority discovery for our purposes)
    let para_validator_keys: String = validator_keys
        .iter()
        .map(|v| v.para_validator)
        .collect::<Vec<_>>()
        .join("");

    // Format validator count as compact encoded
    let validator_count_hex = format!("{:02x}", num_validators * 4); // *4 because we encode each as 4 bytes

    let first_para_id = paras.first().map(|p| p.id()).unwrap_or(1000);
    let para_id_bytes = first_para_id.to_le_bytes();
    let para_id_hex = array_bytes::bytes2hex("", para_id_bytes);

    // Build core descriptor for the first parachain
    let core_descriptor = format!("00010402{}00e100e100010000e1", para_id_hex);

    // Build paras parachains list (just the first para)
    let paras_parachains = format!("04{}", para_id_hex);

    // Build base overrides object
    let mut overrides = json!({
        // Validator Validators (dynamic list)
        "7d9fe37370ac390779f35763d98106e888dcde934c658227ee1dfafcd6e16903": format!("{}{}", validator_count_hex, stash_list),
        // Session Validators (dynamic list)
        "cec5070d609dd3497f72bde07fc96ba088dcde934c658227ee1dfafcd6e16903": format!("{}{}", validator_count_hex, stash_list),
        //  Session QueuedKeys (dynamic list)
        "cec5070d609dd3497f72bde07fc96ba0e0cdd062e6eaf24295ad4ccfc41d4609": format!("{}{}", validator_count_hex, queued_keys),
        // Babe Authorities (dynamic list)
        "1cb6f36e027abb2091cfb5110ab5087f5e0621c4869aa60c02be9adcc98a0d1d": format!("{}{}", validator_count_hex, babe_authorities),
        // Babe NextAuthorities (dynamic list)
        "1cb6f36e027abb2091cfb5110ab5087faacf00b9b41fda7a9268821c2a2b3e4c": format!("{}{}", validator_count_hex, babe_authorities),
        // Grandpa Authorities (dynamic list)
        "5f9cc45b7a00c5899361e1c6099678dc5e0621c4869aa60c02be9adcc98a0d1d": format!("{}{}", validator_count_hex, grandpa_authorities),
        // Staking Invulnerables (dynamic list)
        "5f3e4907f716ac89b6347d15ececedca5579297f4dfb9609e7e4c2ebab9ce40a": format!("{}{}", validator_count_hex, stash_list),
        // paras parachains (dynamic based on first parachain)
        "cd710b30bd2eab0352ddcc26417aa1940b76934f4cc08dee01012d059e1b83ee": paras_parachains,
        // paraScheduler validatorGroup (dynamic groups based on validator count)
        "94eadf0156a8ad5156507773d0471e4a16973e1142f5bd30d9464076794007db": format!("{}{}", validator_groups_count_hex, validator_groups),
        // paraScheduler claimQueue (empty, will auto-fill)
        "94eadf0156a8ad5156507773d0471e4a49f6c9aa90c04982c05388649310f22f": "040000000000",
        // paraShared activeValidatorIndices (dynamic)
        "b341e3a63e58a188839b242d17f8c9f82586833f834350b4d435d5fd269ecc8b": format!("{}{}", validator_count_hex, validator_indices),
        // paraShared activeValidatorKeys (dynamic)
        "b341e3a63e58a188839b242d17f8c9f87a50c904b368210021127f9238883a6e": format!("{}{}", validator_count_hex, para_validator_keys),
        // authorityDiscovery keys (dynamic)
        "2099d7f109d6e535fb000bba623fd4409f99a2ce711f3a31b2fc05604c93f179": format!("{}{}", validator_count_hex, authority_discovery_keys),
        // authorityDiscovery nextKeys (dynamic)
        "2099d7f109d6e535fb000bba623fd4404c014e6bf8b8c2c011e7290b85696bb3": format!("{}{}", validator_count_hex, authority_discovery_keys),
        // Core descriptor, ensure core 0 is assigned to first parachain (dynamic)
        "638595eebaa445ce03a13547bece90e704e6ac775a3245623103ffec2cb2c92fb4def25cfda6ef3ac02a707a7013b12ddc9c5f6a3e1994c51754be175bd6a3d4": core_descriptor,
        // Configuration activeConfig
        "06de3d8a54d27e44a9d5ce189618f22db4b49d95320d9021994c850f25b8e385": "0000300000500000aaaa020000001000fbff0000100000000a000000403800005802000003000000020000000000500000c800008000000000e8764817000000000000000000000000e87648170000000000000000000000e80300000090010080000000009001000c01002000000600c4090000000000000601983a00000000000040380000000600000058020000030000001900000000000000020000000200000002000000140000000100000008030100000014000000040000000105000000010000000100000000000000f401000080b2e60e80c3c90180b2e60e00000000000000000000000005000000",
        // paraScheduler availabilityCores (1 core, free)
        "94eadf0156a8ad5156507773d0471e4ab8ebad86f546c7e0b135a4212aace339": "0400",
        // Sudo Key (Alice)
        "5c0d1176a568c1f92944340dbfed9e9c530ebca703c85910e7164cb7d1c9e47b": "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    });

    // Add DMP and HRMP storage keys for each parachain
    for para in paras {
        let para_id_bytes = para.id().to_le_bytes();
        let para_id_hex = array_bytes::bytes2hex("", para_id_bytes);

        // DMP downwardMessageQueueHeads (empty for each para)
        let dmp_queue_key = format!(
            "63f78c98723ddc9073523ef3beefda0c4d7fefc408aac59dbfe80a72ac8e3ce5b6ff6f7d467b87a9{}",
            para_id_hex
        );
        overrides[dmp_queue_key] =
            json!("0000000000000000000000000000000000000000000000000000000000000000");

        // HRMP hrmpIngressChannelsIndex (empty for each para)
        let hrmp_channels_key = format!(
            "6a0da05ca59913bc38a8630590f2627c1d3719f5b0b12c7105c073c507445948b6ff6f7d467b87a9{}",
            para_id_hex
        );
        overrides[hrmp_channels_key] = json!("00");
    }

    overrides
}

pub async fn generate_default_overrides_for_rc(
    base_dir: &str,
    relay: &Relaychain,
    paras: &Vec<Parachain>,
) -> PathBuf {
    // Alice + Bob + number of parachains
    let num_validators = (2 + paras.len()).min(7);
    let validator_keys = get_validator_keys(num_validators);

    let next_keys_injects = generate_next_keys_injects(&validator_keys);

    // Generate the validator overrides with parachains
    let paras_refs: Vec<&Parachain> = paras.iter().collect();
    let mut overrides = generate_validator_overrides(&validator_keys, &paras_refs);

    // Keys to inject (mostly storage maps that are not present in the current state)
    // <Pallet> < Item>
    let mut injects = next_keys_injects;

    // RcMigrator Manager (set //Alice by default)
    injects["2185d18cb42ae97242af0e70e6ad689012fcd13ee43ae32cc87f798eb5ed3295"] =
        json!("d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d");

    // update the overrides / injects map to use IFF the key is provided
    if let Ok(sudo_key) = env::var("ZOMBIE_SUDO") {
        // Sudo Key
        overrides["5c0d1176a568c1f92944340dbfed9e9c530ebca703c85910e7164cb7d1c9e47b"] =
            Value::String(sudo_key.clone());

        // RcMigrator Manager
        injects["2185d18cb42ae97242af0e70e6ad689012fcd13ee43ae32cc87f798eb5ed3295"] =
            Value::String(sudo_key);
    }

    if let Some(override_wasm) = relay.wasm_overrides() {
        let wasm_content = fs::read(override_wasm)
            .await
            .unwrap_or_else(|_| panic!("Error reading override_wasm from path {}", override_wasm));
        overrides["3a636f6465"] = Value::String(hex::encode(wasm_content));
    }

    // also check if any parachain includes a wasm override
    for para in paras {
        if let Some(override_wasm) = para.wasm_overrides() {
            let wasm_content = fs::read(override_wasm).await.unwrap_or_else(|_| {
                panic!("Error reading override_wasm from path {}", override_wasm)
            });
            let code_hash = hex::encode(subhasher::blake2_256(&wasm_content[..]));

            // we should now override
            let para_id_hash = crate::utils::para_id_hash(para.id());
            // Paras.CurrentCodeHash(paraId)
            let current_code_hash_prefix = array_bytes::bytes2hex(
                "",
                substorager::storage_value_key(&b"Paras"[..], b"CurrentCodeHash"),
            );
            overrides[&format!("{current_code_hash_prefix}{para_id_hash}")] =
                Value::String(code_hash.clone());

            // Paras.CodeByHash (should be injected since is have a reference to hash of the code itself)
            let code_by_hash_prefix = array_bytes::bytes2hex(
                "",
                substorager::storage_value_key(&b"Paras"[..], b"CodeByHash"),
            );
            let validation_code: ValidationCode = ValidationCode(wasm_content);
            let validation_code_encoded = validation_code.encode();
            injects[&format!("{code_by_hash_prefix}{code_hash}")] =
                Value::String(hex::encode(validation_code_encoded));

            // Paras.CodeByHashRefs (should be injected since is have a reference to hash of the code itself)
            let code_by_hash_prefix = array_bytes::bytes2hex(
                "",
                substorager::storage_value_key(&b"Paras"[..], b"CodeByHashRefs"),
            );
            // hardcoded to 1 encoded
            injects[&format!("{code_by_hash_prefix}{code_hash}")] =
                Value::String("01000000".into());
        }
    }

    let full_content = json!({
        "overrides": overrides,
        "injects": injects
    });

    let file_path = PathBuf::from(format!("{base_dir}/rc_overrides.json"));
    let contents = serde_json::to_string_pretty(&full_content).expect("Overrides should be valid.");
    fs::write(&file_path, contents)
        .await
        .expect("write file should works.");
    file_path
}

pub async fn generate_default_overrides_for_para(
    base_dir: &str,
    para: &Parachain,
    relay: &Relaychain,
) -> PathBuf {
    // Determine key type based on relay chain: ed25519 for Polkadot, sr25519 for others
    let key_type = match relay {
        Relaychain::Polkadot { .. } => "ed",
        _ => "sr", // Kusama, Paseo, etc.
    };

    // Generate collator key using "Collator-{para_id}" as seed
    let seed = format!("Collator-{}", para.id());
    let key_to_use = generate_collator_key_from_seed(&seed, key_type);

    // Generate the injects using the helper function
    let injects = generate_collator_next_keys_injects(&seed, &key_to_use);

    // <Pallet> <Item>
    // e.g Validator Validators
    let mut overrides = json!({
        // Session Validators
        "cec5070d609dd3497f72bde07fc96ba088dcde934c658227ee1dfafcd6e16903": &format!("04{key_to_use}"),
        //	Session QueuedKeys
        "cec5070d609dd3497f72bde07fc96ba0e0cdd062e6eaf24295ad4ccfc41d4609": &format!("04{key_to_use}{key_to_use}"),
        // CollatorSelection Invulnerables (collator)
        "15464cac3378d46f113cd5b7a4d71c845579297f4dfb9609e7e4c2ebab9ce40a": &format!("04{key_to_use}"),
        // Aura authorities
        "57f8dc2f5ab09467896f47300f0424385e0621c4869aa60c02be9adcc98a0d1d": &format!("04{key_to_use}"),
        // AuraExt authorities
        "3c311d57d4daf52904616cf69648081e5e0621c4869aa60c02be9adcc98a0d1d": &format!("04{key_to_use}"),
        // parachainSystem lastDmqMqcHead (emtpy)
        "45323df7cc47150b3930e2666b0aa313911a5dd3f1155f5b7d0c5aa102a757f9": "0000000000000000000000000000000000000000000000000000000000000000",
        // CollatorSelection DesiredCandidates (set to 1)
        "15464cac3378d46f113cd5b7a4d71c84476f594316a7dfe49c1f352d95abdaf1": "01000000"
    });

    if let Some(override_wasm) = para.wasm_overrides() {
        let wasm_content = fs::read(override_wasm)
            .await
            .unwrap_or_else(|_| panic!("Error reading override_wasm from path {}", override_wasm));
        overrides["3a636f6465"] = Value::String(hex::encode(wasm_content));
    }

    let full_content = json!({
        "overrides": overrides,
        "injects": injects
    });

    let file_path = PathBuf::from(format!("{base_dir}/{}_overrides.json", para.id()));
    let contents = serde_json::to_string_pretty(&full_content).expect("Overrides should be valid.");
    fs::write(&file_path, contents)
        .await
        .expect("write file should works.");
    file_path
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use crate::utils::get_validator_keys;

    use super::{
        generate_default_overrides_for_rc, generate_next_keys_injects, generate_validator_overrides,
    };

    #[tokio::test]
    async fn overrides_rc() {
        let paras = vec![];
        let _path = generate_default_overrides_for_rc(
            "/tmp",
            &crate::config::Relaychain::new("polakdot"),
            &paras,
        )
        .await;
    }

    #[test]
    fn test_generate_next_keys_injects() {
        let validator_keys = get_validator_keys(2);
        let next_keys_injects = generate_next_keys_injects(&validator_keys);

        let expected = json!({
            // Session NextKeys (alice)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb3e535263148daaf49be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25f": "88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0eed43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d020a1091341fe5664bfa1782d5e04779689068c916b04cb365ec3153755684d9a1",
            // Session NextKeys (bob)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb30e5be00fbc2e15b5fe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e": "d17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae698eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a480390084fdbf27d2b79d26a4f13f0ccd982cb755a661969143c37cbc49ef5b91f27",
        });

        assert_eq!(next_keys_injects, expected);
    }

    #[tokio::test]
    async fn alive_and_bob_inject_keys() {
        // Just 2 is alice and bob
        let validator_keys = get_validator_keys(2);
        let mut next_keys_injects = generate_next_keys_injects(&validator_keys);

        // RcMigrator Manager (set //Alice by default)
        next_keys_injects["2185d18cb42ae97242af0e70e6ad689012fcd13ee43ae32cc87f798eb5ed3295"] =
            json!("d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d");

        let expected = json!({
            // Session NextKeys (alice)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb3e535263148daaf49be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25f": "88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0eed43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27dd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d020a1091341fe5664bfa1782d5e04779689068c916b04cb365ec3153755684d9a1",
            // Session NextKeys (bob)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb30e5be00fbc2e15b5fe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e": "d17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae698eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a488eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a480390084fdbf27d2b79d26a4f13f0ccd982cb755a661969143c37cbc49ef5b91f27",
            // RcMigrator Manager (set //Alice by default) see: https://github.com/polkadot-fellows/runtimes/blob/22116f7d02c220db4f7187c6967dbd6bf89274cf/pallets/rc-migrator/src/lib.rs#L702-L707
            "2185d18cb42ae97242af0e70e6ad689012fcd13ee43ae32cc87f798eb5ed3295": "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
        });

        assert_eq!(next_keys_injects, expected);
    }

    #[test]
    fn test_generate_validator_overrides() {
        // Just 2 is alice and bob
        let validator_keys = get_validator_keys(2);
        let para = crate::config::Parachain::new("asset-hub");
        let paras = vec![&para];
        let overrides = generate_validator_overrides(&validator_keys, &paras);

        // Validator Validators
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
    }
}
