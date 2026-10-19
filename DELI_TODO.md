# DELI — what is missing

What DELI 0.1 lacks compared with other server installers (Ubuntu's Subiquity, Fedora's Anaconda and kickstart, debian-installer, the Proxmox VE installer), checked against the code. Ordered by importance for a server installer.

## Next release

1. RAID1 over two disks.
2. LUKS unlocked by TPM2.
3. VLAN, bonding and bridge.
4. Late commands.

In the longer run, binary packages for updates: that is what will keep DELI usable over time.

## 1. Storage, the largest gap

- **LUKS encryption** with an unattended unlock: TPM2 (`systemd-cryptenroll`) or network (clevis and tang). Without it, an encrypted server waits for a passphrase at every boot.
- **Software RAID** over two disks: mdadm or btrfs RAID1, with an ESP on each disk. It is the most common setup on bare metal, and DELI uses a single disk.
- **LVM**, and partitions spread over several disks (`/var` on a second disk, for example).
- **Snapshots in use**: the `@snapshots` subvolume exists, but nothing uses it (snapper, a snapshot before updates, booting into a snapshot).
- **Layout choice in the wizard**: it offers only the filesystem and swap; the `server` layout (`/var`, `/home`) needs a configuration file.

## 2. Bare metal

- **Secure Boot**, through shim or signed UKIs: many machines enable it by default.
- **Wizard on a serial console** (IPMI SOL, iDRAC): plain mode already exists, it needs to run on `ttyS0`.
- **Disk health** before erasing (SMART), and a memory test in the boot menu.

## 3. Network

- **VLAN, bonding and LACP, bridge**: a bridge is essential for a virtualization host.
- **Dual-stack static addressing**, IPv4 and IPv6: only one address can be given today.
- **Autoinstall on a network without DHCP**: the interactive installer applies its network settings to the live session, an unattended installation does not yet, so GitHub keys and the repository sync need DHCP there.
- **HTTP proxy**, for fetching `deli.autoinstall=<url>`, GitHub keys and `cave sync`.
- Choice of interface and MTU when there are several NICs.
- Configurable NTP servers.

## 4. Fleet deployment

- **Network boot** (PXE, iPXE or UEFI HTTP boot): serve the kernel, initramfs and squashfs, with no ISO to mount.
- **One configuration for a fleet**: a single file, with a section per machine matched by MAC address or serial number (name, address, disk).
- **Custom scripts and files**: commands at the end of installation or at first boot, files to drop in place (the equivalent of Ubuntu's `late-commands`).
- **Status reporting**: a webhook called at start, end or failure, to follow 50 installations without watching 50 consoles.

## 5. After installation, Exherbo's own weak point

- **Updates without rebuilding everything**: a binary package repository (pbins), at least for the kernel and the base system. Today, updating the server means compiling.
- **Extra packages at install time**: the ISO is fixed; optional prebuilt sets are needed (containers, databases…).
- **A default firewall**: the kernel has nftables, but the `nft` tool is not installed.
- Several users and groups.

## 6. Comfort and polish

- **Rescue mode**: `deli rescue` finds the installed system, mounts it and enters it with chroot or nspawn.
- **Reinstall while keeping `/home` or `/var`.**
- **Getting the log of a failure** out: to a USB stick, a URL, or a QR code on screen.
- **Signed ISO** (SHA256SUMS and a signature) and a reproducible build, for public releases.
- Translation of the interface, and aarch64.

## Out of scope

- Legacy BIOS: few machines still in service need it.
- A graphical desktop: outside DELI's purpose.
