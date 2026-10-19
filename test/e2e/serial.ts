/**
 * Capture a VM's serial console through the Proxmox API (termproxy), for
 * `seconds`; with --reset, reset the VM first so the whole boot is captured.
 *
 *   bun test/e2e/serial.ts <vmid> [seconds] [--reset]
 *
 * Same DELI_PVE_* environment as proxmox.ts. Output goes to stdout.
 */

const env = (name: string): string => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
};
const args = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const vmid = Number(args[0]);
const seconds = Number(args[1] ?? 120);
const reset = process.argv.includes("--reset");
const url = env("DELI_PVE_URL").replace(/\/$/, "").replace(/\/api2\/json$/, "");
const headers = { Authorization: `PVEAPIToken=${env("DELI_PVE_TOKEN")}` };
const node = env("DELI_PVE_NODE");

async function api(method: string, path: string, body?: Record<string, string>): Promise<any> {
  const res = await fetch(`${url}/api2/json${path}`, {
    method,
    headers,
    body: body ? new URLSearchParams(body) : undefined,
    tls: { rejectUnauthorized: false },
  } as RequestInit);
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${text}`);
  return JSON.parse(text).data;
}

const proxy = await api("POST", `/nodes/${node}/qemu/${vmid}/termproxy`, { serial: "serial0" });
const socketUrl = `${url.replace(/^http/, "ws")}/api2/json/nodes/${node}/qemu/${vmid}/vncwebsocket?port=${proxy.port}&vncticket=${encodeURIComponent(proxy.ticket)}`;
const ws = new WebSocket(socketUrl, { headers, tls: { rejectUnauthorized: false } } as any);
ws.binaryType = "arraybuffer";
const decoder = new TextDecoder();
let authed = false;

ws.onopen = () => ws.send(`${proxy.user}:${proxy.ticket}\n`);
ws.onmessage = async (event) => {
  const text = typeof event.data === "string" ? event.data : decoder.decode(event.data as ArrayBuffer);
  if (!authed) {
    authed = text.startsWith("OK");
    if (!authed) throw new Error(`termproxy refused: ${text}`);
    if (reset) await api("POST", `/nodes/${node}/qemu/${vmid}/status/reset`);
    else ws.send("0:1:\r");
    return;
  }
  process.stdout.write(text);
};
// A capture that never started, or ended before its time, is a failure,
// not an empty or short log.
ws.onerror = (e) => {
  console.error("websocket error", e);
  process.exit(1);
};
ws.onclose = () => {
  console.error("console closed before the capture ended");
  process.exit(1);
};
setTimeout(() => {
  ws.onclose = null;
  ws.close();
  process.exit(authed ? 0 : 1);
}, seconds * 1000);
