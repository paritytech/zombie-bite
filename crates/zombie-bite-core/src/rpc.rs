//! RPC helpers shared across modules.

use zombienet_sdk::subxt::ext::subxt_rpcs::RpcClient;

/// Connect to `url`, allowing plain `ws://` (local nodes) as well as `wss://`.
pub(crate) async fn rpc_client(url: &str) -> Result<RpcClient, anyhow::Error> {
    let client = if url.starts_with("ws://") {
        RpcClient::from_insecure_url(url).await?
    } else {
        RpcClient::from_url(url).await?
    };
    Ok(client)
}
