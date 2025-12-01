#![allow(dead_code)]
// TODO: don't allow dead_code

use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::anyhow;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sp_core::bytes;
use tokio::fs::{self, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use codec::{CompactAs, Decode, Encode, MaxEncodedLen};
use tracing::trace;

pub struct ValidatorKeys {
    pub name: &'static str,
    pub stash: &'static str,
    pub babe: &'static str,
    pub grandpa: &'static str,
    pub para_validator: &'static str,
    pub para_assignment: &'static str,
    pub authority_discovery: &'static str,
    pub beefy: &'static str,
}

impl ValidatorKeys {
    pub fn session_keys_encoded(&self) -> String {
        format!(
            "{}{}{}{}{}{}",
            self.grandpa,
            self.babe,
            self.para_validator,
            self.para_assignment,
            self.authority_discovery,
            self.beefy
        )
    }

    pub fn session_keys_queuedkeys_format(&self) -> String {
        format!("{}{}", self.stash, self.session_keys_encoded())
    }
}

pub const ALICE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "alice",
    stash: "be5ddb1579b72e84524fc29e78609e3caf42e85aa118ebfe0b0ad404b5bdd25f",
    babe: "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    grandpa: "88dc3417d5058ec4b4503e0c12ea1a0a89be200fe98922423d4334014fa6b0ee",
    para_validator: "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    para_assignment: "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    authority_discovery: "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d",
    beefy: "020a1091341fe5664bfa1782d5e04779689068c916b04cb365ec3153755684d9a1",
};

pub const BOB_KEYS: ValidatorKeys = ValidatorKeys {
    name: "bob",
    stash: "fe65717dad0447d715f660a0a58411de509b42e6efb8375f562f58a554d5860e",
    babe: "8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48",
    grandpa: "d17c2d7823ebf260fd138f2d7e27d114c0145d968b5ff5006125f2414fadae69",
    para_validator: "8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48",
    para_assignment: "8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48",
    authority_discovery: "8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48",
    beefy: "0390084fdbf27d2b79d26a4f13f0ccd982cb755a661969143c37cbc49ef5b91f27",
};

pub const CHARLIE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "charlie",
    stash: "1e07379407fecc4b89eb7dbd287c2c781cfb1907a96947a3eb18e4f8e7198625",
    babe: "90b5ab205c6974c9ea841be688864633dc9ca8a357843eeacf2314649965fe22",
    grandpa: "439660b36c6c03afafca027b910b4fecf99801834c62a5e6006f27d978de234f",
    para_validator: "90b5ab205c6974c9ea841be688864633dc9ca8a357843eeacf2314649965fe22",
    para_assignment: "90b5ab205c6974c9ea841be688864633dc9ca8a357843eeacf2314649965fe22",
    authority_discovery: "90b5ab205c6974c9ea841be688864633dc9ca8a357843eeacf2314649965fe22",
    beefy: "020e7446f3910e15fed2b2db1e71a01c57f3dd85cc2e65f30680220e09f8bbbc79",
};

pub const DAVE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "dave",
    stash: "e860f1b1c7227f7c22602f53f15af80747814dffd839719731ee3bba6edc126c",
    babe: "306721211d5404bd9da88e0204360a1a9ab8b87c66c1bc2fcdd37f3c2222cc20",
    grandpa: "5e639b43e0052c47447dac87d6fd2b6ec50bdd4d0f614e4299c665249bbd09d9",
    para_validator: "306721211d5404bd9da88e0204360a1a9ab8b87c66c1bc2fcdd37f3c2222cc20",
    para_assignment: "306721211d5404bd9da88e0204360a1a9ab8b87c66c1bc2fcdd37f3c2222cc20",
    authority_discovery: "306721211d5404bd9da88e0204360a1a9ab8b87c66c1bc2fcdd37f3c2222cc20",
    beefy: "0227e2b139697b04eb01f4eef7e8f3724431b795c45ce6ef7b8e23a4e93f4abd26",
};

pub const EVE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "eve",
    stash: "8ac59e11963af19174d0b94d5d78041c233f55d2e19324665bafdfb62925af2d",
    babe: "e659a7a1628cdd93febc04a4e0646ea20e9f5f0ce097d9a05290d4a9e054df4e",
    grandpa: "1dfe3e22cc0d45c70779c1095f7489a8ef3cf52d62fbd8c2fa38c9f1723502b5",
    para_validator: "e659a7a1628cdd93febc04a4e0646ea20e9f5f0ce097d9a05290d4a9e054df4e",
    para_assignment: "e659a7a1628cdd93febc04a4e0646ea20e9f5f0ce097d9a05290d4a9e054df4e",
    authority_discovery: "e659a7a1628cdd93febc04a4e0646ea20e9f5f0ce097d9a05290d4a9e054df4e",
    beefy: "031d10105e323c4afce225208f71a6441ee327a65b9e646e772500c74d31f669aa",
};

pub const FERDIE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "ferdie",
    stash: "101191192fc877c24d725b337120fa3edc63d227bbc92705db1e2cb65f56981a",
    babe: "1cbd2d43530a44705ad088af313e18f80b53ef16b36177cd4b77b846f2a5f07c",
    grandpa: "568cb4a574c6d178feb39c27dfc8b3f789e5f5423e19c71633c748b9acf086b5",
    para_validator: "1cbd2d43530a44705ad088af313e18f80b53ef16b36177cd4b77b846f2a5f07c",
    para_assignment: "1cbd2d43530a44705ad088af313e18f80b53ef16b36177cd4b77b846f2a5f07c",
    authority_discovery: "1cbd2d43530a44705ad088af313e18f80b53ef16b36177cd4b77b846f2a5f07c",
    beefy: "0291f1217d5a04cb83312ee3d88a6e6b33284e053e6ccfc3a90339a0299d12967c",
};

pub const ONE_KEYS: ValidatorKeys = ValidatorKeys {
    name: "one",
    stash: "ac859f8a216eeb1b320b4c76d118da3d7407fa523484d0a980126d3b4d0d220a",
    babe: "ac859f8a216eeb1b320b4c76d118da3d7407fa523484d0a980126d3b4d0d220a",
    grandpa: "16f97016bbea8f7b45ae6757b49efc1080accc175d8f018f9ba719b60b0815e4",
    para_validator: "ac859f8a216eeb1b320b4c76d118da3d7407fa523484d0a980126d3b4d0d220a",
    para_assignment: "ac859f8a216eeb1b320b4c76d118da3d7407fa523484d0a980126d3b4d0d220a",
    authority_discovery: "ac859f8a216eeb1b320b4c76d118da3d7407fa523484d0a980126d3b4d0d220a",
    beefy: "036c6ae73d36d0c02b54d7877a57b1734b8e096134bd2c1b829431aa38f18bcce1",
};

/// Get the validator keys for the specified number of validators
pub fn get_validator_keys(count: usize) -> Vec<&'static ValidatorKeys> {
    let all_keys = [
        &ALICE_KEYS,
        &BOB_KEYS,
        &CHARLIE_KEYS,
        &DAVE_KEYS,
        &FERDIE_KEYS,
        &EVE_KEYS,
        &ONE_KEYS,
    ];

    all_keys.into_iter().take(count).collect()
}

/// Generate collator keys from a seed string using specified algorithm
/// Returns the Aura key as a hex string
/// - "ed" for ed25519 (used by Polkadot)
/// - "sr" for sr25519 (used by Kusama, Paseo, etc.)
pub fn generate_collator_key_from_seed(seed: &str, key_type: &str) -> String {
    use sp_core::{ed25519, sr25519, Pair};

    match key_type {
        "ed" => {
            let pair = ed25519::Pair::from_string(&format!("//{}", seed), None)
                .expect("Failed to generate ed25519 key from seed");
            let public = pair.public();
            let bytes: &[u8] = public.as_ref();
            hex::encode(bytes)
        }
        "sr" => {
            let pair = sr25519::Pair::from_string(&format!("//{}", seed), None)
                .expect("Failed to generate sr25519 key from seed");
            let public = pair.public();
            let bytes: &[u8] = public.as_ref();
            hex::encode(bytes)
        }
        _ => panic!("Unsupported key type: {}. Use 'ed' or 'sr'", key_type),
    }
}

/// Generate Session.NextKeys injects for a collator
/// Returns a JSON object with storage keys mapped to the collator's session keys
/// The hash is calculated from the collator key bytes
pub fn generate_collator_next_keys_injects(_seed: &str, collator_key: &str) -> serde_json::Value {
    // Hash the collator key bytes for the storage key prefix
    let key_bytes = hex::decode(collator_key).expect("collator key should be valid hex");
    let stash_hash = array_bytes::bytes2hex("", &subhasher::twox64_concat(&key_bytes)[..8]);

    // Session.NextKeys storage key
    let session_next_keys = format!(
        "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb3{}{}",
        stash_hash, collator_key
    );

    // Session.KeyOwner storage key for Aura
    let session_key_owner = format!(
        "cec5070d609dd3497f72bde07fc96ba0726380404683fc89e8233450c8aa1950eab3d4a1675d3d746175726180{}",
        collator_key
    );

    serde_json::json!({
        session_next_keys: collator_key,
        session_key_owner: collator_key
    })
}

/// Parachain id.
///
/// This is an equivalent of the `polkadot_parachain_primitives::Id`, which is a compact-encoded
/// `u32`.
#[derive(
    Clone,
    CompactAs,
    Copy,
    Decode,
    Default,
    Encode,
    Eq,
    Hash,
    MaxEncodedLen,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub struct ParaId(pub u32);

impl From<u32> for ParaId {
    fn from(id: u32) -> Self {
        ParaId(id)
    }
}

#[derive(
    Clone,
    CompactAs,
    Copy,
    Decode,
    Default,
    Encode,
    Eq,
    Hash,
    MaxEncodedLen,
    Ord,
    PartialEq,
    PartialOrd,
    Debug,
)]
pub struct Bl(pub u32);

/// Parachain head data included in the chain.
#[derive(PartialEq, Eq, Clone, PartialOrd, Ord, Encode, Decode)]
pub struct HeadData(pub Vec<u8>);

/// Parachain validation code.
#[derive(PartialEq, Eq, Clone, Encode, Decode, Serialize, Deserialize)]
pub struct ValidationCode(#[serde(with = "bytes")] pub Vec<u8>);

pub async fn get_random_port() -> u16 {
    let listener = TcpListener::bind("0.0.0.0:0".to_string())
        .await
        .expect("Can't bind a random port");

    listener
        .local_addr()
        .expect("We should always get the local_addr from the listener, qed")
        .port()
}

/// Read the file's content into a [`Vec<u8>`].
async fn read_file_to_vec<P>(path: P) -> Result<Vec<u8>, anyhow::Error>
where
    P: AsRef<Path>,
{
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| anyhow!("Error opening file {}", path.as_ref().to_string_lossy()))?;
    let mut content = Vec::new();

    file.read_to_end(&mut content)
        .await
        .map_err(|_| anyhow!("Error reading file {}", path.as_ref().to_string_lossy()))?;

    Ok(content)
}

/// Read the file's content into a struct implemented [`DeserializeOwned`].
pub async fn read_file_to_struct<P, T>(path: P) -> Result<T, anyhow::Error>
where
    P: AsRef<Path>,
    T: DeserializeOwned,
{
    let content = read_file_to_vec(&path).await?;

    let result = serde_json::from_slice(&content).map_err(|_| {
        anyhow!(
            "Error deserializing  file {}",
            path.as_ref().to_string_lossy()
        )
    })?;

    Ok(result)
}

/// Write the data to file at the given path.
///
/// If the file has already existed, then it will be overwritten.
/// Otherwise, this will create a file at the given path.
pub async fn write_data_to_file<P>(path: P, data: &[u8]) -> Result<(), anyhow::Error>
where
    P: AsRef<Path>,
{
    let mut file = File::create(&path)
        .await
        .map_err(|_| anyhow!("Error creating file {}", path.as_ref().to_string_lossy()))?;

    file.write_all(data)
        .await
        .map_err(|_| anyhow!("Error writting file {}", path.as_ref().to_string_lossy()))
}

pub fn para_head_key(para_id: u32) -> String {
    const PARAS_HEAD_PREFIX: &str =
        "0xcd710b30bd2eab0352ddcc26417aa1941b3c252fcb29d88eff4f3de5de4476c3";
    // let para_id: ParaId = para_id.into();
    let para_id_hash = para_id_hash(para_id);
    format!("{PARAS_HEAD_PREFIX}{para_id_hash}")
    // subhasher::twox64_concat(&para_id.encode());
    // let key = format!(
    //     "{PARAS_HEAD_PREFIX}{}",
    //     array_bytes::bytes2hex("", &para_id_hash)
    // );

    // key
}

/// Returns the hash of the ParaId (without the 0x prefix)
pub fn para_id_hash(para_id: u32) -> String {
    let para_id: ParaId = para_id.into();
    let para_id_hash = subhasher::twox64_concat(para_id.encode());
    array_bytes::bytes2hex("", &para_id_hash)
}

pub async fn localize_config(config_path: impl AsRef<str>) -> Result<(), anyhow::Error> {
    let config_path = PathBuf::from_str(config_path.as_ref())?;
    let base_path = config_path.parent().unwrap();

    let mut localized = false;

    // read config
    let config_content = fs::read_to_string(&config_path)
        .await
        .expect("read config should works");
    let mut config_modified = vec![];
    for line in config_content.lines() {
        match line {
            l if l.starts_with("default_db_snapshot")
                | l.starts_with("chain_spec_path")
                | l.starts_with("db_snapshot") =>
            {
                let parts: Vec<&str> = l.split("=").collect();
                let value_as_path = PathBuf::from_str(parts.last().unwrap())
                    .unwrap_or_else(|_| panic!("value {:?} should be a valid path", parts.last()));
                let maybe_mod_line = if let Ok(false) = fs::try_exists(&value_as_path).await {
                    // localize!
                    localized = true;
                    let mod_line = format!(
                        r#"{} = "{}/{}"#,
                        parts.first().unwrap().trim(),
                        base_path.to_string_lossy(),
                        value_as_path.file_name().unwrap().to_string_lossy()
                    );
                    trace!("localize line from: {l} to {mod_line}");
                    mod_line
                } else {
                    l.to_string()
                };

                config_modified.push(maybe_mod_line)
            }
            l if l.starts_with("base_dir") => {
                localized = true;
                // remove base_path
            }
            _ => {
                config_modified.push(line.to_string());
            }
        }
    }

    if localized {
        // rename original
        fs::rename(
            &config_path,
            &format!("{}/original-config.toml", &base_path.to_string_lossy()),
        )
        .await
        .expect("rename should works");
        fs::write(&config_path, config_modified.join("\n"))
            .await
            .expect("write should works");
    }

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
struct GetBlockHashRpcResponse {
    id: u32,
    result: String, // result contains only the hash
}

#[derive(Serialize, Deserialize, Debug)]
struct GetHeaderRpcResponse {
    id: u32,
    result: serde_json::Value, // result contains an Object with the header
}

pub async fn get_header_from_block(
    block_number: u32,
    endpoint: &str,
) -> Result<serde_json::Value, anyhow::Error> {
    let client = reqwest::ClientBuilder::new().build().unwrap();

    let res = client
        .post(endpoint)
        .json(
            &json!({"method":"chain_getBlockHash","params":[block_number],"id":1,"jsonrpc":"2.0"}),
        )
        .send()
        .await?;
    let hash = res.json::<GetBlockHashRpcResponse>().await?.result;
    trace!("block: {block_number} -> hash: {}", hash);

    let res = client
        .post(endpoint)
        .json(&json!({"method":"chain_getHeader","params":[hash],"id":1,"jsonrpc":"2.0"}))
        .send()
        .await?;
    let header = res.json::<GetHeaderRpcResponse>().await?.result;
    trace!("hash: {} -> header: {:?}", hash, header);

    Ok(header)
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn para_head_key_should_work() {
        let para_id = 1000_u32;
        let head_key = para_head_key(para_id);
        assert_eq!(&head_key, "0xcd710b30bd2eab0352ddcc26417aa1941b3c252fcb29d88eff4f3de5de4476c3b6ff6f7d467b87a9e8030000");
    }

    #[tokio::test]
    async fn localize_config_should_works() {
        let config_path = "./testing/config.toml";
        let config_path_bkp = "./testing/config.toml.bkp";
        let _ = fs::copy(&config_path_bkp, config_path).await;
        localize_config(config_path).await.unwrap();
    }

    #[test]
    fn encode_u32() {
        let one = 29798496_u32;
        let encoded = one.encode();
        println!("{}", array_bytes::bytes2hex("0x", encoded));
    }

    #[test]
    fn block() {
        let z = Bl(29798496).encode();
        println!("{}", array_bytes::bytes2hex("0x", &z));

        let d = Bl::decode(&mut z.as_slice());
        println!("{:?}", d);
    }

    #[tokio::test]
    async fn localize_paseo_config_should_works() {
        tracing_subscriber::fmt::init();
        let config_path = "./testing/config-paseo.toml";
        let config_path_bkp = "./testing/config-paseo.toml.bkp";
        let _ = fs::copy(&config_path_bkp, config_path).await;
        localize_config(config_path).await.unwrap();
        let network_config =
            zombienet_configuration::NetworkConfig::load_from_toml(config_path).unwrap();
        let _alice_db = network_config
            .relaychain()
            .nodes()
            .first()
            .unwrap()
            .db_snapshot()
            .unwrap()
            .to_string();
    }

    #[test]
    fn test_generate_collator_key_from_seed() {
        let key1 = generate_collator_key_from_seed("collator-1000", "sr");
        let key2 = generate_collator_key_from_seed("collator-1005", "sr");

        // Keys should be different
        assert_ne!(key1, key2);

        // Keys should be valid hex and 64 characters (32 bytes)
        assert_eq!(key1.len(), 64);
        assert_eq!(key2.len(), 64);

        // Same seed should generate same key
        let key1_again = generate_collator_key_from_seed("collator-1000", "sr");
        assert_eq!(key1, key1_again);

        // Test both key types with "Collator" seed
        let collator_key_sr = generate_collator_key_from_seed("Collator", "sr");
        let collator_key_ed = generate_collator_key_from_seed("Collator", "ed");

        // Expected keys for "Collator" seed (based on ZombieNet SDK implementation)
        assert_eq!(
            collator_key_sr,
            "005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15"
        );
        assert_eq!(
            collator_key_ed,
            "eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116"
        );

        // Keys should be different for different algorithms
        assert_ne!(collator_key_sr, collator_key_ed);

        println!("Collator key (sr25519/Kusama): {}", collator_key_sr);
        println!("Collator key (ed25519/Polkadot): {}", collator_key_ed);
    }

    #[tokio::test]
    async fn test_generate_collator_injects_for_polkadot() {
        // Test Polkadot (ed25519)
        let polkadot_key = generate_collator_key_from_seed("Collator", "ed");
        let injects = generate_collator_next_keys_injects("Collator", &polkadot_key);

        // Expected key for Polkadot with "Collator" seed (ed25519)
        assert_eq!(
            polkadot_key,
            "eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116"
        );

        // Verify injects structure
        assert!(injects.is_object());
        let obj = injects.as_object().unwrap();
        assert_eq!(obj.len(), 2);

        // Both storage entries should map to the collator key
        for (storage_key, value) in obj.iter() {
            assert!(storage_key.starts_with("cec5070d609dd3497f72bde07fc96ba0"));
            assert_eq!(value.as_str().unwrap(), polkadot_key);
        }

        let expected_injects = json!({
            // Session Nextkeys for `collator` (hash is from Polkadot ed25519 key bytes)
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb39d822490da82ebc3eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116": "eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116",
            // Session KeyOwner
            "cec5070d609dd3497f72bde07fc96ba0726380404683fc89e8233450c8aa1950eab3d4a1675d3d746175726180eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116": "eb2f4b5e6f0bfa7ba42aa4b7eb2f43ba6c42061dbfc765bca066e51bb09f9116",
        });

        assert_eq!(injects, expected_injects);
    }

    #[tokio::test]
    async fn test_generate_collator_injects_for_kusama() {
        // Test Kusama (sr25519)
        let kusama_key = generate_collator_key_from_seed("Collator", "sr");
        let injects = generate_collator_next_keys_injects("Collator", &kusama_key);

        // Expected key for Kusama with "Collator" seed
        assert_eq!(
            kusama_key,
            "005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15"
        );

        // Verify injects structure
        assert!(injects.is_object());
        let obj = injects.as_object().unwrap();
        assert_eq!(obj.len(), 2);

        // Both storage entries should map to the Collator key
        for (storage_key, value) in obj.iter() {
            assert!(storage_key.starts_with("cec5070d609dd3497f72bde07fc96ba0"));
            assert_eq!(value.as_str().unwrap(), kusama_key);
        }

        let expected_injects = json!({
            // Session Nextkeys for `Collator`
            "cec5070d609dd3497f72bde07fc96ba04c014e6bf8b8c2c011e7290b85696bb39af53646681828f1005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15": "005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15",

            // Session KeyOwner
            "cec5070d609dd3497f72bde07fc96ba0726380404683fc89e8233450c8aa1950eab3d4a1675d3d746175726180005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15": "005025ef7c9934c33534cbff35c9c5f0c1d30128e64f076c76942f49788eec15",
        });

        assert_eq!(injects, expected_injects);
    }
}
