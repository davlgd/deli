# DELI — design

DELI (Davlgd Exherbo Linux Installer, `deli` on the command line) is an installer for Exherbo Linux, shipped on its own installation ISO, in the spirit of Ubuntu Server's: boot the ISO, answer a few questions (or none, with an autoinstall file), and get a server system that boots and answers SSH. The first target is servers only.

It follows earlier attempts: an Arch Linux installer, whose architecture and tests it reuses, and a first Exherbo installer driven over SSH, whose lessons about Exherbo are listed at the end.

## Shape of the product

```
deli-<version>-x86_64.iso
├── EFI system partition image: systemd-boot, kernel, live initramfs
└── LiveOS/squashfs.img: an Exherbo server system, kernel modules included
        └── /usr/local/bin/deli, started on tty1 when booted from the ISO
```

- **The ISO boots a live Exherbo**, built from the same package set as the system it installs. dracut's `dmsquash-live` mounts the squashfs with an overlay; the kernel command line carries `deli.live`.
- **Installing copies the live root filesystem to the disk**, then configures it, like Ubuntu's curtin copies its squashfs. Consequences:
  - installation is offline and takes minutes, not the hours a source distribution would need;
  - the kernel is compiled once, when the ISO is built, and the installed system receives it complete with its modules;
  - what you test in the live session is what you get.
- **The installed system maintains itself.** The kernel lives in `/usr/lib/modules/<kver>/vmlinuz`, `kernel-install` and dracut generate its initramfs and boot entry, and systemd-boot boots it. A later kernel built on the system goes through the same path. The first deli reused the live system's kernel and built the initramfs with Arch's mkinitcpio, which left the installed system unable to rebuild either.
- **Live-only files are removed after the copy.** They are listed in `/usr/share/deli/live-manifest`, which the end-to-end test also checks against the installed system. The installer's unit is guarded by `ConditionKernelCommandLine=deli.live`, so a forgotten copy would stay inert; the other live-only files (tty2's autologin, the network and resolver settings, the sshd drop-in that lets root in with a password, the SSH login script) rely on the purge alone.

## Two modes, one pipeline

- **Autoinstall**: `deli install`, which the live session runs on tty1 before handing it to a root shell, looks for a configuration, in order: `--config <file>`, `deli.toml` in the current directory, then, in the live session only (`deli.live` on the kernel command line), `deli.autoinstall=<url>` on the kernel command line and a filesystem labelled `CIDATA` or `DELICFG` (a second ISO, a USB key, a Proxmox cloud-init drive). If found, it installs, then reboots: without asking only with `--yes`, when the kernel command line has `deli.autoinstall`, or when the file says `install.confirm = false`; staying in the live session when it says `install.reboot = false`.
- **Interactive**: without a configuration, the live session sets a password for root made for this boot (twelve characters that read well off a screen) and starts sshd, which accepts it (`/etc/ssh/sshd_config.d/80-deli-live.conf`, a live-only file); a terminal UI on tty1 opens on a welcome screen that says whether the machine can be installed (UEFI, a candidate disk), then asks for the keyboard (loaded as soon as it is chosen, so passwords are typed in the layout the console will keep), language and time zone, network, disk and filesystem, host name, the administrator (password, sudo or a root password instead, SSH key, GitHub user), the repositories to add (all those offered, to leave out the ones not wanted) and packages for the world set. The network settings are applied to the live session too (a unit in `/run/systemd/network` that sorts before the live one, a resolved drop-in that sorts after it), so the installer can check that they reach the Internet and fetch with them; when they do not, it asks whether to review them or go on offline, which leaves out the GitHub user and the repositories, since adding one clones it. A summary and a last confirmation, defaulting to Back, come before anything is written. The installation then runs under the same screens: the step, a journal, and at the end how to reach the new system, or where it stopped and whether the disk was touched. Its only product is the `Config` a `deli.toml` would hold, checked screen by screen by the same validation, and kept on the installed system in `/var/log/deli/deli.toml`, secrets removed, ready to be replayed. `deli install` runs it from any terminal when it finds no configuration, `--dry-run` included; a root login over SSH starts it (`/etc/profile.d/90-deli-live.sh`), hangups ignored, so a lost connection leaves an installation to finish without a terminal. An installation holds `/run/deli/install.lock`: the console's installer and one over SSH cannot both write to a disk.

Both end in the same installation, which validates the whole configuration before touching anything.

**Nothing that can be checked is left for after the disk is erased.** Copying, dracut or the disk itself can still fail; everything a configuration or this machine can get wrong is caught first. Validation covers sizes and their minimums, addresses, SSH keys (parsed, then checked by `ssh-keygen` on the live system), password hashes (complete SHA-512, SHA-256, yescrypt or bcrypt), and whether anyone could log in at all. Preflight then checks the machine: UEFI, the disk and its size with a margin for GPT and alignment, the timezone, locale and keymap in the system about to be copied, and fetches GitHub keys. `storage.disk = "auto"` only works with exactly one candidate disk: deli never picks between disks, one of them may hold data.

**Secrets never reach argv, logs or errors.** Password hashes go to `chpasswd` on stdin, `Secret` hides itself from `Debug`, and a TOML parse error shows only its line when the message would quote a value. An autoinstall URL is logged and reported without its credentials, query or fragment; curl still takes it whole on its command line, in the live session only.

**An unattended install must not loop.** Ejecting the CD usually fails, since the live root is read from it, and a USB stick cannot be ejected at all. Instead: `bootctl` puts the installed system first in the firmware's boot order, and the installed system records the SHA-256 of the configuration that produced it (`/etc/deli/config.sha256`). An unattended live session that finds the same fingerprint on the target disk refuses to install again, unless the kernel command line says `deli.reinstall`. A different configuration installs normally. (Powering off instead of rebooting was considered: it would need a person at every machine.)

## The installer

Rust, a single static (musl) binary:

- **`Sys` trait**: every side effect (run a command, write a file) goes through it. `RealSys` acts; `DrySys` records and prints. `--dry-run` swaps the implementation, and unit tests assert on the recorded actions.
- **`Probe`**: read-only inspection of the machine (disks, firmware, boot medium, kernel command line), kept apart from effects so a dry run sees the real machine.
- **`pipeline()`**: one ordered list of steps. A step is a struct with `name()` and `run(&mut Context)`.
- **Config**: one serde struct tree, `#[serde(default)]` everywhere sensible, validated as a whole before the first destructive step. Secrets are typed so they never print.
- **Wizard** (`src/wizard/`): `form.rs` is the model (screens, controls, the key contract, `Config` out) and `progress.rs` the running installation's state; neither touches the terminal, so tests drive them with key presses. `view.rs` draws them with ratatui after the mockup in `docs/design/tui/`, for 80x24 first: no boxes around the screen, a thin step bar under the header, one strong focus (`›`, `▌`, accent), fields on a quiet surface, keys shown as keycaps, a screen of its own for the erase confirmation that names the disk and what it holds now. From 100 columns the steps show on the left, from 120 a word about the screen on the right; the form scrolls with the focus, the buttons stay in sight, errors sit under their field. `theme.rs` holds the palette (a dark ground, one accent, a colour each for errors, success and warnings) and the symbols, with their ASCII stand-ins. `mod.rs` owns the terminal: on a Linux VT it loads the `eurlatgr` console font and reprograms the console's 8 colours to the palette with `setvtrgb` (the kernel keeps one palette for every console, so the shell on Alt+F2 has it too), then sets back the VGA default the live session starts with when it leaves; a terminal emulator gets it in true colour or 256 colours, as it announces; an unknown terminal or a serial line gets plain mode (ASCII, reverse video), which Ctrl+T toggles on every screen but the running installation's. The installation runs on a scoped thread; progress comes back as typed events, commands' output goes to `/run/deli/commands.log`, never to the screen, and nothing interrupts it. The password is hashed by `openssl passwd -6`, on stdin; drafts validated along the way carry a stand-in hash that never reaches an installation. `cargo test preview -- --ignored` renders every screen to `target/wizard-preview.html`.
- **Install log and config** (without secrets) are written into the target under `/var/log/deli/`, including when a step fails.

### Steps

The installer counts fourteen steps (`steps/mod.rs`); grouped here by what they do:

1. **preflight**: UEFI firmware, target disk exists, is not the boot medium, is large enough; the live root is mounted where expected.
2. **release**: let go of what the live system holds on the disk (swap, mounts); refuse a disk that device-mapper or RAID still uses, rather than tearing those down.
3. **partition**: `systemd-repart` with Discoverable Partitions type GUIDs. `--dry-run=no` is mandatory (it simulates by default).
4. **format and mount**.
5. **copy**: the live root (the squashfs' read-only lower layer, not the overlay) to the target, preserving ownership, modes, ACLs, xattrs and capabilities.
6. **purge**: remove live-only files listed in the manifest.
7. **configure**: fstab by UUID, hostname and hosts, machine-id, locale, timezone, keymap (`systemd-firstboot --root`), network (`systemd-networkd`, DHCP or static, `ClientIdentifier=mac`), DNS (`systemd-resolved`), persistent journal.
8. **users and access**: root locked unless a password hash is given, an admin user with sudo (`/etc/sudoers.d`, mode 0440) unless `admin.sudo = false`, which then needs root's password and puts the account in `wheel` only for `su` (Exherbo's PAM restricts su to wheel; wheel has no sudoers rule), SSH keys (inline or fetched from GitHub), sshd hardening.
9. **boot**: `bootctl install`, `/etc/kernel/cmdline`, then `kernel-install add` inside the target (`systemd-nspawn`), which runs dracut with `hostonly=no` so the disk can move to other hardware, every CPU vendor's microcode in front of the initramfs.
10. **repositories and world**: one `cave resolve -x repository/<name>…` in the target for all the repositories asked for, which writes their configuration and clones them with those they depend on (one call per repository loaded paludis' whole configuration each time, and failed on a repository an earlier one had brought along); `cave update-world` for the packages listed, which only adds them to the world set (nothing is built during an installation: the first `cave resolve -x world` does it, and cave only checks then that they exist); then, if asked, `cave sync` and `cave generate-metadata`. Their names are checked against the `unavailable` and `unavailable-unofficial` indexes (the wizard offers a selection, in `repositories.rs`), and Exherbo's git reached, before the disk is touched; a sync that still fails is a warning, since the system is installed and boots.
11. **finish**: write the log, then the fingerprint (last, so only a complete installation has one), sync, unmount. Rebooting is the caller's: `deli install` after a configuration file (ejecting a CD first), or the wizard when asked to.

### Storage (v1)

Two layouts, both UEFI and GPT:

- `simple`: ESP (1 GiB, mounted on `/boot`) and a root partition filling the disk.
- `server`: ESP, optional swap partition, root, and optional `/var` and `/home` partitions with fixed sizes.

Filesystems: btrfs (default, with subvolumes `@`, `@home`, `@log`, `@snapshots`), ext4 or xfs. LVM, LUKS and RAID are planned.

## Building the images

`iso/build.sh` runs on any x86_64 Linux host with Docker:

1. **rootfs** (`iso/Containerfile`): from the official Exherbo stage image, configure paludis, install the server package set, build the kernel from kernel.org sources with `defconfig` plus `iso/kernel/server.config` (virtio, common server storage and network drivers, squashfs/overlay/iso9660 for the live boot), install deli's live files.
2. **export** the container filesystem, then build the squashfs.
3. **live initramfs**: dracut with `dmsquash-live`, run inside the rootfs.
4. **ESP image**: systemd-boot (taken from the rootfs), kernel, initramfs, loader entries.
5. **ISO**: `xorriso`, hybrid (bootable as a CD and as a USB stick), labelled `DELI`.

Docker layer caching keeps the expensive part (compiling) out of the iteration loop when only deli or the live files change.

## The cloud image

`deli-<version>-x86_64-cloud.qcow2` is the installed system for virtual machines and clouds, configured on its first boot by cloud-init rather than by an installer.

- **Its own stage.** `cloud` starts from `system`, adds cloud-init and growpart from the `davlgd` repository (github.com/davlgd/exheres, not in Exherbo's indexes, so declared in `iso/cloud/etc/paludis/repositories/`) and OpenBSD's netcat from `net` (cloud-init's units signal its stages with `nc -U`), drops linux-firmware and intel-microcode (VirtIO devices load no firmware, the host applies the microcode; a passed-through device may need linux-firmware installed), and enables what an installation enables plus cloud-init's five units. cloud-init renders its network for systemd-networkd (`iso/cloud/etc/cloud/cloud.cfg.d/10-deli.cfg`).
- **The installation's steps, on a file.** `deli image --source <root> <file>` creates a sparse 10 GiB file, attaches it to a loop device and runs the steps that make a system bootable: partition, format, mount, copy, boot; the same layout as an installation's `simple` one. `build.sh` runs it in a privileged container of the live image, which has every tool, with the host's `/dev` (the loop device's partitions appear there), then `qemu-img convert -c`.
- **Nothing of one machine.** Instead of the accounts, network and host name steps, `Blank` removes what identifies a machine (`/etc/hostname`, D-Bus's machine-id, systemd's random seed and credential secret, SSH host keys, cloud-init's state) and writes an empty `/etc/machine-id` before the boot step, so the initramfs copies it empty too. Empty, not missing: systemd then makes an ID on the first boot without treating it as one, so it applies no presets (Exherbo's enable every unit they do not name, dhcpcd included) and systemd-firstboot asks nothing on the console. `/etc/kernel/entry-token` says `exherbo`, so kernels installed later go where the image's went. `bootctl` gets `--root` (the image's token, not the build host's), no EFI variables and no random seed; `Seal` fails the build if any of that state came back, then unmounts.
- **On Proxmox VE**, the image boots in UEFI mode (OVMF, an EFI disk without enrolled keys) from a disk imported with `import-from`, and a cloud-init drive (NoCloud) gives the user, keys and network. `ciupgrade` must be off: otherwise the first boot asks for an upgrade of the world set, which on Exherbo can mean a long build from source.

## Testing

- **Unit**: steps run against `DrySys`, assertions on recorded actions. Runs anywhere (`cargo test`).
- **Dry run of the binary**: `deli install --dry-run` on a sample config.
- **End to end on Proxmox**: upload the ISO, create a VM with a `CIDATA` drive carrying a `deli.toml`, boot, wait for the installed system to answer SSH with the expected hostname, check it, then boot the ISO again and expect a refusal. With `--interactive`, no drive: the test follows the live log through qemu-guest-agent and types the answers with the API's `sendkey`. One command, repeatable.
- **The cloud image on Proxmox** (`test/e2e/cloud.ts`): import the qcow2 as the disk of a VM, grown beyond the image, with a cloud-init drive giving a user and a key; check what cloud-init made (its status, the NoCloud source, the grown root, sudo, the network, no firmware), then that a reboot keeps the machine-id and the host keys.

## Lessons learned

What Exherbo, systemd and the tools need, learned the hard way:
- `systemd-repart` needs `--dry-run=no`; with correct GPT type GUIDs, the live system may auto-mount a previous install's partitions, so release the disk first.
- `ClientIdentifier=mac` in networkd, or the machine changes IP between the live and installed systems.
- Exherbo builds systemd-resolved with DNSSEC `allow-downgrade`; behind a resolver that strips signatures (as some ISP boxes do), every query fails with "DNSSEC validation failed: no-signature". A configuration file gets `DNSSEC=no` unless it asks for validation; the interactive installer asks the session's resolvers for the root zone's SOA with the DNSSEC OK bit (`system/resolvers.rs`) and preselects Required (`yes`) only when every one returns its signature, Off otherwise: Allow downgrade stays offered for a server that will move to other networks, since Required leaves it without DNS behind resolvers that lack DNSSEC. deli uses DHCP's resolvers unless given others: forcing public resolvers was tried, and broke as soon as someone set their own.
- dracut needs `sys-apps/coreutils xattr`; `cave` exits with code 3 ("Cannot proceed without: user configuration changes") when an option is missing.
- `cave resolve -x` exits 0 without installing anything when it needs a permission it was not given (`--permit-downgrade`, `--permit-old-version`): never trust its exit code alone, check that the targets are installed.
- A freshly declared paludis repository is named `x-<name>` until a global `cave sync`.
- In Docker, `paludisbuild` must be in group 0 to build; installed systems must not keep that, so the system stage removes it once everything is built.
- `rm -rf /var/tmp/paludis/*` removes `build/`, which paludis refuses to recreate: clean its contents only.
- sudo ignores a sudoers file that is group- or world-writable.
- `tcp_bbr` must be loaded before its sysctl applies.
- Check the installed system by its hostname after reboot, not just by SSH answering.
- `firmware/linux-firmware` and `firmware/intel-microcode` live in the `hardware` repository, which depends on `x11`. intel-microcode installs a kernel-install hook that adds its own microcode initrd; dracut's `early_microcode` already covers Intel and AMD, so the target masks the hook.
- In `systemd-nspawn`, `/run` is a new tmpfs: the target's `/etc/resolv.conf` points to `/run/systemd/resolve/stub-resolv.conf`, which only exists there once bound from the live session. Network commands run in the target need it: `cave sync`, and `cave resolve -x repository/<name>`, which clones the repository it adds.
- The live system has no machine-id, so systemd applies the presets at boot, and Exherbo ships no rule that disables the rest: every installed unit gets enabled, dhcpcd (a second DHCP client, a second address), git-daemon and rsync included. A live-only preset (`10-deli-live.preset`) disables them.
- Installed systems and the cloud image boot with a machine-id (written, or empty on purpose), so systemd does not apply the first-boot presets on them: what a server needs from Exherbo's presets is enabled at build time: `getty@.service` (a login on tty1 and, through its `autovt@` alias, on the other consoles; without it, a hypervisor's console shows no login prompt), `remote-fs.target` (network filesystems in fstab) and `systemd-boot-update.service` (the boot loader on the ESP follows systemd's upgrades).
- `networkctl reload` applies a changed `.network` and keeps the DHCP lease; `networkctl reconfigure` releases it, and the server may hand out another address, ending the SSH session to the installer. Switching an SSH session's machine to a static address keeps the old one too in the live unit, for the same reason; that only saves the session on the same network: with another subnet or gateway, the route to the client goes and the session ends.
- `cave resolve -x repository/<a>` then `repository/<b>` fails when adding `a` brought `b` along ("Masked by already configured"): all are added in one resolution.
- `cave generate-metadata` takes about five minutes on four cores for the ISO's repositories; `cave sync` is quick, since they are recent.
- `cave resolve -x repository/<name>` fails while another declared repository has never been synced ("No categories file available"): the cloud stage adds `net` before declaring `davlgd`.
- cloud-init 26's units signal its stages with `nc -U`: without OpenBSD's netcat they wait forever. Its `jsonschema` and `oauthlib` are optional (schema validation, MAAS and webhooks) and pull Rust and Python with SQLite into the build: the `davlgd` package suggests them only.
- The Exherbo stage ships `/etc/machine-id` and `/var/lib/dbus/machine-id`: an installation replaces the first, an image removes both.
- In a container without systemd, `systemd-nspawn` needs `--keep-unit` (no bus to create a scope with), which it refuses in a login session.
- udev may record a new partition table late, and an image's loop device belongs to the build host's udev, which may never record it (on GitHub's runners, lsblk found no names): deli reads the partitions' names from the disk itself (`lsblk --properties-by blkid`).
- bootctl reads and writes the machine-id and entry token of the root it runs in, unless given `--root`.
- Exherbo's GNU tar has no ACL support: `--acls` only warns.
- Paludis's git syncer resets to `origin/master` unless a repository's `sync_options` name a branch: the first clone works whatever the default branch, every later `cave sync` fails. `davlgd`'s default branch is `main`, so its configuration says `--branch=main`.
- Proxmox's cloud-init user data names the user with `user`, deprecated since cloud-init 22.2: the first boot ends "degraded done", the next ones "done".

Structure kept from the earlier installer: the `Sys`/`Probe` split, the pipeline list, the config as the single product of the UI, validation before any destructive step, the log written into the target, the three test levels, the static release binary.

Dropped on purpose: driving installation over SSH from a workstation, a PXE/libvirt test bench, reusing the live kernel, mkinitcpio.
