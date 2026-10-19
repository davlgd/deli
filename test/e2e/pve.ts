/**
 * What the end-to-end tests share: the Proxmox VE API, the throwaway VM
 * named deli-e2e, and reaching it over the LAN.
 *
 * Environment:
 *   DELI_PVE_URL        https://host:8006
 *   DELI_PVE_TOKEN      user@realm!tokenid=secret
 *   DELI_PVE_NODE       node name
 *   DELI_PVE_VMID       VM id to use; an existing VM is replaced only if it is named deli-e2e
 *   DELI_PVE_ISO_STORE  storage for ISOs (default local)
 *   DELI_PVE_DISK_STORE storage for disks (default local-lvm)
 *   DELI_PVE_BRIDGE     (default vmbr0)
 *   DELI_SSH_KEY        public key to install (default: first key of ssh-agent)
 *   DELI_LAN            the bridge's network, scanned to find the VM (192.0.2.0/24)
 *
 * The VM's address is found from its MAC in the local ARP table, so this
 * machine must be on the same LAN.
 */

import { $ } from "bun";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const env = (name: string, fallback?: string): string => {
  const value = process.env[name] ?? fallback;
  if (!value) throw new Error(`${name} is not set`);
  return value;
};

const base = `${env("DELI_PVE_URL").replace(/\/$/, "").replace(/\/api2\/json$/, "")}/api2/json`;
const auth = { Authorization: `PVEAPIToken=${env("DELI_PVE_TOKEN")}` };
export const node = env("DELI_PVE_NODE");
export const vmid = Number(env("DELI_PVE_VMID"));
if (!Number.isInteger(vmid) || vmid < 100 || vmid > 999_999_999) throw new Error("DELI_PVE_VMID: a Proxmox VM id, 100 to 999999999");
export const isoStore = env("DELI_PVE_ISO_STORE", "local");
export const diskStore = env("DELI_PVE_DISK_STORE", "local-lvm");
export const bridge = env("DELI_PVE_BRIDGE", "vmbr0");
const lan = env("DELI_LAN");
export const NAME = "deli-e2e";
// Per VM id, so tests on two VMs do not take each other's media or address.
// Locally administered (02:…), then the VM id on four bytes (Proxmox ids fit).
export const MAC = `02:DE:${vmid.toString(16).padStart(8, "0").toUpperCase().match(/../g)!.join(":")}`;
const t0 = Date.now();
export const log = (message: string) => console.log(`[${String(Math.round((Date.now() - t0) / 1000)).padStart(4)}s] ${message}`);
export const sleep = (ms: number) => Bun.sleep(ms);
export const key = process.env.DELI_SSH_KEY ?? (await $`ssh-add -L`.text()).split("\n")[0]!.trim();

export async function api(method: string, path: string, body?: Record<string, string | number> | FormData | URLSearchParams): Promise<any> {
  const init: RequestInit & { tls?: object } = {
    method,
    headers: { ...auth },
    tls: { rejectUnauthorized: false },
    // A hung API call must end the test, not stall it: uploads get longer.
    signal: AbortSignal.timeout(body instanceof FormData ? 15 * 60_000 : 60_000),
  };
  if (body instanceof FormData || body instanceof URLSearchParams) init.body = body;
  else if (body) {
    init.body = new URLSearchParams(Object.entries(body).map(([k, v]) => [k, String(v)]));
  }
  const res = await fetch(`${base}${path}`, init);
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${text}`);
  return JSON.parse(text).data;
}

export async function task(upid: string, minutes = 20): Promise<void> {
  for (const deadline = Date.now() + minutes * 60_000; Date.now() < deadline; ) {
    const status = await api("GET", `/nodes/${node}/tasks/${encodeURIComponent(upid)}/status`);
    if (status.status === "stopped") {
      if (status.exitstatus !== "OK") throw new Error(`task ${upid}: ${status.exitstatus}`);
      return;
    }
    await sleep(1000);
  }
  throw new Error(`task ${upid} still running after ${minutes} minutes`);
}

/** Upload `path` as `name` to `storage`, replacing a previous one; its volume id. */
export async function upload(path: string, name: string, storage = isoStore, content = "iso"): Promise<string> {
  const volid = `${storage}:${content}/${name}`;
  const existing: any[] = await api("GET", `/nodes/${node}/storage/${storage}/content?content=${content}`);
  if (existing.some((v) => v.volid === volid)) await task(await api("DELETE", `/nodes/${node}/storage/${storage}/content/${encodeURIComponent(volid)}`));
  const form = new FormData();
  form.set("content", content);
  form.set("checksum-algorithm", "sha256");
  form.set("checksum", new Bun.CryptoHasher("sha256").update(await Bun.file(path).arrayBuffer()).digest("hex"));
  form.set("filename", Bun.file(path), name);
  await task(await api("POST", `/nodes/${node}/storage/${storage}/upload`, form));
  return volid;
}

/** The VM, if it exists; nothing is touched unless the VM id is free or ours. */
export async function ours(): Promise<any> {
  const vms: any[] = await api("GET", `/nodes/${node}/qemu`);
  const current = vms.find((v) => Number(v.vmid) === vmid);
  if (current && current.name !== NAME) throw new Error(`VM ${vmid} exists and is not ${NAME}: refusing to touch it`);
  return current;
}

/** Upload a CIDATA volume holding `files`, by name, to the ISO storage. */
export async function uploadCidata(files: Record<string, string>, name: string): Promise<void> {
  const work = mkdtempSync(join(tmpdir(), "deli-e2e-"));
  try {
    const dir = join(work, "volume");
    mkdirSync(dir);
    for (const [file, content] of Object.entries(files)) writeFileSync(join(dir, file), content);
    const iso = join(work, "cidata.iso");
    if ((await $`command -v xorriso`.nothrow().quiet()).exitCode === 0) {
      await $`xorriso -as mkisofs -o ${iso} -V CIDATA -J -R ${dir}`.quiet();
    } else {
      await $`hdiutil makehybrid -o ${iso} ${dir} -iso -joliet -default-volume-name CIDATA`.quiet();
    }
    await upload(iso, name);
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

/** Stop and destroy the VM `ours()` returned, if any. */
export async function destroy(current: any): Promise<void> {
  if (!current) return;
  if (current.status === "running") await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/stop`));
  await task(await api("DELETE", `/nodes/${node}/qemu/${vmid}?purge=1&destroy-unreferenced-disks=1`));
}

/** Every address the ARP table gives for `mac`, after refreshing it: DHCP
 * may hand the live session and the installed system different addresses,
 * and stale entries linger. */
async function addresses(mac: string): Promise<string[]> {
  // arp prints 2:de:0:0:0:6c for 02:DE:00:00:00:6C.
  const norm = (m: string) => m.split(":").map((p) => parseInt(p, 16).toString(16)).join(":");
  await $`nmap -sn -n ${lan}`.nothrow().quiet();
  const arp = await $`arp -an`.nothrow().text();
  const found = new Set<string>();
  for (const line of arp.split("\n")) {
    const m = line.match(/\((\d+\.\d+\.\d+\.\d+)\) at ([0-9a-f:]+)/i);
    if (m && norm(m[2]!) === norm(mac)) found.add(m[1]!);
  }
  return [...found];
}

/** Poll until `probe` succeeds on one of the VM's addresses; returns that address. */
export async function reach(probe: (ip: string) => Promise<boolean>, tries: number): Promise<string | undefined> {
  for (let i = 0; i < tries; i++) {
    for (const ip of await addresses(MAC)) if (await probe(ip)) return ip;
    await sleep(5000);
  }
  return undefined;
}

/** Run a command in the guest through qemu-guest-agent; its standard output. */
export async function guest(...command: string[]): Promise<string | undefined> {
  const body = new URLSearchParams(command.map((c) => ["command", c]));
  const started = await api("POST", `/nodes/${node}/qemu/${vmid}/agent/exec`, body).catch(() => undefined);
  if (!started) return undefined;
  const { pid } = started;
  for (let i = 0; i < 20; i++) {
    const status = await api("GET", `/nodes/${node}/qemu/${vmid}/agent/exec-status?pid=${pid}`).catch(() => undefined);
    if (status?.exited) return status["out-data"] ?? "";
    await sleep(500);
  }
  return undefined;
}

/** Whether the hypervisor sees the guest through qemu-guest-agent, within a minute. */
export async function agentAnswers(): Promise<boolean> {
  for (let i = 0; i < 12; i++) {
    if (await api("POST", `/nodes/${node}/qemu/${vmid}/agent/ping`).then(() => true, () => false)) return true;
    await sleep(5000);
  }
  return false;
}

const SSH = ["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "ConnectTimeout=5", "-o", "ServerAliveInterval=15", "-o", "LogLevel=ERROR"];
export const ssh = (target: string, command: string) => $`ssh ${SSH} ${target} ${command}`.nothrow().quiet();

/** What comes first on a new system: sync every repository, then build a package from source. */
export const PALUDIS: [string, string, (out: string) => boolean][] = [
  ["cave sync", "sudo -n timeout 30m cave sync >/tmp/e2e-sync.log 2>&1; s=$?; sed -n '/Sync results/,$p' /tmp/e2e-sync.log; exit $s", (o) => o.includes("Sync results") && !/\bfailed\b/.test(o)],
  [
    "cave resolve -x htop",
    "sudo -n timeout 60m cave resolve -x htop >/tmp/e2e-htop.log 2>&1 && htop --version || { tail -20 /tmp/e2e-htop.log; exit 1; }",
    (o) => o.startsWith("htop "),
  ],
];

/** Run `checks` over SSH as `target`, logging each; how many failed. */
export async function check(target: string, checks: [string, string, (out: string) => boolean][]): Promise<number> {
  let failed = 0;
  for (const [name, command, ok] of checks) {
    const out = await ssh(target, command);
    const pass = out.exitCode === 0 && ok(out.text());
    if (!pass) failed++;
    log(`${pass ? "ok  " : "FAIL"} ${name}${pass ? "" : `: ${(out.text() + out.stderr.toString()).trim().slice(0, 300)}`}`);
  }
  return failed;
}
