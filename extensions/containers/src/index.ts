// Extensao Containers do Duck: lista containers do Docker e do Podman
// (no Windows, o Podman roda dentro das distros do WSL), com logs, shell,
// iniciar/parar/reiniciar e aviso quando um container cai sozinho.
//
// Config (~/.duck.lua):
//   extensions = { containers = { engines = { 'auto' }, poll_seconds = 60 } }
//   engines: 'auto' (docker se existir + cada distro do WSL com podman),
//            'docker', 'podman' (nativo) ou o nome de uma distro do WSL.

import { execFile } from "node:child_process";
import { duck, run, Item, Section, Action } from "./duck";

type Engine = { key: string; label: string; cmd: string[] };
type Container = { id: string; name: string; image: string; state: string; status: string; ports: string; exit: number | null };

const WIN = process.platform === "win32";
let settings: { engines?: string[] | string; poll_seconds?: number } = {};
let engines: Engine[] = [];
const lists = new Map<string, Container[]>();
const errors = new Map<string, string>();
const prev = new Map<string, string>(); // "motor|id" -> state
let viewOpen = false;
let timer: NodeJS.Timeout | undefined;
let first = true;

// ---------------------------------------------------------------- processo

/** wsl.exe responde em UTF-16 em alguns comandos (wsl -l -q). */
function decode(b: Buffer): string {
  const s = (b[0] === 0xff && b[1] === 0xfe) || (b.length > 1 && b[1] === 0) ? b.toString("utf16le") : b.toString("utf8");
  return s.replace(/^﻿/, "").replace(/\0/g, "");
}

function exec(cmd: string, args: string[], timeout = 20000): Promise<string> {
  return new Promise((res, rej) =>
    execFile(cmd, args, { windowsHide: true, timeout, encoding: "buffer", maxBuffer: 32 << 20 }, (e, out, err) => {
      if (e) rej(new Error((decode(err).trim() || e.message).split("\n")[0]));
      else res(decode(out));
    }));
}

const has = (cmd: string, args: string[]) => exec(cmd, args, 8000).then(() => true, () => false);

async function wslDistros(): Promise<string[]> {
  if (!WIN) return [];
  try { return (await exec("wsl.exe", ["-l", "-q"], 8000)).split(/\r?\n/).map((s) => s.trim()).filter(Boolean); }
  catch { return []; }
}

function wslEngine(d: string): Engine {
  return { key: `wsl:${d}`, label: `Podman · ${d}`, cmd: ["wsl.exe", "-d", d, "--exec", "podman"] };
}

async function detectEngines(): Promise<Engine[]> {
  const raw = settings.engines;
  const want = (Array.isArray(raw) ? raw : typeof raw === "string" ? raw.split(",") : ["auto"]).map((s) => s.trim()).filter(Boolean);
  const out: Engine[] = [];
  const docker: Engine = { key: "docker", label: "Docker", cmd: ["docker"] };
  const podman: Engine = { key: "podman", label: "Podman", cmd: ["podman"] };
  for (const w of want.length ? want : ["auto"]) {
    const lw = w.toLowerCase();
    if (lw === "auto") {
      if (await has("docker", ["version", "--format", "{{.Server.Version}}"])) out.push(docker);
      if (!WIN && (await has("podman", ["--version"]))) out.push(podman);
      // ponytail: liga cada distro para testar (wsl -d inicia a distro); use engines explicito para evitar.
      for (const d of await wslDistros()) {
        if (await has("wsl.exe", ["-d", d, "--exec", "sh", "-c", "command -v podman"])) out.push(wslEngine(d));
      }
    } else if (lw === "docker") out.push(docker);
    else if (lw === "podman") out.push(podman);
    else out.push(wslEngine(w.replace(/^wsl:/i, "")));
  }
  return out.filter((e, i) => out.findIndex((x) => x.key === e.key) === i);
}

const run1 = (e: Engine, args: string[], timeout?: number) => exec(e.cmd[0], [...e.cmd.slice(1), ...args], timeout);

// ---------------------------------------------------------------- dados

function portsText(p: any): string {
  if (typeof p === "string") return p; // docker: "0.0.0.0:80->80/tcp"
  if (!Array.isArray(p)) return "";
  return p.map((x) => `${x.host_ip && x.host_ip !== "0.0.0.0" ? x.host_ip + ":" : ""}${x.host_port}→${x.container_port}/${x.protocol ?? "tcp"}`).join(", ");
}

/** Docker manda um JSON por linha; Podman manda um array. */
function parsePs(text: string): Container[] {
  const t = text.trim();
  if (!t) return [];
  const rows: any[] = t.startsWith("[") ? JSON.parse(t) : t.split(/\r?\n/).filter(Boolean).map((l) => JSON.parse(l));
  return rows.map((r) => {
    const status: string = r.Status ?? "";
    const m = /Exited \((-?\d+)\)/.exec(status);
    return {
      id: String(r.Id ?? r.ID ?? ""),
      name: Array.isArray(r.Names) ? r.Names[0] : String(r.Names ?? "").split(",")[0],
      image: r.Image ?? "",
      state: String(r.State ?? "").toLowerCase(),
      status,
      ports: portsText(r.Ports),
      exit: typeof r.ExitCode === "number" ? r.ExitCode : m ? Number(m[1]) : null,
    };
  });
}

async function poll(): Promise<void> {
  await Promise.all(engines.map(async (e) => {
    try {
      const list = parsePs(await run1(e, ["ps", "-a", "--format", "json"]));
      lists.set(e.key, list);
      errors.delete(e.key);
      for (const c of list) {
        const k = `${e.key}|${c.id}`;
        const before = prev.get(k);
        prev.set(k, c.state);
        if (!first && before === "running" && c.state === "exited" && c.exit !== 0 && !expected.delete(k)) {
          duck.notify({
            title: `Container parou: ${c.name}`, body: `${e.label} · ${c.status}`, item: k, color: "blocked",
            actions: [{ id: "logs", title: "Logs" }, { id: "start", title: "Iniciar" }],
          });
        }
      }
    } catch (err) { errors.set(e.key, (err as Error).message); duck.log("ps", e.key, err); }
  }));
  first = false;
  render();
}

function schedule(): void {
  clearTimeout(timer);
  const s = viewOpen ? 10 : Math.max(10, Number(settings.poll_seconds) || 60);
  timer = setTimeout(async () => { await poll(); schedule(); }, s * 1000);
}

// ---------------------------------------------------------------- visao

function item(e: Engine, c: Container): Item {
  const up = c.state === "running";
  const acts: Action[] = [{ id: "detail", title: "Detalhe", primary: true }, { id: "logs", title: "Logs" }];
  if (up) acts.push({ id: "shell", title: "Shell" }, { id: "restart", title: "Reiniciar" }, { id: "stop", title: "Parar" });
  else acts.push({ id: "start", title: "Iniciar" });
  return {
    id: `${e.key}|${c.id}`, title: c.name, icon: "",
    color: up ? "done" : c.exit && c.exit !== 0 ? "blocked" : "muted",
    subtitle: [c.image, c.status, c.ports].filter(Boolean).join(" · "),
    badge: up ? "rodando" : c.state,
    actions: acts,
  };
}

function render(): void {
  const secs: Section[] = [];
  let running = 0;
  for (const e of engines) {
    const list = [...(lists.get(e.key) ?? [])].sort((a, b) => Number(b.state === "running") - Number(a.state === "running") || a.name.localeCompare(b.name));
    const n = list.filter((c) => c.state === "running").length;
    running += n;
    secs.push({ title: `${e.label} (${n}/${list.length} rodando)`, items: list.map((c) => item(e, c)) });
  }
  const err = engines.length ? [...errors].map(([k, m]) => `${k}: ${m}`).join(" | ") : "Nenhum motor encontrado (docker ou podman no WSL)";
  duck.view("containers", secs, { error: err });
  duck.status("containers", engines.length ? ` ${running}` : "", { color: running ? "done" : "muted", view: "containers" });
}

// ---------------------------------------------------------------- acoes

const pending = new Map<string, () => Promise<void>>();
const expected = new Set<string>(); // paradas pedidas pelo usuario nao viram aviso

function find(itemId: string): { e: Engine; c: Container } | null {
  const i = itemId.lastIndexOf("|");
  const e = engines.find((x) => x.key === itemId.slice(0, i));
  const c = e && lists.get(e.key)?.find((x) => x.id === itemId.slice(i + 1));
  return e && c ? { e, c } : null;
}

async function lifecycle(e: Engine, c: Container, verb: "start" | "stop" | "restart"): Promise<void> {
  const label = { start: "Iniciando", stop: "Parando", restart: "Reiniciando" }[verb];
  duck.flash(`${label} ${c.name}...`);
  if (verb !== "start") expected.add(`${e.key}|${c.id}`);
  try { await run1(e, [verb, c.id], 90000); duck.flash(`${c.name}: ok`); }
  catch (err) { duck.flash(`${c.name}: ${(err as Error).message}`); }
  await poll();
}

async function detail(e: Engine, c: Container): Promise<void> {
  const j = JSON.parse(await run1(e, ["inspect", c.id]))[0] ?? {};
  const st = j.State ?? {};
  const cfg = j.Config ?? {};
  const ports = Object.entries(j.NetworkSettings?.Ports ?? {}).map(([k, v]: [string, any]) =>
    `${k} ← ${(v ?? []).map((b: any) => `${b.HostIp || "0.0.0.0"}:${b.HostPort}`).join(", ") || "(sem publicar)"}`);
  const body = [
    `Imagem:     ${cfg.Image ?? c.image}`,
    `Estado:     ${st.Status ?? c.state}${st.ExitCode ? ` (código ${st.ExitCode})` : ""}`,
    `Iniciado:   ${st.StartedAt ?? "-"}`,
    `Terminado:  ${st.FinishedAt && !String(st.FinishedAt).startsWith("0001") ? st.FinishedAt : "-"}`,
    `Reinício:   ${j.HostConfig?.RestartPolicy?.Name || "no"}`,
    `Comando:    ${[...(cfg.Entrypoint ?? []), ...(cfg.Cmd ?? [])].join(" ") || "-"}`,
    `Redes:      ${Object.keys(j.NetworkSettings?.Networks ?? {}).join(", ") || "-"}`,
    "", "── Portas ──", ...(ports.length ? ports : ["(nenhuma)"]),
    "", "── Volumes ──", ...((j.Mounts ?? []).map((m: any) => `${m.Source || m.Name} → ${m.Destination}${m.RW === false ? " (ro)" : ""}`) as string[]),
    "", `── Variáveis (${(cfg.Env ?? []).length}) ──`, ...((cfg.Env ?? []) as string[]).map((v) => v.replace(/=(.{40}).+$/, "=$1…")),
  ].join("\n");
  const up = c.state === "running";
  duck.detail({
    title: c.name, subtitle: `${e.label} · ${c.id.slice(0, 12)} · ${c.status}`, body, item: `${e.key}|${c.id}`,
    actions: up ? [{ id: "logs", title: "Logs" }, { id: "shell", title: "Shell" }, { id: "restart", title: "Reiniciar" }, { id: "stop", title: "Parar" }]
      : [{ id: "start", title: "Iniciar" }, { id: "logs", title: "Logs" }],
  });
}

async function onAction(action: string, itemId: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const f = find(itemId);
  if (!f) { duck.flash("Container não encontrado; atualize a lista"); return; }
  const { e, c } = f;
  switch (action) {
    case "detail": await detail(e, c); break;
    case "logs": duck.openPane([...e.cmd, "logs", "-f", "--tail", "200", c.id], { where: "float", title: `logs · ${c.name}` }); break;
    case "shell":
      duck.openPane([...e.cmd, "exec", "-it", c.id, "sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"], { where: "tab", title: c.name });
      break;
    case "start": await lifecycle(e, c, "start"); break;
    case "stop":
    case "restart": {
      const id = `${action}|${itemId}`;
      pending.set(id, () => lifecycle(e, c, action));
      duck.choose(id, `${action === "stop" ? "Parar" : "Reiniciar"} ${c.name}?`, [
        { id: "yes", title: action === "stop" ? "Parar" : "Reiniciar", hint: e.label },
      ]);
      break;
    }
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as typeof settings;
    duck.register([{ id: "containers", title: "Containers", icon: "" }], [{ id: "containers", title: "Containers: abrir" }]);
    duck.view("containers", [], { loading: true });
    engines = await detectEngines();
    duck.log("motores:", engines.map((e) => e.key).join(", ") || "nenhum");
    await poll();
    schedule();
  },
  async viewOpen() { viewOpen = true; await poll(); schedule(); },
  viewClose() { viewOpen = false; schedule(); },
  command() { duck.showView("containers"); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value === "yes") await fn();
  },
});
