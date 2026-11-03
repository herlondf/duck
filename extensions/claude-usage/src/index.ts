// Extensao "Uso do Claude": limites de 5h/7d e custo por perfil do Claude Code.
//
// Fonte: o statusline grava um JSON por sessao em
// %USERPROFILE%\AppData\Local\Temp\claude-usage\<session_id>.json. O perfil vem
// da pasta ~/.claude*/projects/*/ que tem o transcript <session_id>.jsonl.
// So le arquivos; nao grava nada.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { duck, run, Item, Section } from "./duck";

type Limit = { used_percentage: number; resets_at: number };
type Usage = {
  ts: string; session_id: string; model?: string; cwd?: string; cost_usd?: number;
  rate_limits?: { five_hour?: Limit; seven_day?: Limit };
};
type Profile = { name: string; dir: string };
type Session = Usage & { t: number; profile: Profile };

const SRC = path.join(os.homedir(), "AppData", "Local", "Temp", "claude-usage");
const UNKNOWN: Profile = { name: "sem perfil", dir: "" };
let warn = 80;
let sessions: Session[] = [];
const profileOf = new Map<string, Profile>(); // cache session_id -> perfil
const notified = new Set<string>();

// ---------------------------------------------------------------- leitura

function profileDirs(): Profile[] {
  const home = os.homedir();
  let names: string[] = [];
  try { names = fs.readdirSync(home); } catch { return []; }
  // ponytail: pastas "*backup*" ficam de fora para nao atribuir a sessao a uma copia.
  return names
    .filter((n) => (n === ".claude" || n.startsWith(".claude-")) && !n.includes("backup"))
    .map((n) => ({ name: n === ".claude" ? "pessoal" : n.slice(".claude-".length), dir: path.join(home, n) }))
    .filter((p) => fs.existsSync(path.join(p.dir, "projects")));
}

function findProfiles(ids: string[]): void {
  const missing = ids.filter((id) => !profileOf.has(id));
  if (!missing.length) return;
  for (const p of profileDirs()) {
    const root = path.join(p.dir, "projects");
    let projects: string[] = [];
    try { projects = fs.readdirSync(root); } catch { continue; }
    for (const id of missing) {
      if (profileOf.has(id)) continue;
      if (projects.some((d) => fs.existsSync(path.join(root, d, id + ".jsonl")))) profileOf.set(id, p);
    }
  }
}

function load(): boolean {
  let files: string[];
  try { files = fs.readdirSync(SRC).filter((f) => f.endsWith(".json")); } catch { return false; }
  const week = Date.now() - 8 * 86400_000;
  const list: Usage[] = [];
  for (const f of files) {
    const full = path.join(SRC, f);
    try {
      if (fs.statSync(full).mtimeMs < week) continue;
      const u = JSON.parse(fs.readFileSync(full, "utf8")) as Usage;
      if (u.session_id && u.ts) list.push(u);
    } catch { /* arquivo sendo escrito: pega na proxima */ }
  }
  findProfiles(list.map((u) => u.session_id));
  sessions = list
    .map((u) => ({ ...u, t: Date.parse(u.ts), profile: profileOf.get(u.session_id) ?? UNKNOWN }))
    .sort((a, b) => b.t - a.t);
  return true;
}

// ---------------------------------------------------------------- texto

function ago(t: number): string {
  const m = Math.max(0, (Date.now() - t) / 60000);
  if (m < 60) return `há ${Math.max(1, Math.floor(m))}min`;
  if (m < 1440) return `há ${Math.floor(m / 60)}h`;
  return `há ${Math.floor(m / 1440)}d`;
}

function until(epoch: number): string {
  const m = Math.max(0, (epoch * 1000 - Date.now()) / 60000);
  if (m < 60) return `${Math.ceil(m)}min`;
  if (m < 1440) return `${Math.floor(m / 60)}h${String(Math.floor(m % 60)).padStart(2, "0")}`;
  return `${Math.floor(m / 1440)}d ${Math.floor((m % 1440) / 60)}h`;
}

const money = (v = 0) => `US$ ${v.toFixed(2)}`;

/** Percentual valido agora (janela ja renovada conta 0). */
function pct(l?: Limit): number | null {
  if (!l) return null;
  return l.resets_at * 1000 <= Date.now() ? 0 : Math.round(l.used_percentage);
}

const colorFor = (p: number) => (p >= 90 ? "blocked" : p >= warn ? "working" : undefined);

// ---------------------------------------------------------------- visoes

type Summary = { profile: Profile; latest: Session; cost: number; l5?: Limit; l7?: Limit; five: number | null; seven: number | null };

function summaries(): Summary[] {
  const today = new Date().toDateString();
  const by = new Map<string, Summary>();
  for (const s of sessions) { // ordenado do mais recente: o 1o de cada perfil tem os limites atuais
    let x = by.get(s.profile.name);
    if (!x) { x = { profile: s.profile, latest: s, cost: 0, five: null, seven: null }; by.set(s.profile.name, x); }
    // Sessao nova ainda sem rate_limits: vale o limite da sessao anterior que tem.
    x.l5 ??= s.rate_limits?.five_hour;
    x.l7 ??= s.rate_limits?.seven_day;
    if (new Date(s.t).toDateString() === today) x.cost += s.cost_usd ?? 0;
  }
  for (const x of by.values()) { x.five = pct(x.l5); x.seven = pct(x.l7); }
  // Sem transcript nao da para saber de que conta e o limite: fica so em "Sessoes".
  return [...by.values()].filter((x) => x.profile !== UNKNOWN);
}

function limitItem(id: string, label: string, l: Limit | undefined, p: number | null): Item {
  if (p === null || !l) return { id, title: `${label}: sem dado`, icon: "clock", color: "muted" };
  const renew = l.resets_at * 1000 <= Date.now() ? "já renovou" : `renova em ${until(l.resets_at)}`;
  return { id, title: `${label}: ${p}%`, subtitle: renew, icon: "clock", color: colorFor(p), badge: `${p}%` };
}

function render(): void {
  if (!load()) {
    const help: Item[] = [
      { id: "help1", title: "Pasta de uso não encontrada", subtitle: SRC, icon: "warning", color: "working" },
      { id: "help2", title: "O statusline do Claude Code precisa gravar um JSON por sessão nessa pasta", color: "muted" },
      { id: "help3", title: "Campos: ts, session_id, model, cwd, cost_usd, rate_limits.five_hour/seven_day", color: "muted" },
    ];
    duck.view("usage", [{ title: "Como ativar", items: help }]);
    duck.view("sessions", [{ title: "Como ativar", items: help }]);
    duck.status("usage", "");
    return;
  }

  const sum = summaries();
  const sections: Section[] = sum.map((x) => ({
    title: `Perfil ${x.profile.name}`,
    items: [
      limitItem(`${x.profile.name}|5h`, "5 horas", x.l5, x.five),
      limitItem(`${x.profile.name}|7d`, "7 dias", x.l7, x.seven),
      { id: `${x.profile.name}|cost`, title: `Hoje: ${money(x.cost)}`, subtitle: `atualizado ${ago(x.latest.t)} · ${x.latest.model ?? ""}`, icon: "star" },
    ],
  }));
  duck.view("usage", sections.length ? sections : [{ items: [{ id: "none", title: "Nenhuma sessão nos últimos 7 dias", color: "muted" }] }]);

  const day = Date.now() - 86400_000;
  const recent = sessions.filter((s) => s.t >= day);
  duck.view("sessions", [{
    title: `Últimas 24h (${recent.length})`,
    items: recent.length ? recent.map((s) => ({
      id: s.session_id,
      title: `${path.basename(s.cwd ?? "") || s.cwd || "?"} · ${s.model ?? "?"}`,
      subtitle: `${s.profile.name} · ${money(s.cost_usd)} · ${ago(s.t)} · ${s.cwd ?? ""}`,
      icon: "comment",
      actions: [
        { id: "resume", title: "Retomar numa aba", primary: true },
        { id: "folder", title: "Abrir pasta" },
      ],
    })) : [{ id: "none", title: "Nenhuma sessão nas últimas 24h", color: "muted" }],
  }]);

  // Barra: o perfil mais apertado.
  const worst = sum
    .filter((x) => x.five !== null || x.seven !== null)
    .sort((a, b) => Math.max(b.five ?? 0, b.seven ?? 0) - Math.max(a.five ?? 0, a.seven ?? 0))[0];
  if (worst) {
    const top = Math.max(worst.five ?? 0, worst.seven ?? 0);
    const who = sum.length > 1 ? `${worst.profile.name} ` : "";
    duck.status("usage", `${who}5h ${worst.five ?? "-"}% · 7d ${worst.seven ?? "-"}%`, { color: colorFor(top), view: "usage" });
  } else duck.status("usage", "");

  // Aviso uma vez por janela (resets_at identifica a janela) e por limiar.
  for (const x of sum) {
    for (const [label, l, p] of [["5h", x.l5, x.five], ["7d", x.l7, x.seven]] as const) {
      if (!l || p === null) continue;
      for (const thr of [warn, 95]) {
        const key = `${x.profile.name}|${label}|${l.resets_at}|${thr}`;
        if (p < thr || notified.has(key)) continue;
        notified.add(key);
        duck.notify({
          title: `Claude ${x.profile.name}: ${label} em ${p}%`,
          body: `Renova em ${until(l.resets_at)}.`,
          color: thr >= 95 ? "blocked" : "working",
          actions: [{ id: "open-usage", title: "Ver uso" }],
        });
      }
    }
  }
}

// ---------------------------------------------------------------- protocolo

const psq = (s: string) => s.replace(/'/g, "''");

run({
  init(m) {
    const s = (m.settings ?? {}) as { poll_seconds?: number; warn_percent?: number };
    warn = Number(s.warn_percent) || 80;
    duck.register([
      { id: "usage", title: "Uso", icon: "" },
      { id: "sessions", title: "Sessões", icon: "comment" },
    ]);
    render();
    setInterval(render, Math.max(15, Number(s.poll_seconds) || 60) * 1000);
  },
  viewOpen() { render(); },
  action(m) {
    if (m.action === "refresh") return render();
    if (m.action === "open-usage") return duck.showView("usage");
    const s = sessions.find((x) => x.session_id === m.item);
    if (!s) return duck.flash("Sessão não encontrada");
    if (m.action === "folder" && s.cwd) return duck.openUrl(s.cwd);
    if (m.action === "resume") {
      if (!/^[\w-]+$/.test(s.session_id)) return duck.flash("Id de sessão inválido");
      const env = s.profile.dir ? `$env:CLAUDE_CONFIG_DIR='${psq(s.profile.dir)}'; ` : "";
      duck.openPane(["pwsh", "-NoLogo", "-NoExit", "-Command", `${env}claude --resume ${s.session_id}`],
        { cwd: s.cwd, title: `claude ${path.basename(s.cwd ?? "")}` });
    }
  },
});
