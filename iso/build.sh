#!/usr/bin/env bash
# Build the deli ISO and the cloud image. Runs on any x86_64 Linux host with
# Docker; the cloud image needs privileged containers and loop devices.
#
#   iso/build.sh            -> out/deli-<date>-x86_64.iso
#                              out/deli-<date>-x86_64-cloud.qcow2
#
# JOBS (default: nproc), VERSION (default: date), OUT (default: ./out).
# CACHE: a registry repository for BuildKit's cache (ghcr.io/<owner>/<name>),
# so a fresh machine reuses what an earlier build compiled. Each stage has
# its own tag, written once it is built: a build stopped by a time limit
# resumes where it got to.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"
out="$(realpath -m "${OUT:-$repo/out}")"
jobs="${JOBS:-$(nproc)}"
version="${VERSION:-$(date +%Y%m%d)}"
image="deli-live:$version"
cloud="deli-cloud:$version"
iso="$out/deli-$version-x86_64.iso"
qcow2="$out/deli-$version-x86_64-cloud.qcow2"

step() { printf '\n== %s\n' "$*"; }

# The root filesystem of image $1, as a tar archive at $2.
export_root() {
    local cid
    cid="$(docker create "$1" /bin/true)"
    docker export "$cid" > "$2" || { docker rm "$cid" >/dev/null; return 1; }
    docker rm "$cid" >/dev/null
}

checksum() { ( cd "$out" && sha256sum "$(basename "$1")" > "$(basename "$1").sha256" ); }

step "deli binary (static, musl)"
docker run --rm -v "$repo:/src" -w /src -v deli-cargo:/usr/local/cargo/registry \
    rust:1-alpine sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release --locked'
install -m 0755 "$repo/target/release/deli" "$here/deli"
install -m 0644 "$repo/examples/server.toml" "$here/server.toml"

step "system images (compiled on the first run, cached afterwards)"
if [ -n "${CACHE:-}" ]; then
    stages=(kernel base system cloud live)
    from=()
    for stage in "${stages[@]}"; do from+=(--cache-from "type=registry,ref=$CACHE:$stage"); done
    for stage in "${stages[@]}"; do
        case "$stage" in
            live) output=(--load -t "$image") ;;
            cloud) output=(--load -t "$cloud") ;;
            *) output=(--output type=cacheonly) ;;
        esac
        docker buildx build "${output[@]}" "${from[@]}" --cache-to "type=registry,ref=$CACHE:$stage,mode=max" \
            --target "$stage" --build-arg JOBS="$jobs" -f "$here/Containerfile" "$here"
    done
else
    for target in cloud live; do
        docker build --target "$target" --build-arg JOBS="$jobs" -t "deli-$target:$version" -f "$here/Containerfile" "$here"
    done
fi

rm -rf "$out/work" && mkdir -p "$out/work"
kver="$(docker run --rm "$image" ls /usr/x86_64-pc-linux-gnu/lib/modules)"
[ "$(printf '%s\n' "$kver" | wc -l)" -eq 1 ] || { echo "expected one kernel, got: $kver" >&2; exit 1; }

step "live initramfs for $kver"
docker run --rm -v "$out/work:/work" "$image" sh -c "
    dracut --force --no-hostonly --no-hostonly-cmdline --add 'dmsquash-live' --kver '$kver' /work/initrd.img \
 && cp /usr/x86_64-pc-linux-gnu/lib/modules/$kver/vmlinuz /work/vmlinuz \
 && cp /usr/x86_64-pc-linux-gnu/lib/systemd/boot/efi/systemd-bootx64.efi /work/BOOTX64.EFI"

step "root filesystem"
export_root "$image" "$out/work/rootfs.tar"

step "ISO"
docker run --rm -v "$out:/out" -v "$here/assemble.sh:/assemble.sh:ro" \
    -e VERSION="$version" -e KVER="$kver" alpine:3.22 sh /assemble.sh
mv "$out/deli.iso" "$iso"
rm -rf "$out/work"
checksum "$iso"

step "cloud image"
mkdir -p "$out/work"
export_root "$cloud" "$out/work/cloud.tar"
# deli makes it with the installation's own steps, on a loop device: the
# container sees the host's devices, partitions included as they appear.
docker run --rm --privileged -v /dev:/dev --tmpfs /run -v "$out/work:/work" "$image" sh -c '
    mkdir /work/root \
 && tar -C /work/root --numeric-owner --xattrs --xattrs-include="*" -xpf /work/cloud.tar \
 && deli image --source /work/root /work/deli.raw
    status=$?; rm -rf /work/root /work/cloud.tar; exit $status'
docker run --rm -v "$out:/out" alpine:3.22 sh -c "
    apk add --no-cache qemu-img >/dev/null \
 && qemu-img convert -O qcow2 -c /out/work/deli.raw '/out/$(basename "$qcow2")' \
 && rm /out/work/deli.raw"
rm -rf "$out/work"
checksum "$qcow2"
ls -lh "$iso" "$qcow2"
