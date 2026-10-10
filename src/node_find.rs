//! #### PR #42
//! Finding BCH nodes, Fulcrum servers and ZMQ block notices on this computer
//! or on another one the miner names. Only the network's usual ports are
//! tried, at most ten, each once with a short timeout; a public address is
//! tried only when the miner says so.

use crate::config::MiningNetwork;
use crate::node::NodeCheck;
use std::io::Read;
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// What usually answers at a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A node's JSON-RPC, and who usually listens there.
    Node(&'static str),
    ElectrumTcp,
    ElectrumWs,
    ElectrumWss,
    Zmq,
}

/// Something that answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Node {
        url: String,
        who: &'static str,
        check: NodeCheck,
    },
    Fulcrum {
        url: String,
        report: Result<FulcrumReport, String>,
    },
    Zmq {
        url: String,
    },
}

/// What a Fulcrum server says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FulcrumReport {
    /// Such as "Fulcrum 1.12.0".
    pub server: String,
    pub height: u64,
}

impl Found {
    /// One line for screens, never with a login.
    pub fn summary(&self) -> String {
        match self {
            Self::Node { url, who, check } => format!("{who} at {url}: {}", check.summary()),
            Self::Fulcrum {
                url,
                report: Ok(report),
            } => format!(
                "{} at {url} · height {} (add it under Fulcrum servers)",
                report.server, report.height
            ),
            Self::Fulcrum {
                url,
                report: Err(error),
            } => format!("a server at {url} that is not a Fulcrum Pickaxe can use ({error})"),
            Self::Zmq { url } => format!("ZMQ block notices at {url} (not used yet)"),
        }
    }
}

/// The usual ports on `network`, at most ten.
pub fn candidates(network: MiningNetwork) -> Vec<(u16, Kind)> {
    let mut ports = match network {
        MiningNetwork::Mainnet => vec![
            (8332, Kind::Node("Bitcoin Cash Node, Knuth or Flowee")),
            (50001, Kind::ElectrumTcp),
            (50021, Kind::ElectrumTcp),
            (50003, Kind::ElectrumWs),
            (50004, Kind::ElectrumWss),
            (28332, Kind::Zmq),
        ],
        MiningNetwork::Chipnet => vec![
            (48332, Kind::Node("Bitcoin Cash Node")),
            (8332, Kind::Node("Knuth (its default on every network)")),
            (64001, Kind::ElectrumTcp),
            (50001, Kind::ElectrumTcp),
            (64003, Kind::ElectrumWs),
            (50003, Kind::ElectrumWs),
            (64004, Kind::ElectrumWss),
            (50004, Kind::ElectrumWss),
            (28332, Kind::Zmq),
        ],
    };
    ports.truncate(10);
    ports
}

/// What this computer offers on `network`.
pub fn find_on_this_computer(network: MiningNetwork) -> Vec<Found> {
    find_at("127.0.0.1", network, Duration::from_millis(300))
}

/// What `host` (a name, `.local` name or address) offers on `network`. A
/// public address is tried only with `public_ok`, and otherwise refused
/// with a question for the miner.
pub fn find_on_host(
    host: &str,
    network: MiningNetwork,
    public_ok: bool,
) -> Result<Vec<Found>, String> {
    let host = host.trim();
    if host.is_empty() || host.contains(['/', ' ', '@']) {
        return Err("enter a computer's name or address, such as 192.168.0.55".into());
    }
    let ip = (host, 0)
        .to_socket_addrs()
        .map_err(|_| format!("{host} does not resolve"))?
        .next()
        .ok_or_else(|| format!("{host} does not resolve"))?
        .ip();
    if !public_ok && !home_address(ip) {
        return Err(format!(
            "{host} is on the internet, not this computer or your network; try it anyway?"
        ));
    }
    Ok(find_at(host, network, Duration::from_millis(1500)))
}

/// Whether `ip` is this computer or on the home network (private,
/// link-local, Tailscale's shared range, IPv6 unique-local or link-local).
fn home_address(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (first == 100 && second & 0xc0 == 0x40)
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn find_at(host: &str, network: MiningNetwork, connect: Duration) -> Vec<Found> {
    candidates(network)
        .into_iter()
        .filter_map(|(port, kind)| probe(host, port, kind, network, connect))
        .collect()
}

/// ZMTP 3's greeting starts with 0xff and ends its signature with 0x7f; a
/// ZMQ publisher sends it as soon as the connection opens.
fn is_zmtp_greeting(greeting: &[u8; 10]) -> bool {
    greeting[0] == 0xff && greeting[9] == 0x7f
}

fn probe(
    host: &str,
    port: u16,
    kind: Kind,
    network: MiningNetwork,
    connect: Duration,
) -> Option<Found> {
    let address = (host, port).to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&address, connect).ok()?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Some(match kind {
        Kind::Node(who) => {
            drop(stream);
            let url = format!("http://{authority}");
            Found::Node {
                check: crate::node::check_node(&url, network),
                url,
                who,
            }
        }
        Kind::Zmq => {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let mut greeting = [0u8; 10];
            stream.read_exact(&mut greeting).ok()?;
            if !is_zmtp_greeting(&greeting) {
                return None;
            }
            Found::Zmq {
                url: format!("tcp://{authority}"),
            }
        }
        Kind::ElectrumTcp | Kind::ElectrumWs | Kind::ElectrumWss => {
            drop(stream);
            let scheme = match kind {
                Kind::ElectrumTcp => "tcp",
                Kind::ElectrumWs => "ws",
                _ => "wss",
            };
            let url = format!("{scheme}://{authority}");
            Found::Fulcrum {
                report: fulcrum_report(&url, network),
                url,
            }
        }
    })
}

/// What a Fulcrum server at `url` says: its version and height.
fn fulcrum_report(url: &str, network: MiningNetwork) -> Result<FulcrumReport, String> {
    let deployment = crate::config::MiningToken::Photon.photon_deployment(network);
    let mut session = crate::electrum::ElectrumSession::connect_failover_for_deployment(
        &[url.to_owned()],
        deployment,
    )
    .map_err(|error| error.lines().last().unwrap_or_default().to_owned())?;
    let server = session
        .server_version
        .get(0)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("an Electrum server")
        .chars()
        .filter(|ch| !ch.is_control())
        .take(48)
        .collect();
    let height = session
        .rpc("blockchain.headers.subscribe", serde_json::json!([]))?
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .ok_or("the server gave no height")?;
    Ok(FulcrumReport { server, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    // #### PR #42
    // What: each network's usual ports are at most ten, nodes first, and
    // the ZMTP greeting is told from other bytes.
    // Look here if: candidates or is_zmtp_greeting changes.
    #[test]
    fn this_computer_is_searched_on_at_most_ten_ports_and_nowhere_else() {
        for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
            let ports = candidates(network);
            assert!(!ports.is_empty() && ports.len() <= 10, "{network:?}");
            assert!(matches!(ports[0].1, Kind::Node(_)));
        }
        assert_eq!(candidates(MiningNetwork::Chipnet)[0].0, 48332);
        assert!(is_zmtp_greeting(&[0xff, 0, 0, 0, 0, 0, 0, 0, 1, 0x7f]));
        assert!(!is_zmtp_greeting(b"HTTP/1.1 4"));
        assert!(home_address("192.168.0.55".parse().unwrap()));
        assert!(home_address("100.101.102.103".parse().unwrap()));
        assert!(!home_address("8.8.8.8".parse().unwrap()));
        let error = find_on_host("8.8.8.8", MiningNetwork::Mainnet, false).unwrap_err();
        assert!(error.contains("try it anyway?"), "{error}");
        assert!(find_on_host("http://x", MiningNetwork::Mainnet, false).is_err());
    }

    // #### PR #42
    // What: a Fulcrum server over plain TCP and a ZMQ publisher are
    // recognised at their ports, with the server's version and height.
    // Look here if: probe or fulcrum_report changes.
    #[test]
    fn fulcrum_and_zmq_are_recognised() {
        let fulcrum = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = fulcrum.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            // The probe's quick check of the port, then the session.
            let _ = fulcrum.accept().unwrap();
            let (stream, _) = fulcrum.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "server.version" => serde_json::json!(["Fulcrum 1.12.0", "1.5"]),
                    _ => serde_json::json!({"height": 951_204, "hex": "00"}),
                };
                writeln!(
                    writer,
                    "{}",
                    serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                )
                .unwrap();
            }
        });
        let found = probe(
            "127.0.0.1",
            port,
            Kind::ElectrumTcp,
            MiningNetwork::Chipnet,
            Duration::from_secs(1),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(
            found,
            Found::Fulcrum {
                url: format!("tcp://127.0.0.1:{port}"),
                report: Ok(FulcrumReport {
                    server: "Fulcrum 1.12.0".into(),
                    height: 951_204
                })
            }
        );
        assert!(found
            .summary()
            .contains("height 951204 (add it under Fulcrum servers)"));
        let zmq = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = zmq.local_addr().unwrap().port();
        let publisher = std::thread::spawn(move || {
            let (mut stream, _) = zmq.accept().unwrap();
            stream
                .write_all(&[0xff, 0, 0, 0, 0, 0, 0, 0, 1, 0x7f])
                .unwrap();
        });
        let found = probe(
            "127.0.0.1",
            port,
            Kind::Zmq,
            MiningNetwork::Chipnet,
            Duration::from_secs(1),
        )
        .unwrap();
        publisher.join().unwrap();
        assert_eq!(
            found.summary(),
            format!("ZMQ block notices at tcp://127.0.0.1:{port} (not used yet)")
        );
        // A closed port is not found.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        assert!(probe(
            "127.0.0.1",
            port,
            Kind::Zmq,
            MiningNetwork::Chipnet,
            Duration::from_millis(300)
        )
        .is_none());
    }
}
