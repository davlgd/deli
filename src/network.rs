//! The network configuration as systemd reads it, for the installed system
//! and for the live session, which the interactive installer switches to
//! the same settings so it can check them and fetch what the installation
//! needs.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::config::{Network, NetworkMode};
use crate::error::{Error, Result};
use crate::system::{Command, Sys};

/// Where Exherbo's repositories are synced from.
pub const EXHERBO_GIT: &str = "https://gitlab.exherbo.org";
/// Where the administrator's GitHub keys are fetched from.
pub const GITHUB: &str = "https://github.com";

/// In the live session: before its own DHCP unit (`80-deli-live.network`),
/// so it wins, and after its resolver drop-in, so it wins too.
const LIVE_UNIT: &str = "/run/systemd/network/10-deli.network";
const LIVE_RESOLVED: &str = "/run/systemd/resolved.conf.d/90-deli.conf";

/// The networkd unit.
pub fn unit(net: &Network) -> String {
    unit_keeping(net, None)
}

/// The networkd unit, with `kept` too when it sets another static address:
/// the one an SSH connection to the live session goes to, which the switch
/// would otherwise end. On the same network only: another subnet or gateway
/// takes the route to the client away, and the session ends anyway.
fn unit_keeping(net: &Network, kept: Option<IpAddr>) -> String {
    let mut text = format!(
        "# Written by deli.\n[Match]\nName={}\n\n[Network]\n",
        net.interfaces
    );
    match net.mode {
        NetworkMode::Dhcp => text.push_str("DHCP=yes\n"),
        NetworkMode::Static => {
            let address = net.address.as_deref().unwrap_or_default();
            writeln!(text, "Address={address}").unwrap();
            let configured = address.split('/').next().and_then(|ip| ip.parse().ok());
            if let Some(kept) = kept.filter(|&ip| Some(ip) != configured) {
                let prefix = if kept.is_ipv4() { 32 } else { 128 };
                writeln!(text, "Address={kept}/{prefix}").unwrap();
            }
            writeln!(
                text,
                "Gateway={}",
                net.gateway.as_deref().unwrap_or_default()
            )
            .unwrap();
        }
    }
    for dns in &net.dns {
        writeln!(text, "DNS={dns}").unwrap();
    }
    if net.mode == NetworkMode::Dhcp {
        // networkd identifies itself with a DUID by default: the DHCP server
        // sees another client than the live session and hands out another IP.
        text.push_str("\n[DHCPv4]\nClientIdentifier=mac\n");
        if !net.dns.is_empty() {
            text.push_str("UseDNS=no\n");
        }
    }
    // Router advertisements bring resolvers too, and may start DHCPv6,
    // static address or not: the ones given are the only ones.
    if !net.dns.is_empty() {
        text.push_str("\n[DHCPv6]\nUseDNS=no\n\n[IPv6AcceptRA]\nUseDNS=no\n");
    }
    text
}

/// The systemd-resolved drop-in.
pub fn resolved(net: &Network) -> String {
    let mut text = format!("# Written by deli.\n[Resolve]\nDNSSEC={}\n", net.dnssec);
    if !net.dns.is_empty() {
        writeln!(text, "DNS={}", net.dns.join(" ")).unwrap();
    }
    text
}

/// Switch this live session to `net`, keeping the address this SSH
/// connection goes to, if any. networkd applies a changed unit on reload,
/// keeping its DHCP lease: reconfiguring would release it, and the server
/// may hand out another address. Commands are captured: nothing they say
/// reaches the screen.
pub fn apply_to_live(sys: &dyn Sys, net: &Network) -> Result<()> {
    let kept = std::env::var("SSH_CONNECTION")
        .ok()
        .as_deref()
        .and_then(ssh_server);
    sys.write(LIVE_UNIT, &unit_keeping(net, kept), 0o644)?;
    sys.write(LIVE_RESOLVED, &resolved(net), 0o644)?;
    sys.capture(&Command::new("networkctl").arg("reload"))?;
    sys.capture(&Command::new("systemctl").args(["restart", "systemd-resolved.service"]))
        .map(drop)
}

/// The address an SSH connection reached, from `SSH_CONNECTION`: client
/// address and port, then server address and port.
fn ssh_server(connection: &str) -> Option<IpAddr> {
    connection.split_whitespace().nth(2)?.parse().ok()
}

/// Whether `url` answers, whatever it answers.
pub fn reachable(sys: &dyn Sys, url: &str) -> Result<()> {
    sys.capture(&Command::new("curl").args([
        "--silent",
        "--show-error",
        "--head",
        "--output",
        "/dev/null",
        "--max-time",
        "8",
        url,
    ]))
    .map(drop)
}

/// Whether this session reaches the Internet: Exherbo's git or GitHub,
/// tried for `patience` while an address comes. Why not, in curl's words.
pub fn online(sys: &dyn Sys, patience: Duration) -> std::result::Result<(), String> {
    let start = Instant::now();
    loop {
        let mut why = String::new();
        for url in [EXHERBO_GIT, GITHUB] {
            match reachable(sys, url) {
                Ok(()) => return Ok(()),
                Err(Error::Command { stderr, .. }) if why.is_empty() => {
                    stderr.trim().clone_into(&mut why);
                }
                Err(e) if why.is_empty() => why = e.to_string(),
                Err(_) => {}
            }
        }
        if start.elapsed() >= patience {
            return Err(why);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::DrySys;

    #[test]
    fn dhcp_uses_mac_client_id() {
        let text = unit(&Network::default());
        assert!(text.contains("DHCP=yes"));
        assert!(text.contains("ClientIdentifier=mac"));
        // DHCP's resolvers, unless some are configured.
        assert!(!text.contains("DNS="));
        assert!(!text.contains("UseDNS=no"));
    }

    #[test]
    fn static_addresses_and_resolvers() {
        let net = Network {
            mode: NetworkMode::Static,
            address: Some("192.0.2.10/24".into()),
            gateway: Some("192.0.2.1".into()),
            dns: vec!["192.0.2.53".into()],
            dnssec: "yes".into(),
            ..Network::default()
        };
        let text = unit(&net);
        assert!(text.contains("Address=192.0.2.10/24\nGateway=192.0.2.1\nDNS=192.0.2.53\n"));
        assert!(!text.contains("DHCP="));
        // A section's lines, up to the next section.
        let section = |text: &str, name: &str| -> String {
            text.split("\n[")
                .find(|s| s.starts_with(name))
                .unwrap_or_else(|| panic!("no [{name}] in {text}"))
                .to_owned()
        };
        for name in ["DHCPv6]", "IPv6AcceptRA]"] {
            assert!(section(&text, name).contains("UseDNS=no"), "{text}");
        }
        let dhcp = unit(&Network {
            dns: vec!["192.0.2.53".into()],
            ..Network::default()
        });
        for name in ["DHCPv4]", "DHCPv6]", "IPv6AcceptRA]"] {
            assert!(section(&dhcp, name).contains("UseDNS=no"), "{dhcp}");
        }
        assert_eq!(
            resolved(&net),
            "# Written by deli.\n[Resolve]\nDNSSEC=yes\nDNS=192.0.2.53\n"
        );
    }

    #[test]
    fn the_ssh_address_survives_a_static_switch() {
        let net = Network {
            mode: NetworkMode::Static,
            address: Some("192.0.2.10/24".into()),
            gateway: Some("192.0.2.1".into()),
            ..Network::default()
        };
        let kept = ssh_server("192.0.2.99 50000 192.0.2.145 22");
        assert_eq!(kept, Some("192.0.2.145".parse().unwrap()));
        let text = unit_keeping(&net, kept);
        assert!(
            text.contains("Address=192.0.2.10/24\nAddress=192.0.2.145/32\n"),
            "{text}"
        );
        // Already the address, or DHCP: nothing more.
        let same = ssh_server("192.0.2.99 50000 192.0.2.10 22");
        assert_eq!(unit_keeping(&net, same), unit(&net));
        assert_eq!(
            unit_keeping(&Network::default(), kept),
            unit(&Network::default())
        );
        assert_eq!(ssh_server(""), None);
    }

    #[test]
    fn offline_says_why() {
        let why = online(&DrySys::failing_on("curl"), Duration::ZERO).unwrap_err();
        assert!(why.contains("curl"), "{why}");
        assert_eq!(online(&DrySys::default(), Duration::ZERO), Ok(()));
    }
}
