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
    /// #### PR #42: where it answered.
    pub fn url(&self) -> &str {
        match self {
            Self::Node { url, .. } | Self::Fulcrum { url, .. } | Self::Zmq { url } => url,
        }
    }

    /// One line for screens, never with a login.
    pub fn summary(&self) -> String {
        match self {
            Self::Node { url, who, check } => format!("{who} at {url}: {}", check.summary()),
            Self::Fulcrum {
                url,
                report: Ok(report),
            } => format!("{} at {url} · height {}", report.server, report.height),
            Self::Fulcrum {
                url,
                report: Err(error),
            } => format!("a server at {url} that is not a Fulcrum Pickaxe can use ({error})"),
            Self::Zmq { url } => format!(
                "ZMQ block notices at {url} (the ASIC server listens when bitcoin.conf names them, or with --node-zmq)"
            ),
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

// #### PR #42: another computer
// What: a computer the miner names (a name, `.local` name or address, or a
// Tailscale computer) is searched on the same usual ports as this one, with
// a 1.5 s connect; a public address only once the miner says yes. The ports
// are tried side by side, and each of a name's addresses in turn.
// Why: a node on another computer was typed in full, port included.
// Look here if: a computer's node answers but the search lists nothing.
/// Why another computer was not searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSearched {
    /// Not a computer's name or address, or it does not resolve.
    Bad(String),
    /// On the internet: searched only once the miner says yes.
    NeedsYes(String),
}

/// What `host` (a name, `.local` name or address) offers on `network`. A
/// public address is tried only with `public_ok`, and otherwise refused
/// with a question for the miner.
pub fn find_on_host(
    host: &str,
    network: MiningNetwork,
    public_ok: bool,
) -> Result<Vec<Found>, NotSearched> {
    let host = checked_host(host, public_ok)?;
    Ok(find_at(host, network, Duration::from_millis(1500)))
}

/// `host` trimmed, once it resolves and, unless `public_ok`, is this
/// computer or on the home network.
fn checked_host(host: &str, public_ok: bool) -> Result<&str, NotSearched> {
    let host = host.trim();
    if host.is_empty() || host.contains(['/', ' ', '@']) {
        return Err(NotSearched::Bad(
            "enter a computer's name or address, such as 192.168.0.55".into(),
        ));
    }
    let unresolved = || NotSearched::Bad(format!("{host} does not resolve"));
    let addresses: Vec<IpAddr> = (host, 0)
        .to_socket_addrs()
        .map_err(|_| unresolved())?
        .map(|address| address.ip())
        .collect();
    if addresses.is_empty() {
        return Err(unresolved());
    }
    if !public_ok && !addresses.iter().all(|ip| home_address(*ip)) {
        return Err(NotSearched::NeedsYes(format!(
            "{host} is on the internet, not this computer or your network; try it anyway?"
        )));
    }
    Ok(host)
}

// #### PR #42: Tailscale computers
// What: the computers of the miner's Tailscale network that are online,
// from `tailscale status --json` (3 seconds at most), by name with their
// first IPv4 address, for a search there.
// Why: a node on another computer of the tailnet is easier to pick than to
// type.
// Look here if: the Tailscale list is empty while computers are online.
/// A computer on the miner's Tailscale network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailscalePeer {
    pub name: String,
    pub address: String,
}

/// The online computers in `tailscale status --json` output, by name.
pub fn tailscale_peers_from(status: &serde_json::Value) -> Vec<TailscalePeer> {
    let mut peers: Vec<TailscalePeer> = status
        .get("Peer")
        .and_then(serde_json::Value::as_object)
        .map(|peers| {
            peers
                .values()
                .filter(|peer| {
                    peer.get("Online").and_then(serde_json::Value::as_bool) == Some(true)
                })
                .filter_map(|peer| {
                    let address = peer
                        .get("TailscaleIPs")?
                        .as_array()?
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())?
                        .to_owned();
                    let name = peer
                        .get("HostName")
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(address.as_str())
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .take(64)
                        .collect();
                    Some(TailscalePeer { name, address })
                })
                .collect()
        })
        .unwrap_or_default();
    peers.sort_by(|a, b| a.name.cmp(&b.name));
    peers
}

/// Asks the `tailscale` command on this computer for its online computers.
pub fn tailscale_peers() -> Result<Vec<TailscalePeer>, String> {
    let mut child = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Tailscale is not installed or not on the PATH".to_owned())?;
    let mut stdout = child.stdout.take().ok_or("Tailscale gave no output")?;
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = send.send(stdout.read_to_end(&mut output).map(|_| output));
    });
    let output = match receive.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(output)) => output,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Tailscale did not answer within 3 seconds".into());
        }
    };
    let _ = child.wait();
    let status: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|_| "Tailscale is not running or not signed in".to_owned())?;
    Ok(tailscale_peers_from(&status))
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

/// Tries every candidate port side by side, in the list's order.
fn find_at(host: &str, network: MiningNetwork, connect: Duration) -> Vec<Found> {
    std::thread::scope(|scope| {
        let probes: Vec<_> = candidates(network)
            .into_iter()
            .map(|(port, kind)| scope.spawn(move || probe(host, port, kind, network, connect)))
            .collect();
        probes
            .into_iter()
            .filter_map(|probe| probe.join().ok().flatten())
            .collect()
    })
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
    // Each of a name's addresses in turn (localhost may be ::1 first).
    let mut stream = (host, port)
        .to_socket_addrs()
        .ok()?
        .take(4)
        .find_map(|address| TcpStream::connect_timeout(&address, connect).ok())?;
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
    }

    // #### PR #42
    // What: a named computer is searched by its name (a home name or address
    // at once, a public one only after yes), and what answers there is
    // listed under that name.
    // Look here if: checked_host or probe changes.
    #[test]
    fn a_named_host_is_searched_by_name_and_a_public_one_needs_yes() {
        assert_eq!(checked_host(" localhost ", false), Ok("localhost"));
        assert_eq!(checked_host("192.168.0.55", false), Ok("192.168.0.55"));
        assert_eq!(
            checked_host("100.101.102.103", false),
            Ok("100.101.102.103")
        );
        assert!(matches!(
            checked_host("8.8.8.8", false),
            Err(NotSearched::NeedsYes(question)) if question.ends_with("try it anyway?")
        ));
        assert_eq!(checked_host("8.8.8.8", true), Ok("8.8.8.8"));
        assert!(matches!(
            find_on_host("http://x", MiningNetwork::Mainnet, false),
            Err(NotSearched::Bad(_))
        ));
        let zmq = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = zmq.local_addr().unwrap().port();
        let publisher = std::thread::spawn(move || {
            let (mut stream, _) = zmq.accept().unwrap();
            stream
                .write_all(&[0xff, 0, 0, 0, 0, 0, 0, 0, 1, 0x7f])
                .unwrap();
        });
        let found = probe(
            "localhost",
            port,
            Kind::Zmq,
            MiningNetwork::Chipnet,
            Duration::from_secs(1),
        )
        .unwrap();
        publisher.join().unwrap();
        assert_eq!(found.url(), format!("tcp://localhost:{port}"));
    }

    // #### PR #42
    // What: online Tailscale computers are listed by name with their IPv4
    // address; offline ones and ones without an IPv4 address are not.
    // Look here if: tailscale_peers_from changes.
    #[test]
    fn tailscale_status_json_lists_online_peers() {
        let status = serde_json::json!({
            "Self": {"HostName": "this-pc", "TailscaleIPs": ["100.64.0.1"], "Online": true},
            "Peer": {
                "nodekey:a": {"HostName": "cypherpunkdeb", "Online": true,
                    "TailscaleIPs": ["fd7a:115c:a1e0::1", "100.101.102.103"]},
                "nodekey:b": {"HostName": "laptop", "Online": false,
                    "TailscaleIPs": ["100.101.102.104"]},
                "nodekey:c": {"HostName": "v6-only", "Online": true,
                    "TailscaleIPs": ["fd7a:115c:a1e0::2"]},
                "nodekey:d": {"HostName": "", "Online": true, "TailscaleIPs": ["100.101.102.105"]},
            }
        });
        assert_eq!(
            tailscale_peers_from(&status),
            vec![
                TailscalePeer {
                    name: "100.101.102.105".into(),
                    address: "100.101.102.105".into()
                },
                TailscalePeer {
                    name: "cypherpunkdeb".into(),
                    address: "100.101.102.103".into()
                },
            ]
        );
        assert!(tailscale_peers_from(&serde_json::json!({})).is_empty());
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
        assert!(found.summary().ends_with("· height 951204"));
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
            format!(
                "ZMQ block notices at tcp://127.0.0.1:{port} (the ASIC server listens when bitcoin.conf names them, or with --node-zmq)"
            )
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
