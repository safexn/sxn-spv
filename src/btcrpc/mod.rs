mod proof;
pub use proof::{verify_tx, verify_tx_doge};

use crate::server::AppState;
use crate::utils::SimpleCache;
use axum::{extract::State, Json};
use bitcoinnakamoto::{consensus::serialize, BlockHash};
use nakamoto::client::handle::Handle as _;
use nakamoto::client::Handle;
use nakamoto::net::Waker;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;

const GETBESTBLOCKHASH: &str = "getbestblockhash";
const GETBLOCKHEADER: &str = "getblockheader";
const GETBLOCKHASH: &str = "getblockhash";

#[derive(Serialize, Deserialize)]
pub struct BtcReqNoJsonRpc {
    method: String,
    params: Value,
    id: Value,
}

#[derive(Serialize, Deserialize)]
pub struct BtcReq {
    jsonrpc: String,
    method: String,
    params: Value,
    id: Value,
}

#[derive(Serialize, Deserialize)]
pub struct BtcResp {
    error: Option<String>,
    id: Value,
    result: Option<Value>,
}

fn parse_request(req: Value) -> Result<BtcReq, (String, Value)> {
    let id = req.get("id").map_or(json!(1), |v| v.clone());
    if req.get("jsonrpc").is_some() {
        serde_json::from_value(req).map_err(|e| (format!("deser error {e}"), id))
    } else {
        let res: BtcReqNoJsonRpc =
            serde_json::from_value(req).map_err(|e| (format!("deser error {e}"), id))?;
        Ok(BtcReq {
            jsonrpc: "2.0".to_string(),
            method: res.method,
            params: res.params,
            id: res.id,
        })
    }
}

fn create_btc_response(error: Option<String>, id: &Value, result: Option<Value>) -> BtcResp {
    BtcResp {
        error,
        id: id.clone(),
        result,
    }
}

async fn handle_fetch_header<W: Waker>(
    nakamoto_handle: Arc<Handle<W>>,
    payload: Vec<Value>,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> String {
    let resps: Vec<_> = payload
        .iter()
        .map(async |req| {
            let req = match parse_request(req.clone()) {
                Ok(v) => v,
                Err((e, id)) => {
                    return create_btc_response(
                        Some(format!("Invalid request {e} {req}")),
                        &id,
                        None,
                    )
                }
            };

            let params = match req.params.as_array() {
                Some(v) => v,
                None => {
                    return create_btc_response(
                        Some("Invalid params type, an array is required".to_string()),
                        &req.id,
                        None,
                    )
                }
            };

            match req.method.as_str() {
                GETBESTBLOCKHASH => handle_getbestblockhash(&nakamoto_handle, params, &req.id),
                GETBLOCKHEADER => {
                    handle_getblockheader(&nakamoto_handle, params, &req.id, response_cache.clone())
                        .await
                }
                GETBLOCKHASH => {
                    handle_getblockhash(&nakamoto_handle, params, &req.id, response_cache.clone())
                        .await
                }
                _ => create_btc_response(
                    Some(format!("method {} not support", &req.method)),
                    &req.id,
                    None,
                ),
            }
        })
        .collect();
    let resps = futures::future::join_all(resps).await;

    let result = serde_json::to_string(&resps).unwrap();
    deeps_rsv::create_sgx_response_v2(result, deeps_rsv::KeyType::SGX)
}

fn handle_getbestblockhash<W: Waker>(
    handle: &Arc<Handle<W>>,
    params: &[Value],
    id: &Value,
) -> BtcResp {
    if !params.is_empty() {
        return create_btc_response(Some("Invalid numbers of params".to_string()), id, None);
    }
    match handle.get_tip() {
        Ok((_, header, _)) => {
            create_btc_response(None, id, Some(json!(header.block_hash().to_string())))
        }
        Err(e) => create_btc_response(Some(e.to_string()), id, None),
    }
}

async fn handle_getblockheader<W: Waker>(
    handle: &Arc<Handle<W>>,
    params: &[Value],
    id: &Value,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> BtcResp {
    if params.is_empty() || params.len() > 2 {
        return create_btc_response(Some("invalid numbers of params".to_string()), id, None);
    }

    let verbose = if params.len() == 2 {
        params[1].as_bool().unwrap_or(true)
    } else {
        true
    };

    let hash_str = match params[0].as_str() {
        Some(str) => str,
        None => return create_btc_response(Some("params type wrong".to_string()), id, None),
    };

    let hash: BlockHash = match BlockHash::from_str(hash_str) {
        Ok(str) => str,
        Err(e) => return create_btc_response(Some(format!("BlockHash error {e}")), id, None),
    };

    if let Some(header) = response_cache.get(&hash.to_vec()).await {
        println!("[btc_handle_getblockheader] from cache {hash}");
        let header_str = serde_json::to_value(&header).unwrap();
        return create_btc_response(None, id, Some(header_str));
    }

    match handle.get_block(&hash) {
        Ok(Some((height, header))) => {
            let header = if verbose {
                let mut h: Value = serde_json::to_value(header).unwrap();
                h["height"] = json!(height);
                h
            } else {
                json!(hex::encode(serialize(&header)))
            };
            let header_str = serde_json::to_value(&header).unwrap();
            response_cache
                .insert(hash.to_vec(), header_str.clone())
                .await;
            create_btc_response(None, id, Some(header))
        }
        Ok(None) => create_btc_response(Some("header not found".to_string()), id, None),
        Err(e) => create_btc_response(Some(e.to_string()), id, None),
    }
}

async fn handle_getblockhash<W: Waker>(
    handle: &Arc<Handle<W>>,
    params: &[Value],
    id: &Value,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> BtcResp {
    if params.len() != 1 {
        return create_btc_response(Some("Invalid numbers of params".to_string()), id, None);
    }
    let height = match params[0].as_u64() {
        Some(str) => str,
        None => {
            return create_btc_response(
                Some("params type wrong, int is required".to_string()),
                id,
                None,
            )
        }
    };

    if let Some(header) = response_cache.get(&height.to_be_bytes().to_vec()).await {
        println!("[btc_handle_getblockhash] from cache {height}");
        let header_str = serde_json::to_value(&header).unwrap();
        return create_btc_response(None, id, Some(header_str));
    }

    match handle.get_block_by_height(height) {
        Ok(Some(header)) => {
            let header_str = serde_json::to_value(&header).unwrap();
            response_cache
                .insert(height.to_be_bytes().to_vec(), header_str.clone())
                .await;
            create_btc_response(None, id, Some(json!(header.block_hash().to_string())))
        }
        Ok(None) => create_btc_response(Some(format!("block height {height} not found")), id, None),
        Err(e) => create_btc_response(Some(e.to_string()), id, None),
    }
}

pub async fn btc(State(state): State<AppState>, Json(payload): Json<Vec<Value>>) -> String {
    let nakamoto_handle = state.chain_state.handle.clone().unwrap();
    let response_cache = state.chain_state.response_cache.clone();

    handle_fetch_header(nakamoto_handle, payload, response_cache).await
}

pub async fn doge(State(state): State<AppState>, Json(payload): Json<Vec<Value>>) -> String {
    let nakamoto_handle = state.chain_state.doge_handle.clone().unwrap();
    let response_cache = state.chain_state.response_cache.clone();

    handle_fetch_header(nakamoto_handle, payload, response_cache).await
}
