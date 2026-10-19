//! Put the system on the disk and give it its identity.

use std::fmt::Write as _;

use super::disk::mount_table;
use super::{Context, Step};
use crate::config::NetworkMode;
use crate::error::{Error, Result};
use crate::network;
use crate::storage::Format;
use crate::system::{Command, Sys, TARGET, target_path};

/// Files the ISO adds to the live system, one path per line, removed from
/// the installed copy.
pub const LIVE_MANIFEST: &str = "/usr/share/deli/live-manifest";

pub struct Copy;

impl Step for Copy {
    fn name(&self) -> &'static str {
        "copying the system"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        // -a keeps ownership, modes, links, ACLs, xattrs and file capabilities.
        ctx.sys.run(&Command::new("cp").args([
            "-a",
            &format!("{}/.", ctx.source),
            &format!("{TARGET}/"),
        ]))
    }
}

pub struct Purge;

impl Step for Purge {
    fn name(&self) -> &'static str {
        "removing live-session files"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let manifest = target_path(LIVE_MANIFEST);
        // Checked by preflight: without it, live files would stay.
        let listed = if ctx.sys.is_real() {
            std::fs::read_to_string(&manifest)
                .map_err(|e| Error::io(format!("cannot read {manifest}"), e))?
        } else {
            String::new()
        };
        let mut count = 0;
        for path in listed.lines().map(str::trim).filter(|l| l.starts_with('/')) {
            ctx.sys.remove(&target_path(path))?;
            count += 1;
        }
        // Identity of the live session: the installed system gets its own.
        for path in ["/etc/machine-id", "/etc/hostname", "/usr/share/deli"] {
            ctx.sys.remove(&target_path(path))?;
        }
        if count > 0 {
            ctx.info(format!("{count} live-only paths removed"));
        }
        Ok(())
    }
}

pub fn fstab(ctx: &Context<'_>) -> String {
    let mut text =
        String::from("# Written by deli. <device> <mount point> <type> <options> <dump> <pass>\n");
    for entry in mount_table(ctx.plan) {
        writeln!(
            text,
            "UUID={} {} {} {} 0 {}",
            ctx.uuid(entry.label),
            entry.path,
            entry.fstype,
            entry.options,
            entry.pass
        )
        .unwrap();
    }
    for p in ctx
        .plan
        .partitions
        .iter()
        .filter(|p| p.format == Format::Swap)
    {
        writeln!(text, "UUID={} none swap defaults 0 0", ctx.uuid(p.label)).unwrap();
    }
    text
}

/// What an installed system and an image share: time zone, locale and
/// keymap, set with systemd-firstboot along with `firstboot`, the mount
/// table and a persistent journal.
pub fn settle(ctx: &Context<'_>, firstboot: &[String]) -> Result<()> {
    let sys = ctx.sys;
    let system = &ctx.config.system;
    sys.run(
        &Command::new("systemd-firstboot")
            .args([
                format!("--root={TARGET}"),
                "--force".to_owned(),
                format!("--timezone={}", system.timezone),
                format!("--locale={}", system.locale),
                format!("--keymap={}", system.keymap),
            ])
            .args(firstboot.iter().cloned()),
    )?;
    if !super::compiled_locale(&system.locale) {
        let (name, charset) = system.locale.split_once('.').expect("validated locale");
        sys.run(
            &Command::new("localedef")
                .args(["-i", name, "-f", charset, &system.locale])
                .in_target(),
        )?;
    }
    sys.write(&target_path("/etc/fstab"), &fstab(ctx), 0o644)?;
    sys.mkdir_p(&target_path("/var/log/journal"), 0o755)
}

pub struct Identity;

impl Step for Identity {
    fn name(&self) -> &'static str {
        "configuring the system"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let system = &ctx.config.system;
        settle(
            ctx,
            &[
                format!("--hostname={}", system.hostname),
                "--setup-machine-id".to_owned(),
            ],
        )?;
        // The host name also resolves locally: some services and test
        // suites expect it.
        ctx.sys.write(
            &target_path("/etc/hosts"),
            &format!(
                "127.0.0.1 localhost\n::1 localhost\n127.0.1.1 {0}\n",
                system.hostname
            ),
            0o644,
        )?;
        ctx.info(format!(
            "{}, {}, {}, keymap {}",
            system.hostname, system.timezone, system.locale, system.keymap
        ));
        Ok(())
    }
}

/// Names resolve through systemd-resolved's stub, with deli's settings
/// (DNSSEC, resolvers), whoever configures the network.
pub fn resolver(sys: &dyn Sys, net: &crate::config::Network) -> Result<()> {
    sys.write(
        &target_path("/etc/systemd/resolved.conf.d/10-deli.conf"),
        &network::resolved(net),
        0o644,
    )?;
    sys.symlink(
        "../run/systemd/resolve/stub-resolv.conf",
        &target_path("/etc/resolv.conf"),
    )
}

pub struct Network;

impl Step for Network {
    fn name(&self) -> &'static str {
        "configuring the network"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        let net = &ctx.config.network;
        sys.write(
            &target_path("/etc/systemd/network/20-deli.network"),
            &network::unit(net),
            0o644,
        )?;
        resolver(sys, net)?;
        sys.run(&Command::new("systemctl").args([
            &format!("--root={TARGET}"),
            "enable",
            "systemd-networkd.service",
            "systemd-resolved.service",
            "systemd-timesyncd.service",
        ]))?;
        let how = match net.mode {
            NetworkMode::Dhcp => "DHCP".to_owned(),
            NetworkMode::Static => net.address.clone().unwrap_or_default(),
        };
        ctx.info(format!("{how} on {}", net.interfaces));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn fstab_lists_subvolumes_esp_and_swap() {
        let mut config = config("");
        config.storage.swap = "2G".into();
        let actions = actions(&config, &facts(), |ctx| {
            let text = fstab(ctx);
            assert!(
                text.contains("UUID=<uuid-of-root> / btrfs compress=zstd:1,noatime,subvol=@ 0 0"),
                "{text}"
            );
            assert!(
                text.contains("UUID=<uuid-of-esp> /boot vfat umask=0077 0 2"),
                "{text}"
            );
            assert!(
                text.contains("UUID=<uuid-of-swap> none swap defaults 0 0"),
                "{text}"
            );
            Ok(())
        });
        // Reading the UUIDs changes nothing.
        assert_eq!(actions.len(), 0);
    }

    #[test]
    fn locale_outside_the_stage_is_generated_in_the_target() {
        let mut config = config("");
        config.system.locale = "fr_FR.UTF-8".into();
        let actions = actions(&config, &facts(), |ctx| Identity.run(ctx));
        assert!(contains(
            &actions,
            "-- localedef -i fr_FR -f UTF-8 fr_FR.UTF-8"
        ));
        let default = actions_for_default();
        assert!(!contains(&default, "localedef"));
    }

    fn actions_for_default() -> Vec<String> {
        actions(&config(""), &facts(), |ctx| Identity.run(ctx))
    }
}
