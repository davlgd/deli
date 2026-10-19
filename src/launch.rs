//! `deli install`: find the configuration and install it, or ask with the
//! interactive installer.
//!
//! The configuration comes from, in order:
//! 1. `--config <file>`;
//! 2. `deli.toml` in the current directory;
//! 3. in the live session only: `deli.autoinstall=<url>` on the kernel
//!    command line, then `deli.toml` at the root of a filesystem labelled
//!    `CIDATA` or `DELICFG` (a second ISO, a USB key, a Proxmox cloud-init
//!    drive).
//!
//! Without one, the interactive installer asks. The disk is erased without
//! asking only with `--yes`, when the kernel command line has
//! `deli.autoinstall`, or when the file itself says `install.confirm = false`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::error::{Error, ExitStatus, Result};
use crate::install::{self, Options};
use crate::report::Reporter;
use crate::system::probe::{self, cmdline_arg, lsblk_column};
use crate::system::{Command, RealSys, Sys};

const MEDIA: &str = "/run/deli/media";
const CONFIG_LABELS: [&str; 3] = ["CIDATA", "cidata", "DELICFG"];
/// The live session's log: the end-to-end test follows it.
const LIVE_LOG: &str = "/run/deli/live.log";
/// root's password in the live session, for SSH: the interactive installer
/// shows it.
pub const SSH_PASSWORD: &str = "/run/deli/ssh-password";

/// What `deli install` was asked.
pub struct Request {
    pub config: Option<PathBuf>,
    pub dry_run: bool,
    /// Do not ask before erasing the disk.
    pub yes: bool,
    /// Stay in the live session when done.
    pub no_reboot: bool,
    /// Root filesystem to copy: another than the live session's is for
    /// dry runs only (preflight refuses it).
    pub source: String,
}

pub fn install(request: &Request) -> Result<()> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    // The ISO's live session; never touched by a dry run.
    let live = cmdline_arg(&cmdline, "deli.live").is_some() && !request.dry_run;
    let mut report = Reporter::default();
    if live {
        report.tee(LIVE_LOG);
        if Path::new("/dev/ttyS0").exists() {
            report.tee("/dev/ttyS0");
        }
    }
    let result = find(request, live, &cmdline).and_then(|found| match found {
        Some((origin, text)) => from_file(request, live, &cmdline, &origin, &text, &mut report),
        None => interactive(request, live, &mut report),
    });
    if let Err(e) = &result {
        // On the screen from main; in the log and on the serial console
        // from here.
        report.record(&format!("deli: {e}"));
    }
    result
}

/// Where the configuration is, and what it says.
fn find(request: &Request, live: bool, cmdline: &str) -> Result<Option<(String, String)>> {
    let read = |path: &Path| {
        std::fs::read_to_string(path)
            .map(|text| Some((path.display().to_string(), text)))
            .map_err(|e| Error::io(format!("cannot read {}", path.display()), e))
    };
    if let Some(path) = &request.config {
        return read(path);
    }
    let here = Path::new("deli.toml");
    if here.is_file() {
        return read(here);
    }
    if !live {
        return Ok(None);
    }
    let sys = RealSys::default();
    match cmdline_arg(cmdline, "deli.autoinstall") {
        Some(url) if !url.is_empty() => Ok(Some((shown(url), fetch(url, &sys)?))),
        _ => from_media(&sys),
    }
}

/// The configuration at `url`. Errors name it without what may be a
/// secret, credentials or a token in the query: neither curl's command line
/// nor its messages, which may quote the URL, are shown.
fn fetch(url: &str, sys: &RealSys) -> Result<String> {
    // The network may still be coming up.
    let mut status = ExitStatus::Signal;
    for _ in 0..12 {
        match sys.capture(&Command::new("curl").args([
            "--fail",
            "--silent",
            "--globoff",
            "--location",
            "--max-time",
            "20",
            url,
        ])) {
            Ok(text) => return Ok(text),
            Err(Error::Command { status: s, .. }) => status = s,
            Err(e) => return Err(e),
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    Err(fetch_error(url, status))
}

fn fetch_error(url: &str, status: ExitStatus) -> Error {
    Error::Other(format!("cannot fetch {} (curl: {status})", shown(url)))
}

/// `url` without its credentials, query and fragment.
fn shown(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or_default();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, None), |(authority, path)| (authority, Some(path)));
    let host = authority.rsplit('@').next().unwrap_or_default();
    match path {
        Some(path) => format!("{scheme}://{host}/{path}"),
        None => format!("{scheme}://{host}"),
    }
}

fn from_media(sys: &RealSys) -> Result<Option<(String, String)>> {
    for (device, label) in lsblk_column(sys, "LABEL", None)? {
        if !CONFIG_LABELS.contains(&label.as_str()) {
            continue;
        }
        sys.mkdir_p(MEDIA, 0o755)?;
        if sys
            .run(&Command::new("mount").args(["--read-only", &device, MEDIA]))
            .is_err()
        {
            continue;
        }
        let text = std::fs::read_to_string(Path::new(MEDIA).join("deli.toml"));
        sys.run(&Command::new("umount").arg(MEDIA))?;
        if let Ok(text) = text {
            return Ok(Some((format!("{device} ({label})"), text)));
        }
    }
    Ok(None)
}

/// The fingerprint recorded by a previous deli installation on `disk`, if any.
fn installed_fingerprint(sys: &RealSys, disk: &str) -> Option<String> {
    const PEEK: &str = "/run/deli/peek";
    let (root, _) = probe::partition_names(sys, disk)
        .ok()?
        .into_iter()
        .find(|(_, label)| label == "root")?;
    sys.mkdir_p(PEEK, 0o700).ok()?;
    sys.run(&Command::new("mount").args(["--read-only", &root, PEEK]))
        .ok()?;
    // btrfs: the top level holds the @ subvolume; others: the root itself.
    let found = ["@", ""].iter().find_map(|sub| {
        std::fs::read_to_string(
            Path::new(PEEK)
                .join(sub)
                .join(install::FINGERPRINT.trim_start_matches('/')),
        )
        .ok()
    });
    let _ = sys.run(&Command::new("umount").arg(PEEK));
    found.map(|f| f.trim().to_owned())
}

/// Install from a configuration file.
fn from_file(
    request: &Request,
    live: bool,
    cmdline: &str,
    origin: &str,
    text: &str,
    report: &mut Reporter,
) -> Result<()> {
    report.line(&format!("deli: configuration from {origin}"));
    let config = Config::parse(text)?;
    let facts = install::facts_for(&config, request.dry_run)?;
    // Nothing happens, not even SSH into the live session, for a
    // configuration that would not install.
    let plan = install::plan(&config, &facts)?;
    let sys = RealSys::default();
    if live {
        open_ssh(&config, &sys, report)?;
    }
    let autoinstall = cmdline_arg(cmdline, "deli.autoinstall").is_some();
    let unattended = request.yes || autoinstall || !config.install.confirm;
    // The firmware booted the installer again after an unattended install
    // (it still prefers the medium): do not erase what was just installed.
    if live && unattended && cmdline_arg(cmdline, "deli.reinstall").is_none() {
        let fingerprint = install::fingerprint(&sys, &config)?;
        if installed_fingerprint(&sys, &plan.disk).as_deref() == Some(fingerprint.as_str()) {
            report.line(&format!(
                "deli: {} already holds the system this configuration describes; not installing it again.",
                plan.disk
            ));
            report.line("Boot from the disk (remove the installation medium), or add deli.reinstall to the kernel command line.");
            return Ok(());
        }
    }
    let options = Options {
        dry_run: request.dry_run,
        yes: unattended,
        source: request.source.clone(),
        quiet: false,
    };
    install::run(&config, &facts, &options, report)?;
    if config.install.reboot {
        reboot(request, report)?;
    }
    Ok(())
}

/// Into the installed system, unless asked not to; never under an
/// installation another session runs.
fn reboot(request: &Request, report: &mut Reporter) -> Result<()> {
    if request.dry_run {
        return Ok(());
    }
    if request.no_reboot {
        report.line("not rebooting (--no-reboot)");
        return Ok(());
    }
    let _lock = install::lock()?;
    let sys = RealSys::default();
    // bootctl put the installed system first in the firmware's boot order,
    // which a hypervisor's own boot order may override. Ejecting usually
    // fails (the live root is read from the disc); if the firmware still
    // boots the medium, the fingerprint check stops a second unattended
    // installation.
    if let Some(medium) = probe::boot_disk().filter(|d| d.starts_with("/dev/sr")) {
        let _ = sys.run(&Command::new("eject").arg(&medium));
    }
    report.line("rebooting into the installed system (remove the installation medium)");
    sys.write(install::REBOOTING, "", 0o600)?;
    let rebooting = sys.run(&Command::new("systemctl").arg("reboot"));
    if rebooting.is_err() {
        let _ = sys.remove(install::REBOOTING);
    }
    rebooting
}

/// Ask with the interactive installer; in the live session, over SSH too.
fn interactive(request: &Request, live: bool, report: &mut Reporter) -> Result<()> {
    if live {
        report.line("deli: no configuration found (deli.autoinstall=<url>, or deli.toml on a CIDATA volume): the interactive installer asks.");
        if let Err(e) = open_ssh_with_password(&RealSys::default()) {
            report.line(&format!("deli: no SSH into this session: {e}"));
        }
    }
    match crate::wizard::run(&request.source, request.dry_run, report)? {
        crate::wizard::Ending::Rebooting => reboot(request, report),
        crate::wizard::Ending::Shell => {
            report.line("Installer closed: `deli install` starts it again.");
            Ok(())
        }
    }
}

/// Let the configured keys SSH into the live session, for watching or
/// debugging an installation.
fn open_ssh(config: &Config, sys: &RealSys, report: &mut Reporter) -> Result<()> {
    if config.live.authorized_keys.is_empty() {
        return Ok(());
    }
    sys.mkdir_p("/root/.ssh", 0o700)?;
    sys.write(
        "/root/.ssh/authorized_keys",
        &(config.live.authorized_keys.join("\n") + "\n"),
        0o600,
    )?;
    sys.run(&Command::new("systemctl").args(["start", "sshd.service"]))?;
    let addresses = sys.capture(
        &Command::new("ip").args(["-brief", "-4", "address", "show", "scope", "global"]),
    )?;
    report.line(&format!(
        "live session reachable over SSH as root: {}",
        addresses
            .split_whitespace()
            .filter(|w| w.contains('/'))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    Ok(())
}

/// Let anyone who reads the console SSH into the live session as root,
/// with a password made for this boot, kept if the installer starts again:
/// the interactive installer shows it, and starts on its own for them.
fn open_ssh_with_password(sys: &RealSys) -> Result<()> {
    if !Path::new(SSH_PASSWORD).exists() {
        let mut random = [0; 64];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random))
            .map_err(|e| Error::io("cannot read /dev/urandom", e))?;
        let password = password(&random)
            .ok_or_else(|| Error::Other("not enough randomness for a password".into()))?;
        sys.run(&Command::new("chpasswd").stdin(format!("root:{password}\n")))?;
        sys.write(SSH_PASSWORD, &password, 0o600)?;
    }
    sys.run(&Command::new("systemctl").args(["start", "sshd.service"]))
}

/// Twelve characters from `random`, none mistaken for another when read
/// off a screen (no 0/o, 1/l/i), in groups of four: about 59 bits.
fn password(random: &[u8]) -> Option<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let fair = 256 - 256 % ALPHABET.len();
    let chars: Vec<char> = random
        .iter()
        .map(|&b| usize::from(b))
        // Bytes past the last whole run of the alphabet would favour its
        // first letters.
        .filter(|&b| b < fair)
        .map(|b| char::from(ALPHABET[b % ALPHABET.len()]))
        .take(12)
        .collect();
    (chars.len() == 12).then(|| {
        chars
            .chunks(4)
            .map(|group| group.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_fetch_tells_no_secret() {
        let error = fetch_error(
            "https://ops:SYNTHETIC_SECRET@example.com/deli.toml?token=SYNTHETIC_TOKEN",
            ExitStatus::Code(22),
        )
        .to_string();
        assert_eq!(
            error,
            "cannot fetch https://example.com/deli.toml (curl: exit code 22)"
        );
    }

    #[test]
    fn urls_are_shown_without_secrets() {
        assert_eq!(
            shown("https://ops:s3cret@example.com/deli.toml?token=abc#x"),
            "https://example.com/deli.toml"
        );
        assert_eq!(shown("http://example.com"), "http://example.com");
    }

    #[test]
    fn passwords_read_well_off_a_screen() {
        let random: Vec<u8> = (0..=255).collect();
        assert_eq!(password(&random).as_deref(), Some("abcd-efgh-jkmn"));
        assert!(password(&[250; 64]).is_none(), "only unfair bytes");
    }
}
