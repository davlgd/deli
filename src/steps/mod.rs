//! The installation pipeline: an ordered list of small steps.
//!
//! A step depends only on its [`Context`]: the validated configuration, the
//! storage plan, the facts gathered before starting, the [`Sys`] effects
//! boundary and the reporter. Values a step discovers (UUIDs, kernel
//! versions) go into [`State`] for later steps.

mod access;
mod boot;
mod disk;
mod finish;
mod image;
mod preflight;
mod repositories;
mod system;

use std::collections::BTreeMap;

pub use boot::kernels_in;

use crate::config::Config;
use crate::error::Result;
use crate::report::Reporter;
use crate::storage::Plan;
use crate::system::Sys;
use crate::system::probe::Facts;

/// Locales the Exherbo stage ships compiled; others are built with localedef.
pub const STAGE_LOCALES: [&str; 3] = ["C.UTF-8", "en_US.UTF-8", "en_GB.UTF-8"];

/// Whether the stage ships `locale` compiled.
pub fn compiled_locale(locale: &str) -> bool {
    STAGE_LOCALES.iter().any(|l| l.eq_ignore_ascii_case(locale))
}

pub struct Context<'a> {
    pub config: &'a Config,
    pub plan: &'a Plan,
    pub facts: &'a Facts,
    pub sys: &'a dyn Sys,
    pub report: &'a mut Reporter,
    /// The live root filesystem to copy: dmsquash-live's read-only lower layer.
    pub source: &'a str,
    pub state: State,
}

/// What steps learn along the way.
#[derive(Debug, Default)]
pub struct State {
    /// The administrator's SSH keys, GitHub's included: fetched before the
    /// disk is touched, so a network or GitHub failure costs nothing.
    pub admin_keys: Vec<String>,
    /// The packages to add to the world set, bare names resolved.
    pub world: Vec<String>,
    /// Partition label → device node, once partitioned.
    pub devices: BTreeMap<&'static str, String>,
    /// Partition label → filesystem UUID, once formatted.
    pub uuids: BTreeMap<&'static str, String>,
}

impl Context<'_> {
    pub fn info(&mut self, message: impl AsRef<str>) {
        self.report.info(message.as_ref());
    }

    /// Device node of a partition: discovered after partitioning, predicted
    /// from the plan in a dry run.
    pub fn device(&self, label: &str) -> String {
        if let Some(device) = self.state.devices.get(label) {
            return device.clone();
        }
        let index = self
            .plan
            .partitions
            .iter()
            .position(|p| p.label == label)
            .expect("label from the plan");
        self.plan.device(index + 1)
    }

    /// Filesystem UUID of a partition, or a placeholder in a dry run.
    pub fn uuid(&self, label: &str) -> String {
        self.state
            .uuids
            .get(label)
            .cloned()
            .unwrap_or_else(|| format!("<uuid-of-{label}>"))
    }
}

pub trait Step {
    fn name(&self) -> &'static str;
    fn run(&self, ctx: &mut Context<'_>) -> Result<()>;
}

/// The whole installation, in order.
fn pipeline() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(preflight::Preflight),
        Box::new(disk::Release),
        Box::new(disk::Partition),
        Box::new(disk::Format),
        Box::new(disk::Mount),
        Box::new(system::Copy),
        Box::new(system::Purge),
        Box::new(system::Identity),
        Box::new(system::Network),
        Box::new(access::Accounts),
        Box::new(access::Ssh),
        Box::new(boot::Boot { image: false }),
        Box::new(repositories::Repositories),
        Box::new(finish::Finish),
    ]
}

/// A cloud image: the installation's steps that make a system bootable, on
/// an image instead of a disk, and nothing that makes it one machine.
fn image_pipeline() -> Vec<Box<dyn Step>> {
    vec![
        Box::new(disk::Partition),
        Box::new(disk::Format),
        Box::new(disk::Mount),
        Box::new(system::Copy),
        Box::new(image::Blank),
        Box::new(boot::Boot { image: true }),
        Box::new(image::Seal),
    ]
}

/// Leave the transcript in the target after a failure, when it is mounted.
pub fn record_failure(ctx: &mut Context<'_>) -> Result<()> {
    finish::record(ctx)
}

pub fn run(ctx: &mut Context<'_>) -> Result<()> {
    execute(ctx, &pipeline())
}

pub fn run_image(ctx: &mut Context<'_>) -> Result<()> {
    execute(ctx, &image_pipeline())
}

fn execute(ctx: &mut Context<'_>, steps: &[Box<dyn Step>]) -> Result<()> {
    let total = steps.len();
    for (i, step) in steps.iter().enumerate() {
        ctx.report.step(i + 1, total, step.name());
        step.run(ctx)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{Context, State};
    use crate::config::Config;
    use crate::report::Reporter;
    use crate::storage::Plan;
    use crate::system::DrySys;
    use crate::system::probe::{Disk, Facts};

    pub fn config(extra: &str) -> Config {
        let config = Config::parse(&format!(
            "[storage]\ndisk = \"/dev/sda\"\n[admin]\nname = \"ops\"\nauthorized_keys = [\"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM7w6tvHPjMHe2grkjvMyevSUvyZE/gsp1Xnad41MMU0 ops@example\"]\n{extra}"
        ))
        .unwrap();
        config.validate().unwrap();
        config
    }

    pub fn facts() -> Facts {
        Facts {
            uefi: true,
            disks: vec![Disk {
                path: "/dev/sda".into(),
                size: 64 << 30,
                model: "QEMU HARDDISK".into(),
                serial: "drive-scsi0".into(),
                removable: false,
                read_only: false,
                ..Default::default()
            }],
            boot_disk: Some("/dev/sr0".into()),
        }
    }

    /// Run `f` with a context on `sys`.
    pub fn with_context<T>(
        config: &Config,
        facts: &Facts,
        sys: &DrySys,
        f: impl FnOnce(&mut Context<'_>) -> T,
    ) -> T {
        let plan = Plan::new(&config.storage, &config.storage.disk);
        let mut report = Reporter::default();
        f(&mut Context {
            config,
            plan: &plan,
            facts,
            sys,
            report: &mut report,
            source: "/run/rootfsbase",
            state: State::default(),
        })
    }

    /// Run `f` with a dry-run context and return the recorded actions.
    pub fn actions(
        config: &Config,
        facts: &Facts,
        f: impl FnOnce(&mut Context<'_>) -> crate::error::Result<()>,
    ) -> Vec<String> {
        let sys = DrySys::default();
        with_context(config, facts, &sys, f).unwrap();
        sys.actions()
    }

    pub fn contains(actions: &[String], needle: &str) -> bool {
        actions.iter().any(|a| a.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn full_pipeline_dry_run() {
        let config = config("");
        let facts = facts();
        let actions = actions(&config, &facts, run);
        let position = |needle: &str| {
            actions
                .iter()
                .position(|a| a.contains(needle))
                .unwrap_or_else(|| panic!("no action with {needle}"))
        };
        // Partition before format before copy before kernel-install before unmount.
        assert!(position("systemd-repart") < position("mkfs.btrfs"));
        assert!(position("mkfs.btrfs") < position("cp -a"));
        assert!(position("cp -a") < position("kernel-install"));
        assert!(position("kernel-install") < position("umount --recursive"));
        // The ESP is mounted after the copy: cp cannot set ownership on vfat.
        assert!(position("cp -a") < position("umask=0077"));
        // The fingerprint means "installed": written after everything else.
        assert!(position("kernel-install") < position("config.sha256"));
    }

    #[test]
    fn a_failed_installation_leaves_no_fingerprint() {
        let config = config("");
        let sys = crate::system::DrySys::failing_on("kernel-install");
        with_context(&config, &facts(), &sys, |ctx| {
            assert!(run(ctx).is_err());
            record_failure(ctx).unwrap();
        });
        let actions = sys.actions();
        assert!(contains(&actions, "kernel-install"), "{actions:?}");
        assert!(contains(&actions, "install.log"), "{actions:?}");
        assert!(!contains(&actions, "config.sha256"), "{actions:?}");
    }
}
