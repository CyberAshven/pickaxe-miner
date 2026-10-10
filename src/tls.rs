//! #### PR #42: TLS through rustls
//! What: wss:// Fulcrum servers and https:// nodes connect through rustls
//! (the aws-lc-rs provider, already in the build for the ASIC device
//! library), checked by the operating system's verifier: the system's
//! certificates, including a CA the miner installed (Start9's, for one).
//! Why: native-tls linked the system's OpenSSL on Linux, so a binary built
//! on a new Linux would not start on an old one (HiveOS is Ubuntu 20.04).
//! Look here if: a wss:// server or https:// node fails its TLS handshake.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Response;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, WebSocket};

/// The client configuration every TLS connection uses, built once.
pub fn client_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::BuilderVerifierExt;
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let builder = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|error| format!("tls setup: {error}"))?
                .with_platform_verifier()
                .map_err(|error| format!("tls setup: {error}"))?;
            Ok(Arc::new(builder.with_no_client_auth()))
        })
        .clone()
}

/// A TLS stream to `host` over `stream`, with its handshake done.
pub fn connect(
    host: &str,
    stream: TcpStream,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, String> {
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| format!("tls: {host} is not a server name"))?;
    let connection = rustls::ClientConnection::new(client_config()?, name)
        .map_err(|error| format!("tls setup: {error}"))?;
    let mut tls = rustls::StreamOwned::new(connection, stream);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|error| format!("tls handshake: {error}"))?;
    }
    Ok(tls)
}

/// A WebSocket to `url` (ws:// or wss://), with a 10 s connect to each of
/// its addresses in turn.
pub fn websocket(url: &str) -> Result<(WebSocket<MaybeTlsStream<TcpStream>>, Response), String> {
    let request = url
        .into_client_request()
        .map_err(|error| format!("connect: {error}"))?;
    let uri = request.uri();
    let secure = uri.scheme_str() == Some("wss");
    let host = uri
        .host()
        .ok_or("connect: the URL has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let stream = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| format!("connect: {error}"))?
        .find_map(|address| TcpStream::connect_timeout(&address, Duration::from_secs(10)).ok())
        .ok_or_else(|| format!("connect: nothing answers at {host}:{port}"))?;
    let _ = stream.set_nodelay(true);
    let connector = if secure {
        Connector::Rustls(client_config()?)
    } else {
        Connector::Plain
    };
    tungstenite::client_tls_with_config(request, stream, None, Some(connector))
        .map_err(|error| format!("connect: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    // What: the TLS configuration builds (the provider and the system's
    // verifier), once, and a name that is not a server name is refused
    // before any connection.
    // Look here if: client_config or connect changes.
    #[test]
    fn the_tls_configuration_builds_once() {
        let first = client_config().unwrap();
        let second = client_config().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let error = connect("not a name", stream).unwrap_err();
        assert!(error.contains("is not a server name"), "{error}");
    }

    // #### PR #42
    // What: the built-in Fulcrum servers of each network answer over rustls
    // with the system's verifier (their certificates are accepted).
    // Look here if: wss:// servers stop connecting after a TLS change.
    #[test]
    #[ignore = "reaches the built-in Fulcrum servers over the internet"]
    fn the_built_in_fulcrum_servers_answer_over_rustls() {
        use crate::config::{MiningNetwork, MiningToken};
        for (network, servers) in [
            (
                MiningNetwork::Mainnet,
                crate::protocol::FULCRUM_WSS_BOOTSTRAP,
            ),
            (
                MiningNetwork::Chipnet,
                crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP,
            ),
        ] {
            let deployment = MiningToken::Photon.photon_deployment(network);
            let answered: Vec<&str> = servers
                .iter()
                .copied()
                .filter(|url| {
                    let result = crate::electrum::ElectrumSession::connect_failover_for_deployment(
                        &[(*url).to_owned()],
                        deployment,
                    );
                    match &result {
                        Ok(_) => eprintln!("{url}: ok"),
                        Err(error) => eprintln!("{url}: {}", error.lines().last().unwrap_or("")),
                    }
                    result.is_ok()
                })
                .collect();
            assert!(!answered.is_empty(), "no {network:?} server answered");
        }
    }
}
