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
  error?: string;
};

type Run = { task: string; title: string; at: number; ms: number; ok: boolean; text: string; error?: string; sessionId?: string; file?: string; quiet?: boolean };
type State = { last: Record<string, number>; paused: Record<string, boolean>; runs: Run[] };

let tasks: Task[] = [];
let state: State = { last: {}, paused: {}, runs: [] };
let queue: string[] = [];
let running: string | null = null;
let filesSig = "";

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

function tick(): void {
  scan();
  const now = Date.now();
  for (const t of tasks) {
    if (t.error || !t.enabled || state.paused[t.id] || queue.includes(t.id) || running === t.id) continue;
    const due = next(t);
    if (due === null || due > now) continue;
    // Horario perdido (Duck fechado, PC dormindo): so roda se catchup ou se foi agora ha pouco.
    if (t.catchup || now - due < 10 * 60_000) queue.push(t.id);
    else { state.last[t.id] = now; saveState(); duck.log(`pulou ${t.id} (horario de ${new Date(due).toLocaleString("pt-BR")} perdido)`); }
  }
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
        (t.quiet ? ` Se nao houver nada que precise de atencao, responda apenas: ${t.quiet}` : ""),
    });
  } catch (e) {
    r = { ok: false, text: "", error: (e as Error).message };
  }
  const run: Run = { task: id, title: t.title, at: started, ms: Date.now() - started, ok: r.ok, text: r.text, error: r.error, sessionId: r.sessionId };
  run.quiet = r.ok && !!t.quiet && r.text.trim().toUpperCase().startsWith(t.quiet.toUpperCase());
  try {
    const dir = path.join(HIST, id);
    fs.mkdirSync(dir, { recursive: true });
    run.file = path.join(dir, new Date(started).toISOString().replace(/[:.]/g, "-") + ".md");
    fs.writeFileSync(run.file, `# ${t.title}\n\n${new Date(started).toLocaleString("pt-BR")} · ${Math.round(run.ms / 1000)}s · ${r.ok ? "ok" : `erro: ${r.error}`}\n\n${r.text}\n`);
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
    else sub = n ? `próxima: ${when(n)}` : "sem horário";
    if (last && !t.error) sub += ` · última: ${last.ok ? (last.quiet ? "nada a relatar" : "ok") : "falhou"} ${ago(last.at)}`;
    return {
      id: t.id, title: t.title, subtitle: sub, icon: t.error ? "warning" : "clock", color,
      badge: t.cronText ?? (t.everyMs ? `a cada ${Math.round(t.everyMs / 60000)} min` : undefined),
      actions: [
        { id: "run", title: "Rodar agora", primary: true },
        { id: "edit", title: "Editar" },
        ...(t.enabled && !t.error ? [{ id: paused ? "resume-task" : "pause", title: paused ? "Retomar" : "Pausar" }] : []),
      ],
    };
  });
  items.push({ id: "_new", title: "Nova rotina", subtitle: `arquivo .md em ${DIR}`, icon: "rocket", actions: [{ id: "new", title: "Criar", primary: true }, { id: "folder", title: "Abrir pasta" }] });
  const runs: Item[] = state.runs.slice(0, 15).map((r) => ({
    id: `${r.task}|${r.at}`,
    title: r.title,
    subtitle: `${ago(r.at)} · ${Math.round(r.ms / 1000)}s · ${r.ok ? (r.quiet ? "nada a relatar" : r.text.replace(/\s+/g, " ").slice(0, 120)) : r.error}`,
    icon: r.ok ? "check" : "x",
    color: r.ok ? (r.quiet ? "muted" : "done") : "blocked",
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
enabled: false
---
Escreva aqui o pedido, como escreveria para o Claude Code.
`);
  duck.openUrl(f);
  scan();
}

run({
  init() {
    duck.register([{ id: "rotinas", title: "Rotinas", icon: "clock" }], [
      { id: "new", title: "Rotinas: nova rotina" },
      { id: "folder", title: "Rotinas: abrir pasta" },
    ]);
    loadState();
    scan();
    tick();
    setInterval(tick, 20_000);
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
        subtitle: `${new Date(r.at).toLocaleString("pt-BR")} · ${Math.round(r.ms / 1000)}s`,
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
