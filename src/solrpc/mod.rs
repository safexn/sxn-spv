use crate::chains::sol::SolClient;
use crate::server::AppState;
use crate::utils::SimpleCache;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use solana_client::rpc_client::GetConfirmedSignaturesForAddress2Config;
use solana_client::rpc_config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcSignaturesForAddressConfig,
    RpcTransactionConfig,
};
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta;
use std::str::FromStr;
use std::sync::Arc;

enum SOLMethod {
    GetTransaction,
    GetSlot,
    GetAccountInfo,
    GetSignaturesForAddress,
    GetProgramAccounts,
}

impl TryFrom<String> for SOLMethod {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.eq("getTransaction") {
            Ok(SOLMethod::GetTransaction)
        } else if value.eq("getSlot") {
            Ok(SOLMethod::GetSlot)
        } else if value.eq("getAccountInfo") {
            Ok(SOLMethod::GetAccountInfo)
        } else if value.eq("getSignaturesForAddress") {
            Ok(SOLMethod::GetSignaturesForAddress)
        } else if value.eq("getProgramAccounts") {
            Ok(SOLMethod::GetProgramAccounts)
        } else {
            Err(format!("Unsupported method {}", value))
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SOLReq {
    jsonrpc: String,
    method: String,
    params: Value,
    id: Value,
}

#[derive(Serialize, Deserialize)]
pub struct SOLResp {
    jsonrpc: String,
    error: Option<String>,
    id: Value,
    result: Option<Value>,
}

#[derive(Serialize, Deserialize)]
pub struct SOLRespOk {
    jsonrpc: String,
    id: Value,
    result: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct SOLRespErr {
    jsonrpc: String,
    error: JsonRpcError,
    id: Value,
}

pub async fn sol(State(state): State<AppState>, Json(payload): Json<SOLReq>) -> String {
    let response_cache = state.chain_state.response_cache.clone();
    let sol_client = state.chain_state.sol_client.clone().unwrap();
    let req = payload;
    let resp = handle_request(&sol_client, req, response_cache).await;
    let resp = convert(vec![resp]);

    deeps_rsv::create_sgx_response_v2(resp[0].clone(), deeps_rsv::KeyType::SGX)
}

async fn handle_request(
    sol_client: &Arc<SolClient>,
    req: SOLReq,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> SOLResp {
    let params = match req.params.as_array() {
        Some(v) => v,
        None => {
            return create_sol_response(
                Some("Invalid params type, an array is required".to_string()),
                &req.id,
                None,
            )
        }
    };

    match SOLMethod::try_from(req.method) {
        Ok(method) => match method {
            SOLMethod::GetTransaction => {
                handle_get_transaction(sol_client, params, &req.id, response_cache).await
            }
            SOLMethod::GetSlot => handle_get_slot(sol_client, params, &req.id).await,
            SOLMethod::GetAccountInfo => handle_get_account(sol_client, params, &req.id).await,
            SOLMethod::GetSignaturesForAddress => {
                handle_get_signatures_for_address(sol_client, params, &req.id).await
            }
            SOLMethod::GetProgramAccounts => {
                handle_get_program_accounts(sol_client, params, &req.id).await
            }
        },
        Err(err) => create_sol_response(Some(err), &req.id, None),
    }
}

async fn handle_get_program_accounts(
    sol_client: &Arc<SolClient>,
    params: &[Value],
    id: &Value,
) -> SOLResp {
    let pubkey = match params.first() {
        None => {
            return create_sol_response(Some("Invalid numbers of params".to_string()), id, None);
        }
        Some(pbk_v) => {
            let pbk_s = if let Some(s) = pbk_v.as_str() {
                s
            } else {
                return create_sol_response(Some("Invalid type of params".to_string()), id, None);
            };

            match Pubkey::from_str(pbk_s) {
                Ok(pbk) => pbk,
                Err(err) => {
                    return create_sol_response(Some(format!("Invalid Pubkey: {}", err)), id, None);
                }
            }
        }
    };

    let config = match params.get(1) {
        None => None,
        Some(c) => match serde_json::from_value::<Option<RpcProgramAccountsConfig>>(c.clone()) {
            Ok(config) => config,
            Err(err) => {
                return create_sol_response(
                    Some(format!("Invalid RpcAccountInfoConfig: {}", err)),
                    id,
                    None,
                );
            }
        },
    };

    println!("[get_program_accounts] {}", pubkey.to_string());
    match sol_client.get_program_accounts(pubkey, config).await {
        Ok(account) => {
            let account_v = serde_json::to_value(&account).unwrap();
            create_sol_response(None, id, Some(account_v))
        }
        Err(err) => {
            println!("{err:?}");
            create_sol_response(Some(err.to_string()), id, None)
        }
    }
}

async fn handle_get_signatures_for_address(
    sol_client: &Arc<SolClient>,
    params: &[Value],
    id: &Value,
) -> SOLResp {
    let pubkey = match params.first() {
        None => {
            return create_sol_response(Some("Invalid numbers of params".to_string()), id, None);
        }
        Some(pbk_v) => {
            let pbk_s = if let Some(s) = pbk_v.as_str() {
                s
            } else {
                return create_sol_response(Some("Invalid type of params".to_string()), id, None);
            };

            match Pubkey::from_str(pbk_s) {
                Ok(pbk) => pbk,
                Err(err) => {
                    return create_sol_response(Some(format!("Invalid Pubkey: {}", err)), id, None);
                }
            }
        }
    };

    let config = match params.get(1) {
        None => None,
        Some(c) => match serde_json::from_value::<Option<RpcSignaturesForAddressConfig>>(c.clone())
        {
            Ok(config) => {
                if let Some(config) = config {
                    let before = match config.before {
                        None => Option::<Signature>::None,
                        Some(b) => match Signature::from_str(&b) {
                            Ok(before) => Some(before),
                            Err(err) => {
                                return create_sol_response(
                                    Some(format!("Invalid RpcSignaturesForAddressConfig: {}", err)),
                                    id,
                                    None,
                                );
                            }
                        },
                    };

                    let until = match config.until {
                        None => None,
                        Some(u) => match Signature::from_str(&u) {
                            Ok(until) => Some(until),
                            Err(err) => {
                                return create_sol_response(
                                    Some(format!("Invalid RpcSignaturesForAddressConfig: {}", err)),
                                    id,
                                    None,
                                );
                            }
                        },
                    };

                    Some(GetConfirmedSignaturesForAddress2Config {
                        before,
                        until,
                        limit: config.limit,
                        commitment: config.commitment,
                    })
                } else {
                    None
                }
            }
            Err(err) => {
                return create_sol_response(
                    Some(format!("Invalid RpcSignaturesForAddressConfig: {}", err)),
                    id,
                    None,
                );
            }
        },
    };

    println!("[get_signatures_for_address] {}", pubkey.to_string());
    match sol_client.get_signatures_for_address(pubkey, config).await {
        Ok(signatures) => {
            let signatures_v = serde_json::to_value(&signatures).unwrap();
            create_sol_response(None, id, Some(signatures_v))
        }
        Err(err) => {
            println!("{err:?}");
            create_sol_response(Some(err.to_string()), id, None)
        }
    }
}

async fn handle_get_account(sol_client: &Arc<SolClient>, params: &[Value], id: &Value) -> SOLResp {
    let pubkey = match params.first() {
        None => {
            return create_sol_response(Some("Invalid numbers of params".to_string()), id, None);
        }
        Some(pbk_v) => {
            let pbk_s = if let Some(s) = pbk_v.as_str() {
                s
            } else {
                return create_sol_response(Some("Invalid type of params".to_string()), id, None);
            };

            match Pubkey::from_str(pbk_s) {
                Ok(pbk) => pbk,
                Err(err) => {
                    return create_sol_response(Some(format!("Invalid Pubkey: {}", err)), id, None);
                }
            }
        }
    };

    let config = match params.get(1) {
        None => None,
        Some(c) => match serde_json::from_value::<Option<RpcAccountInfoConfig>>(c.clone()) {
            Ok(config) => config,
            Err(err) => {
                return create_sol_response(
                    Some(format!("Invalid RpcAccountInfoConfig: {}", err)),
                    id,
                    None,
                );
            }
        },
    };

    println!("[get_account_info] {}", pubkey.to_string());
    match sol_client.get_account_info(pubkey, config).await {
        Ok(account) => {
            let account_v = serde_json::to_value(&account).unwrap();
            create_sol_response(None, id, Some(account_v))
        }
        Err(err) => {
            println!("{err:?}");
            create_sol_response(Some(err.to_string()), id, None)
        }
    }
}

async fn handle_get_slot(sol_client: &Arc<SolClient>, params: &[Value], id: &Value) -> SOLResp {
    let commitment_config = match params.first() {
        None => None,
        Some(c) => match serde_json::from_value::<Option<CommitmentConfig>>(c.clone()) {
            Ok(config) => config,
            Err(err) => {
                return create_sol_response(
                    Some(format!("Invalid CommitmentConfig: {}", err)),
                    id,
                    None,
                );
            }
        },
    };

    println!("[get_sol_slot]");
    match sol_client.get_slot(commitment_config).await {
        Ok(slot) => {
            let tx_v = serde_json::to_value(&slot).unwrap();
            create_sol_response(None, id, Some(tx_v))
        }
        Err(err) => {
            println!("{err:?}");
            create_sol_response(Some(err.to_string()), id, None)
        }
    }
}

async fn handle_get_transaction(
    sol_client: &Arc<SolClient>,
    params: &[Value],
    id: &Value,
    response_cache: Arc<SimpleCache<Vec<u8>, Value>>,
) -> SOLResp {
    let signature = match params.first() {
        None => {
            return create_sol_response(Some("Invalid numbers of params".to_string()), id, None);
        }
        Some(signature_v) => {
            let signature_s = if let Some(s) = signature_v.as_str() {
                s
            } else {
                return create_sol_response(Some("Invalid type of params".to_string()), id, None);
            };

            match Signature::from_str(signature_s) {
                Ok(sig) => sig,
                Err(err) => {
                    return create_sol_response(
                        Some(format!("Invalid signature: {}", err)),
                        id,
                        None,
                    );
                }
            }
        }
    };

    let cache_key = signature.as_ref().to_vec();
    match response_cache.get(&cache_key).await {
        None => {}
        Some(tx_v) => {
            match serde_json::from_value::<EncodedConfirmedTransactionWithStatusMeta>(tx_v) {
                Ok(tx) => {
                    let tx_res = serde_json::to_value(&tx).unwrap();
                    return create_sol_response(None, id, Some(tx_res));
                }
                Err(_err) => {
                    response_cache.remove(&cache_key).await;
                }
            }
        }
    }

    let tx_config = match params.get(1) {
        None => None,
        Some(c) => match serde_json::from_value::<Option<RpcTransactionConfig>>(c.clone()) {
            Ok(config) => config,
            Err(err) => {
                return create_sol_response(
                    Some(format!("Invalid RpcTransactionConfig: {}", err)),
                    id,
                    None,
                );
            }
        },
    };
    println!("[get_sol_transaction] {signature}");
    match sol_client.get_transaction(signature, tx_config).await {
        Ok(tx) => {
            let tx_v = serde_json::to_value(&tx).unwrap();
            response_cache.insert(cache_key, tx_v.clone()).await;
            create_sol_response(None, id, Some(tx_v))
        }
        Err(err) => {
            println!("{err:?}");
            create_sol_response(Some(err.to_string()), id, None)
        }
    }
}

fn create_sol_response(error: Option<String>, id: &Value, result: Option<Value>) -> SOLResp {
    SOLResp {
        jsonrpc: String::from("2.0"),
        error,
        id: id.clone(),
        result,
    }
}

fn convert(inputs: Vec<SOLResp>) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for input in inputs {
        if input.error.is_none() {
            let resp = SOLRespOk {
                jsonrpc: input.jsonrpc,
                id: input.id,
                result: input.result,
            };
            let resp = serde_json::to_string(&resp).unwrap();
            all.push(resp);
        } else {
            let resp = SOLRespErr {
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
