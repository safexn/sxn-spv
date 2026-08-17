use axum::{
    extract::{Path, State},
    response::IntoResponse,
};
use std::str::FromStr;

use crate::server::AppState;
use crate::types::MerkleProofParam;
use crate::types::MerkleProofParamChain;
use bitcoin::Txid;
use deeps_rsv::create_sgx_response_v2;
use deeps_rsv::KeyType;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Response {
    result: String,
    error: bool,
}

impl Response {
    fn new(error: bool, result: String) -> String {
        let response = Self { result, error };
        serde_json::to_string(&response).unwrap()
    }
}

/// Helper function to create an SGX response
fn create_sgx_response(input: String) -> String {
    create_sgx_response_v2(input, KeyType::SGX)
}

/// Helper function to validate and parse a transaction ID
fn parse_txid(txid: &str) -> Result<Txid, String> {
    Txid::from_str(txid).map_err(|e| format!("Invalid Txid: {}", e))
}

/// Helper function to verify a transaction
async fn verify_transaction(
    txid: String,
    state: AppState,
    chain: MerkleProofParamChain,
) -> impl IntoResponse {
    let txid = match parse_txid(&txid) {
        Ok(txid) => txid,
        Err(e) => return create_sgx_response(Response::new(true, e)),
    };

    let response_cache = state.chain_state.response_cache.clone();

    let param = MerkleProofParam::new(vec![txid], None, chain);

    match state
        .chain_state
        .fetch_proof_and_verify(&param, response_cache)
        .await
    {
        Ok(_) => create_sgx_response(Response::new(false, txid.to_string())),
        Err(e) => create_sgx_response(Response::new(true, e)),
    }
}

pub async fn verify_tx(
    Path(txid): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    verify_transaction(txid, state, MerkleProofParamChain::BitCoin).await
}

pub async fn verify_tx_doge(
    Path(txid): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    verify_transaction(txid, state, MerkleProofParamChain::DogeCoin).await
}
