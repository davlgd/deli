//! Leave a record in the installed system, then let go of the disk.

use super::{Context, Step};
use crate::error::Result;
use crate::system::{Command, TARGET, target_path};

pub const LOG_DIR: &str = "/var/log/deli";

/// Write the transcript and the configuration (without secrets) into the
/// target. Also used when an installation fails, so it must not fail itself
/// for want of a mounted target.
pub fn record(ctx: &mut Context<'_>) -> Result<()> {
    let sys = ctx.sys;
    sys.mkdir_p(&target_path(LOG_DIR), 0o700)?;
    sys.write(
        &target_path(&format!("{LOG_DIR}/deli.toml")),
        &ctx.config.redacted(),
        0o600,
    )?;
    sys.write(
        &target_path(&format!("{LOG_DIR}/install.log")),
        &ctx.report.transcript(),
        0o600,
    )
}

pub struct Finish;

impl Step for Finish {
    fn name(&self) -> &'static str {
        "finishing"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        ctx.info(format!("log: {LOG_DIR}/install.log"));
        record(ctx)?;
        // Last, and only here: an unattended session that finds it on the
        // disk does not install again, so a failed installation must not
        // leave it.
        let fingerprint = crate::install::fingerprint(ctx.sys, ctx.config)?;
        ctx.sys.write(
            &target_path(crate::install::FINGERPRINT),
            &format!("{fingerprint}\n"),
            0o644,
        )?;
        ctx.sys.run(&Command::new("sync"))?;
        // btrfs may still hold writes in memory: unmount rather than rely on
        // reboot. The system is installed and synced by now: a shell left in
        // the target (Alt+F2) only delays that to the reboot.
        if let Err(e) = ctx
            .sys
            .run(&Command::new("umount").args(["--recursive", TARGET]))
        {
            ctx.report
                .warn(&format!("{TARGET} stays mounted until the reboot: {e}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn a_target_still_busy_is_no_failure() {
        let config = config("");
        let sys = crate::system::DrySys::failing_on("umount --recursive");
        with_context(&config, &facts(), &sys, |ctx| Finish.run(ctx).unwrap());
        assert!(contains(&sys.actions(), "config.sha256"));
    }
}
