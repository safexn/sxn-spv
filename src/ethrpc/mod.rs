use std::sync::Arc;

use crate::{server::AppState, utils::SimpleCache};
use alloy::{hex::FromHex, rpc::types::Filter};
use axum::{extract::State, Json};
use helios::{
    common::types::BlockTag,
    ethereum::{database::ConfigDB, EthereumClient},
    opstack::OpStackClient,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const GET_LOGS: &str = "eth_getLogs";
const GET_BLOCKBYNUMBER: &str = "eth_getBlockByNumber";
const GET_TRANSACTION_RECEIPT: &str = "eth_getTransactionReceipt";

//  curl -X POST -H 'Content-Type: application/json' --data
//    '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'  http://127.0.0.1:3033/
// {"jsonrpc":"2.0","result":"0x13ddfa0","id":1}
//  errors:
// {"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}
// {"jsonrpc":"2.0","error":{"code":-32602,"message":"invalid string length at line 1 column 2"},"id":1}
#[derive(Serialize, Deserialize, Clone)]
pub struct ETHReq {
    jsonrpc: String,
    method: String,
    params: Value,
    id: Value,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct ETHResp {
    jsonrpc: String,
    error: Option<String>,
    id: Value,
    result: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct ETHRespOk {
    jsonrpc: String,
    id: Value,
    result: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct ETHRespErr {
    jsonrpc: String,
    error: JsonRpcError,
    id: Value,
}
async fn handle_request_op(
    helios_op_client: &Arc<OpStackClient>,
    req: ETHReq,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> ETHResp {
    let params = match req.params.as_array() {
        Some(v) => v,
        None => {
            return create_eth_response(
                Some("Invalid params type, an array is required".to_string()),
                &req.id,
                None,
            )
        }
    };

    match req.method.as_str() {
        GET_LOGS => handle_get_logs_op(helios_op_client, params, &req.id).await,
        GET_BLOCKBYNUMBER => handle_get_block_by_number_op(helios_op_client, params, &req.id).await,
        GET_TRANSACTION_RECEIPT => {
            handle_get_transaction_receipt_op(helios_op_client, params, &req.id, response_cache)
                .await
        }
        _ => create_eth_response(
            Some(format!("method {} not support", &req.method)),
            &req.id,
            None,
        ),
    }
}

async fn handle_get_logs_op(
    helios_op_client: &Arc<OpStackClient>,
    params: &[Value],
    id: &Value,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("invalid numbers of params".to_string()), id, None);
    }

    let filter = match Filter::deserialize(&params[0]) {
        Ok(f) => f,
        Err(e) => {
            return create_eth_response(Some(format!("Filter deserialize error {e}")), id, None)
        }
    };

    match helios_op_client.get_logs(&filter).await {
        Ok(logs) => {
            let logs_str = serde_json::to_value(&logs).unwrap();
            create_eth_response(None, id, Some(logs_str))
        }
        Err(e) => create_eth_response(Some(e.to_string()), id, None),
    }
}

async fn handle_get_block_by_number_op(
    helios_op_client: &Arc<OpStackClient>,
    params: &[Value],
    id: &Value,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("Invalid numbers of params".to_string()), id, None);
    }

    let height = match params[0].as_str() {
        Some(str) => str,
        None => {
            return create_eth_response(
                Some("params type wrong, int is required".to_string()),
                id,
                None,
            )
        }
    };

    let num = match height.parse::<u64>() {
        Ok(str) => str,
        Err(e) => return create_eth_response(Some(format!("error number {e}")), id, None),
    };

    match helios_op_client
        .get_block_by_number(BlockTag::Number(num), false)
        .await
    {
        Ok(Some(block)) => {
            let block_str = serde_json::to_value(&block).unwrap();
            create_eth_response(None, id, Some(block_str))
        }
        Ok(None) => create_eth_response(None, id, None),
        Err(e) => create_eth_response(Some(e.to_string()), id, None),
    }
}

async fn handle_get_transaction_receipt_op(
    helios_op_client: &Arc<OpStackClient>,
    params: &[Value],
    id: &Value,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("Invalid numbers of params".to_string()), id, None);
    }

    let hex = match params[0].as_str() {
        Some(str) => str,
        None => return create_eth_response(Some("error TX hash".to_string()), id, None),
    };

    let txhash = match alloy::primitives::B256::from_hex(hex) {
        Ok(f) => f,
        Err(e) => {
            return create_eth_response(Some(format!("H256 deserialize error {e}")), id, None)
        }
    };

    if let Some(receipt) = response_cache.get(&txhash.to_vec()).await {
        println!("[get_transaction_receipt_op] from cache {txhash}");
        let receipt_str = serde_json::to_value(&receipt).unwrap();
        return create_eth_response(None, id, Some(receipt_str));
    }

    println!("[get_transaction_receipt_op] {txhash}");
    match helios_op_client.get_transaction_receipt(txhash).await {
        Ok(Some(receipt)) => {
            let receipt_str = serde_json::to_value(&receipt).unwrap();
            response_cache
                .insert(txhash.to_vec(), receipt_str.clone())
                .await;
            create_eth_response(None, id, Some(receipt_str))
        }
        Ok(None) => create_eth_response(None, id, None),
        Err(e) => {
            println!("[get_transaction_receipt_op] error {e}");
            create_eth_response(Some(e.to_string()), id, None)
        }
    }
}

pub async fn optimism(State(state): State<AppState>, Json(payload): Json<ETHReq>) -> String {
    let response_cache = state.chain_state.response_cache.clone();

    let helios_op_client: Arc<OpStackClient> = state.chain_state.helios_op_client.clone().unwrap();
    let req = payload;

    let resp = handle_request_op(&helios_op_client, req, response_cache).await;
    let resp = convert(vec![resp]);

    deeps_rsv::create_sgx_response_v2(resp[0].clone(), deeps_rsv::KeyType::SGX)
}

pub async fn eth2(State(state): State<AppState>, Json(payload): Json<ETHReq>) -> String {
    let response_cache = state.chain_state.response_cache.clone();

    let helios_client: Arc<EthereumClient<ConfigDB>> =
        state.chain_state.helios_client.clone().unwrap();
    let req = payload;

    let resp = handle_request(&helios_client, req, response_cache).await;
    let resp = convert(vec![resp]);

    deeps_rsv::create_sgx_response_v2(resp[0].clone(), deeps_rsv::KeyType::SGX)
}

pub async fn eth(State(state): State<AppState>, Json(payload): Json<Vec<ETHReq>>) -> String {
    let response_cache = state.chain_state.response_cache.clone();

    let helios_client = state.chain_state.helios_client.clone().unwrap();
    let resps: Vec<_> = payload
        .iter()
        .map(|req| handle_request(&helios_client, req.clone(), response_cache.clone()))
        .collect();

    let mut resps_all = Vec::new();
    for i in resps {
        resps_all.push(i.await);
    }

    let resp = convert(resps_all);
    if resp.len() == 1 {
        return resp[0].clone();
    }
    serde_json::to_string(&resp).unwrap()
}

async fn handle_request(
    helios_client: &Arc<EthereumClient<ConfigDB>>,
    req: ETHReq,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> ETHResp {
    let params = match req.params.as_array() {
        Some(v) => v,
        None => {
            return create_eth_response(
                Some("Invalid params type, an array is required".to_string()),
                &req.id,
                None,
            )
        }
    };

    match req.method.as_str() {
        GET_LOGS => handle_get_logs(helios_client, params, &req.id).await,
        GET_BLOCKBYNUMBER => handle_get_block_by_number(helios_client, params, &req.id).await,
        GET_TRANSACTION_RECEIPT => {
            handle_get_transaction_receipt(helios_client, params, &req.id, response_cache).await
        }
        _ => create_eth_response(
            Some(format!("method {} not support", &req.method)),
            &req.id,
            None,
        ),
    }
}

async fn handle_get_logs(
    helios_client: &Arc<EthereumClient<ConfigDB>>,
    params: &[Value],
    id: &Value,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("invalid numbers of params".to_string()), id, None);
    }

    let filter = match Filter::deserialize(&params[0]) {
        Ok(f) => f,
        Err(e) => {
            return create_eth_response(Some(format!("Filter deserialize error {e}")), id, None)
        }
    };

    match helios_client.get_logs(&filter).await {
        Ok(logs) => {
            let logs_str = serde_json::to_value(&logs).unwrap();
            create_eth_response(None, id, Some(logs_str))
        }
        Err(e) => create_eth_response(Some(e.to_string()), id, None),
    }
}

async fn handle_get_block_by_number(
    helios_client: &Arc<EthereumClient<ConfigDB>>,
    params: &[Value],
    id: &Value,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("Invalid numbers of params".to_string()), id, None);
    }

    let height = match params[0].as_str() {
        Some(str) => str,
        None => {
            return create_eth_response(
                Some("params type wrong, int is required".to_string()),
                id,
                None,
            )
        }
    };

    let num = match height.parse::<u64>() {
        Ok(str) => str,
        Err(e) => return create_eth_response(Some(format!("error number {e}")), id, None),
    };

    match helios_client
        .get_block_by_number(BlockTag::Number(num), false)
        .await
    {
        Ok(Some(block)) => {
            let block_str = serde_json::to_value(&block).unwrap();
            create_eth_response(None, id, Some(block_str))
        }
        Ok(None) => create_eth_response(None, id, None),
        Err(e) => create_eth_response(Some(e.to_string()), id, None),
    }
}

async fn handle_get_transaction_receipt(
    helios_client: &Arc<EthereumClient<ConfigDB>>,
    params: &[Value],
    id: &Value,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> ETHResp {
    if params.len() != 1 {
        return create_eth_response(Some("Invalid numbers of params".to_string()), id, None);
    }

    let hex = match params[0].as_str() {
        Some(str) => str,
        None => return create_eth_response(Some("error TX hash".to_string()), id, None),
    };

    let txhash = match alloy::primitives::B256::from_hex(hex) {
        Ok(f) => f,
        Err(e) => {
            return create_eth_response(Some(format!("H256 deserialize error {e}")), id, None)
        }
    };

    if let Some(receipt) = response_cache.get(&txhash.to_vec()).await {
        println!("[get_transaction_receipt] from cache {txhash}");
        let receipt_str = serde_json::to_value(&receipt).unwrap();
        return create_eth_response(None, id, Some(receipt_str));
    }

    println!("[get_transaction_receipt] {txhash}");
    match helios_client.get_transaction_receipt(txhash).await {
        Ok(Some(receipt)) => {
            let receipt_str = serde_json::to_value(&receipt).unwrap();
            response_cache
                .insert(txhash.to_vec(), receipt_str.clone())
                .await;
            create_eth_response(None, id, Some(receipt_str))
        }
        Ok(None) => create_eth_response(None, id, None),
        Err(e) => {
            println!("[get_transaction_receipt] error {e}");
            create_eth_response(Some(e.to_string()), id, None)
        }
    }
}

fn create_eth_response(error: Option<String>, id: &Value, result: Option<Value>) -> ETHResp {
    ETHResp {
        jsonrpc: String::from("2.0"),
        error,
        id: id.clone(),
        result,
    }
}

fn convert(inputs: Vec<ETHResp>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for input in inputs {
        if input.error.is_none() {
            let resp = ETHRespOk {
                jsonrpc: input.jsonrpc,
                id: input.id,
                result: input.result,
            };
            let resp = serde_json::to_string(&resp).unwrap();
            all.push(resp);
        } else {
            let resp = ETHRespErr {
                jsonrpc: input.jsonrpc,
                id: input.id,
                error: JsonRpcError {
                    code: -999,
                    message: input.error.unwrap(),
                    data: None,
                },
            };
            let resp = serde_json::to_string(&resp).unwrap();
            all.push(resp);
        }
    }
    all
}
