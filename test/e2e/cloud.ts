/**
 * End-to-end test of the cloud image on a Proxmox VE host: import the qcow2
 * as a throwaway VM's disk, larger than the image, give it a cloud-init
 * drive with a user and a key, boot it, then check over SSH what
 * cloud-init made of it, and that a reboot keeps it. A second VM from the
 * image, configured by a cloud-config of its own, must be another machine
 * and get the packages that cloud-config lists.
 *
 *   bun test/e2e/cloud.ts out/deli-<version>-x86_64-cloud.qcow2
 *
 * Environment: see pve.ts, and DELI_PVE_IMPORT_STORE, a storage that holds
 * disk images to import (content "import"; default local).
 */

import { basename } from "node:path";

import { agentAnswers, api, bridge, check, destroy, diskStore, env, isoStore, key, log, MAC, NAME, node, ours, PALUDIS, reach, ssh, task, upload, uploadCidata, vmid } from "./pve";

const [imagePath] = process.argv.slice(2);
if (!imagePath) throw new Error("usage: bun test/e2e/cloud.ts <deli-cloud.qcow2>");
const importStore = env("DELI_PVE_IMPORT_STORE", "local");
const USER = "ops";
/** The VM's disk, larger than the image: the root must grow into it. */
const DISK_GB = 16;

const current = await ours();
log(`uploading ${basename(imagePath)}`);
const volume = await upload(imagePath, `deli-e2e-${vmid}.qcow2`, importStore, "import");
await destroy(current);

/** The cloud-config of the second VM, on a NoCloud volume of its own. */
const CIDATA = `deli-e2e-${vmid}-cidata.iso`;
const PACKAGE = "sys-process/htop";
const userData = `#cloud-config
users:
  - name: ${USER}
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - ${JSON.stringify(key)}
packages:
  - ${PACKAGE}
`;

/** A fresh VM from the image: a new SMBIOS UUID, a new cloud-init instance.
 * Configured by Proxmox's cloud-init drive, or by the volume `cidata`. */
async function create(cidata?: string): Promise<void> {
  log(`creating VM ${vmid} from it`);
  await task(
    await api("POST", `/nodes/${node}/qemu`, {
      vmid,
      name: NAME,
      ostype: "l26",
      machine: "q35",
      bios: "ovmf",
      efidisk0: `${diskStore}:1,efitype=4m,pre-enrolled-keys=0`,
      cpu: "x86-64-v2-AES",
      cores: 2,
      memory: 2048,
      scsihw: "virtio-scsi-single",
      scsi0: `${diskStore}:0,import-from=${volume},discard=on,ssd=1,iothread=1`,
      ...(cidata
        ? { ide2: `${isoStore}:iso/${cidata},media=cdrom` }
        : {
            ide2: `${diskStore}:cloudinit`,
            citype: "nocloud",
            ciuser: USER,
            // Proxmox wants the keys URL-encoded, spaces as %20.
            sshkeys: encodeURIComponent(`${key}\n`),
            ipconfig0: "ip=dhcp",
            // Proxmox otherwise asks for a package upgrade: on Exherbo,
            // building every update from source.
            ciupgrade: 0,
          }),
      net0: `virtio=${MAC},bridge=${bridge}`,
      serial0: "socket",
      agent: "enabled=1",
      boot: "order=scsi0",
    }),
  );
  await task(await api("PUT", `/nodes/${node}/qemu/${vmid}/resize`, { disk: "scsi0", size: `${DISK_GB}G` }));
  await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/start`));
}

/** Boot the VM until USER answers with the host name NAME; its address. */
async function boot(): Promise<string> {
  log("booting the image");
  // cloud-init names the machine after the VM and gives the key to USER.
  const answers = async (ip: string) => {
    const out = await ssh(`${USER}@${ip}`, "hostname");
    return out.exitCode === 0 && out.text().trim() === NAME;
  };
  const ip = await reach(answers, 60);
  if (!ip) throw new Error(`the image never answered as ${USER} with the host name ${NAME}`);
  log(`the image answers as ${USER}@${ip}`);
  return ip;
}

await create();
const ip = await boot();

const GB = 1 << 30;
// It exits 2, "degraded done", after an error it recovered from.
const STATUS = "timeout 300 cloud-init status --wait --format json || test $? -eq 2";
/** cloud-init finished without errors, but for one deprecation: Proxmox's
 * own user data still names the user with `user`, deprecated since 22.2. */
const parse = (json: string): any => {
  try {
    return JSON.parse(json);
  } catch {
    return {};
  }
};
const settled = (json: string) => {
  const status = parse(json);
  const recovered = Object.entries(status.recoverable_errors ?? {}) as [string, string[]][];
  return (
    status.status === "done" &&
    status.errors?.length === 0 &&
    recovered.every(([kind, messages]) => kind === "DEPRECATED" && messages.every((m) => m.startsWith("'user' of type string is deprecated")))
  );
};
const checks: [string, string, (out: string) => boolean][] = [
  ["cloud-init done, without errors", STATUS, (o) => settled(o)],
  ["configured from Proxmox's NoCloud drive", STATUS, (o) => parse(o).datasource === "nocloud"],
  ["root grown to the disk", "findmnt -bno SIZE /", (o) => Number(o.trim()) > (DISK_GB - 2) * GB],
  ["root on btrfs subvolume @", "findmnt -no FSTYPE,OPTIONS /", (o) => o.startsWith("btrfs") && o.includes("subvol=/@")],
  ["sudo without password", "sudo -n true && echo ok", (o) => o.includes("ok")],
  ["root locked", "sudo -n grep '^root:' /etc/shadow", (o) => /^root:[*!]/.test(o)],
  ["passwords refused over SSH", "sudo -n sshd -T | grep -i '^passwordauthentication '", (o) => o.toLowerCase().includes("no")],
  ["machine-id made on first boot", "cat /etc/machine-id", (o) => /^[0-9a-f]{32}$/.test(o.trim())],
  ["no failed units", "systemctl --failed --no-legend", (o) => o.trim() === ""],
  ["networkd manages the network", "networkctl list --no-legend", (o) => /routable\s+configured/.test(o)],
  ["DNS works", "getent hosts exherbo.org", (o) => o.trim() !== ""],
  ["boot entry under the entry token", "sudo -n sh -c 'ls /boot/loader/entries'", (o) => /^exherbo-.*\.conf$/m.test(o)],
  ["no firmware", "ls /lib/firmware 2>/dev/null | wc -l", (o) => o.trim() === "0"],
  ["dhcpcd not running", "systemctl is-active dhcpcd.service || true", (o) => o.trim() !== "active"],
  ["getty on tty1 active", "systemctl is-active getty@tty1.service", (o) => o.trim() === "active"],
  ["remote-fs and boot loader updates enabled", "systemctl is-enabled remote-fs.target systemd-boot-update.service", (o) => o.trim().split(/\s+/).every((s) => s === "enabled")],
  // davlgd included: its branch is not Paludis's default.
  ...PALUDIS,
];
let failed = await check(`${USER}@${ip}`, checks);
const agent = await agentAnswers();
log(`${agent ? "ok  " : "FAIL"} qemu-guest-agent answers the hypervisor`);
if (!agent) failed++;

// --- what the first boot made, a reboot keeps
const identity = "cat /etc/machine-id; sudo -n cat /etc/ssh/ssh_host_ed25519_key.pub";
const bootId = "cat /proc/sys/kernel/random/boot_id";
const before = await ssh(`${USER}@${ip}`, identity);
const firstBoot = (await ssh(`${USER}@${ip}`, bootId)).text().trim();
if (before.exitCode !== 0 || !before.text().trim() || !firstBoot) throw new Error("cannot read the image's identity");
await ssh(`${USER}@${ip}`, "sudo -n systemctl reboot");
// The old boot answers for a moment after the reboot is asked: wait for a new one.
const kept = await reach(async (addr) => {
  const boot = await ssh(`${USER}@${addr}`, bootId);
  if (boot.exitCode !== 0 || boot.text().trim() === firstBoot) return false;
  const status = await ssh(`${USER}@${addr}`, STATUS);
  const after = await ssh(`${USER}@${addr}`, identity);
  return status.exitCode === 0 && settled(status.text()) && after.exitCode === 0 && after.text() === before.text();
}, 40);
log(`${kept ? "ok  " : "FAIL"} machine-id, host key and cloud-init state kept across a reboot`);
if (!kept) failed++;

// --- another VM from the same image is another machine
await destroy(await ours());
await uploadCidata({ "meta-data": `instance-id: deli-e2e-${Date.now()}\nlocal-hostname: ${NAME}\n`, "user-data": userData }, CIDATA);
await create(CIDATA);
const clone = await boot();
const other = await ssh(`${USER}@${clone}`, identity);
const distinct = other.exitCode === 0 && other.text().trim() !== "" && other.text().split("\n").every((line) => !line || !before.text().includes(line));
log(`${distinct ? "ok  " : "FAIL"} a second VM from the image has its own machine-id and host key`);
if (!distinct) failed++;
// cloud-init syncs the repositories, then builds the package, before it is done.
const status = await ssh(`${USER}@${clone}`, "timeout 60m cloud-init status --wait --format json || test $? -eq 2");
const installed = await ssh(`${USER}@${clone}`, `htop --version && grep -x ${PACKAGE} /var/db/paludis/repositories/installed/world`);
const packages = status.exitCode === 0 && settled(status.text()) && installed.exitCode === 0;
log(`${packages ? "ok  " : "FAIL"} the packages of a cloud-config installed on first boot`);
if (!packages) {
  log(`${status.text().trim()}\n${installed.text().trim()}`);
  failed++;
}
const total = checks.length + 4;
log(`${total - failed}/${total} checks passed`);
process.exit(failed ? 1 : 0);
