/**
 * End-to-end test on a Proxmox VE host: boot the ISO in a throwaway VM with a
 * CIDATA volume carrying a deli.toml, let it install, then check the
 * installed system over SSH. With --interactive, no volume: the answers are
 * typed into the interactive installer through the hypervisor's keyboard.
 *
 *   bun test/e2e/proxmox.ts [--interactive] out/deli-<version>-x86_64.iso [test/e2e/deli.toml]
 *
 * Environment: see pve.ts.
 */

import { $ } from "bun";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

import { agentAnswers, api, bridge, check, destroy, diskStore, guest, isoStore, key, log, MAC, NAME, node, ours, PALUDIS, reach, sleep, ssh, task, upload, uploadCidata, vmid } from "./pve";

const args = process.argv.slice(2);
const interactive = args.includes("--interactive");
const [isoPath, configPath = join(import.meta.dir, "deli.toml")] = args.filter((a) => a !== "--interactive");
if (!isoPath) throw new Error("usage: bun test/e2e/proxmox.ts [--interactive] <deli.iso> [deli.toml]");

const ISO = `deli-e2e-${vmid}.iso`;
const CIDATA = `deli-e2e-${vmid}-cidata.iso`;

/** QEMU key names for what a US keyboard types. */
const KEYS: Record<string, string> = { " ": "spc", "-": "minus", _: "shift-minus", ".": "dot", "/": "slash", "+": "shift-equal", "=": "equal", "@": "shift-2" };
async function press(...keys: string[]): Promise<void> {
  for (const key of keys) {
    await api("PUT", `/nodes/${node}/qemu/${vmid}/sendkey`, { key });
    await sleep(60);
  }
}
async function type(text: string): Promise<void> {
  for (const c of text) {
    const key = KEYS[c] ?? (/[A-Z]/.test(c) ? `shift-${c.toLowerCase()}` : /[a-z0-9]/.test(c) ? c : undefined);
    if (!key) throw new Error(`cannot type ${JSON.stringify(c)}`);
    await press(key);
  }
}

/** Wait until the live session's log has `text`; returns the log. */
async function liveLog(text: string, tries: number): Promise<string> {
  for (let i = 0; i < tries; i++) {
    const out = await guest("cat", "/run/deli/live.log");
    if (out?.includes("FAILED")) throw new Error(`installation failed:\n${out}`);
    if (out?.includes(text)) return out;
    await sleep(5000);
  }
  throw new Error(`the live log never showed "${text}"`);
}

// --- prepare
// Interactive: what the answers typed below amount to, for the checks.
const config = interactive
  ? `[system]\nhostname = "deli-tui"\ntimezone = "Europe/Berlin"\nlocale = "en_US.UTF-8"\n[admin]\nname = "ops"\n`
  : readFileSync(configPath, "utf8").replaceAll("@SSH_KEY@", key);
const hostname = config.match(/^hostname\s*=\s*"([^"]+)"/m)?.[1] ?? "exherbo";
const admin = config.match(/^\[admin\][^[]*?name\s*=\s*"([^"]+)"/ms)?.[1] ?? "root";

// Nothing is touched, ISOs included, unless the VM id is free or ours.
const current = await ours();

log(`uploading ${basename(isoPath)}${interactive ? "" : " and the CIDATA volume"}`);
await upload(isoPath, ISO);
if (!interactive) await uploadCidata({ "deli.toml": config }, CIDATA);

await destroy(current);
log(`creating VM ${vmid}`);
await task(
  await api("POST", `/nodes/${node}/qemu`, {
    vmid,
    name: NAME,
    ostype: "l26",
    machine: "q35",
    bios: "ovmf",
    efidisk0: `${diskStore}:1,efitype=4m,pre-enrolled-keys=0`,
    cpu: "host",
    cores: 4,
    memory: 4096,
    scsihw: "virtio-scsi-single",
    scsi0: `${diskStore}:32,discard=on,ssd=1,iothread=1`,
    ide2: `${isoStore}:iso/${ISO},media=cdrom`,
    ...(interactive ? {} : { ide3: `${isoStore}:iso/${CIDATA},media=cdrom` }),
    net0: `virtio=${MAC},bridge=${bridge}`,
    serial0: "socket",
    agent: "enabled=1",
    boot: "order=scsi0;ide2",
  }),
);
await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/start`));

// --- the live session, then the installed system
log("booting the ISO");
if (interactive) {
  await liveLog("interactive installer", 30);
  await sleep(2000);
  // SSH into the installer with the password its first screen shows.
  let password: string | undefined;
  for (let i = 0; i < 10 && !password; i++) {
    password = (await guest("cat", "/run/deli/ssh-password"))?.trim() || undefined;
    if (!password) await sleep(2000);
  }
  if (!password) throw new Error("the live session has no SSH password");
  const askpassDir = mkdtempSync(join(tmpdir(), "deli-askpass-"));
  const askpass = join(askpassDir, "askpass");
  writeFileSync(askpass, `#!/bin/sh\necho '${password}'\n`, { mode: 0o700 });
  const byPassword = ["-o", "PreferredAuthentications=password", "-o", "PubkeyAuthentication=no", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null", "-o", "ConnectTimeout=5", "-o", "LogLevel=ERROR"];
  const askpassEnv = { ...process.env, SSH_ASKPASS: askpass, SSH_ASKPASS_REQUIRE: "force", DISPLAY: ":0" };
  const liveIp = await reach(async (ip) => {
    const out = await $`ssh ${byPassword} root@${ip} cat /run/deli/ssh-password`.env(askpassEnv).nothrow().quiet();
    return out.exitCode === 0 && out.text().trim() === password;
  }, 12);
  rmSync(askpassDir, { recursive: true, force: true });
  if (!liveIp) throw new Error("cannot SSH into the installer with the password it shows");
  log(`ok   SSH into the installer as root@${liveIp}, with the password it shows`);
  // Enabled for installed systems, it runs in the live session too, where no ESP is mounted.
  const bootUpdate = ((await guest("systemctl", "show", "systemd-boot-update.service", "--property=Result", "--value")) ?? "").trim();
  if (bootUpdate !== "success") throw new Error(`systemd-boot-update.service in the live session: ${bootUpdate || "no answer"}`);
  log("ok   boot loader update harmless in the live session");
  log("answering the interactive installer");
  const [keyType, keyData] = key.split(/\s+/);
  // Shift+Tab from a screen's first field reaches its Continue button.
  const next = () => press("shift-tab", "ret");
  await press("ret"); // welcome
  await next(); // keyboard: us
  await press("tab");
  await type("berlin"); // the time zone's list opens on the search
  await press("ret", "ret"); // choose it, then Continue
  await next(); // network: DHCP, DHCP's resolvers, then the connection is checked
  await next(); // storage: the only disk, btrfs, no swap
  await press("ctrl-u");
  await type("deli-tui");
  await press("ret", "ret"); // host name, then Continue
  await type("ops");
  // Passwords left empty, sudo, SSH server enabled, no GitHub user.
  await press("tab", "tab", "tab", "tab", "tab", "tab");
  await type(`${keyType} ${keyData}`);
  await press("ret", "ret"); // the key is the last field, then Continue
  // The list of repositories, all of them taken: none, then perl alone.
  await press("ret", "ctrl-a");
  await type("perl");
  await press("spc", "ret", "ret"); // take perl, close the list, sync now
  await type("htop"); // to build later, its category found by the installer
  await press("ret", "ret"); // the last field, then Continue
  await press("ret", "right", "ret"); // Install, then erase and install
  // Syncing and caching the repositories' metadata takes minutes.
  const done = await liveLog("done: the system is installed", 150);
  for (const line of done.trimEnd().split("\n")) log(`live | ${line}`);
  await sleep(2000);
  await press("ret");
}
let lastLines = 0;
const installedProbe = async (ip: string) => {
  const live = await ssh(`root@${ip}`, "cat /run/deli/live.log 2>/dev/null");
  if (live.exitCode === 0) {
    const lines = live.text().trimEnd().split("\n");
    for (const line of lines.slice(lastLines)) if (line) log(`live | ${line}`);
    lastLines = Math.max(lastLines, lines.length);
    if (lines.some((l) => l.startsWith("FAILED"))) throw new Error("installation failed, see above");
  }
  const probe = await ssh(`${admin}@${ip}`, "hostname; test -d /run/deli && echo live || echo installed");
  return probe.exitCode === 0 && probe.text().includes(hostname) && probe.text().includes("installed");
};
const ip = await reach(installedProbe, interactive ? 60 : 120);
if (!ip) throw new Error("the installed system never answered");
log(`installed system answers as ${admin}@${ip}`);

// --- checks on the installed system
// iso/live/usr/share/deli/live-manifest, and what the live session adds.
const LIVE_ONLY = readFileSync(join(import.meta.dir, "../../iso/live/usr/share/deli/live-manifest"), "utf8")
  .split("\n")
  .filter((p) => p.startsWith("/") && p !== "/root/.ssh")
  .concat(["/root/.ssh/authorized_keys", "/usr/share/deli"]);
const checks: [string, string, (out: string) => boolean][] = [
  ["hostname", "hostname", (o) => o.trim() === hostname],
  ["root on btrfs subvolume @", "findmnt -no FSTYPE,OPTIONS /", (o) => o.startsWith("btrfs") && o.includes("subvol=/@")],
  ["ESP on /boot", "findmnt -no FSTYPE /boot", (o) => o.trim() === "vfat"],
  ["boot entry with an initrd", "sudo sh -c 'cat /boot/loader/entries/*.conf'", (o) => /^initrd /m.test(o) && /^linux /m.test(o)],
  ["machine-id set", "cat /etc/machine-id", (o) => /^[0-9a-f]{32}$/.test(o.trim())],
  ["no failed units", "systemctl --failed --no-legend", (o) => o.trim() === ""],
  ["getty on tty1 active", "systemctl is-active getty@tty1.service", (o) => o.trim() === "active"],
  ["remote-fs and boot loader updates enabled", "systemctl is-enabled remote-fs.target systemd-boot-update.service", (o) => o.trim().split(/\s+/).every((s) => s === "enabled")],
  // /root/.ssh itself is created, empty, by systemd's tmpfiles provision.conf.
  ["no live files", `sudo sh -c 'ls -d ${LIVE_ONLY.join(" ")} 2>/dev/null; true'`, (o) => o.trim() === ""],
  ["root cannot log in over SSH", "sudo -n sshd -T | grep -i -E '^(permitrootlogin|passwordauthentication) '", (o) => o.toLowerCase().includes("permitrootlogin no") && o.toLowerCase().includes("passwordauthentication no")],
  ["DNS works", "getent hosts exherbo.org", (o) => o.trim() !== ""],
  // Validation fails every query behind resolvers that strip signatures.
  // The configuration's default is off; the installer preselects
  // Required only if the network's resolvers pass DNSSEC on, and
  // "DNS works" proves it chose right.
  [
    interactive ? "DNSSEC as preselected" : "DNSSEC validation off",
    "resolvectl status",
    (o) => (interactive ? /DNSSEC=(no|yes)\//.test(o) : o.includes("DNSSEC=no")),
  ],
  ["paludis mirror resolves", "getent hosts gitlab.exherbo.org", (o) => o.trim() !== ""],
  ["sudo without password", "sudo -n true && echo ok", (o) => o.includes("ok")],
  ["time zone", "timedatectl show --property=Timezone --value", (o) => o.trim() === (config.match(/^timezone\s*=\s*"([^"]+)"/m)?.[1] ?? "UTC")],
  ["locale", "localectl status", (o) => o.includes(config.match(/^locale\s*=\s*"([^"]+)"/m)?.[1] ?? "en_US.UTF-8")],
  ["paludis can build", "sudo -n -u paludisbuild test -w /var/tmp/paludis/build && echo ok", (o) => o.includes("ok")],
  ["build user not in root's group", "id -nG paludisbuild", (o) => o.trim() !== "" && !o.split(/\s+/).includes("root")],
  ["terminfo for Ghostty", "TERM=xterm-ghostty infocmp >/dev/null && echo ok", (o) => o.includes("ok")],
  ["firmware installed", "ls /lib/firmware/amd-ucode", (o) => o.trim() !== ""],
  [
    "early microcode for Intel and AMD",
    "sudo -n sh -c 'lsinitrd /boot/*/*/initrd*' 2>/dev/null",
    (o) => o.includes("kernel/x86/microcode/GenuineIntel.bin") && o.includes("kernel/x86/microcode/AuthenticAMD.bin"),
  ],
  // Named x-perl until a sync.
  ["perl repository added and synced", "sudo -n cave print-repositories 2>/dev/null", (o) => o.split(/\s+/).includes("perl")],
  [
    "htop in world, not built",
    "grep -x sys-process/htop /var/db/paludis/repositories/installed/world && ! command -v htop && echo ok",
    (o) => o.includes("ok"),
  ],
  ...PALUDIS,
];
let failed = await check(`${admin}@${ip}`, checks);
const agent = await agentAnswers();
log(`${agent ? "ok  " : "FAIL"} qemu-guest-agent answers the hypervisor`);
if (!agent) failed++;
const kernel = (await ssh(`${admin}@${ip}`, "uname -r")).text().trim();
const machineId = (await ssh(`${admin}@${ip}`, "cat /etc/machine-id")).text().trim();
const total = checks.length + 1; // and the agent
log(`kernel ${kernel}, ${total - failed}/${total} checks passed`);

// --- a firmware that boots the medium again must not reinstall
if (interactive) process.exit(failed ? 1 : 0);
log("rebooting with the ISO first: the installer must refuse to install again");
await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/stop`));
await api("PUT", `/nodes/${node}/qemu/${vmid}/config`, { boot: "order=ide2;scsi0" });
await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/start`));
const refused = Boolean(
  await reach(async (addr) => {
    const live = await ssh(`root@${addr}`, "cat /run/deli/live.log 2>/dev/null");
    if (/^\s+erasing /m.test(live.text())) throw new Error("the installer started installing again");
    return live.exitCode === 0 && live.text().includes("not installing it again");
  }, 60),
);
log(`${refused ? "ok  " : "FAIL"} installer refused to reinstall the same configuration`);
await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/stop`));
await api("PUT", `/nodes/${node}/qemu/${vmid}/config`, { boot: "order=scsi0;ide2" });
await task(await api("POST", `/nodes/${node}/qemu/${vmid}/status/start`));
const intact = Boolean(
  await reach(async (addr) => {
    const out = await ssh(`${admin}@${addr}`, "cat /etc/machine-id");
    return out.exitCode === 0 && out.text().trim() === machineId;
  }, 30),
);
log(`${intact ? "ok  " : "FAIL"} installed system intact after that boot`);
if (!refused || !intact) failed++;
process.exit(failed ? 1 : 0);
