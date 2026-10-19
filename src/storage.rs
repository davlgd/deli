//! From the `[storage]` section to a concrete plan: partitions, filesystems,
//! btrfs subvolumes and mount points. Pure: no effects, fully testable, and
//! printed by `deli plan` before anything is touched.

use std::fmt::Write as _;

use crate::config::{Filesystem, Layout, Storage, parse_size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Esp,
    Swap,
    Root,
    Var,
    Home,
}

impl Role {
    /// systemd-repart `Type=`: Discoverable Partitions Specification names.
    fn repart_type(self) -> &'static str {
        match self {
            Self::Esp => "esp",
            Self::Swap => "swap",
            Self::Root => "root",
            Self::Var => "var",
            Self::Home => "home",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Vfat,
    Swap,
    Fs(Filesystem),
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vfat => f.write_str("vfat"),
            Self::Swap => f.write_str("swap"),
            Self::Fs(fs) => fs.fmt(f),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub role: Role,
    /// GPT partition label and filesystem label.
    pub label: &'static str,
    /// `None`: takes the rest of the disk.
    pub size: Option<u64>,
    pub format: Format,
    pub mount: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subvolume {
    pub name: &'static str,
    pub mount: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub disk: String,
    pub partitions: Vec<Partition>,
    /// Only for a btrfs root; mounted with `subvol=`.
    pub subvolumes: Vec<Subvolume>,
}

/// Smallest root deli accepts: the system alone takes a few GiB.
pub const MIN_ROOT: u64 = 8 << 30;

/// GPT headers, backup table and 1 MiB alignment of each partition.
const GPT_MARGIN: u64 = 64 << 20;

pub const BTRFS_OPTIONS: &str = "compress=zstd:1,noatime";
/// Mount options of the ESP: only root reads the boot loader's files.
pub const ESP_OPTIONS: &str = "umask=0077";

impl Plan {
    /// `storage` must have been validated (sizes parse).
    pub fn new(storage: &Storage, disk: &str) -> Self {
        let size = |s: &str| parse_size(s).expect("validated size");
        let mut partitions = vec![Partition {
            role: Role::Esp,
            label: "esp",
            size: Some(size(&storage.esp_size)),
            format: Format::Vfat,
            mount: Some("/boot"),
        }];
        if storage.swap != "none" {
            partitions.push(Partition {
                role: Role::Swap,
                label: "swap",
                size: Some(size(&storage.swap)),
                format: Format::Swap,
                mount: None,
            });
        }
        partitions.push(Partition {
            role: Role::Root,
            label: "root",
            size: None,
            format: Format::Fs(storage.filesystem),
            mount: Some("/"),
        });
        if storage.layout == Layout::Server {
            for (role, label, mount, value) in [
                (Role::Var, "var", "/var", &storage.var_size),
                (Role::Home, "home", "/home", &storage.home_size),
            ] {
                if let Some(value) = value {
                    partitions.push(Partition {
                        role,
                        label,
                        size: Some(size(value)),
                        format: Format::Fs(storage.filesystem),
                        mount: Some(mount),
                    });
                }
            }
        }

        let subvolumes = if storage.filesystem == Filesystem::Btrfs {
            let has = |mount: &str| partitions.iter().any(|p| p.mount == Some(mount));
            [
                ("@", "/"),
                ("@home", "/home"),
                ("@log", "/var/log"),
                ("@snapshots", "/.snapshots"),
            ]
            .into_iter()
            // A separate partition already holds /home or /var: no subvolume for it.
            .filter(|(_, mount)| {
                *mount == "/" || !(has(mount) || (mount.starts_with("/var/") && has("/var")))
            })
            .map(|(name, mount)| Subvolume { name, mount })
            .collect()
        } else {
            Vec::new()
        };

        Self {
            disk: disk.to_owned(),
            partitions,
            subvolumes,
        }
    }

    /// The smallest disk this plan fits on.
    pub fn min_disk_size(&self) -> u64 {
        self.fixed_bytes()
            .saturating_add(MIN_ROOT)
            .saturating_add(GPT_MARGIN)
    }

    pub fn root(&self) -> &Partition {
        self.partitions
            .iter()
            .find(|p| p.role == Role::Root)
            .expect("a plan always has a root")
    }

    /// Bytes taken by fixed-size partitions.
    fn fixed_bytes(&self) -> u64 {
        self.partitions
            .iter()
            .filter_map(|p| p.size)
            .fold(0, u64::saturating_add)
    }

    /// Device node of the n-th partition (1-based): `sda1`, `nvme0n1p1`.
    pub fn device(&self, index: usize) -> String {
        let separator = if self.disk.ends_with(|c: char| c.is_ascii_digit()) {
            "p"
        } else {
            ""
        };
        format!("{}{separator}{index}", self.disk)
    }

    /// One `repart.d` file per partition, in disk order: `(file name, contents)`.
    pub fn repart_definitions(&self) -> Vec<(String, String)> {
        self.partitions
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut text = format!(
                    "[Partition]\nType={}\nLabel={}\n",
                    p.role.repart_type(),
                    p.label
                );
                match p.size {
                    Some(bytes) => {
                        write!(text, "SizeMinBytes={bytes}\nSizeMaxBytes={bytes}\n").unwrap();
                    }
                    None => write!(text, "SizeMinBytes={MIN_ROOT}\nWeight=1000\n").unwrap(),
                }
                (format!("{:02}-{}.conf", (i + 1) * 10, p.label), text)
            })
            .collect()
    }

    /// Human-readable plan for `deli plan` and confirmation prompts.
    pub fn explain(&self) -> String {
        let mut out = format!("{}\n", self.disk);
        for (i, p) in self.partitions.iter().enumerate() {
            let size = p.size.map_or_else(|| "rest".to_owned(), human);
            let format = p.format.to_string();
            writeln!(
                out,
                "  {:<14} {:>8}  {:<6} {:<5} {}",
                self.device(i + 1),
                size,
                format,
                p.label,
                p.mount.unwrap_or("")
            )
            .unwrap();
            if p.role == Role::Root {
                for sv in &self.subvolumes {
                    writeln!(
                        out,
                        "  {:<14} {:>8}  {:<6} {:<5} {}",
                        "", "", "subvol", sv.name, sv.mount
                    )
                    .unwrap();
                }
            }
        }
        out
    }
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(layout: Layout, fs: Filesystem) -> Storage {
        Storage {
            layout,
            filesystem: fs,
            ..Storage::default()
        }
    }

    #[test]
    fn simple_btrfs() {
        let plan = Plan::new(&storage(Layout::Simple, Filesystem::Btrfs), "/dev/sda");
        let roles: Vec<_> = plan.partitions.iter().map(|p| p.role).collect();
        assert_eq!(roles, [Role::Esp, Role::Root]);
        let subvolumes: Vec<_> = plan.subvolumes.iter().map(|s| s.name).collect();
        assert_eq!(subvolumes, ["@", "@home", "@log", "@snapshots"]);
        assert_eq!(plan.device(2), "/dev/sda2");
    }

    #[test]
    fn server_partitions_replace_subvolumes() {
        let mut s = storage(Layout::Server, Filesystem::Btrfs);
        s.swap = "2G".into();
        s.var_size = Some("10G".into());
        s.home_size = Some("20G".into());
        let plan = Plan::new(&s, "/dev/nvme0n1");
        let labels: Vec<_> = plan.partitions.iter().map(|p| p.label).collect();
        assert_eq!(labels, ["esp", "swap", "root", "var", "home"]);
        let subvolumes: Vec<_> = plan.subvolumes.iter().map(|s| s.name).collect();
        assert_eq!(subvolumes, ["@", "@snapshots"]);
        assert_eq!(plan.device(3), "/dev/nvme0n1p3");
    }

    #[test]
    fn ext4_has_no_subvolumes() {
        let plan = Plan::new(&storage(Layout::Simple, Filesystem::Ext4), "/dev/vda");
        assert_eq!(plan.subvolumes.len(), 0);
    }

    #[test]
    fn repart_definitions_are_ordered_and_sized() {
        let plan = Plan::new(&storage(Layout::Simple, Filesystem::Ext4), "/dev/sda");
        let defs = plan.repart_definitions();
        assert_eq!(defs[0].0, "10-esp.conf");
        assert!(defs[0].1.contains("Type=esp\n"));
        assert!(
            defs[0]
                .1
                .contains(&format!("SizeMaxBytes={}\n", 1u64 << 30))
        );
        assert_eq!(defs[1].0, "20-root.conf");
        assert!(defs[1].1.contains("Weight=1000"));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(1 << 30), "1G");
        assert_eq!(human(3 << 29), "1.5G");
        assert_eq!(human(512 << 20), "512M");
    }
}
