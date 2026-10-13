//! LAN server discovery (spec 8): a UDP broadcast of
//! `Who is JellyfinServer?` to port 7359; each server answers with a small
//! JSON object naming itself and its address.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use serde::Deserialize;

pub const PORT: u16 = 7359;
pub const MESSAGE: &[u8] = b"Who is JellyfinServer?";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredServer {
    pub id: String,
    pub name: String,
    /// Base URL the server advertises, e.g. `http://192.168.1.5:8096`.
    pub address: String,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(rename = "Id", default)]
    id: Option<String>,
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "Address", default)]
    address: Option<String>,
}

/// Parses one reply; None for anything that isn't a usable answer.
pub fn parse_reply(bytes: &[u8]) -> Option<DiscoveredServer> {
    let r: Reply = serde_json::from_slice(bytes).ok()?;
    let address = r.address.filter(|a| !a.is_empty())?;
    Some(DiscoveredServer {
        id: r.id.unwrap_or_default(),
        name: r.name.unwrap_or_else(|| address.clone()),
        address,
    })
}

/// Broadcasts and collects answers for `wait`. Blocking: call from a
/// worker thread.
pub fn discover(wait: Duration) -> io::Result<Vec<DiscoveredServer>> {
    discover_to(SocketAddr::from((Ipv4Addr::BROADCAST, PORT)), wait)
}

/// `discover`, to a chosen address (tests use a local fake server).
pub fn discover_to(target: SocketAddr, wait: Duration) -> io::Result<Vec<DiscoveredServer>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_broadcast(true)?;
    sock.send_to(MESSAGE, target)?;
    let deadline = Instant::now() + wait;
    let mut found: Vec<DiscoveredServer> = Vec::new();
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        sock.set_read_timeout(Some(left))?;
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => match parse_reply(&buf[..n]) {
                Some(s) if !found.iter().any(|f| f.id == s.id && f.address == s.address) => {
                    tracing::info!(name = s.name, address = s.address, %from, "found Jellyfin server");
                    found.push(s);
                }
                Some(_) => {}
                None => tracing::debug!(%from, "ignoring non-Jellyfin discovery reply"),
            },
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_reply() {
        let s = parse_reply(
            br#"{"Address":"http://192.168.1.5:8096","Id":"f00d","Name":"Basement","EndpointAddress":null}"#,
        )
        .unwrap();
        assert_eq!(s.address, "http://192.168.1.5:8096");
        assert_eq!(s.name, "Basement");
        assert_eq!(parse_reply(b"not json"), None);
        assert_eq!(parse_reply(br#"{"Id":"x"}"#), None);
    }

    #[test]
    fn discovers_a_local_fake_server() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (n, from) = server.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], MESSAGE);
            let reply = br#"{"Address":"http://127.0.0.1:8096","Id":"a1","Name":"Fake"}"#;
            server.send_to(reply, from).unwrap();
            server.send_to(reply, from).unwrap(); // duplicates are dropped
        });
        let found = discover_to(addr, Duration::from_millis(300)).unwrap();
        t.join().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Fake");
    }
}
