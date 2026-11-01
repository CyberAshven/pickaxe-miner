//! Where other devices reach a listener on this computer, for the connection
//! details the dashboards show: its address on the local network and, when
//! Tailscale is up, on the tailnet. Nothing is sent and no outside service is
//! asked: connecting a UDP socket only makes the system choose the interface.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process::{Command, Stdio};

/// Where an address works from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Only programs on this computer: a loopback listener.
    ThisComputer,
    /// Devices on the same local network.
    LocalNetwork,
    /// Devices on the same Tailscale network, wherever they are.
    Tailscale,
}

impl Place {
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisComputer => "this computer",
            Self::LocalNetwork => "your network",
            Self::Tailscale => "Tailscale",
        }
    }
}

/// This computer's addresses on its networks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Interfaces {
    pub local: Option<IpAddr>,
    pub tailscale: Option<IpAddr>,
}

impl Interfaces {
    /// The interfaces the system would send from: toward a documentation
    /// address (the default route) and toward Tailscale's own service
    /// address, which only a running Tailscale routes into the tailnet.
    pub fn detect() -> Self {
        Self::from_found(
            [
                Ipv4Addr::new(192, 0, 2, 1),
                Ipv4Addr::new(100, 100, 100, 100),
            ]
            .into_iter()
            .filter_map(outgoing),
        )
    }

    fn from_found(found: impl IntoIterator<Item = IpAddr>) -> Self {
        let mut interfaces = Self::default();
        for ip in found {
            if is_tailscale(ip) {
                interfaces.tailscale.get_or_insert(ip);
            } else {
                interfaces.local.get_or_insert(ip);
            }
        }
        interfaces
    }
}

fn outgoing(probe: Ipv4Addr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((probe, 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// Tailscale gives each device an address in 100.64.0.0/10.
fn is_tailscale(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(ip) if ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
}

/// The addresses other devices use for a listener, the local network first.
/// A wildcard listener is reachable on every interface; a loopback one only
/// from this computer.
pub fn addresses(listen: SocketAddr, interfaces: Interfaces) -> Vec<(Place, SocketAddr)> {
    let ip = listen.ip();
    if ip.is_loopback() {
        return vec![(Place::ThisComputer, listen)];
    }
    if !ip.is_unspecified() {
        let place = if is_tailscale(ip) {
            Place::Tailscale
        } else {
            Place::LocalNetwork
        };
        return vec![(place, listen)];
    }
    let port = listen.port();
    let mut found = Vec::new();
    if let Some(ip) = interfaces.local {
        found.push((Place::LocalNetwork, SocketAddr::new(ip, port)));
    }
    if let Some(ip) = interfaces.tailscale {
        found.push((Place::Tailscale, SocketAddr::new(ip, port)));
    }
    if found.is_empty() {
        found.push((
            Place::ThisComputer,
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
        ));
    }
    found
}

/// Shown when no Tailscale address was found: how devices in other places
/// can reach this computer without opening router ports.
pub const TAILSCALE_HINT: &str =
    "Devices in another place? Install Tailscale on this computer and on them, and its address appears here.";

/// Copies text to the clipboard through the terminal (OSC 52, which also
/// reaches the clipboard of a computer connected over SSH) and through the
/// system's own clipboard tool where it has one. Returns whether either
/// took it; a terminal without OSC 52 ignores the request.
pub fn copy(text: &str) -> bool {
    let terminal = crossterm::execute!(
        std::io::stdout(),
        crossterm::clipboard::CopyToClipboard::to_clipboard_from(text)
    )
    .is_ok();
    let system = if cfg!(windows) {
        pipe("clip", text)
    } else if cfg!(target_os = "macos") {
        pipe("pbcopy", text)
    } else {
        false
    };
    terminal || system
}

fn pipe(program: &str, text: &str) -> bool {
    let Ok(mut child) = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|status| status.success()) && written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn tailscale_addresses_are_the_cgnat_range() {
        assert!(!is_tailscale(ip("100.63.255.255")));
        assert!(is_tailscale(ip("100.64.0.0")));
        assert!(is_tailscale(ip("100.127.255.255")));
        assert!(!is_tailscale(ip("100.128.0.0")));
        assert!(!is_tailscale(ip("192.168.0.160")));
        assert!(!is_tailscale(ip("fd7a:115c:a1e0::1")));
    }

    #[test]
    fn the_found_interfaces_are_sorted_by_network() {
        let lan = ip("192.168.0.160");
        let tailnet = ip("100.101.102.103");
        // Tailscale down: both probes leave through the local network.
        assert_eq!(
            Interfaces::from_found([lan, lan]),
            Interfaces {
                local: Some(lan),
                tailscale: None
            }
        );
        assert_eq!(
            Interfaces::from_found([lan, tailnet]),
            Interfaces {
                local: Some(lan),
                tailscale: Some(tailnet)
            }
        );
        // A Tailscale exit node carries the default route too.
        assert_eq!(
            Interfaces::from_found([tailnet, tailnet]),
            Interfaces {
                local: None,
                tailscale: Some(tailnet)
            }
        );
    }

    #[test]
    fn a_listener_is_shown_at_the_addresses_devices_can_use() {
        let interfaces = Interfaces {
            local: Some(ip("192.168.0.160")),
            tailscale: Some(ip("100.101.102.103")),
        };
        let at = |listen: &str| addresses(listen.parse().unwrap(), interfaces);
        assert_eq!(
            at("0.0.0.0:3333"),
            [
                (Place::LocalNetwork, "192.168.0.160:3333".parse().unwrap()),
                (Place::Tailscale, "100.101.102.103:3333".parse().unwrap()),
            ]
        );
        assert_eq!(at("[::]:3333"), at("0.0.0.0:3333"));
        assert_eq!(
            at("127.0.0.1:3340"),
            [(Place::ThisComputer, "127.0.0.1:3340".parse().unwrap())]
        );
        assert_eq!(
            at("192.168.0.55:3338"),
            [(Place::LocalNetwork, "192.168.0.55:3338".parse().unwrap())]
        );
        assert_eq!(
            at("100.70.0.1:3338"),
            [(Place::Tailscale, "100.70.0.1:3338".parse().unwrap())]
        );
        // With no network at all, only this computer can connect.
        assert_eq!(
            addresses("0.0.0.0:3333".parse().unwrap(), Interfaces::default()),
            [(Place::ThisComputer, "127.0.0.1:3333".parse().unwrap())]
        );
    }
}
