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
                    .expect(&format!("value {:?} should be a valid path", parts.last()));
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

    #[tokio::test]
    async fn get_header_from_block_should_work() {
        let expeted_header = json!({"parentHash":"0xc890131fe604ae48cd385eb91be80f70ad99307bdc9e79d5969b8e8b507ad138","number":"0x1c6b060","stateRoot":"0x1c5bfccd40794b5a0bd286aa8a5bc95d399399881b86428a3dff50c01cec41d1","extrinsicsRoot":"0x0815e5a6dc935c696e0619181fa26d28481205e9dc378ab380d92a36a97672a1","digest":{"logs":["0x0642414245b50103340100003cd6711100000000c2b1b49c3abff403a816c9f9f4ca96ce914a48e2c2579dbf050e58cbe478197c12d22ca51a2f42237b40472bf9a2de48df2968eb89a10ebcc4e6e046b9be470316c1bce81d762e343e78516c6d114295d6f338788f44dbd3f9718b7b80710703","0x04424545468403889f6e18a352a24d88267f842803fbe97262bf72d87c8a2f91b59f646693d3c5","0x0446524e4b2403d900000000000000","0x04424142451402d9000000","0x04424545461402d9000000","0x0446524e4b24037602000000000000","0x0442414245140276020000","0x0442454546140276020000","0x05424142450101e8c608382a1eb7d08a7d252b58d74f157834c1b98c754f0b3554fe7fcbfa056a17c1005d7e989635f6545b35cda5643e8797f709b9d6f49670e909c326c5e282"]}});
        let header = get_header_from_block(29798496, "https://kusama-rpc.dwellir.com")
            .await
            .unwrap();
        assert_eq!(expeted_header["parentHash"], header["parentHash"]);
        assert_eq!(expeted_header["stateRoot"], header["stateRoot"]);
        assert_eq!(expeted_header["number"], header["number"]);
        assert_eq!(expeted_header["extrinsicsRoot"], header["extrinsicsRoot"]);
    }

    #[test]
    fn para_head_key_should_work() {
        let para_id = 1000_u32;
        let head_key = para_head_key(para_id);
        assert_eq!(&head_key, "0xcd710b30bd2eab0352ddcc26417aa1941b3c252fcb29d88eff4f3de5de4476c3b6ff6f7d467b87a9e8030000");
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
        let _ = localize_config(config_path).await.unwrap();
        let network_config =
            zombienet_configuration::NetworkConfig::load_from_toml(&config_path).unwrap();
        let _alice_db = network_config
            .relaychain()
            .nodes()
            .first()
            .unwrap()
            .db_snapshot()
            .unwrap()
            .to_string();
    }
}
