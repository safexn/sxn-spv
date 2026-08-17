use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{Path, State},
    response::IntoResponse as _,
    routing::{get, post},
    Router,
};

use crate::{
    chainstate::ChainsState,
    config::{Config, Service},
    types::Network,
};
use deeps_rsv::{create_sgx_response_v2, KeyType};
use nakamoto::client::traits::Handle;
use serde::Serialize;

#[derive(Clone)]
pub struct NetworkType {
    btc: Network,
    doge: Network,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) chain_state: Arc<ChainsState>,
    pub(crate) sgx: bool,
    pub(crate) network: NetworkType,
    pub(crate) service: Service,
}

#[derive(Clone, Serialize)]
struct SyncStatus {
    p2p_btc_height: u64,
    rpc_btc_tip: u64,
    lightclient_eth_height: u64,
    p2p_doge_height: u64,
    lightclient_op_height: u64,
    btc_chainid: u64,
    eth_chainid: u64,
    doge_chainid: u64,
    op_chainid: u64,
}

async fn status(State(state): State<AppState>) -> String {
    let btc_chainid = match state.network.btc {
        Network::Bitcoin => 0xa0898816u64,
        Network::Testnet => 0x10340fc0,
        _ => 0x0,
    };

    let doge_chainid = match state.network.doge {
        Network::DogecoinMainnet => 0xad6c4d97u64,
        Network::DogecoinTestnet => 0x343b2383,
        _ => 0x0,
    };

    let p2p_btc_height = if state.service.btc {
        match state.chain_state.handle.as_ref().unwrap().get_tip() {
            Ok((p2p_height, _, _)) => p2p_height,
            Err(_) => 0u64,
        }
    } else {
        0u64
    };

    let p2p_doge_height = if state.service.doge {
        match state.chain_state.doge_handle.as_ref().unwrap().get_tip() {
            Ok((p2p_height, _, _)) => p2p_height,
            Err(_) => 0u64,
        }
    } else {
        0u64
    };

    let (lightclient_eth_height, eth_chainid) = if state.service.eth {
        let helios_client = state.chain_state.helios_client.clone().unwrap();
        let eth_chainid = helios_client.chain_id().await;
        (
            match helios_client.get_block_number().await {
                Ok(res) => res.as_limbs()[0],
                Err(_) => 0u64,
            },
            eth_chainid,
        )
    } else {
        (0u64, 0u64)
    };

    let (lightclient_op_height, op_chainid) = if state.service.op {
        let helios_client = state.chain_state.helios_op_client.clone().unwrap();
        let eth_chainid = helios_client.chain_id().await;
        (
            match helios_client.get_block_number().await {
                Ok(res) => res.as_limbs()[0],
                Err(_) => 0u64,
            },
            eth_chainid,
        )
    } else {
        (0u64, 0u64)
    };

    let status = SyncStatus {
        p2p_btc_height,
        rpc_btc_tip: 0,
        lightclient_eth_height,
        p2p_doge_height,
        lightclient_op_height,
        btc_chainid,
        eth_chainid,
        doge_chainid,
        op_chainid,
    };

    let str = serde_json::to_string(&status).unwrap();
    create_sgx_response_v2(str, KeyType::SGX)
}

async fn ping(
    //Json(payload): Json<MerkleProofParam>,
    Path(str): Path<String>,
    State(state): State<AppState>,
) -> String {
    let (p2p_height, p2p_tip, _) = state
        .chain_state
        .handle
        .as_ref()
        .unwrap()
        .get_tip()
        .unwrap();
    let p2p_tip_hash = p2p_tip.block_hash();

    let rpc_tip = {
        let handle = state.chain_state.store.sync_headers.read().unwrap();
        let tip = *handle.tip();
        drop(handle);
        tip
    };

    let (doge_p2p_height, _, _) = state
        .chain_state
        .doge_handle
        .as_ref()
        .unwrap()
        .get_tip()
        .unwrap();

    format!(
        "\n [{str}] sgx=[{}] p2p_height=[{p2p_height}] 
    p2p_tip=[{p2p_tip_hash:?}] rpc_tip=[{rpc_tip}] doge_p2p_height=[{doge_p2p_height}]\n",
        state.sgx
    )
}

async fn start(
    chain_state: Arc<ChainsState>,
    addr: SocketAddr,
    sgx: bool,
    network: NetworkType,
    service: Service,
) {
    // Initialize the application state
    let state = AppState {
        chain_state,
        sgx,
        network,
        service,
    };

    // Define the routes for the application
    let routes = Router::new()
        .route("/status", get(status)) // Endpoint to get the sync status
        .route("/verify_tx/{txid}", get(crate::btcrpc::verify_tx)) // Endpoint to verify a Bitcoin transaction
        .route("/verify_tx_doge/{txid}", get(crate::btcrpc::verify_tx_doge)) // Endpoint to verify a Dogecoin transaction
        .route("/ping/{str}", get(ping)) // Endpoint to ping the server
        .route("/btc", post(crate::btcrpc::btc)) // Endpoint for Bitcoin-related operations
        .route("/doge", post(crate::btcrpc::doge)) // Endpoint for Dogecoin-related operations
        .route("/eth", post(crate::ethrpc::eth)) // Endpoint for Ethereum-related operations
        .route("/eth2", post(crate::ethrpc::eth2)) // Endpoint for batch Ethereum-related operations
        .route("/optimism", post(crate::ethrpc::optimism)) // Endpoint for Optimism-related operations
        .route("/sol", post(crate::solrpc::sol)); // Endpoint for Solana-related operations

    // Apply middleware to the routes
    let app = routes
        .layer(middleware::from_fn_with_state(state.clone(), middle_ware)) // Apply custom middleware
        .with_state(state); // Attach the application state

    // Bind the server to the specified address
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!(target: "spv", "listening on {}", listener.local_addr().unwrap());

    // Start the server
    axum::serve(listener, app).await.unwrap();
}

pub fn run_server(chain_state: Arc<ChainsState>, config: Config) {
    let service = config.service;
    let config = &config.configcli;

    crate::RUNTIME.block_on(async move {
        tracing::info!(target: "spv", "now running on a worker thread");
        start(
            chain_state,
            config.http_addr,
            config.sgx_enable,
            NetworkType {
                btc: config.network_type,
                doge: config.doge_network_type,
            },
            service,
        )
        .await;
        tracing::info!(target: "spv", "server stoped");
    });
}

use axum::middleware;

async fn middle_ware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    // Helper function to check if a service is enabled for a given route
    fn is_service_enabled(service: &Service, path: &str) -> bool {
        println!("path {path}");

        if path.contains("/verify_tx_doge") {
            return service.doge;
        } else if path.contains("/verify_tx") && !path.contains("/verify_tx_doge") {
            return service.btc;
        }

        match path {
            "/btc" => service.btc,
            "/doge" => service.doge,
            "/eth" => service.eth,
            "/eth2" => service.eth,
            "/optimism" => service.op,
            "/sol" => service.sol,
            "/status" => true,
            _ => false,
        }
    }

    if is_service_enabled(&state.service, request.uri().path()) {
        return next.run(request).await;
    } else {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
}
