#!/bin/sh
# Second half of build.sh, run in an Alpine container as root:
# /out/work holds rootfs.tar, vmlinuz, initrd.img, BOOTX64.EFI.
set -eu

apk add --no-cache tar squashfs-tools xorriso mtools dosfstools >/dev/null

work=/out/work
root=/tmp/root
stage=/tmp/iso

mkdir -p "$root"
tar -C "$root" --numeric-owner --xattrs --xattrs-include='*' --acls -xpf "$work/rootfs.tar"

# Docker leaves its own view of these; the live system gets real ones.
rm -f "$root/.dockerenv" "$root/etc/hostname" "$root/etc/machine-id"
printf '127.0.0.1 localhost\n::1 localhost\n' > "$root/etc/hosts"
# The stage's fstab lists placeholders (/dev/ROOT, /dev/SWAP): systemd would
# wait 90 s for each. dracut sets up the live root; the installer writes the
# installed system's own fstab.
printf '# deli live session: nothing to mount.\n' > "$root/etc/fstab"
ln -sfn ../run/systemd/resolve/stub-resolv.conf "$root/etc/resolv.conf"

mkdir -p "$stage/LiveOS"
mksquashfs "$root" "$stage/LiveOS/squashfs.img" -comp zstd -Xcompression-level 15 -b 1M -xattrs -noappend -quiet

# The live kernel command line. systemd.firstboot=off: the live system has no
# machine-id and would otherwise stop on tty1 to ask for a locale and a password.
options="root=live:CDLABEL=DELI rd.live.image rd.live.overlay.overlayfs=1 deli.live systemd.firstboot=off systemd.hostname=deli console=ttyS0,115200 console=tty0"

mkdir -p "$stage/EFI"
esp="$stage/EFI/efiboot.img"
size_kib=$(( ($(stat -c %s "$work/vmlinuz") + $(stat -c %s "$work/initrd.img")) / 1024 + 32768 ))
# FAT size left to mkfs.vfat (FAT16 here), like archiso: OVMF did not boot
# the same image formatted as FAT32 from El Torito.
mkfs.vfat -C -n DELI-ESP "$esp" "$size_kib" >/dev/null
mmd -i "$esp" ::/EFI ::/EFI/BOOT ::/loader ::/loader/entries
mcopy -i "$esp" "$work/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$esp" "$work/vmlinuz" ::/vmlinuz
mcopy -i "$esp" "$work/initrd.img" ::/initrd.img
printf 'timeout 5\ndefault deli.conf\nconsole-mode keep\neditor yes\n' > /tmp/loader.conf
mcopy -i "$esp" /tmp/loader.conf ::/loader/loader.conf
printf 'title deli %s (Exherbo, kernel %s)\nlinux /vmlinuz\ninitrd /initrd.img\noptions %s\n' "$VERSION" "$KVER" "$options" > /tmp/deli.conf
printf 'title deli %s, copied to RAM\nlinux /vmlinuz\ninitrd /initrd.img\noptions %s rd.live.ram=1\n' "$VERSION" "$options" > /tmp/deli-ram.conf
mcopy -i "$esp" /tmp/deli.conf ::/loader/entries/deli.conf
mcopy -i "$esp" /tmp/deli-ram.conf ::/loader/entries/deli-ram.conf

# Hybrid image: El Torito UEFI boot from a CD, and a protective MBR + GPT
# whose ESP partition is the same image, for USB sticks
# (-isohybrid-gpt-basdat alone writes no table without an isohybrid MBR). The image lives inside the ISO9660 volume: past
# 32 MiB, El Torito's size field is 0 and OVMF sizes the image as "from its
# start to the end of the volume", which is nothing for an image appended
# after the volume.
rm -f /out/deli.iso.partial
xorriso -as mkisofs \
    -o /out/deli.iso.partial \
    -V DELI -iso-level 3 -full-iso9660-filenames -joliet -rational-rock \
    -e EFI/efiboot.img -no-emul-boot -efi-boot-part --efi-boot-image \
    "$stage" > /tmp/xorriso.log 2>&1 || { cat /tmp/xorriso.log >&2; exit 1; }
# Both boot paths must be there.
report="$(xorriso -indev /out/deli.iso.partial -report_system_area plain -report_el_torito plain 2>/dev/null)"
echo "$report" | grep -q 'El Torito boot img : *1 *UEFI' || { echo "no UEFI El Torito entry" >&2; exit 1; }
echo "$report" | grep -qi 'GPT type GUID *: *[0-9]* *28732ac11ff8d211ba4b00a0c93ec93b' || { echo "no ESP in the GPT" >&2; exit 1; }
mv /out/deli.iso.partial /out/deli.iso
