// Extensão Rollbar do Duck: erros (error/critical) e avisos ativos por projeto,
// detalhe com a última ocorrência, resolver e silenciar.
//
// Config (~/.duck.lua):
//   extensions = { rollbar = { accounts = {
//     { name = 'api', account_slug = 'empresa', project_slug = 'api' },              -- slugs opcionais (links)
//   }, environments = { 'production' } } }
// Token (project access token, read; write para resolver/silenciar):
//   echo <token> | duck secret set rollbar/<name>

import { duck, run, Item, Section, Action } from "./duck";

type AccountCfg = { name: string; account_slug?: string; project_slug?: string };
type Settings = { accounts?: AccountCfg[]; poll_seconds?: number; environments?: string[] };
type Account = AccountCfg & { token: string; project?: string };
type RItem = {
  id: number; counter: number; title: string; level: string; environment: string; status: string;
  total_occurrences: number; last_occurrence_timestamp: number; first_occurrence_timestamp: number;
  last_occurrence_id?: number;
};

const API = "https://api.rollbar.com/api/1";
const LEVELS: Record<string, number> = { critical: 0, error: 1, warning: 2, info: 3, debug: 4 };
let settings: Settings = {};
let accounts: Account[] = [];
const items = new Map<string, RItem[]>(); // conta -> itens ativos
const snap = new Map<string, number>();   // "conta|id" -> total_occurrences
const errors = new Map<string, string>();
let first = true;

// ---------------------------------------------------------------- HTTP

async function call(a: Account, path: string, init: RequestInit = {}): Promise<any> {
  const headers: Record<string, string> = { "X-Rollbar-Access-Token": a.token, Accept: "application/json" };
  if (init.body) headers["Content-Type"] = "application/json";
  let r: Response;
  try { r = await fetch(API + path, { ...init, headers }); }
  catch { throw new Error("sem acesso a api.rollbar.com (rede fora do ar?)"); }
  const text = await r.text();
  let j: any = null;
  try { j = text ? JSON.parse(text) : null; } catch { /* html */ }
  // O Rollbar repete o token na mensagem de erro: nunca deixa ele ir para tela/log.
  if (!r.ok || (j && j.err)) throw new Error(`${r.status} ${String(j?.message ?? text.slice(0, 160)).split(a.token).join("***")}`);
  return j?.result;
}

// ---------------------------------------------------------------- texto

function ago(sec: number): string {
  const s = Math.max(0, Date.now() / 1000 - sec);
  if (s < 3600) return `há ${Math.max(1, Math.floor(s / 60))}min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)}h`;
  return `há ${Math.floor(s / 86400)}d`;
}
function local(sec: number) { return new Date(sec * 1000).toLocaleString("pt-BR"); }
function isError(i: RItem) { return i.level === "error" || i.level === "critical"; }
function idOf(a: Account, i: RItem) { return `r|${a.name}|${i.id}`; }

// ---------------------------------------------------------------- dados

async function loadAccounts(): Promise<Account[]> {
  const out: Account[] = [];
  for (const c of settings.accounts ?? []) {
    const token = await duck.secret(`rollbar/${c.name}`);
    if (!token) { errors.set(c.name, `sem token: echo <token> | duck secret set rollbar/${c.name}`); continue; }
    const a: Account = { ...c, token };
    // Nome do projeto é só enfeite: token sem permissão para /project não impede o resto.
    try { const p = await call(a, "/project"); a.project = p?.name; } catch (e) { duck.log("project", c.name, (e as Error).message); }
    out.push(a);
  }
  return out;
}

async function fetchActive(a: Account): Promise<RItem[]> {
  const all: RItem[] = [];
  // ponytail: no máximo 3 páginas (300 itens) por nível; o resto fica de fora.
  for (const level of ["critical", "error", "warning"]) {
    for (let page = 1; page <= 3; page++) {
      const r = await call(a, `/items?status=active&level=${level}&page=${page}`);
      const list: RItem[] = r?.items ?? [];
      all.push(...list);
      if (list.length < 100) break;
    }
  }
  const envs = (settings.environments ?? []).map((e) => e.trim()).filter(Boolean);
  return all
    .filter((i) => !envs.length || envs.includes(i.environment))
    .sort((x, y) => (LEVELS[x.level] ?? 9) - (LEVELS[y.level] ?? 9) || y.last_occurrence_timestamp - x.last_occurrence_timestamp);
}

async function poll(): Promise<void> {
  for (const a of accounts) {
    try {
      const list = await fetchActive(a);
      items.set(a.name, list);
      errors.delete(a.name);
      for (const i of list) {
        const k = `${a.name}|${i.id}`;
        const before = snap.get(k);
        snap.set(k, i.total_occurrences);
        if (first || !isError(i)) continue;
        const acts: Action[] = [{ id: "open", title: "Abrir" }, { id: "web", title: "Navegador" }];
        if (before === undefined) {
          duck.notify({ title: `Rollbar · erro novo em ${a.name}`, body: `#${i.counter} ${i.title}`, item: idOf(a, i), actions: acts, color: "blocked" });
        } else if (i.total_occurrences > before) {
          duck.notify({ title: `Rollbar · voltou a ocorrer (+${i.total_occurrences - before})`, body: `#${i.counter} ${i.title}`, item: idOf(a, i), actions: acts, color: "working" });
        }
      }
    } catch (e) { errors.set(a.name, (e as Error).message); duck.log("poll", a.name, (e as Error).message); }
  }
  first = false;
  render();
}

function row(a: Account, i: RItem): Item {
  return {
    id: idOf(a, i), title: `#${i.counter}  ${i.title}`,
    icon: isError(i) ? "bug" : "warning",
    color: i.level === "critical" ? "blocked" : i.level === "error" ? "working" : "muted",
    badge: `${i.total_occurrences}x`,
    subtitle: [i.level, i.environment, `visto ${ago(i.last_occurrence_timestamp)}`].join(" · "),
    actions: [{ id: "open", title: "Abrir", primary: true }, { id: "web", title: "Navegador" }, { id: "resolve", title: "Resolver" }, { id: "mute", title: "Silenciar" }],
  };
}

function render(): void {
  const multi = (settings.accounts?.length ?? 0) > 1;
  const eSecs: Section[] = [];
  const wSecs: Section[] = [];
  let recent = 0;
  const dayAgo = Date.now() / 1000 - 86400;
  for (const a of accounts) {
    const p = multi ? `${a.project ?? a.name} · ` : "";
    const list = items.get(a.name) ?? [];
    const errs = list.filter(isError);
    const warns = list.filter((i) => !isError(i));
    recent += errs.filter((i) => i.last_occurrence_timestamp >= dayAgo).length;
    eSecs.push({ title: `${p}Erros ativos (${errs.length})`, items: errs.map((i) => row(a, i)) });
    wSecs.push({ title: `${p}Avisos (${warns.length})`, items: warns.map((i) => row(a, i)) });
  }
  const err = [...errors.entries()].map(([n, e]) => n === "config" ? e : `${n}: ${e}`).join(" | ");
  duck.view("errors", eSecs, { error: err });
  duck.view("warnings", wSecs, { error: err });
  duck.status("errors", accounts.length ? ` ${recent}` : "", { color: recent ? "blocked" : "muted", view: "errors" });
}

// ---------------------------------------------------------------- detalhe e ações

function find(name: string, id: string): { a?: Account; i?: RItem } {
  const a = accounts.find((x) => x.name === name);
  return { a, i: items.get(name)?.find((x) => String(x.id) === id) };
}

async function lastInstance(a: Account, id: string): Promise<any> {
  const r = await call(a, `/item/${id}/instances`);
  const list: any[] = r?.instances ?? [];
  return list.sort((x, y) => (y.timestamp ?? 0) - (x.timestamp ?? 0))[0];
}

function traceText(body: any): string[] {
  const out: string[] = [];
  const traces = body?.trace_chain ?? (body?.trace ? [body.trace] : []);
  for (const t of traces) {
    out.push(`${t.exception?.class ?? "Exceção"}: ${t.exception?.message ?? ""}`);
    // Rollbar guarda do mais externo para o mais interno; mostra o ponto da falha primeiro.
    for (const f of [...(t.frames ?? [])].reverse()) {
      out.push(`  em ${f.method ?? "?"} (${f.filename ?? "?"}${f.lineno ? `:${f.lineno}` : ""})`);
      if (f.code) out.push(`      ${String(f.code).trim()}`);
    }
    out.push("");
  }
  if (body?.message?.body) out.push(String(body.message.body), "");
  if (body?.crash_report?.raw) out.push(String(body.crash_report.raw), "");
  return out;
}

async function webUrl(a: Account, id: string, i?: RItem): Promise<string | null> {
  // O item ja traz o project_slug; falta so o slug da conta (a API nao devolve).
  const project = a.project_slug || (i as any)?.project_slug;
  if (a.account_slug && project && i) return `https://rollbar.com/${a.account_slug}/${project}/items/${i.counter}/`;
  const inst = await lastInstance(a, id).catch(() => null);
  const uuid = inst?.data?.uuid ?? inst?.uuid;
  return uuid ? `https://rollbar.com/item/uuid/?uuid=${uuid}` : null;
}

async function showItem(a: Account, id: string): Promise<void> {
  const i: RItem = items.get(a.name)?.find((x) => String(x.id) === id) ?? await call(a, `/item/${id}`);
  // Token so de leitura de listas (ex.: o do MCP) nao abre a ocorrencia: mostra o resumo assim mesmo.
  const inst = await lastInstance(a, id).catch((e) => { duck.log("instance", a.name, (e as Error).message); return null; });
  const d = inst?.data ?? {};
  const req = d.request;
  const body = [
    `Ocorrências: ${i.total_occurrences} · primeira ${local(i.first_occurrence_timestamp)} · última ${local(i.last_occurrence_timestamp)}`,
    "",
    "── Última ocorrência ──",
    ...(inst ? traceText(d.body) : ["(sem ocorrência disponível)"]),
    ...(req ? ["── Request ──", `${req.method ?? ""} ${req.url ?? ""}`.trim(), ...(req.user_ip ? [`IP: ${req.user_ip}`] : []), ""] : []),
    ...(d.server?.host ? [`Servidor: ${d.server.host}`] : []),
    ...(d.code_version ? [`Versão: ${d.code_version}`] : []),
  ].join("\n");
  duck.detail({
    title: `#${i.counter} · ${i.title}`, item: `r|${a.name}|${id}`,
    subtitle: [a.project ?? a.name, i.level, i.environment, i.status, `visto ${ago(i.last_occurrence_timestamp)}`].join(" · "),
    body,
    actions: [{ id: "web", title: "Navegador" }, { id: "resolve", title: "Resolver" }, { id: "mute", title: "Silenciar" }],
  });
}

const pending = new Map<string, () => Promise<void>>();

function confirm(item: string, title: string, yes: string, fn: () => Promise<void>): void {
  const id = `c|${item}`;
  pending.set(id, fn);
  duck.choose(id, title, [{ id: "sim", title: yes }]);
}

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [, name, id] = item.split("|");
  const { a, i } = find(name, id);
  if (!a || !id) return;
  const label = i ? `#${i.counter}` : `item ${id}`;
  const setStatus = (status: string, done: string) => async () => {
    try { await call(a, `/item/${id}`, { method: "PATCH", body: JSON.stringify({ status }) }); }
    catch (e) { duck.flash(`Rollbar: não deu para alterar ${label}: ${(e as Error).message}`); return; }
    duck.flash(`${label} ${done}`);
    await poll();
  };
  switch (action) {
    case "open": await showItem(a, id); break;
    case "web": {
      const url = await webUrl(a, id, i);
      if (url) duck.openUrl(url);
      else duck.flash(`Rollbar: informe o slug da conta (rollbar.com/<slug>/...) em Ctrl+Shift+P > Extensões > Rollbar > conta ${a.name}`);
      break;
    }
    case "resolve": confirm(item, `Resolver ${label}?`, "Sim, marcar como resolvido", setStatus("resolved", "resolvido")); break;
    case "mute": confirm(item, `Silenciar ${label}?`, "Sim, silenciar", setStatus("muted", "silenciado")); break;
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register(
      [{ id: "errors", title: "Rollbar · Erros ativos", icon: "bug" }, { id: "warnings", title: "Rollbar · Avisos", icon: "warning" }],
      [{ id: "errors", title: "Rollbar: erros ativos" }, { id: "warnings", title: "Rollbar: avisos" }],
    );
    if (!settings.accounts?.length) {
      errors.set("config", "Configure extensions.rollbar.accounts (Extensões → Rollbar → Projetos)");
      render();
      return;
    }
    accounts = await loadAccounts();
    await poll();
    setInterval(() => void poll(), Math.max(30, settings.poll_seconds ?? 120) * 1000);
  },
  async viewOpen(m) { if (m.view) render(); },
  async command(m) { duck.showView(m.command); },
  async action(m) {
    try { await onAction(m.action, m.item); }
    catch (e) { duck.flash(`Rollbar: ${(e as Error).message}`); }
  },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value === "sim") await fn();
  },
});
