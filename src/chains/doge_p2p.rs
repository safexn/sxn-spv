use nakamoto::client::network::Network;
use nakamoto::client::{Client, Config, Error, Handle};
use nakamoto::net::poll::Waker;
use std::process::exit;
use std::str::FromStr;
use std::sync::Arc;
use std::{net, thread};

type Reactor = nakamoto::net::poll::Reactor<net::TcpStream>;

pub fn run_p2p_doge_client(
    config: &crate::config::ConfigCli,
    seal: Option<Vec<u8>>,
) -> Result<Arc<Handle<Waker>>, Error> {
    // Extract network type and database path from the configuration
    let network_type = config.doge_network_type.to_string();
    let database_path = &config.store;

    // Initialize the client configuration with the network, database path, and optional seal
    let mut client_config = Config::new(
        Network::from_str(&network_type).unwrap(),
        format!("{database_path}/nak-doge").into(),
        seal,
    );

    // Handle special case for Dogecoin Regtest network
    if config.doge_network_type == crate::types::Network::DogecoinRegtest {
        if !config.regtest_peer.is_empty() {
            // Use the provided regtest peer if available
            client_config.connect = config.regtest_peer.clone();
        } else {
            // Default to localhost with the regtest port if no peer is specified
            client_config.connect.push(
                format!("127.0.0.1:{}", Network::DOGECOINREGTEST.port())
                    .parse()
                    .unwrap(),
            );
        }
    }

    // Log the network and database path for debugging
    tracing::info!(
        target: "spv",
        "Dogecoin network: {:?}, Database path: {:?}",
        client_config.network,
        client_config.root
    );

    // Create a new client and obtain its handle
    let client = Client::<Reactor>::new()?;
    let client_handle = Arc::new(client.handle());

    // Spawn a new thread to run the client
    thread::spawn(move || {
        client.run(client_config).unwrap();
    });

    let r_h = client_handle.clone();
    thread::spawn(move || {
        restart(r_h);
    });

    // Return the client handle wrapped in an Arc
    Ok(client_handle)
}

fn restart(client_handle: Arc<Handle<Waker>>) {
    use nakamoto::client::handle::Handle;
    std::thread::sleep(std::time::Duration::from_secs(5 * 60));
    loop {
        let result = client_handle.get_tip();
        tracing::info!(target:"restart", "doge test");

        if result.is_err() {
            tracing::error!(target:"restart", "doge exit");
            exit(0)
            //panic!("panic");
        }
        std::thread::sleep(std::time::Duration::from_secs(40));
    }
}
