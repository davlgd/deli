//! Make the system boot on its own, and able to update its own kernel.
//!
//! The kernel ships in `/usr/lib/modules/<kver>/vmlinuz`, where
//! `kernel-install` expects it. Running `kernel-install add` inside the
//! target makes dracut build the initramfs and writes a Boot Loader
//! Specification entry on the ESP: exactly what happens later when the
//! administrator installs a new kernel.

use std::fmt::Write as _;
use std::path::Path;

use super::disk::target_dir;
use super::{Context, Step};
use crate::error::{Error, Result};
use crate::storage::ESP_OPTIONS;
use crate::system::{Command, TARGET, target_path};

pub fn kernel_cmdline(ctx: &Context<'_>) -> String {
    let root = ctx.plan.root();
    let mut cmdline = format!("root=UUID={} rw", ctx.uuid(root.label));
    if let Some(sv) = ctx.plan.subvolumes.iter().find(|s| s.mount == "/") {
        write!(cmdline, " rootflags=subvol={}", sv.name).unwrap();
    }
    if ctx.config.boot.serial_console {
        // The last console= is /dev/console: keep the screen there.
        cmdline.push_str(" console=ttyS0,115200 console=tty0");
    }
    cmdline
}

/// Kernel versions shipped in the target, each with its vmlinuz.
fn kernels(ctx: &Context<'_>) -> Result<Vec<String>> {
    if !ctx.sys.is_real() {
        return Ok(vec!["<kver>".into()]);
    }
    kernels_in(&target_path("/"))
}

/// Kernel versions shipped in the root filesystem at `root`, each with its
/// vmlinuz.
pub fn kernels_in(root: &str) -> Result<Vec<String>> {
    let dir = format!("{}/usr/lib/modules", root.trim_end_matches('/'));
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| Error::io(format!("cannot list {dir}"), e))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().join("vmlinuz").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    if found.is_empty() {
        return Err(Error::Other(format!("no kernel in {dir}")));
    }
    Ok(found)
}

pub struct Boot {
    /// For an image: neither the build host's NVRAM nor a random seed that
    /// every copy would share.
    pub image: bool,
}

impl Step for Boot {
    fn name(&self) -> &'static str {
        "installing the boot loader and kernel"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        let esp = ctx
            .plan
            .partitions
            .iter()
            .find(|p| p.mount == Some("/boot"))
            .expect("plans have an ESP");
        let boot = target_dir("/boot");
        sys.mkdir_p(&boot, 0o755)?;
        sys.run(&Command::new("mount").args([
            "--types",
            "vfat",
            "--options",
            ESP_OPTIONS,
            &ctx.device(esp.label),
            &boot,
        ]))?;

        let cmdline = kernel_cmdline(ctx);
        sys.write(
            &target_path("/etc/kernel/cmdline"),
            &format!("{cmdline}\n"),
            0o644,
        )?;
        sys.write(
            &target_path("/etc/kernel/install.conf"),
            "layout=bls\n",
            0o644,
        )?;
        // dracut's rescue plugin would add a second, larger initramfs to the ESP.
        sys.symlink(
            "/dev/null",
            &target_path("/etc/kernel/install.d/51-dracut-rescue.install"),
        )?;
        // dracut puts every CPU vendor's microcode in front of the
        // initramfs; intel-microcode's own hook would add Intel's again, as a
        // second initrd.
        sys.symlink(
            "/dev/null",
            &target_path("/etc/kernel/install.d/95-microcode.install"),
        )?;
        // Generic initramfs: the disk keeps booting if moved to other hardware.
        sys.write(
            &target_path("/etc/dracut.conf.d/10-deli.conf"),
            "hostonly=\"no\"\nearly_microcode=\"yes\"\n",
            0o644,
        )?;

        // --graceful: firmware that refuses new EFI variables still boots the
        // fallback path bootctl installs (\EFI\BOOT\BOOTX64.EFI).
        let bootctl = Command::new("bootctl").args(["install", "--graceful"]);
        let bootctl = if self.image {
            // The image's own identity and entry token, not the build host's.
            bootctl.args([
                &format!("--root={TARGET}"),
                "--esp-path=/boot",
                "--variables=no",
                "--random-seed=no",
            ])
        } else {
            bootctl.arg(format!("--esp-path={boot}"))
        };
        sys.run(&bootctl.env("SYSTEMD_RELAX_ESP_CHECKS", "1"))?;
        sys.write(
            &format!("{boot}/loader/loader.conf"),
            &format!(
                "timeout {}\nconsole-mode keep\neditor no\n",
                ctx.config.boot.timeout
            ),
            0o644,
        )?;

        for kver in kernels(ctx)? {
            sys.run(
                &Command::new("kernel-install")
                    .args([
                        "--esp-path=/boot",
                        "add",
                        &kver,
                        &format!("/usr/lib/modules/{kver}/vmlinuz"),
                    ])
                    .env("SYSTEMD_RELAX_ESP_CHECKS", "1")
                    .in_target(),
            )?;
            ctx.info(format!("kernel {kver}"));
        }

        // If the container did not see the mounted ESP, the entry went into
        // the root filesystem underneath it, and the machine would not boot.
        if sys.is_real() {
            let entries = Path::new(&boot).join("loader/entries");
            let any = std::fs::read_dir(&entries).is_ok_and(|mut d| d.next().is_some());
            if !any {
                return Err(Error::Other(format!(
                    "no boot entry in {}: kernel-install did not write to the ESP",
                    entries.display()
                )));
            }
        }
        ctx.info(format!("cmdline: {cmdline}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn cmdline_with_subvolume_and_serial() {
        let config = config("");
        actions(&config, &facts(), |ctx| {
            assert_eq!(
                kernel_cmdline(ctx),
                "root=UUID=<uuid-of-root> rw rootflags=subvol=@ console=ttyS0,115200 console=tty0"
            );
            Ok(())
        });
    }

    #[test]
    fn esp_mounted_then_bootctl_then_kernel_install_in_target() {
        let config = config("");
        let actions = actions(&config, &facts(), |ctx| Boot { image: false }.run(ctx));
        let at = |needle: &str| {
            actions
                .iter()
                .position(|a| a.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {actions:#?}"))
        };
        assert!(
            at("mount --types vfat --options umask=0077 /dev/sda1 /run/deli/target/boot")
                < at("bootctl install")
        );
        assert!(at("bootctl install") < at("kernel-install"));
        assert!(contains(&actions, "systemd-nspawn"));
        assert!(contains(&actions, "| hostonly=\"no\""));
        assert!(contains(&actions, "| early_microcode=\"yes\""));
    }
}
