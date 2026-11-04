// Extensao "Rotinas": pedidos ao Claude Code em horarios marcados, enquanto o
// Duck estiver aberto (as extensoes rodam no server, sem depender da janela).
//
// Cada tarefa e um arquivo ~/.duck/rotinas/<id>.md: cabecalho "chave: valor"
// entre linhas "---" e o pedido no corpo. Exemplo:
//
//   ---
//   title: Noticias de IA
//   cron: 0 8,12,18 * * 1-5        (ou every: 30m)
//   tools: WebSearch, WebFetch
//   ---
//   Resuma as noticias de IA das ultimas horas em 3 frases.
//
// Uma execucao por vez (para nao estourar o limite do plano). O resultado vira
// aviso (cartao) e fica no historico em ~/.duck/rotinas/historico.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import { duck, run, Item, Section, AgentResult } from "./duck";

const DIR = path.join(os.homedir(), ".duck", "rotinas");
const HIST = path.join(DIR, "historico");
const STATE = path.join(DIR, ".estado.json");

type Task = {
  id: string;
  file: string;
  title: string;
  prompt: string;
  cron?: Cron;
  cronText?: string;
  everyMs?: number;
  profile: string;
  tools: string[];
  mode?: "acceptEdits" | "plan";
  cwd: string;
  model?: string;
  maxTurns?: number;
  /** Resposta que comeca com isto nao vira aviso (heartbeat: "nada a relatar"). */
  quiet?: string;
  notify: boolean;
  catchup: boolean;
  enabled: boolean;
  /** Echo do Home Assistant que le a resposta (escritorio, cozinha, todas...). */
  speak?: string;
  /** Push no celular pelo Home Assistant. */
  phone: boolean;
  /** Fala tambem entre 22h e 7h (senao vira so push). */
  speakNight: boolean;
  /** Tamanho maximo do texto falado. */
  speakMax: number;
  /** Adia se o limite de 5h do perfil passou disto (%). 0 = nao olha. */
  usageMax: number;
  /** Adia enquanto um agente seu trabalha no mesmo repositorio. */
  yieldToAgent: boolean;
  error?: string;
};

type Run = { task: string; title: string; at: number; ms: number; ok: boolean; text: string; error?: string; sessionId?: string; file?: string; quiet?: boolean; sent?: string; sendError?: string };
type State = { last: Record<string, number>; paused: Record<string, boolean>; runs: Run[] };

let tasks: Task[] = [];
let state: State = { last: {}, paused: {}, runs: [] };
let queue: string[] = [];
let running: string | null = null;
let filesSig = "";
/** Enderecos do Home Assistant, na ordem (local primeiro, tunel depois). */
let haUrls: string[] = [];
/** Rotinas na hora, esperando o limite ou o agente: id -> motivo. */
const deferred = new Map<string, { since: number; reason: string }>();
let ticking = false;

// ---------------------------------------------------------------- Home Assistant

/** Texto para ser lido em voz alta: sem markdown, links nem quebras, cortado numa frase. */
function speakable(text: string, max: number): string {
  let s = text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, "")
    .replace(/[*_`#>|]/g, "")
    .replace(/^\s*[-•]\s+/gm, "")
    .replace(/\s+/g, " ")
    .trim();
  if (s.length <= max) return s;
  s = s.slice(0, max);
  const cut = Math.max(s.lastIndexOf(". "), s.lastIndexOf("! "), s.lastIndexOf("? "));
  return cut > max * 0.5 ? s.slice(0, cut + 1) : s.replace(/\s+\S*$/, "") + "…";
}

function night(): boolean { const h = new Date().getHours(); return h >= 22 || h < 7; }

/** Manda pelo webhook de avisos do HA ({mensagem, canal, echo, titulo}). Devolve o canal usado. */
async function sendHa(t: Task, text: string): Promise<string | null> {
  let canal: "echo" | "celular" | "ambos" | null = null;
  if (t.speak && (t.speakNight || !night())) canal = t.phone ? "ambos" : "echo";
  else if (t.speak || t.phone) canal = "celular";
  if (!canal) return null;
  const id = await duck.secret("rotinas/ha-webhook");
  if (!id) throw new Error("sem o webhook: echo <id> | duck secret set rotinas/ha-webhook");
  if (!haUrls.length) throw new Error("configure ha_urls nas configuracoes da extensao Rotinas");
  const body = JSON.stringify({ mensagem: speakable(text, t.speakMax), canal, echo: t.speak || "escritorio", titulo: t.title });
  let last = "";
  for (const base of haUrls) {
    try {
      const r = await fetch(`${base.replace(/\/+$/, "")}/api/webhook/${id}`, {
        method: "POST", headers: { "content-type": "application/json; charset=utf-8" }, body, signal: AbortSignal.timeout(8000),
      });
      if (r.ok) return canal === "celular" ? "celular" : `${canal} (${t.speak})`;
      last = `${new URL(base).host}: HTTP ${r.status}`;
    } catch (e) {
      last = `${(() => { try { return new URL(base).host; } catch { return base; } })()}: ${(e as Error).message}`;
    }
  }
  throw new Error(`Home Assistant não respondeu (${last})`);
}

// ---------------------------------------------------------------- cron

type Cron = { min: Set<number>; hour: Set<number>; dom: Set<number>; mon: Set<number>; dow: Set<number>; domAny: boolean; dowAny: boolean };

function field(s: string, lo: number, hi: number): Set<number> {
  const out = new Set<number>();
  for (const part of s.split(",")) {
    const [range, stepS] = part.split("/");
    const step = stepS ? Number(stepS) : 1;
    let a = lo, b = hi;
    if (range !== "*") {
      const m = /^(\d+)(?:-(\d+))?$/.exec(range);
      if (!m) throw new Error(`campo invalido "${part}"`);
      a = Number(m[1]);
      b = m[2] !== undefined ? Number(m[2]) : (stepS ? hi : a);
    }
    if (!(step >= 1) || a < lo || b > hi || a > b) throw new Error(`fora do intervalo "${part}"`);
    for (let v = a; v <= b; v += step) out.add(v);
  }
  return out;
}

function parseCron(s: string): Cron {
  const f = s.trim().split(/\s+/);
  if (f.length !== 5) throw new Error("cron precisa de 5 campos: minuto hora dia mes dia-da-semana");
  const dow = field(f[4], 0, 7);
  if (dow.has(7)) dow.add(0);
  return { min: field(f[0], 0, 59), hour: field(f[1], 0, 23), dom: field(f[2], 1, 31), mon: field(f[3], 1, 12), dow, domAny: f[2] === "*", dowAny: f[4] === "*" };
}

/** Proximo minuto que casa com o cron, depois de `after` (hora local). */
function nextCron(c: Cron, after: number): number | null {
  const d = new Date(after);
  d.setSeconds(0, 0);
  d.setMinutes(d.getMinutes() + 1);
  // Ate 8 anos: cobre 29/02 (o laco pula mes/dia/hora inteiros, e rapido).
  const limit = after + 8 * 366 * 86400_000;
  while (d.getTime() < limit) {
    if (!c.mon.has(d.getMonth() + 1)) { d.setMonth(d.getMonth() + 1, 1); d.setHours(0, 0); continue; }
    const dom = c.dom.has(d.getDate()), dow = c.dow.has(d.getDay());
    // Como no cron: com os dois restritos, basta um casar.
    const day = c.domAny && c.dowAny ? true : c.domAny ? dow : c.dowAny ? dom : dom || dow;
    if (!day) { d.setDate(d.getDate() + 1); d.setHours(0, 0); continue; }
    if (!c.hour.has(d.getHours())) { d.setHours(d.getHours() + 1, 0); continue; }
    if (!c.min.has(d.getMinutes())) { d.setMinutes(d.getMinutes() + 1); continue; }
    return d.getTime();
  }
  return null;
}

function parseEvery(s: string): number {
  const m = /^(\d+)\s*(m|min|h|d)?$/i.exec(s.trim());
  if (!m) throw new Error(`every invalido "${s}" (use 30m, 2h, 1d)`);
  const n = Number(m[1]), u = (m[2] ?? "m").toLowerCase();
  const ms = n * (u === "h" ? 3600_000 : u === "d" ? 86400_000 : 60_000);
  if (ms < 5 * 60_000) throw new Error("every minimo e 5m");
  return ms;
}

// ---------------------------------------------------------------- arquivos

const read = (f: string) => { try { return fs.readFileSync(f, "utf8"); } catch { return ""; } };

function loadState(): void {
  try { state = { last: {}, paused: {}, runs: [], ...JSON.parse(read(STATE) || "{}") }; } catch { /* comeca do zero */ }
}
function saveState(): void {
  fs.mkdirSync(DIR, { recursive: true });
  fs.writeFileSync(STATE, JSON.stringify(state, null, 1));
}

function parseTask(file: string): Task {
  const id = path.basename(file, ".md");
  const text = read(file).replace(/\r/g, "");
  const m = /^---\n([\s\S]*?)\n---\n?([\s\S]*)$/.exec(text);
  const head: Record<string, string> = {};
  for (const line of (m?.[1] ?? "").split("\n")) {
    const kv = /^\s*([\w-]+)\s*:\s*(.*?)\s*$/.exec(line);
    if (kv) head[kv[1].toLowerCase()] = kv[2].replace(/^["']|["']$/g, "");
  }
  const bool = (k: string, d: boolean) => (head[k] === undefined ? d : !/^(false|nao|não|no|0|off)$/i.test(head[k]));
  const t: Task = {
    id, file,
    title: head.title || id,
    prompt: (m?.[2] ?? text).trim(),
    profile: head.profile ?? "",
    tools: (head.tools ?? "").split(",").map((s) => s.trim()).filter(Boolean),
    mode: head.mode === "acceptEdits" || head.mode === "plan" ? head.mode : undefined,
    cwd: head.cwd ? head.cwd.replace(/^~(?=$|[\\/])/, os.homedir()) : DIR,
    model: head.model || undefined,
    maxTurns: Number(head.max_turns) || undefined,
    quiet: head.quiet || undefined,
    notify: bool("notify", true),
    catchup: bool("catchup", false),
    enabled: bool("enabled", true),
    speak: head.falar || undefined,
    phone: bool("celular", false),
    speakNight: bool("falar_noite", false),
    speakMax: Math.min(600, Number(head.falar_max) || 250),
    usageMax: head.uso_max !== undefined ? Math.max(0, Math.min(100, Number(head.uso_max) || 0)) : 85,
    yieldToAgent: bool("ceder", true),
  };
  try {
    if (!m) throw new Error("falta o cabecalho entre linhas ---");
    if (!t.prompt) throw new Error("o pedido (corpo do arquivo) esta vazio");
    if (head.cron) { t.cron = parseCron(head.cron); t.cronText = head.cron; }
    else if (head.every) t.everyMs = parseEvery(head.every);
    else throw new Error("falta cron: ou every:");
    if (!fs.existsSync(t.cwd)) throw new Error(`cwd nao existe: ${t.cwd}`);
  } catch (e) { t.error = (e as Error).message; }
  return t;
}

const EXAMPLE = `---
title: Exemplo: notícias de IA
cron: 0 9 * * 1-5
tools: WebSearch, WebFetch
enabled: false
---
Pesquise as notícias mais relevantes sobre IA das últimas 24 horas e resuma
em até 5 itens curtos, com a fonte de cada um.
`;

function scan(): void {
  fs.mkdirSync(DIR, { recursive: true });
  let names = fs.readdirSync(DIR).filter((n) => n.endsWith(".md"));
  if (!names.length && !state.runs.length && !fs.existsSync(path.join(DIR, ".exemplo-criado"))) {
    fs.writeFileSync(path.join(DIR, "exemplo.md"), EXAMPLE);
    fs.writeFileSync(path.join(DIR, ".exemplo-criado"), "");
    names = ["exemplo.md"];
  }
  const sig = names.map((n) => { try { return `${n}:${fs.statSync(path.join(DIR, n)).mtimeMs}`; } catch { return n; } }).join("|");
  if (sig === filesSig) return;
  filesSig = sig;
  tasks = names.sort().map((n) => parseTask(path.join(DIR, n)));
  // Tarefa nova comeca a contar de agora (nao dispara o horario que ja passou).
  const now = Date.now();
  for (const t of tasks) if (state.last[t.id] === undefined) state.last[t.id] = now;
  saveState();
  render();
}

// ---------------------------------------------------------------- horarios

function next(t: Task): number | null {
  const last = state.last[t.id] ?? Date.now();
  if (t.cron) return nextCron(t.cron, last);
  if (t.everyMs) return last + t.everyMs;
  return null;
}

// ---------------------------------------------------------------- quando adiar

const USAGE_DIR = path.join(os.homedir(), "AppData", "Local", "Temp", "claude-usage");
const usageProfile = new Map<string, string>(); // session_id -> pasta do perfil (minusculas)

function profileDir(profile: string): string {
  if (!profile || profile === "default") return path.join(os.homedir(), ".claude");
  return /[\\/]/.test(profile) ? profile : path.join(os.homedir(), `.claude-${profile}`);
}

/** Limite de 5h do perfil, pelo que o statusline das suas sessoes grava (mesma fonte da extensao Uso do Claude). */
function usage5h(profile: string): { pct: number; resets: number } | null {
  const want = path.resolve(profileDir(profile)).toLowerCase();
  let files: { f: string; t: number }[] = [];
  try {
    files = fs.readdirSync(USAGE_DIR).filter((f) => f.endsWith(".json"))
      .map((f) => ({ f, t: fs.statSync(path.join(USAGE_DIR, f)).mtimeMs }))
      .filter((x) => x.t > Date.now() - 6 * 3600_000).sort((a, b) => b.t - a.t);
  } catch { return null; }
  for (const { f } of files) {
    let u: { session_id?: string; rate_limits?: { five_hour?: { used_percentage: number; resets_at: number } } };
    try { u = JSON.parse(read(path.join(USAGE_DIR, f))); } catch { continue; }
    const l = u.rate_limits?.five_hour;
    const sid = u.session_id;
    if (!sid || !l) continue;
    let dir = usageProfile.get(sid);
    if (dir === undefined) {
      dir = "";
      const home = os.homedir();
      let names: string[] = [];
      try { names = fs.readdirSync(home).filter((n) => n === ".claude" || n.startsWith(".claude-")); } catch { /* sem home */ }
      for (const n of names) {
        const root = path.join(home, n, "projects");
        try {
          if (fs.readdirSync(root).some((d) => fs.existsSync(path.join(root, d, sid + ".jsonl")))) { dir = path.join(home, n).toLowerCase(); break; }
        } catch { /* sem projects */ }
      }
      usageProfile.set(sid, dir);
    }
    if (dir !== want) continue;
    if (l.resets_at * 1000 <= Date.now()) return { pct: 0, resets: 0 };
    return { pct: Math.round(l.used_percentage), resets: l.resets_at * 1000 };
  }
  return null;
}

const tops = new Map<string, string>();
/** Raiz do repositorio git da pasta, em minusculas ("" fora de repositorio ou caminho do WSL). */
function repoRoot(dir: string): string {
  if (!dir || dir.startsWith("/")) return "";
  const k = dir.toLowerCase();
  if (!tops.has(k)) {
    let top = "";
    try {
      top = execFileSync("git", ["-C", dir, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true, stdio: ["ignore", "pipe", "ignore"], timeout: 5000 }).trim();
    } catch { /* nao e repositorio */ }
    tops.set(k, top ? path.resolve(top).toLowerCase() : "");
  }
  return tops.get(k)!;
}

/** Motivo para esperar agora, ou null para rodar. */
async function gate(t: Task): Promise<string | null> {
  if (t.usageMax > 0) {
    const u = usage5h(t.profile);
    if (u && u.pct >= t.usageMax) {
      const at = new Date(u.resets).toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
      return `limite de 5h em ${u.pct}% (renova às ${at})`;
    }
  }
  if (t.yieldToAgent) {
    const mine = repoRoot(t.cwd);
    if (mine) {
      const list = await duck.agents();
      const busy = (list ?? []).find((a) => (a.agent.state === "working" || a.agent.state === "blocked") && repoRoot(a.cwd) === mine);
      if (busy) return `agente trabalhando no mesmo repositório (aba ${busy.tab || busy.title})`;
    }
  }
  return null;
}

async function tick(): Promise<void> {
  if (ticking) return;
  ticking = true;
  try {
    scan();
    const now = Date.now();
    for (const t of tasks) {
      if (t.error || !t.enabled || state.paused[t.id] || queue.includes(t.id) || running === t.id) { deferred.delete(t.id); continue; }
      const due = next(t);
      if (due === null || due > now) { deferred.delete(t.id); continue; }
      const waiting = deferred.get(t.id);
      // Horario perdido (Duck fechado, PC dormindo): so roda se catchup ou se foi agora ha pouco.
      // Adiada pelo limite ou pelo agente: espera ate o proximo horario, depois desiste deste.
      const nextSlot = t.cron ? nextCron(t.cron, due) : t.everyMs ? due + t.everyMs : null;
      const fresh = t.catchup || now - due < 10 * 60_000 || (!!waiting && (nextSlot === null || nextSlot > now));
      if (!fresh) {
        state.last[t.id] = now; saveState(); deferred.delete(t.id);
        duck.log(`pulou ${t.id} (horario de ${new Date(due).toLocaleString("pt-BR")} perdido${waiting ? `, adiada: ${waiting.reason}` : ""})`);
        render();
        continue;
      }
      const why = await gate(t);
      if (why) {
        if (waiting?.reason !== why) { deferred.set(t.id, { since: waiting?.since ?? now, reason: why }); duck.log(`${t.id}: adiada, ${why}`); render(); }
        continue;
      }
      if (waiting) { deferred.delete(t.id); duck.log(`${t.id}: liberada depois de ${Math.round((now - waiting.since) / 60000)} min`); }
      queue.push(t.id);
    }
  } finally { ticking = false; }
  void pump();
}

async function pump(): Promise<void> {
  if (running || !queue.length) return;
  const id = queue.shift()!;
  const t = tasks.find((x) => x.id === id);
  if (!t) return void pump();
  running = id;
  state.last[id] = Date.now();
  saveState();
  duck.status("run", `⏳ ${t.title}`, { color: "working", view: "rotinas" });
  render();
  const started = Date.now();
  let r: AgentResult;
  try {
    r = await duck.agent({
      prompt: t.prompt,
      cwd: t.cwd,
      profile: t.profile,
      tools: t.tools,
      permissionMode: t.mode,
      model: t.model,
      maxTurns: t.maxTurns,
      system: "Voce esta rodando sozinho, pelas rotinas do Duck: ninguem vai responder perguntas nem aprovar nada. " +
        "Faca o que der com as ferramentas liberadas e responda em portugues, direto e curto, porque a resposta vira um aviso na tela." +
        (t.speak ? ` A resposta sera lida em voz alta numa Echo: no maximo ${t.speakMax} caracteres, frases curtas e naturais, sem markdown, listas, links, emojis ou codigo.` : "") +
        (t.quiet ? ` Se nao houver nada que precise de atencao, responda apenas: ${t.quiet}` : ""),
    });
  } catch (e) {
    r = { ok: false, text: "", error: (e as Error).message };
  }
  const run: Run = { task: id, title: t.title, at: started, ms: Date.now() - started, ok: r.ok, text: r.text, error: r.error, sessionId: r.sessionId };
  run.quiet = r.ok && !!t.quiet && r.text.trim().toUpperCase().startsWith(t.quiet.toUpperCase());
  if (r.ok && !run.quiet && r.text.trim()) {
    try { run.sent = (await sendHa(t, r.text)) ?? undefined; }
    catch (e) { run.sendError = (e as Error).message; duck.log(`${id}: aviso pelo HA falhou: ${run.sendError}`); }
  }
  try {
    const dir = path.join(HIST, id);
    fs.mkdirSync(dir, { recursive: true });
    run.file = path.join(dir, new Date(started).toISOString().replace(/[:.]/g, "-") + ".md");
    fs.writeFileSync(run.file, `# ${t.title}\n\n${new Date(started).toLocaleString("pt-BR")} · ${Math.round(run.ms / 1000)}s · ${r.ok ? "ok" : `erro: ${r.error}`}${run.sent ? ` · enviado: ${run.sent}` : ""}${run.sendError ? ` · aviso falhou: ${run.sendError}` : ""}\n\n${r.text}\n`);
  } catch { /* sem historico em disco, segue */ }
  state.runs.unshift(run);
  state.runs = state.runs.slice(0, 30);
  saveState();
  running = null;
  duck.status("run", "");
  if (t.notify && !run.quiet) {
    const key = String(run.at);
    duck.notify({
      title: r.ok ? t.title : `${t.title}: falhou`,
      body: r.ok ? r.text.slice(0, 400) : (r.error ?? "erro"),
      color: r.ok ? "done" : "blocked",
      item: `${id}|${key}`,
      seconds: 30,
      actions: [{ id: "show", title: "Ver", primary: true }, ...(r.sessionId ? [{ id: "resume", title: "Continuar no Claude" }] : [])],
    });
  }
  render();
  void pump();
}

// ---------------------------------------------------------------- visao

function when(ms: number): string {
  const d = new Date(ms), now = new Date();
  const hm = d.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return `hoje ${hm}`;
  const tomorrow = new Date(now); tomorrow.setDate(now.getDate() + 1);
  if (d.toDateString() === tomorrow.toDateString()) return `amanhã ${hm}`;
  return `${d.toLocaleDateString("pt-BR", { weekday: "short", day: "2-digit", month: "2-digit" })} ${hm}`;
}

function ago(ms: number): string {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 60) return "agora";
  if (s < 3600) return `há ${Math.round(s / 60)} min`;
  if (s < 86400) return `há ${Math.round(s / 3600)} h`;
  return `há ${Math.round(s / 86400)} d`;
}

function render(): void {
  const items: Item[] = tasks.map((t) => {
    const paused = !!state.paused[t.id];
    const last = state.runs.find((r) => r.task === t.id);
    const n = next(t);
    let sub: string;
    let color: string | undefined;
    if (t.error) { sub = `erro no arquivo: ${t.error}`; color = "blocked"; }
    else if (running === t.id) { sub = "rodando agora…"; color = "working"; }
    else if (queue.includes(t.id)) { sub = "na fila"; color = "working"; }
    else if (!t.enabled) { sub = "desligada (enabled: false)"; color = "muted"; }
    else if (paused) { sub = "pausada"; color = "muted"; }
    else if (deferred.has(t.id)) { sub = `adiada: ${deferred.get(t.id)!.reason}`; color = "working"; }
    else sub = n ? `próxima: ${when(n)}` : "sem horário";
    if (last && !t.error) sub += ` · última: ${last.ok ? (last.quiet ? "nada a relatar" : "ok") : "falhou"} ${ago(last.at)}`;
    return {
      id: t.id, title: t.title, subtitle: sub, icon: t.error ? "warning" : "clock", color,
      badge: t.cronText ?? (t.everyMs ? `a cada ${Math.round(t.everyMs / 60000)} min` : undefined),
      actions: [
        { id: "run", title: "Rodar agora", primary: true },
        { id: "edit", title: "Editar" },
        ...(t.speak || t.phone ? [{ id: "test-send", title: t.speak ? "Testar fala" : "Testar push" }] : []),
        ...(t.enabled && !t.error ? [{ id: paused ? "resume-task" : "pause", title: paused ? "Retomar" : "Pausar" }] : []),
      ],
    };
  });
  items.push({ id: "_new", title: "Nova rotina", subtitle: `arquivo .md em ${DIR}`, icon: "rocket", actions: [{ id: "new", title: "Criar", primary: true }, { id: "folder", title: "Abrir pasta" }] });
  const runs: Item[] = state.runs.slice(0, 15).map((r) => ({
    id: `${r.task}|${r.at}`,
    title: r.title,
    subtitle: `${ago(r.at)} · ${Math.round(r.ms / 1000)}s · ${r.sendError ? `aviso falhou: ${r.sendError} · ` : r.sent ? `${r.sent} · ` : ""}${r.ok ? (r.quiet ? "nada a relatar" : r.text.replace(/\s+/g, " ").slice(0, 120)) : r.error}`,
    icon: r.ok ? (r.sendError ? "warning" : "check") : "x",
    color: r.ok ? (r.sendError ? "blocked" : r.quiet ? "muted" : "done") : "blocked",
    actions: [{ id: "show", title: "Ver", primary: true }, ...(r.sessionId ? [{ id: "resume", title: "Continuar no Claude" }] : [])],
  }));
  const sections: Section[] = [{ title: "Rotinas", items }];
  if (runs.length) sections.push({ title: "Últimas execuções", items: runs });
  duck.view("rotinas", sections);
}

// ---------------------------------------------------------------- acoes

const psq = (s: string) => s.replace(/'/g, "''");

function findRun(item: string): Run | undefined {
  const [task, at] = item.split("|");
  return state.runs.find((r) => r.task === task && String(r.at) === at);
}

function newTask(): void {
  fs.mkdirSync(DIR, { recursive: true });
  let n = 1;
  while (fs.existsSync(path.join(DIR, `rotina-${n}.md`))) n++;
  const f = path.join(DIR, `rotina-${n}.md`);
  fs.writeFileSync(f, `---
title: Nova rotina
# cron: minuto hora dia mês dia-da-semana (ex.: 0 8 * * 1-5 = 8h nos dias úteis)
cron: 0 8 * * 1-5
# every: 30m          (no lugar do cron: a cada N minutos/horas)
# profile: trabalho      (perfil do Claude Code; vazio = ~/.claude)
# tools: WebSearch, WebFetch, Read, Bash(gh issue list:*)
# cwd: ~/projetos/repo
# quiet: OK           (resposta que começa com isto não vira aviso)
# falar: escritorio   (Echo que lê a resposta pelo Home Assistant; entre 22h e 7h vira push)
# celular: true       (push no celular pelo Home Assistant)
# uso_max: 85         (adia se o limite de 5h do perfil passou disto; 0 = não olha)
# ceder: true         (adia enquanto um agente seu trabalha no mesmo repositório)
enabled: false
---
Escreva aqui o pedido, como escreveria para o Claude Code.
`);
  duck.openUrl(f);
  scan();
}

run({
  init(m) {
    const s = (m.settings ?? {}) as { ha_urls?: string | string[] };
    haUrls = (Array.isArray(s.ha_urls) ? s.ha_urls : String(s.ha_urls ?? "").split(",")).map((u) => String(u).trim()).filter((u) => /^https?:\/\//.test(u));
    duck.register([{ id: "rotinas", title: "Rotinas", icon: "clock" }], [
      { id: "new", title: "Rotinas: nova rotina" },
      { id: "folder", title: "Rotinas: abrir pasta" },
    ]);
    loadState();
    scan();
    void tick();
    setInterval(() => void tick(), 20_000);
    // Atualiza "próxima"/"há N min" na visao.
    setInterval(render, 60_000);
  },
  viewOpen() { scan(); render(); },
  command(m) {
    if (m.command === "new") newTask();
    if (m.command === "folder") duck.openUrl(DIR);
  },
  action(m) {
    if (m.action === "refresh") { filesSig = ""; scan(); return render(); }
    if (m.action === "new") return newTask();
    if (m.action === "folder") return duck.openUrl(DIR);
    scan(); // arquivo criado/editado agora ainda nao entrou no tick
    const t = tasks.find((x) => x.id === m.item);
    if (m.action === "run" && t) {
      if (t.error) return duck.flash(`Arquivo com erro: ${t.error}`);
      if (running === t.id || queue.includes(t.id)) return duck.flash("Já está rodando ou na fila");
      queue.push(t.id);
      duck.flash(running ? `${t.title}: na fila` : `${t.title}: rodando`);
      void pump();
      return render();
    }
    if (m.action === "edit" && t) return duck.openUrl(t.file);
    if (m.action === "test-send" && t) {
      // Fala de teste mesmo de noite: foi pedida agora.
      return void sendHa({ ...t, speakNight: true }, `Teste da rotina ${t.title}. Se você está ouvindo isto, o aviso funciona.`)
        .then((c) => duck.flash(`Enviado: ${c}`), (e) => duck.flash(`Falhou: ${(e as Error).message}`));
    }
    if (m.action === "pause" && t) { state.paused[t.id] = true; saveState(); return render(); }
    if (m.action === "resume-task" && t) {
      // Retomar conta de agora: nao dispara o que passou enquanto estava pausada.
      delete state.paused[t.id]; state.last[t.id] = Date.now(); saveState(); return render();
    }
    const r = findRun(m.item);
    if (!r) return;
    if (m.action === "show") {
      return duck.detail({
        title: r.title,
        subtitle: `${new Date(r.at).toLocaleString("pt-BR")} · ${Math.round(r.ms / 1000)}s` +
          (r.sent ? ` · enviado: ${r.sent}` : "") + (r.sendError ? ` · aviso falhou: ${r.sendError}` : ""),
        body: r.ok ? r.text || "(sem texto)" : `Falhou: ${r.error}\n\n${r.text}`,
        item: m.item,
        actions: [...(r.sessionId ? [{ id: "resume", title: "Continuar no Claude" }] : []), ...(r.file ? [{ id: "file", title: "Abrir arquivo" }] : [])],
      });
    }
    if (m.action === "file" && r.file) return duck.openUrl(r.file);
    if (m.action === "resume" && r.sessionId) {
      if (!/^[\w-]+$/.test(r.sessionId)) return;
      const task = tasks.find((x) => x.id === r.task);
      const dir = task?.profile ? (/[\\/]/.test(task.profile) ? task.profile : path.join(os.homedir(), `.claude-${task.profile}`)) : "";
      const env = dir ? `$env:CLAUDE_CONFIG_DIR='${psq(dir)}'; ` : "";
      duck.openPane(["pwsh", "-NoLogo", "-NoExit", "-Command", `${env}claude --resume ${r.sessionId}`],
        { cwd: task?.cwd ?? DIR, title: `rotina: ${r.title}` });
    }
  },
});
