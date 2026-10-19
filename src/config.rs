//! The installation configuration: `deli.toml`.
//!
//! The autoinstall file and the interactive installer both produce this
//! structure; nothing else drives an installation. Unknown keys are errors,
//! since a typo in an unattended install would otherwise be silently ignored.

use std::fmt;
use std::net::IpAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub storage: Storage,
    pub system: System,
    pub network: Network,
    pub admin: Option<Admin>,
    pub root: Root,
    pub ssh: Ssh,
    pub boot: Boot,
    pub repositories: Repositories,
    pub packages: Packages,
    pub live: Live,
    pub install: Install,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Storage {
    /// Target disk (`/dev/…`, `/dev/disk/by-id/…`) or `auto`: the only
    /// fixed disk besides the installation medium.
    pub disk: String,
    pub layout: Layout,
    pub filesystem: Filesystem,
    pub esp_size: String,
    /// `none` or a size such as `4G`.
    pub swap: String,
    /// `server` layout: sizes of separate `/var` and `/home` partitions.
    pub var_size: Option<String>,
    pub home_size: Option<String>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            disk: "auto".into(),
            layout: Layout::Simple,
            filesystem: Filesystem::Btrfs,
            esp_size: "1G".into(),
            swap: "none".into(),
            var_size: None,
            home_size: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Simple,
    Server,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
    Btrfs,
    Ext4,
    Xfs,
}

impl fmt::Display for Filesystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Btrfs => "btrfs",
            Self::Ext4 => "ext4",
            Self::Xfs => "xfs",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct System {
    pub hostname: String,
    pub timezone: String,
    pub locale: String,
    pub keymap: String,
}

impl Default for System {
    fn default() -> Self {
        Self {
            hostname: "exherbo".into(),
            timezone: "UTC".into(),
            locale: "en_US.UTF-8".into(),
            keymap: "us".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Network {
    /// `dhcp` or `static`.
    pub mode: NetworkMode,
    /// Interfaces matched, as networkd `Name=` globs.
    pub interfaces: String,
    /// Static mode: `192.0.2.10/24`.
    pub address: Option<String>,
    pub gateway: Option<String>,
    /// Resolvers; empty: those DHCP gives.
    pub dns: Vec<String>,
    /// `yes`, `allow-downgrade` or `no`. Exherbo builds systemd-resolved
    /// with `allow-downgrade`, which fails every query, without a word,
    /// behind a resolver that strips signatures (as some ISP boxes do): off
    /// unless asked for.
    pub dnssec: String,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Dhcp,
            interfaces: "en* eth*".into(),
            address: None,
            gateway: None,
            dns: Vec::new(),
            dnssec: "no".into(),
        }
    }
}

impl Network {
    fn check(&self, problems: &mut Problems) {
        if self.mode == NetworkMode::Static {
            match self.address.as_deref().map(parse_cidr) {
                Some(Some(_)) => {}
                _ => problems.add(
                    "network.address",
                    "static mode needs an address with a prefix, like 192.0.2.10/24",
                ),
            }
            match self.gateway.as_deref().map(str::parse::<IpAddr>) {
                Some(Ok(_)) => {}
                Some(Err(_)) => problems.add("network.gateway", "not an IP address"),
                None => problems.add("network.gateway", "static mode needs a gateway"),
            }
            if self.dns.is_empty() {
                problems.add("network.dns", "static mode needs at least one resolver");
            }
        }
        for dns in &self.dns {
            if dns.parse::<IpAddr>().is_err() {
                problems.add("network.dns", format!("\"{dns}\" is not an IP address"));
            }
        }
        if self.interfaces.trim().is_empty() || self.interfaces.contains('\n') {
            problems.add(
                "network.interfaces",
                "expected interface name globs, like \"en* eth*\"",
            );
        }
        if !["yes", "no", "allow-downgrade"].contains(&self.dnssec.as_str()) {
            problems.add(
                "network.dnssec",
                format!("\"{}\" is not yes, no or allow-downgrade", self.dnssec),
            );
        }
    }
}

/// `192.0.2.10/24`, `2001:db8::10/64`.
fn parse_cidr(text: &str) -> Option<(IpAddr, u8)> {
    let (ip, prefix) = text.split_once('/')?;
    let ip: IpAddr = ip.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    (prefix <= max).then_some((ip, prefix))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    Dhcp,
    Static,
}

/// The first account: the administrator, with sudo, unless root is given a
/// password to administer with instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admin {
    pub name: String,
    /// sudo for this account. Off, it is an ordinary one, and root needs a
    /// password: the way to administer is then `su`.
    #[serde(default = "yes")]
    pub sudo: bool,
    /// crypt(3) hash (`openssl passwd -6`). Without one, the account has no
    /// password, logs in by key only, and sudo does not ask.
    #[serde(default)]
    pub password_hash: Option<Secret>,
    #[serde(default)]
    pub authorized_keys: Vec<String>,
    /// Also install the public keys GitHub publishes for this user.
    #[serde(default)]
    pub github: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Root {
    /// Locked unless a hash is given.
    pub password_hash: Option<Secret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ssh {
    pub enable: bool,
    /// Password logins. Off by default whenever a key is configured.
    pub password_authentication: Option<bool>,
}

fn yes() -> bool {
    true
}

impl Default for Ssh {
    fn default() -> Self {
        Self {
            enable: true,
            password_authentication: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Boot {
    /// Add `console=ttyS0,115200` so the system also talks on the first
    /// serial port (IPMI serial-over-LAN, hypervisor consoles).
    pub serial_console: bool,
    pub timeout: u32,
}

impl Default for Boot {
    fn default() -> Self {
        Self {
            serial_console: true,
            timeout: 3,
        }
    }
}

/// Exherbo repositories, besides those the installed packages come from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Repositories {
    /// Repositories to add, by the name Exherbo's indexes give them, such
    /// as `net` or `perl`; those they depend on come with them. Each is cloned: this needs the network.
    pub enable: Vec<String>,
    /// Sync every repository (`cave sync`) and build paludis' metadata cache
    /// (`cave generate-metadata`) at the end of the installation: a few more
    /// minutes, and the network.
    pub sync: bool,
}

impl Repositories {
    fn check(&self, problems: &mut Problems) {
        for name in &self.enable {
            if !valid_repository_name(name) {
                problems.add(
                    "repositories.enable",
                    format!("\"{name}\" is not a repository name"),
                );
            }
        }
    }
}

/// Packages for the installed system to build, later.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Packages {
    /// Added to the world set, not built: the first `cave resolve -x world` on
    /// the installed system builds them. `category/name`, such as
    /// `sys-process/htop`, or the name alone when one category has it. No
    /// slot: `cave update-world` takes package names only.
    pub world: Vec<String>,
}

impl Packages {
    fn check(&self, problems: &mut Problems) {
        for spec in &self.world {
            if !valid_package_spec(spec) {
                problems.add(
                    "packages.world",
                    format!("\"{spec}\" is not a package like htop or sys-process/htop"),
                );
            }
        }
    }
}

/// Only used by the live session on the ISO, never by the installed system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Live {
    /// Keys allowed to SSH into the installer as root while it runs.
    pub authorized_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Install {
    /// Ask before erasing the disk. An unattended install needs either
    /// `confirm = false` here or `deli.autoinstall` on the kernel command line.
    pub confirm: bool,
    pub reboot: bool,
}

impl Default for Install {
    fn default() -> Self {
        Self {
            confirm: true,
            reboot: true,
        }
    }
}

/// A value that never appears in logs or debug output.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<secret>")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::io(format!("cannot read {}", path.display()), e))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        // toml's own message quotes the offending source line, which may hold
        // a password hash: report the position and the reason only.
        toml::from_str(text).map_err(|e| {
            let at = e.span().map_or_else(String::new, |span| {
                let line = text[..span.start.min(text.len())].lines().count().max(1);
                format!("line {line}: ")
            });
            // Only messages about keys are shown: the others quote the
            // rejected value, which may be a secret.
            let message = e.message();
            let about_keys = ["unknown field", "missing field", "duplicate key"]
                .iter()
                .any(|p| message.starts_with(p));
            let message = if about_keys {
                message
            } else {
                "invalid value (not shown: it may be a secret)"
            };
            Error::Config(format!("{at}{message}"))
        })
    }

    fn check_access(&self, problems: &mut Problems) {
        let mut console = self.root.password_hash.is_some();
        let mut remote = false;
        if let Some(admin) = &self.admin {
            if !valid_username(&admin.name) {
                problems.add(
                    "admin.name",
                    format!("\"{}\" is not a valid user name", admin.name),
                );
            }
            if admin.name == "root" {
                problems.add("admin.name", "use [root] for root");
            }
            for (i, key) in admin.authorized_keys.iter().enumerate() {
                if !crate::sshkey::is_valid(key) {
                    problems.add(
                        format!("admin.authorized_keys[{i}]"),
                        "not an OpenSSH public key (\"<type> <base64> [comment]\")",
                    );
                }
            }
            if admin.github.as_ref().is_some_and(|u| {
                !(1..=39).contains(&u.len())
                    || !u.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            }) {
                problems.add("admin.github", "not a GitHub user name");
            }
            let has_keys = admin
                .authorized_keys
                .iter()
                .any(|k| crate::sshkey::is_valid(k))
                || admin.github.is_some();
            let passwords = self.ssh.password_authentication.unwrap_or(!has_keys);
            console |= admin.password_hash.is_some();
            remote |= self.ssh.enable && (has_keys || (passwords && admin.password_hash.is_some()));
        }
        remote |= self.ssh.enable
            && self.admin.is_none()
            && self.root.password_hash.is_some()
            && self.ssh.password_authentication != Some(false);
        for (key, hash) in [
            ("root.password_hash", self.root.password_hash.as_ref()),
            (
                "admin.password_hash",
                self.admin.as_ref().and_then(|a| a.password_hash.as_ref()),
            ),
        ] {
            if hash.is_some_and(|h| !valid_crypt_hash(h.expose())) {
                problems.add(key, "expected a complete SHA-512, SHA-256, yescrypt or bcrypt hash, such as the output of `openssl passwd -6`");
            }
        }
        if self.admin.as_ref().is_some_and(|a| !a.sudo) && self.root.password_hash.is_none() {
            problems.add(
                "admin.sudo",
                "without sudo, nobody could administer this system: give root a password hash",
            );
        }
        if !console && !remote {
            problems.add("admin", "nobody could log in: give [admin] keys, a GitHub user or a password hash, or root.password_hash");
        }
    }

    /// The configuration as it is recorded in the installed system: secrets removed.
    pub fn redacted(&self) -> String {
        let mut copy = self.clone();
        copy.root.password_hash = copy.root.password_hash.map(|_| Secret("<redacted>".into()));
        if let Some(admin) = copy.admin.as_mut() {
            admin.password_hash = admin
                .password_hash
                .take()
                .map(|_| Secret("<redacted>".into()));
        }
        toml::to_string_pretty(&copy).unwrap_or_default()
    }

    /// Every problem at once, so an unattended install fails on its first
    /// read rather than on the first issue.
    pub fn validate(&self) -> Result<()> {
        let problems = self.problems();
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(
                problems.iter().map(ToString::to_string).collect(),
            ))
        }
    }

    /// What is wrong, each problem with the key it is about.
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = Problems::default();
        self.storage.check(&mut problems);
        self.system.check(&mut problems);
        self.network.check(&mut problems);
        self.repositories.check(&mut problems);
        self.packages.check(&mut problems);
        self.check_access(&mut problems);
        problems.0
    }
}

impl Storage {
    fn check(&self, problems: &mut Problems) {
        let s = self;
        if s.disk.trim().is_empty() {
            problems.add("storage.disk", "empty, use a device path or \"auto\"");
        }
        for (key, value) in [
            ("storage.esp_size", Some(&s.esp_size)),
            ("storage.var_size", s.var_size.as_ref()),
            ("storage.home_size", s.home_size.as_ref()),
        ] {
            if let Some(value) = value
                && parse_size(value).is_none()
            {
                problems.add(key, format!("\"{value}\" is not a size like 512M or 4G"));
            }
        }
        if s.swap != "none" && parse_size(&s.swap).is_none() {
            problems.add(
                "storage.swap",
                format!("\"{}\" is neither \"none\" nor a size", s.swap),
            );
        }
        if parse_size(&s.esp_size).is_some_and(|b| b < 512 << 20) {
            problems.add(
                "storage.esp_size",
                "at least 512M (it holds kernels and initramfs images, and FAT32 needs room on 4K-sector disks)",
            );
        }
        // Below these, mkfs or repart fail, after the disk has been erased.
        for (key, value, min, text) in [
            ("storage.var_size", s.var_size.as_deref(), 1u64 << 30, "1G"),
            ("storage.home_size", s.home_size.as_deref(), 1 << 30, "1G"),
            (
                "storage.swap",
                (s.swap != "none").then_some(s.swap.as_str()),
                128 << 20,
                "128M",
            ),
        ] {
            if value.and_then(parse_size).is_some_and(|b| b < min) {
                problems.add(key, format!("at least {text}"));
            }
        }
        if s.layout == Layout::Simple && (s.var_size.is_some() || s.home_size.is_some()) {
            problems.add("storage", "var_size and home_size need layout = \"server\"");
        }
    }
}

impl System {
    fn check(&self, problems: &mut Problems) {
        if !valid_hostname(&self.hostname) {
            problems.add(
                "system.hostname",
                format!("\"{}\" is not a valid host name", self.hostname),
            );
        }
        if self.timezone.is_empty()
            || self.timezone.contains("..")
            || self.timezone.starts_with('/')
        {
            problems.add(
                "system.timezone",
                format!("\"{}\" is not a zone name like Europe/Paris", self.timezone),
            );
        }
        if !self.locale.contains('.') {
            problems.add(
                "system.locale",
                format!("\"{}\" needs a charset, like en_US.UTF-8", self.locale),
            );
        }
    }
}

/// A configuration problem: the key it is about (`network.dns`, or a whole
/// section such as `admin`), and what is wrong with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

#[derive(Default)]
struct Problems(Vec<Problem>);

impl Problems {
    fn add(&mut self, key: impl Into<String>, message: impl Into<String>) {
        self.0.push(Problem {
            key: key.into(),
            message: message.into(),
        });
    }
}

const EXBIBYTE: f64 = 1_152_921_504_606_846_976.0;

/// `512M`, `4G`, `1.5G`, `4GiB`, `1T`: binary units. Bare numbers are bytes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let shift = match unit.trim().trim_end_matches("iB").trim_end_matches('B') {
        "" => 0,
        "K" | "k" => 10,
        "M" => 20,
        "G" => 30,
        "T" => 40,
        _ => return None,
    };
    #[allow(clippy::cast_precision_loss)]
    let bytes = number * (1u64 << shift) as f64;
    // Nothing deli partitions is anywhere near 1 EiB: reject rather than saturate.
    if !bytes.is_finite() || !(1.0..EXBIBYTE).contains(&bytes) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(bytes as u64)
}

/// The shape of a crypt(3) hash libxcrypt verifies: SHA-512 (`$6$`),
/// SHA-256 (`$5$`), yescrypt (`$y$`) or bcrypt (`$2b$`, `$2y$`), with every
/// field and the digest's exact length. Parameters inside a field (yescrypt's
/// cost) are not interpreted. Catches what is not a hash at all, or a
/// truncated one: it would be the only way in, and would never match.
fn valid_crypt_hash(hash: &str) -> bool {
    let b64 = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '/')
    };
    let parts: Vec<&str> = hash.split('$').collect();
    match parts.as_slice() {
        ["", id @ ("5" | "6"), rest @ ..] => {
            let digest = if *id == "6" { 86 } else { 43 };
            let rest = match rest.split_first() {
                Some((rounds, tail)) if rounds.starts_with("rounds=") => {
                    if !rounds["rounds=".len()..]
                        .parse::<u32>()
                        .is_ok_and(|r| (1000..=999_999_999).contains(&r))
                    {
                        return false;
                    }
                    tail
                }
                _ => rest,
            };
            matches!(rest, [salt, sum] if b64(salt) && salt.len() <= 16 && b64(sum) && sum.len() == digest)
        }
        ["", "y", params, salt, sum] => b64(params) && b64(salt) && b64(sum) && sum.len() == 43,
        ["", "2b" | "2y", cost, rest] => {
            cost.parse::<u8>().is_ok_and(|c| (4..=31).contains(&c))
                && cost.len() == 2
                && rest.len() == 53
                && b64(rest)
        }
        _ => false,
    }
}

fn valid_hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// What Exherbo names its repositories: `arbor`, `net`, `CleverCloud`.
fn valid_repository_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `category/name` or `name`: what `cave update-world` takes.
fn valid_package_spec(spec: &str) -> bool {
    let part = |text: &str| {
        !text.is_empty()
            && !text.starts_with('-')
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+_.-".contains(c))
    };
    match spec.split_once('/') {
        Some((category, name)) => part(category) && part(name),
        None => part(spec),
    }
}

fn valid_username(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Config {
        Config::parse(
            r#"
            [admin]
            name = "ops"
            authorized_keys = ["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0 ops@example"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn minimal_config_is_valid_with_defaults() {
        let config = minimal();
        config.validate().unwrap();
        assert_eq!(config.storage.disk, "auto");
        assert_eq!(config.storage.filesystem, Filesystem::Btrfs);
        assert!(config.install.confirm);
    }

    #[test]
    fn the_documented_configurations_are_valid() {
        let example = Config::parse(include_str!("../examples/server.toml")).unwrap();
        example.validate().unwrap();
        let readme = include_str!("../README.md");
        let snippet = readme
            .split("```toml\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .unwrap();
        Config::parse(snippet).unwrap().validate().unwrap();
    }

    #[test]
    fn the_esp_has_room_for_fat32_on_4k_disks() {
        let mut config = minimal();
        config.storage.esp_size = "300M".into();
        assert!(config.validate().is_err());
        config.storage.esp_size = "512M".into();
        config.validate().unwrap();
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::parse("[storage]\ndisck = \"/dev/sda\"\n").unwrap_err();
        assert!(err.to_string().contains("disck"), "{err}");
    }

    #[test]
    fn all_problems_reported_together() {
        let config = Config::parse(
            r#"
            [storage]
            swap = "lots"
            var_size = "8G"
            [system]
            hostname = "-bad-"
            [network]
            mode = "static"
            "#,
        )
        .unwrap();
        let Err(Error::Invalid(problems)) = config.validate() else {
            panic!("expected invalid")
        };
        let text = problems.join("\n");
        for needle in [
            "storage.swap",
            "layout = \"server\"",
            "hostname",
            "network.address",
            "network.gateway",
            "nobody could log in",
        ] {
            assert!(text.contains(needle), "missing {needle} in:\n{text}");
        }
    }

    #[test]
    fn package_specs() {
        for good in [
            "sys-process/htop",
            "htop",
            "x11-libs/gtk+",
            "dev-libs/libxml2",
        ] {
            assert!(valid_package_spec(good), "{good}");
        }
        for bad in [
            "",
            "app-editors/vim:0",
            "sys-process/",
            "/htop",
            "a/b/c",
            "a/b:",
            "-a/b",
            "a/b c",
        ] {
            assert!(!valid_package_spec(bad), "{bad}");
        }
    }

    #[test]
    fn plain_password_is_refused() {
        let mut config = minimal();
        config.root.password_hash = Some(Secret("hunter2".into()));
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("SHA-512")
        );
    }

    #[test]
    fn redaction_hides_hashes() {
        let mut config = minimal();
        config.root.password_hash = Some(Secret("$6$salt$hash".into()));
        let text = config.redacted();
        assert!(!text.contains("$6$salt$hash"));
        assert!(text.contains("<redacted>"));
        assert_eq!(format!("{:?}", config.root.password_hash), "Some(<secret>)");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("512M"), Some(512 << 20));
        assert_eq!(parse_size("1G"), Some(1 << 30));
        assert_eq!(parse_size("4GiB"), Some(4 << 30));
        assert_eq!(parse_size("1.5G"), Some(3 << 29));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("lots"), None);
        assert_eq!(parse_size("0G"), None);
    }

    #[test]
    fn unusable_access_and_network_are_refused() {
        let config = Config::parse(
            r#"
            [admin]
            name = "ops"
            authorized_keys = ["not-an-ssh-key", "ssh-ed25519 AAAAC3NzaC1yc2EAAAADAQABAAABAQ comment"]
            [network]
            mode = "static"
            address = "garbage/999"
            gateway = "oops"
            dns = ["oops"]
            [repositories]
            enable = ["net", "../etc"]
            [packages]
            world = ["sys-process/htop", "htop", "app-editors/vim:0", "a/b c", "a/"]
            "#,
        )
        .unwrap();
        let text = config.validate().unwrap_err().to_string();
        for needle in [
            "authorized_keys[0]",
            "authorized_keys[1]",
            "network.address",
            "network.gateway",
            "\"oops\" is not an IP",
            "\"../etc\" is not a repository name",
            "\"app-editors/vim:0\" is not a package",
            "\"a/b c\" is not a package",
            "\"a/\" is not a package",
        ] {
            assert!(text.contains(needle), "missing {needle} in:\n{text}");
        }
        assert!(text.contains("nobody could log in"), "{text}");
    }

    #[test]
    fn parse_errors_do_not_quote_the_source() {
        let err = Config::parse("[root]\npassword_hash = [\"$6$SYNTHETIC_SECRET\"]\n")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SYNTHETIC_SECRET"), "{err}");
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn type_errors_do_not_quote_the_value() {
        let err = Config::parse("[install]\nconfirm = \"$6$SYNTHETIC_SECRET\"\n")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SYNTHETIC_SECRET"), "{err}");
    }

    #[test]
    fn rejected_values_are_never_echoed() {
        for text in [
            "[storage]\nfilesystem = \"$6$SYNTHETIC_SECRET\"\n",
            "[install]\nconfirm = \"before\\\"$6$SYNTHETIC_SECRET\"\n",
        ] {
            let err = Config::parse(text).unwrap_err().to_string();
            assert!(!err.contains("SYNTHETIC_SECRET"), "{err}");
        }
    }

    #[test]
    fn hashes_cannot_smuggle_chpasswd_lines() {
        let mut config = minimal();
        config.root.password_hash = Some(Secret::new("$6$x$y\nops:$6$evil"));
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("root.password_hash")
        );
        for good in [
            "$6$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0",
            "$5$testsalt$iVAGM2YiokR0DpzgivIlU0TMv7iA4MDfErDE0SW2rz1",
            "$6$rounds=5000$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0",
        ] {
            config.root.password_hash = Some(Secret::new(good));
            config.validate().unwrap();
        }
        for bad in [
            "$not-a-hash",
            "$6$testsalt$tooshort",
            "$1$testsalt$abcdefghijklmnopqrstuv",
        ] {
            config.root.password_hash = Some(Secret::new(bad));
            assert!(config.validate().is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn partitions_below_filesystem_minimums_are_refused() {
        let mut config = minimal();
        config.storage.layout = Layout::Server;
        config.storage.home_size = Some("1".into());
        config.storage.swap = "1M".into();
        let text = config.validate().unwrap_err().to_string();
        assert!(text.contains("storage.home_size: at least 1G"), "{text}");
        assert!(text.contains("storage.swap: at least 128M"), "{text}");
    }

    #[test]
    fn absurd_sizes_are_rejected() {
        assert_eq!(parse_size("999999999999999999999999T"), None);
        assert_eq!(parse_size("inf"), None);
        assert_eq!(parse_size("0.1"), None);
    }

    #[test]
    fn without_sudo_root_needs_a_password() {
        let mut config = minimal();
        config.admin.as_mut().unwrap().sudo = false;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("admin.sudo")
        );
        config.root.password_hash = Some(Secret::new(
            "$6$testsalt$tJbUl1kXqW33QAR3uSZ526jhi2VR/8b5Oc.fgGcuj1amRP1gtYnGoqbDwnND9jnHaR.tZ1.Uag0nWYDafTUxX0",
        ));
        config.validate().unwrap();
        assert!(minimal().admin.unwrap().sudo, "sudo by default");
    }

    #[test]
    fn ssh_disabled_with_keys_only_locks_everyone_out() {
        let mut config = minimal();
        config.ssh.enable = false;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("nobody could log in")
        );
    }
}
