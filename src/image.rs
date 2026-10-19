//! `deli image`: a disk image of the system for virtual machines and
//! clouds, which cloud-init configures on its first boot.
//!
//! The installation's own steps run on a loop device over a sparse file:
//! the same partitions, filesystems, copy and boot loader as on a disk.

use std::path::{Path, PathBuf};

use crate::config::{Config, parse_size};
use crate::error::{Error, Result};
use crate::install::release_target;
use crate::report::Reporter;
use crate::steps::{self, Context, State, kernels_in};
use crate::storage::Plan;
use crate::system::probe::Facts;
use crate::system::{Command, Sys, TARGET};

/// The image's disk: the smallest an installation accepts, rounded up. A
/// virtual machine's disk can be larger: the root grows to fill it.
const SIZE: &str = "10G";

/// Build the raw image `output` from the root filesystem `source`.
pub fn build(sys: &dyn Sys, source: &str, output: &str, report: &mut Reporter) -> Result<()> {
    if sys.is_real() {
        check_source(source, output)?;
    }
    // What a failed attempt left mounted there is not this image's.
    release_target(sys)?;
    sys.create_sparse(output, parse_size(SIZE).expect("a valid size"))?;
    let result = fill(sys, source, output, report);
    if result.is_err() {
        let _ = sys.remove(output);
    }
    result
}

/// Neither the whole machine nor a tree holding the image or the target:
/// a root filesystem with a kernel, checked before hours are spent.
fn check_source(source: &str, output: &str) -> Result<()> {
    let canonical = |path: &Path| {
        path.canonicalize()
            .map_err(|e| Error::io(format!("cannot resolve {}", path.display()), e))
    };
    let root = canonical(Path::new(source))?;
    let parent = Path::new(output)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let inside = |path: &PathBuf| path.starts_with(&root);
    // Under /run/deli, the target's mounts would hide it.
    if root == Path::new("/")
        || root.starts_with("/run/deli")
        || inside(&canonical(parent)?)
        || inside(&PathBuf::from(TARGET))
    {
        return Err(Error::Other(format!(
            "{source} cannot be copied into {output}: one holds the other"
        )));
    }
    kernels_in(&root.to_string_lossy()).map(drop)
}

/// Run the image's steps on a loop device over `output`.
fn fill(sys: &dyn Sys, source: &str, output: &str, report: &mut Reporter) -> Result<()> {
    let device = sys
        .capture(&Command::new("losetup").args(["--find", "--show", "--partscan", output]))?
        .trim()
        .to_owned();
    let config = Config::default();
    let plan = Plan::new(&config.storage, &device);
    report.line(&format!(
        "deli {}: image of {source} in {output} ({SIZE}, on {device})",
        env!("CARGO_PKG_VERSION")
    ));
    let built = steps::run_image(&mut Context {
        config: &config,
        plan: &plan,
        facts: &Facts::default(),
        sys,
        report: &mut *report,
        source,
        state: State::default(),
    });
    if built.is_err() {
        // A failed step may leave the image mounted, which keeps it attached.
        if let Err(e) = release_target(sys) {
            report.warn(&format!("{TARGET} stays mounted: {e}"));
        }
    }
    built.and(sys.run(&Command::new("losetup").args(["--detach", &device])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::DrySys;

    fn contains_action(actions: &[String], needle: &str) -> bool {
        actions.iter().any(|a| a.contains(needle))
    }

    fn build_with(sys: &DrySys) -> (Result<()>, Vec<String>) {
        let mut report = Reporter::default();
        let result = build(sys, "/work/root", "/work/deli.raw", &mut report);
        (result, sys.actions())
    }

    #[test]
    fn the_image_holds_an_installation() {
        let config = Config::default();
        let plan = Plan::new(&config.storage, "/dev/loop0");
        assert!(parse_size(SIZE).unwrap() >= plan.min_disk_size());
    }

    #[test]
    fn a_blank_system_on_a_loop_device() {
        let sys = DrySys::default().answering("losetup", "/dev/loop0\n");
        let (result, actions) = build_with(&sys);
        result.unwrap();
        let at = |needle: &str| {
            actions
                .iter()
                .position(|a| a.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing: {actions:#?}"))
        };
        assert!(at("create /work/deli.raw (10737418240 bytes, sparse)") < at("losetup --find"));
        assert!(at("systemd-repart") < at("cp -a /work/root/. /run/deli/target/"));
        assert!(at("mount --types vfat --options umask=0077 /dev/loop0p1") > at("cp -a"));
        assert!(at("umount --recursive /run/deli/target") < at("losetup --detach /dev/loop0"));
        // Never the build host's firmware.
        assert!(actions.iter().any(
            |a| a.contains("bootctl install --graceful --root=/run/deli/target --esp-path=/boot --variables=no --random-seed=no")
        ));
        // Nothing that makes one machine: cloud-init does that on first boot.
        assert!(
            contains_action(&actions, "write /run/deli/target/etc/machine-id (444)"),
            "{actions:#?}"
        );
        assert!(contains_action(&actions, "| DNSSEC=no"), "{actions:#?}");
        for absent in [
            "--hostname",
            "--setup-machine-id",
            "useradd",
            "20-deli.network",
            "sshd_config",
        ] {
            assert!(!contains_action(&actions, absent), "{absent}: {actions:#?}");
        }
    }

    #[test]
    fn a_failed_image_is_unmounted_detached_and_removed() {
        let sys = DrySys::failing_on("kernel-install")
            .answering("losetup", "/dev/loop0\n")
            .answering("findmnt", "/run/deli/target\n");
        let (result, actions) = build_with(&sys);
        assert!(result.is_err());
        let tail: Vec<&str> = actions.iter().rev().take(3).map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "remove /work/deli.raw",
                "run: losetup --detach /dev/loop0",
                "run: umount --recursive /run/deli/target"
            ],
            "{actions:#?}"
        );
    }

    #[test]
    fn an_image_that_cannot_be_attached_is_removed() {
        let sys = DrySys::failing_on("losetup");
        let (result, actions) = build_with(&sys);
        assert!(result.is_err());
        assert_eq!(actions.last().unwrap(), "remove /work/deli.raw");
    }

    #[test]
    fn a_failed_subvolume_releases_the_btrfs_top_level() {
        let sys = DrySys::failing_on("btrfs subvolume create").answering("losetup", "/dev/loop0\n");
        let (result, actions) = build_with(&sys);
        assert!(result.is_err());
        let mounted = actions
            .iter()
            .position(|a| a == "run: mount /dev/loop0p2 /run/deli/btrfs")
            .expect("the top level was mounted");
        assert!(
            actions[mounted..]
                .iter()
                .any(|a| a == "run: umount /run/deli/btrfs"),
            "{actions:#?}"
        );
    }

    #[test]
    fn a_source_holding_the_image_is_refused() {
        let dir = std::env::temp_dir().join(format!("deli-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("deli.raw");
        let refused = check_source(&dir.to_string_lossy(), &output.to_string_lossy());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("one holds the other")
        );
        assert!(check_source("/", "/tmp/deli.raw").is_err());
    }
}
