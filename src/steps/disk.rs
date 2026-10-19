//! Release, partition, format and mount the target disk.

use super::{Context, Step};
use crate::config::Filesystem;
use crate::error::{Error, Result};
use crate::storage::{BTRFS_OPTIONS, ESP_OPTIONS, Format as Fmt, Partition as Part, Plan, Role};
use crate::system::probe::{lsblk_column, partition_names};
use crate::system::{Command, TARGET};

const REPART_DIR: &str = "/run/deli/repart.d";
const BTRFS_TOP: &str = "/run/deli/btrfs";

/// Whether `source` (a device node) lives on `disk`: the disk itself or one
/// of its partitions (`/dev/sda1`, `/dev/nvme0n1p1`), not `/dev/sdaa1`,
/// `/dev/sdap1` or `/dev/nvme0n10`.
fn on_disk(source: &str, disk: &str) -> bool {
    // findmnt reports btrfs subvolume mounts as `/dev/sda2[/@home]`.
    let source = source.split('[').next().unwrap_or(source);
    let Some(rest) = source.strip_prefix(disk) else {
        return false;
    };
    // A disk whose name ends in a digit numbers its partitions after a p.
    let number = if disk.ends_with(|c: char| c.is_ascii_digit()) {
        match rest.strip_prefix('p') {
            Some(number) => number,
            None => return rest.is_empty(),
        }
    } else {
        rest
    };
    rest.is_empty() || (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

/// Stop everything the live system holds on the disk. With correct GPT type
/// GUIDs, systemd may auto-mount a previous installation's partitions, and
/// swap units reactivate swap: the disk must be released before repart.
pub struct Release;

impl Step for Release {
    fn name(&self) -> &'static str {
        "releasing the disk"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let disk = ctx.plan.disk.clone();
        let sys = ctx.sys;

        // Before anything changes: a disk under device-mapper or RAID is
        // left as it is.
        let mapped: Vec<String> = lsblk_column(sys, "TYPE", Some(&disk))?
            .into_iter()
            .filter(|(_, kind)| {
                matches!(
                    kind.as_str(),
                    "lvm"
                        | "crypt"
                        | "dm"
                        | "mpath"
                        | "linear"
                        | "raid0"
                        | "raid1"
                        | "raid4"
                        | "raid5"
                        | "raid6"
                        | "raid10"
                )
            })
            .map(|(name, _)| name)
            .collect();
        if !mapped.is_empty() {
            return Err(Error::Other(format!(
                "{disk} is in use by device-mapper or RAID ({}); deactivate them first",
                mapped.join(", ")
            )));
        }

        let swaps =
            sys.capture(&Command::new("swapon").args(["--show=NAME", "--noheadings", "--raw"]))?;
        for swap in swaps.lines().map(str::trim).filter(|s| on_disk(s, &disk)) {
            sys.run(&Command::new("swapoff").arg(swap))?;
            ctx.info(format!("swap off: {swap}"));
        }

        let mounts = sys.capture(&Command::new("findmnt").args([
            "--raw",
            "--noheadings",
            "--output",
            "TARGET,SOURCE",
        ]))?;
        let mut targets: Vec<&str> = mounts
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(_, source)| on_disk(source, &disk))
            .map(|(target, _)| target)
            .collect();
        // Deepest first.
        targets.sort_by_key(|t| std::cmp::Reverse(t.matches('/').count()));
        for target in targets {
            sys.run(&Command::new("umount").arg(target))?;
            ctx.info(format!("unmounted {target}"));
        }

        ctx.report.erasing(&disk);
        sys.run(&Command::new("wipefs").args(["--all", "--force", &disk]))?;
        sys.run(&Command::new("udevadm").arg("settle"))?;
        Ok(())
    }
}

pub struct Partition;

impl Step for Partition {
    fn name(&self) -> &'static str {
        "partitioning"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        sys.remove(REPART_DIR)?;
        for (file, contents) in ctx.plan.repart_definitions() {
            sys.write(&format!("{REPART_DIR}/{file}"), &contents, 0o644)?;
        }
        // repart only simulates unless told otherwise.
        sys.run(&Command::new("systemd-repart").args([
            &format!("--definitions={REPART_DIR}"),
            "--empty=force",
            "--dry-run=no",
            "--no-pager",
            &ctx.plan.disk,
        ]))?;
        sys.run(&Command::new("udevadm").arg("settle"))?;

        if sys.is_real() {
            let listing = partition_names(sys, &ctx.plan.disk)?;
            for p in &ctx.plan.partitions {
                let device = listing
                    .iter()
                    .find(|(_, label)| label == p.label)
                    .map(|(name, _)| name.clone())
                    .ok_or_else(|| {
                        Error::Other(format!(
                            "partition \"{}\" missing after systemd-repart",
                            p.label
                        ))
                    })?;
                ctx.state.devices.insert(p.label, device);
            }
        }
        for p in &ctx.plan.partitions {
            let device = ctx.device(p.label);
            ctx.info(format!("{device}: {}", p.label));
        }
        Ok(())
    }
}

fn mkfs(p: &Part, device: &str) -> Command {
    match p.format {
        Fmt::Vfat => Command::new("mkfs.vfat").args(["-F", "32", "-n", "ESP", device]),
        Fmt::Swap => Command::new("mkswap").args(["--label", p.label, device]),
        Fmt::Fs(Filesystem::Btrfs) => {
            Command::new("mkfs.btrfs").args(["--force", "--label", p.label, device])
        }
        Fmt::Fs(Filesystem::Ext4) => Command::new("mkfs.ext4").args(["-F", "-L", p.label, device]),
        Fmt::Fs(Filesystem::Xfs) => Command::new("mkfs.xfs").args(["-f", "-L", p.label, device]),
    }
}

pub struct Format;

impl Step for Format {
    fn name(&self) -> &'static str {
        "creating filesystems"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        for p in &ctx.plan.partitions {
            let device = ctx.device(p.label);
            ctx.sys.run(&mkfs(p, &device))?;
            if ctx.sys.is_real() {
                let uuid = ctx.sys.capture(&Command::new("blkid").args([
                    "--match-tag",
                    "UUID",
                    "--output",
                    "value",
                    &device,
                ]))?;
                ctx.state.uuids.insert(p.label, uuid.trim().to_owned());
            }
        }
        Ok(())
    }
}

/// One line of the target's mount table, used to mount now and for fstab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub label: &'static str,
    pub path: &'static str,
    pub fstype: String,
    pub options: String,
    pub pass: u8,
}

/// Everything mounted under the target, parents before children. The ESP is
/// included; [`Mount`] leaves it to the boot step.
pub fn mount_table(plan: &Plan) -> Vec<MountEntry> {
    let root = plan.root();
    let mut entries = Vec::new();
    for p in &plan.partitions {
        let Some(path) = p.mount else { continue };
        let (fstype, options) = match p.format {
            Fmt::Vfat => ("vfat".to_owned(), ESP_OPTIONS.to_owned()),
            Fmt::Swap => continue,
            Fmt::Fs(Filesystem::Btrfs) => ("btrfs".to_owned(), BTRFS_OPTIONS.to_owned()),
            Fmt::Fs(fs) => (fs.to_string(), "noatime".to_owned()),
        };
        if p.role == Role::Root && !plan.subvolumes.is_empty() {
            for sv in &plan.subvolumes {
                entries.push(MountEntry {
                    label: root.label,
                    path: sv.mount,
                    fstype: fstype.clone(),
                    options: format!("{options},subvol={}", sv.name),
                    pass: 0,
                });
            }
            continue;
        }
        let pass = match (p.role, p.format) {
            (_, Fmt::Fs(Filesystem::Btrfs | Filesystem::Xfs)) => 0,
            (Role::Root, _) => 1,
            _ => 2,
        };
        entries.push(MountEntry {
            label: p.label,
            path,
            fstype,
            options,
            pass,
        });
    }
    entries.sort_by_key(|e| {
        if e.path == "/" {
            0
        } else {
            e.path.matches('/').count()
        }
    });
    entries
}

pub fn target_dir(path: &str) -> String {
    if path == "/" {
        TARGET.to_owned()
    } else {
        format!("{TARGET}{path}")
    }
}

/// Mount the root and data filesystems. Not the ESP: the copy comes next,
/// and cp cannot set ownership on vfat.
pub struct Mount;

impl Step for Mount {
    fn name(&self) -> &'static str {
        "mounting the target"
    }

    fn run(&self, ctx: &mut Context<'_>) -> Result<()> {
        let sys = ctx.sys;
        let root = ctx.plan.root().clone();
        if !ctx.plan.subvolumes.is_empty() {
            let device = ctx.device(root.label);
            sys.mkdir_p(BTRFS_TOP, 0o755)?;
            sys.run(&Command::new("mount").args([&device, BTRFS_TOP]))?;
            // Unmounted whatever happens: it would keep the disk busy.
            let created = ctx.plan.subvolumes.iter().try_for_each(|sv| {
                sys.run(&Command::new("btrfs").args([
                    "subvolume",
                    "create",
                    &format!("{BTRFS_TOP}/{}", sv.name),
                ]))
            });
            let released = sys.run(&Command::new("umount").arg(BTRFS_TOP));
            created.and(released)?;
        }
        for entry in mount_table(ctx.plan).iter().filter(|e| e.path != "/boot") {
            let dir = target_dir(entry.path);
            sys.mkdir_p(&dir, 0o755)?;
            sys.run(&Command::new("mount").args([
                "--types",
                &entry.fstype,
                "--options",
                &entry.options,
                &ctx.device(entry.label),
                &dir,
            ]))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn disk_membership() {
        assert!(on_disk("/dev/sda", "/dev/sda"));
        assert!(on_disk("/dev/sda2", "/dev/sda"));
        assert!(on_disk("/dev/nvme0n1p3", "/dev/nvme0n1"));
        assert!(on_disk("/dev/sda2[/@home]", "/dev/sda"));
        assert!(!on_disk("/dev/sdaa1", "/dev/sda"));
        assert!(!on_disk("/dev/sdap1", "/dev/sda"));
        assert!(!on_disk("/dev/nvme0n10", "/dev/nvme0n1"));
        assert!(!on_disk("/dev/nvme0n10p1", "/dev/nvme0n1"));
        assert!(!on_disk("/dev/nvme0n1p", "/dev/nvme0n1"));
        assert!(!on_disk("/dev/sdb1", "/dev/sda"));
        assert!(!on_disk("overlay", "/dev/sda"));
    }

    #[test]
    fn release_lets_go_of_its_disk_only() {
        let config = config("");
        let sys = crate::system::DrySys::default()
            .answering("swapon", "/dev/sda3\n/dev/sdap2\n")
            .answering(
                "findmnt",
                "/mnt/old /dev/sda2[/@]\n/mnt/old/boot /dev/sda1\n/srv /dev/sdap1\n",
            )
            .answering("lsblk", "/dev/sda disk\n/dev/sda1 part\n/dev/sda2 part\n");
        with_context(&config, &facts(), &sys, |ctx| Release.run(ctx).unwrap());
        let actions = sys.actions();
        let at = |needle: &str| actions.iter().position(|a| a.ends_with(needle));
        assert!(at("swapoff /dev/sda3").is_some(), "{actions:?}");
        assert!(at("swapoff /dev/sdap2").is_none());
        // Deepest first, and only those on the disk.
        assert!(at("umount /mnt/old/boot").is_some(), "{actions:?}");
        assert!(at("umount /mnt/old/boot") < at("umount /mnt/old"));
        assert!(at("umount /srv").is_none());
        assert!(at("umount /mnt/old") < at("wipefs --all --force /dev/sda"));
    }

    #[test]
    fn release_leaves_a_disk_under_lvm_untouched() {
        let config = config("");
        let sys = crate::system::DrySys::default()
            .answering("swapon", "/dev/sda3\n")
            .answering(
                "lsblk",
                "/dev/sda disk\n/dev/sda1 part\n/dev/mapper/vg-data lvm\n",
            );
        with_context(&config, &facts(), &sys, |ctx| {
            let error = Release.run(ctx).unwrap_err().to_string();
            assert!(error.contains("/dev/mapper/vg-data"), "{error}");
        });
        assert!(!contains(&sys.actions(), "swapoff"));
        assert!(!contains(&sys.actions(), "wipefs"));
    }

    #[test]
    fn btrfs_mount_table() {
        let config = config("");
        let plan = Plan::new(&config.storage, "/dev/sda");
        let table = mount_table(&plan);
        let paths: Vec<_> = table.iter().map(|e| e.path).collect();
        assert_eq!(paths, ["/", "/boot", "/home", "/.snapshots", "/var/log"]);
        assert_eq!(table[0].options, "compress=zstd:1,noatime,subvol=@");
        assert_eq!(table[1].options, "umask=0077");
    }

    #[test]
    fn mount_skips_esp_and_creates_subvolumes_first() {
        let config = config("");
        let actions = actions(&config, &facts(), |ctx| Mount.run(ctx));
        assert!(contains(
            &actions,
            "btrfs subvolume create /run/deli/btrfs/@snapshots"
        ));
        assert!(!contains(&actions, "/run/deli/target/boot"));
        let create = actions
            .iter()
            .position(|a| a.contains("subvolume create"))
            .unwrap();
        let mount_root = actions
            .iter()
            .position(|a| a.contains("subvol=@ /dev/sda2 /run/deli/target"))
            .unwrap();
        assert!(create < mount_root);
    }

    #[test]
    fn ext4_root_is_checked_at_boot() {
        let mut config = config("");
        config.storage.filesystem = Filesystem::Ext4;
        let plan = Plan::new(&config.storage, "/dev/sda");
        let root = mount_table(&plan)
            .into_iter()
            .find(|e| e.path == "/")
            .unwrap();
        assert_eq!((root.fstype.as_str(), root.pass), ("ext4", 1));
    }
}
