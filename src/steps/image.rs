//! Steps of a cloud image only: what makes one machine is left blank, for
//! cloud-init and systemd to fill on its first boot.

use super::finish::LOG_DIR;
use super::system::{resolver, settle};
use super::{Context, Step};
use crate::error::{Error, Result};
use crate::system::{Command, TARGET, target_path};

/// What one machine makes for itself, or `docker export` leaves of the
/// container the root came from: no copy of the image may share it.
const MACHINE_STATE: [&str; 8] = [
    "/.dockerenv",
    "/etc/hostname",
    "/var/lib/dbus/machine-id",
    "/var/lib/systemd/random-seed",
    "/var/lib/systemd/credential.secret",
    "/var/lib/cloud",
    "/var/log/cloud-init.log",
    "/var/log/cloud-init-output.log",
];

/// Machine state present in the target, SSH host keys included (each
/// machine's sshd-keygen.service makes its own) and the boot loader's
/// random seed.
fn machine_state(ctx: &Context<'_>) -> Result<Vec<String>> {
    if !ctx.sys.is_real() {
        return Ok(MACHINE_STATE.iter().map(|p| target_path(p)).collect());
    }
    let mut found: Vec<String> = MACHINE_STATE
        .iter()
        .map(|p| target_path(p))
        .chain([target_path("/boot/loader/random-seed")])
        .filter(|p| std::path::Path::new(p).symlink_metadata().is_ok())
        .collect();
    let dir = target_path("/etc/ssh");
    let entries =
        std::fs::read_dir(&dir).map_err(|e| Error::io(format!("cannot list {dir}"), e))?;
    for entry in entries.filter_map(std::result::Result::ok) {
        if entry.file_name().to_string_lossy().starts_with("ssh_host_") {
            found.push(entry.path().to_string_lossy().into_owned());
        }
    }
    Ok(found)
}

pub struct Blank;

impl Step for Blank {
    fn name(&self) -> &'static str {
        "leaving the identity blank"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        for path in machine_state(ctx)? {
            sys.remove(&path)?;
        }
        settle(ctx, &[])?;
        // Empty, not missing: systemd makes an ID on first boot without
        // treating it as one, so it neither applies presets (Exherbo's enable
        // every unit they do not name) nor asks for settings on the console.
        // Before the boot step: the initramfs copies it.
        sys.write(&target_path("/etc/machine-id"), "", 0o444)?;
        // Kernels installed later, once the machine has an ID, go where
        // these go: kernel-install would otherwise name them after the ID.
        sys.write(&target_path("/etc/kernel/entry-token"), "exherbo\n", 0o644)?;
        sys.write(
            &target_path("/etc/hosts"),
            "127.0.0.1 localhost\n::1 localhost\n",
            0o644,
        )?;
        // DNSSEC off, as on an installed system: Exherbo's resolved fails
        // every query behind a resolver that strips signatures.
        resolver(sys, &ctx.config.network)
    }
}

/// Check that nothing of one machine is left, keep the build's transcript
/// in the image, then let go of it.
pub struct Seal;

impl Step for Seal {
    fn name(&self) -> &'static str {
        "finishing"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        if sys.is_real() {
            let mut left = machine_state(ctx)?;
            let id = target_path("/etc/machine-id");
            if std::fs::metadata(&id)
                .map_err(|e| Error::io(format!("cannot read {id}"), e))?
                .len()
                > 0
            {
                left.push(id);
            }
            if !left.is_empty() {
                return Err(Error::Other(format!(
                    "the image would share machine state: {}",
                    left.join(", ")
                )));
            }
        }
        sys.mkdir_p(&target_path(LOG_DIR), 0o700)?;
        sys.write(
            &target_path(&format!("{LOG_DIR}/image.log")),
            &ctx.report.transcript(),
            0o600,
        )?;
        sys.run(&Command::new("sync"))?;
        sys.run(&Command::new("umount").args(["--recursive", TARGET]))
    }
}
