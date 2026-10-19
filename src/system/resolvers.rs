//! Whether this network's resolvers pass DNSSEC on. systemd-resolved's
//! `allow-downgrade` fails every lookup behind a resolver that answers
//! DNSSEC queries but strips the signatures, as some ISP boxes do; asking
//! each resolver for the root zone's SOA, always signed, with the DNSSEC OK
//! bit tells them apart: a resolver that passes DNSSEC on returns its RRSIG.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::Duration;

/// The resolvers systemd-resolved forwards to.
const RESOLV_CONF: &str = "/run/systemd/resolve/resolv.conf";
const TIMEOUT: Duration = Duration::from_secs(2);
const RRSIG: u16 = 46;

/// Every resolver of this session returns signatures; false without any.
pub fn pass_dnssec() -> bool {
    let servers = servers(&std::fs::read_to_string(RESOLV_CONF).unwrap_or_default());
    !servers.is_empty() && servers.into_iter().all(signs)
}

fn servers(resolv_conf: &str) -> Vec<IpAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| line.strip_prefix("nameserver"))
        .filter_map(|address| address.trim().parse().ok())
        .filter(|address: &IpAddr| !address.is_loopback())
        .collect()
}

/// The root zone's SOA from `server`, with its signature.
fn signs(server: IpAddr) -> bool {
    let any: IpAddr = match server {
        IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let id = u16::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() % 65_536),
    )
    .unwrap_or(0);
    let Ok(socket) = UdpSocket::bind(SocketAddr::new(any, 0)) else {
        return false;
    };
    let mut answer = [0; 1232];
    socket.set_read_timeout(Some(TIMEOUT)).is_ok()
        && socket.send_to(&query(id), (server, 53)).is_ok()
        && socket
            .recv(&mut answer)
            .is_ok_and(|n| signed(&answer[..n], id) == Some(true))
}

fn query(id: u16) -> Vec<u8> {
    let mut query = id.to_be_bytes().to_vec();
    // Recursion desired; one question, one additional record.
    query.extend([0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1]);
    // ".", SOA, IN.
    query.extend([0, 0, 6, 0, 1]);
    // OPT: 1232-byte answers, DNSSEC OK.
    query.extend([0, 0, 41, 0x04, 0xd0, 0, 0, 0x80, 0, 0, 0]);
    query
}

/// Whether `answer`, to the query `id`, holds a signature; `None` when it
/// is not a complete answer to it.
fn signed(answer: &[u8], id: u16) -> Option<bool> {
    let word = |at: usize| Some(u16::from_be_bytes([*answer.get(at)?, *answer.get(at + 1)?]));
    let flags = word(2)?;
    // Not ours, truncated, or an error.
    if word(0)? != id || flags & 0x0200 != 0 || flags & 0x000f != 0 {
        return None;
    }
    let mut at = 12;
    for _ in 0..word(4)? {
        at = skip_name(answer, at)? + 4;
    }
    for _ in 0..word(6)? {
        at = skip_name(answer, at)?;
        if word(at)? == RRSIG {
            return Some(true);
        }
        at += 10 + usize::from(word(at + 8)?);
    }
    Some(false)
}

fn skip_name(answer: &[u8], mut at: usize) -> Option<usize> {
    loop {
        match *answer.get(at)? {
            0 => return Some(at + 1),
            pointer if pointer & 0xc0 == 0xc0 => return Some(at + 2),
            label => at += 1 + usize::from(label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer to `query(7)`: the SOA, and its RRSIG when `signature`.
    fn answer(signature: bool) -> Vec<u8> {
        let mut answer = vec![
            0,
            7,
            0x81,
            0x80,
            0,
            1,
            0,
            1 + u8::from(signature),
            0,
            0,
            0,
            0,
        ];
        answer.extend([0, 0, 6, 0, 1]);
        answer.extend([0, 0, 6, 0, 1, 0, 0, 0, 60, 0, 2, 0xab, 0xcd]);
        if signature {
            answer.extend([0xc0, 12, 0, 46, 0, 1, 0, 0, 0, 60, 0, 1, 0xef]);
        }
        answer
    }

    #[test]
    fn a_signature_in_the_answer_is_found() {
        assert_eq!(signed(&answer(true), 7), Some(true));
        assert_eq!(signed(&answer(false), 7), Some(false));
        assert_eq!(signed(&answer(true), 8), None, "another query's answer");
        assert_eq!(signed(&answer(true)[..20], 7), None, "cut short");
        let mut truncated = answer(true);
        truncated[2] |= 0x02;
        assert_eq!(signed(&truncated, 7), None);
    }

    #[test]
    fn the_query_asks_for_dnssec() {
        let query = query(7);
        assert_eq!(&query[..2], [0, 7]);
        assert_eq!(query[query.len() - 4], 0x80, "DNSSEC OK");
    }

    #[test]
    fn resolvers_are_read_from_resolv_conf() {
        let conf = "# generated\nnameserver 192.0.2.53\nnameserver 2001:db8::53\nnameserver 127.0.0.53\nsearch example.com\n";
        assert_eq!(
            servers(conf),
            [
                "192.0.2.53".parse::<IpAddr>().unwrap(),
                "2001:db8::53".parse().unwrap()
            ]
        );
    }
}
