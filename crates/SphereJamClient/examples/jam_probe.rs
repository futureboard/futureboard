//! Minimal live probe: open the signaling socket and print what happens.
//!
//! A development aid for pointing the client at a local `jamd`, kept out of the
//! test suite because it needs a running server and reports rather than asserts.
//!
//! ```sh
//! cargo run -p sphere-jam-client --example jam_probe
//! ```

use std::time::Duration;

use sphere_jam_client::config::{EnvSource, JamConfig};
use sphere_jam_client::signaling::SignalingClient;

fn main() {
    let config = JamConfig::from_source(&EnvSource::from_pairs([
        ("FUTUREBOARD_ENV", "development"),
        ("FUTUREBOARD_JAM_API_URL", "http://127.0.0.1:8090"),
        ("FUTUREBOARD_JAM_WS_URL", "ws://127.0.0.1:8090/v1/realtime"),
    ]))
    .expect("config");
    println!("signaling url: {}", config.websocket_url);

    match SignalingClient::connect(&config.websocket_url, "dev:probe", Duration::from_secs(10)) {
        Ok((_client, ready)) => println!(
            "auth.ready: user={} connection={} node={} protocol=v{}",
            ready.user.id, ready.connection_id, ready.server_node_id, ready.protocol_version
        ),
        Err(error) => println!("connect failed: {error}"),
    }
}
