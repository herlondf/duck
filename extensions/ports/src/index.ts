// Extensao Portas do Duck: portas TCP escutando e o processo dono de cada uma.
// Windows: Get-NetTCPConnection + Get-Process (PowerShell). Linux/macOS: lsof.
//
// Config (~/.duck.lua):
//   extensions = { ports = { poll_seconds = 30, ignore = { 'Spotify' } } }

import { execFile } from "node:child_process";
import { duck, run, Item } from "./duck";

type Port = { port: number; pid: number; name: string; addrs: string[]; session: number };

const WIN = process.platform === "win32";
// Processos de desenvolvimento entram em "Desenvolvimento" mesmo rodando como servico (sessao 0).
const DEV = /^(node|deno|bun|dotnet|java|javaw|python\d*|pythonw|ruby|php|go|cargo|podman|wslrelay|docker|com\.docker.*|vpnkit|nginx|caddy|postgres|mysqld|mongod|redis-server|code|esbuild|gradle.*|kotlin.*)$/i;
let settings: { poll_seconds?: number; ignore?: string[] | string } = {};
let ports: Port[] = [];
let error = "";
let viewOpen = false;

function exec(cmd: string, args: string[], timeout = 20000): Promise<string> {
  return new Promise((res, rej) =>
    execFile(cmd, args, { windowsHide: true, timeout, maxBuffer: 16 << 20 }, (e, out, err) => {
      if (e) rej(new Error((String(err).trim() || e.message).split("\n")[0])); else res(String(out));
    }));
}

const ps = (script: string) => exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script]);

const PS_LIST = `
$p = @{}; Get-Process | ForEach-Object { $p[[int]$_.Id] = $_ }
$r = Get-NetTCPConnection -State Listen | ForEach-Object {
  $x = $p[[int]$_.OwningProcess]
  [pscustomobject]@{ a = $_.LocalAddress; port = [int]$_.LocalPort; pid = [int]$_.OwningProcess; name = $x.ProcessName; s = $x.SessionId }
}
ConvertTo-Json -InputObject @($r) -Compress`;

async function list(): Promise<Port[]> {
  const rows: { a: string; port: number; pid: number; name: string | null; s: number | null }[] = [];
  if (WIN) {
    rows.push(...JSON.parse((await ps(PS_LIST)).trim() || "[]"));
  } else {
    // lsof sem sudo so ve processos do usuario: todos contam como sessao do usuario.
    const out = await exec("lsof", ["-iTCP", "-sTCP:LISTEN", "-P", "-n"]).catch((e) => { if (/ENOENT/.test(e.message)) throw new Error("lsof não encontrado"); return ""; });
    for (const l of out.split("\n").slice(1)) {
      const f = l.trim().split(/\s+/);
      const m = /^(.*):(\d+)$/.exec(f[8] ?? "");
      if (m) rows.push({ a: m[1], port: Number(m[2]), pid: Number(f[1]), name: f[0], s: 1 });
    }
  }
  const map = new Map<string, Port>();
  for (const r of rows) {
    const k = `${r.port}|${r.pid}`;
    const p = map.get(k) ?? { port: r.port, pid: r.pid, name: r.name ?? (r.pid === 4 ? "System" : "?"), addrs: [], session: r.s ?? 0 };
    if (!p.addrs.includes(r.a)) p.addrs.push(r.a);
    map.set(k, p);
  }
  return [...map.values()].sort((a, b) => a.port - b.port);
}

function ignored(): Set<string> {
  const v = settings.ignore;
  const l = Array.isArray(v) ? v : typeof v === "string" ? v.split(",") : [];
  return new Set(l.map((s) => s.trim().toLowerCase()).filter(Boolean));
}

const isDev = (p: Port) => p.port >= 1024 && p.port <= 49151 && (p.session !== 0 || DEV.test(p.name));

async function poll(): Promise<void> {
  try { const ign = ignored(); ports = (await list()).filter((p) => !ign.has(p.name.toLowerCase())); error = ""; }
  catch (e) { error = (e as Error).message; duck.log("poll", e); }
  render();
}

function item(p: Port): Item {
  return {
    id: `${p.port}|${p.pid}`, title: `:${p.port}  ${p.name}`, icon: "",
    subtitle: `PID ${p.pid} · ${p.addrs.join(", ")}`,
    color: isDev(p) ? "accent" : "muted",
    actions: [
      { id: "browser", title: "Navegador", primary: true },
      { id: "copy", title: "Copiar endereço" },
      { id: "kill", title: "Encerrar" },
    ],
  };
}

function render(): void {
  const dev = ports.filter(isDev);
  const sys = ports.filter((p) => !isDev(p));
  duck.view("ports", [
    { title: `Desenvolvimento (${dev.length})`, items: dev.map(item) },
    // ponytail: o protocolo nao tem secao recolhida; "Sistema" vai no fim e so aparece com a visao aberta.
    { title: `Sistema (${sys.length})`, items: sys.map(item) },
  ], { error });
  duck.status("ports", error ? "" : ` ${dev.length}`, { color: dev.length ? "accent" : "muted", view: "ports" });
}

const url = (port: number) => `http://localhost:${port}`;
const pending = new Map<string, () => Promise<void>>();

async function onAction(action: string, id: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [port, pid] = id.split("|").map(Number);
  const p = ports.find((x) => x.port === port && x.pid === pid);
  switch (action) {
    case "browser": duck.openUrl(url(port)); break;
    case "copy": {
      const u = url(port);
      // Nao ha API de clipboard no protocolo: no Windows usa Set-Clipboard; nos outros so mostra o endereco.
      const ok = WIN && (await ps(`Set-Clipboard -Value '${u}'`).then(() => true, () => false));
      duck.flash(ok ? `Copiado: ${u}` : u);
      break;
    }
    case "kill": {
      if (!p) { duck.flash("Processo não encontrado; atualize a lista"); return; }
      const key = `kill|${id}`;
      pending.set(key, async () => {
        try {
          if (WIN) await ps(`Stop-Process -Id ${pid} -Force -ErrorAction Stop`); else await exec("kill", [String(pid)]);
          duck.flash(`${p.name} (PID ${pid}) encerrado`);
        } catch (e) { duck.flash(`Não deu para encerrar ${p.name}: ${(e as Error).message}`); }
        await poll();
      });
      duck.choose(key, `Encerrar ${p.name} (PID ${pid}, porta ${port})?`, [{ id: "yes", title: "Encerrar processo", hint: p.addrs.join(", ") }]);
      break;
    }
  }
}

function loop(): void {
  const s = Math.max(5, Number(settings.poll_seconds) || 30);
  setTimeout(async () => { await poll(); loop(); }, (viewOpen ? Math.min(s, 10) : s) * 1000);
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as typeof settings;
    duck.register([{ id: "ports", title: "Portas", icon: "" }], [{ id: "ports", title: "Portas: abrir" }]);
    await poll();
    loop();
  },
  async viewOpen() { viewOpen = true; await poll(); },
  viewClose() { viewOpen = false; },
  command() { duck.showView("ports"); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value === "yes") await fn();
  },
});
