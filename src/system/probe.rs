//! Read-only inspection of the machine deli runs on.
//!
//! Gathered once into [`Facts`] before anything happens, so steps and
//! validation work on plain data and tests can build their own.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::storage::human;
use crate::system::Sys;

/// Where dracut's dmsquash-live mounts the medium the ISO booted from.
pub const LIVE_MEDIUM_MOUNT: &str = "/run/initramfs/live";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Disk {
    pub path: String,
    pub size: u64,
    pub model: String,
    /// WWN or serial number: tells apart disks of the same model and size,
    /// whose kernel names may swap. Empty when the disk reports neither.
    pub serial: String,
    /// Its stable name under `/dev/disk/by-id`, when udev gave it one.
    pub by_id: Option<String>,
    pub removable: bool,
    pub read_only: bool,
    /// The partition table's type (`gpt`, `dos`), if lsblk recognised one.
    pub table: Option<String>,
    /// What is on it now: shown before it is erased.
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    pub path: String,
    pub size: u64,
    pub fstype: String,
    pub label: String,
}

impl Disk {
    /// `/dev/sda (32G, QEMU HARDDISK, drive-scsi0)`: the serial number tells
    /// twins apart.
    pub fn describe(&self) -> String {
        let details: Vec<String> = [human(self.size), self.model.clone(), self.serial.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        format!("{} ({})", self.path, details.join(", "))
    }

    /// What tells this disk from a like one: its serial and by-id name.
    pub fn identity(&self) -> String {
        let serial = (!self.serial.is_empty()).then(|| format!("serial {}", self.serial));
        let by_id = self.by_id.as_ref().map(|name| format!("by-id {name}"));
        [serial, by_id]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("   ")
    }

    pub fn contents(&self) -> String {
        if self.parts.is_empty() {
            return match &self.table {
                Some(table) => format!("an empty {table} partition table"),
                None => "no partition table lsblk recognises".to_owned(),
            };
        }
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
        self.parts
            .iter()
            .map(|p| {
                let label = if p.label.is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", p.label)
                };
                let fstype = if p.fstype.is_empty() {
                    "unknown"
                } else {
                    &p.fstype
                };
                format!("{} {} {fstype}{label}", name(&p.path), human(p.size))
            })
            .collect::<Vec<_>>()
            .join("   ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub uefi: bool,
    pub disks: Vec<Disk>,
    /// The disk the live system booted from, never a valid target.
    pub boot_disk: Option<String>,
}

impl Facts {
    pub fn gather() -> Result<Self> {
        Ok(Self {
            uefi: Path::new("/sys/firmware/efi").exists(),
            disks: disks()?,
            boot_disk: boot_disk(),
        })
    }

    pub fn disk(&self, path: &str) -> Option<&Disk> {
        let resolved = resolve(path);
        self.disks
            .iter()
            .find(|d| d.path == path || d.path == resolved)
    }

    /// Fixed, writable disks other than the boot medium.
    pub fn candidate_disks(&self) -> Vec<&Disk> {
        self.disks
            .iter()
            .filter(|d| !d.removable && !d.read_only && Some(&d.path) != self.boot_disk.as_ref())
            .collect()
    }
}

/// The value of `key=value` on a kernel command line; `Some("")` for a bare
/// `key`.
pub fn cmdline_arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_whitespace()
        .find_map(|word| match word.split_once('=') {
            Some((k, v)) if k == key => Some(v),
            None if word == key => Some(""),
            _ => None,
        })
}

/// Follow symlinks such as `/dev/disk/by-id/…` to the kernel name.
pub fn resolve(path: &str) -> String {
    fs::canonicalize(path).map_or_else(|_| path.to_owned(), |p| p.to_string_lossy().into_owned())
}

/// Each device's path and `column` (`LABEL`, `PARTLABEL`, `TYPE`…), from
/// `lsblk --list`: `device` and what it holds, or every device.
pub fn lsblk_column(
    sys: &dyn Sys,
    column: &str,
    device: Option<&str>,
) -> Result<Vec<(String, String)>> {
    lsblk(sys, column, device, &[])
}

/// The partitions of `disk` and their GPT names, read from the disk itself:
/// udev's may lag behind a new partition table, and the build host's udev,
/// which owns an image's loop device, may never record them.
pub fn partition_names(sys: &dyn Sys, disk: &str) -> Result<Vec<(String, String)>> {
    lsblk(sys, "PARTLABEL", Some(disk), &["--properties-by", "blkid"])
}

fn lsblk(
    sys: &dyn Sys,
    column: &str,
    device: Option<&str>,
    options: &[&str],
) -> Result<Vec<(String, String)>> {
    let output = format!("NAME,{column}");
    let mut command = crate::system::Command::new("lsblk")
        .args(["--list", "--noheadings", "--paths", "--output", &output])
        .args(options.iter().copied());
    if let Some(device) = device {
        command = command.arg(device);
    }
    Ok(sys
        .capture(&command)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (name, value) = line.split_once(' ').unwrap_or((line, ""));
            (name.trim().to_owned(), value.trim().to_owned())
        })
        .collect())
}

#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<LsblkDevice>,
}

#[derive(Clone, Deserialize)]
struct LsblkDevice {
    path: String,
    size: Option<u64>,
    #[serde(rename = "type")]
    kind: String,
    rm: bool,
    ro: bool,
    model: Option<String>,
    serial: Option<String>,
    wwn: Option<String>,
    pttype: Option<String>,
    fstype: Option<String>,
    label: Option<String>,
    #[serde(default)]
    children: Vec<LsblkDevice>,
}

fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::io(format!("cannot run {program}"), e))?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A disk's own words (model, serial, label), trimmed and with control
/// characters replaced: they are shown on a terminal.
fn printable(text: Option<String>) -> String {
    text.unwrap_or_default()
        .trim()
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn parse_lsblk(json: &str) -> Result<Vec<Disk>> {
    let parsed: Lsblk =
        serde_json::from_str(json).map_err(|e| Error::Other(format!("lsblk output: {e}")))?;
    Ok(parsed
        .blockdevices
        .into_iter()
        .filter(|d| d.kind == "disk")
        .map(|mut d| {
            let mut parts: Vec<Part> = std::mem::take(&mut d.children)
                .into_iter()
                .filter(|c| c.kind == "part")
                .map(Part::from)
                .collect();
            // A filesystem on the whole disk, without a partition table.
            if parts.is_empty() && d.fstype.is_some() {
                parts.push(Part::from(d.clone()));
            }
            Disk {
                path: d.path,
                size: d.size.unwrap_or(0),
                model: printable(d.model),
                serial: [d.wwn, d.serial]
                    .into_iter()
                    .map(printable)
                    .find(|s| !s.is_empty())
                    .unwrap_or_default(),
                by_id: None,
                removable: d.rm,
                read_only: d.ro,
                table: d.pttype,
                parts,
            }
        })
        .collect())
}

impl From<LsblkDevice> for Part {
    fn from(device: LsblkDevice) -> Self {
        Self {
            path: device.path,
            size: device.size.unwrap_or(0),
            fstype: printable(device.fstype),
            label: printable(device.label),
        }
    }
}

fn disks() -> Result<Vec<Disk>> {
    let mut disks = parse_lsblk(&output(
        "lsblk",
        &[
            "--json",
            "--tree",
            "--bytes",
            "--output",
            "PATH,SIZE,TYPE,RM,RO,MODEL,SERIAL,WWN,PTTYPE,FSTYPE,LABEL",
        ],
    )?)?;
    for disk in &mut disks {
        disk.by_id = by_id(&disk.path);
    }
    Ok(disks)
}

/// The disk's name under `/dev/disk/by-id`: the bus and model one rather
/// than its `wwn-`, which the serial number already shows.
fn by_id(path: &str) -> Option<String> {
    let mut names: Vec<String> = fs::read_dir("/dev/disk/by-id")
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|e| fs::canonicalize(e.path()).is_ok_and(|t| t == Path::new(path)))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by_key(|n| (n.starts_with("wwn-"), n.clone()));
    names.into_iter().next()
}

/// The disk the live system booted from.
pub fn boot_disk() -> Option<String> {
    let source = output(
        "findmnt",
        &["--noheadings", "--output", "SOURCE", LIVE_MEDIUM_MOUNT],
    )
    .ok()?;
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let parent = output(
        "lsblk",
        &["--noheadings", "--nodeps", "--output", "PKNAME", source],
    )
    .ok()?;
    match parent.trim() {
        "" => Some(source.to_owned()),
        name => Some(format!("/dev/{name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsblk_keeps_disks_only() {
        let json = r#"{"blockdevices":[
            {"path":"/dev/sda","size":34359738368,"type":"disk","rm":false,"ro":false,"model":"QEMU HARDDISK   ","serial":"drive-scsi0","wwn":null},
            {"path":"/dev/sr0","size":1073741824,"type":"rom","rm":true,"ro":false,"model":"QEMU DVD-ROM"},
            {"path":"/dev/loop0","size":null,"type":"loop","rm":false,"ro":true,"model":null}
        ]}"#;
        let disks = parse_lsblk(json).unwrap();
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].path, "/dev/sda");
        assert_eq!(disks[0].model, "QEMU HARDDISK");
        assert_eq!(disks[0].serial, "drive-scsi0");
    }

    #[test]
    fn lsblk_tells_what_a_disk_holds() {
        let json = r#"{"blockdevices":[
            {"path":"/dev/sda","size":34359738368,"type":"disk","rm":false,"ro":false,"model":"QEMU HARDDISK","pttype":"gpt","children":[
                {"path":"/dev/sda1","size":536870912,"type":"part","rm":false,"ro":false,"fstype":"vfat","label":"E\u001b[2JSP"},
                {"path":"/dev/sda2","size":33822867456,"type":"part","rm":false,"ro":false,"fstype":"crypto_LUKS","label":null,"children":[
                    {"path":"/dev/mapper/root","size":33806090240,"type":"crypt","rm":false,"ro":false,"fstype":"ext4"}
                ]}
            ]},
            {"path":"/dev/sdb","size":4294967296,"type":"disk","rm":false,"ro":false,"model":"QEMU HARDDISK","pttype":"gpt"},
            {"path":"/dev/sdc","size":4294967296,"type":"disk","rm":false,"ro":false,"model":"QEMU HARDDISK"},
            {"path":"/dev/sdd","size":4294967296,"type":"disk","rm":false,"ro":false,"model":"QEMU HARDDISK","fstype":"ext4","label":"data"}
        ]}"#;
        let disks = parse_lsblk(json).unwrap();
        assert_eq!(
            disks[0].contents(),
            "sda1 512M vfat \"E?[2JSP\"   sda2 31.5G crypto_LUKS"
        );
        assert_eq!(disks[1].contents(), "an empty gpt partition table");
        assert_eq!(disks[2].contents(), "no partition table lsblk recognises");
        assert_eq!(disks[3].contents(), "sdd 4G ext4 \"data\"");
    }

    #[test]
    fn candidates_skip_boot_medium_and_removable() {
        let disk = |path: &str, size: u64, removable: bool| Disk {
            path: path.into(),
            size,
            model: String::new(),
            serial: String::new(),
            removable,
            read_only: false,
            ..Default::default()
        };
        let facts = Facts {
            disks: vec![
                disk("/dev/sda", 100, false),
                disk("/dev/sdb", 500, false),
                disk("/dev/sdc", 900, true),
            ],
            boot_disk: Some("/dev/sdb".into()),
            ..Facts::default()
        };
        let candidates: Vec<_> = facts
            .candidate_disks()
            .iter()
            .map(|d| d.path.as_str())
            .collect();
        assert_eq!(candidates, ["/dev/sda"]);
    }

    #[test]
    fn cmdline_arguments() {
        let cmdline = "root=live:CDLABEL=DELI deli.live deli.autoinstall=http://x/y.toml quiet";
        assert_eq!(cmdline_arg(cmdline, "deli.live"), Some(""));
        assert_eq!(
            cmdline_arg(cmdline, "deli.autoinstall"),
            Some("http://x/y.toml")
        );
        assert_eq!(cmdline_arg(cmdline, "deli"), None);
    }
}
