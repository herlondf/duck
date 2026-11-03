// Extensao Jira do Duck: tarefas atribuidas (com prazos), issues acompanhadas
// e avisos de comentario novo e mudanca de status. Jira Server/Data Center
// (API v2, token pessoal Bearer) e Jira Cloud (API v3, email + token).
//
// Config (~/.duck.lua):
//   extensions = { jira = { accounts = {
//     { name = 'trabalho', url = 'https://jira.empresa.com', secret = 'jira/trabalho' },         -- Server/DC
//     { name = 'cloud', url = 'https://x.atlassian.net', type = 'cloud', email = 'eu@x.com' },     -- Cloud
//   } } }
// Token: echo <token> | duck secret set jira/<name>

import { duck, run, Item, Section, Action } from "./duck";

type AccountCfg = { name: string; url: string; type?: "server" | "cloud"; email?: string; secret?: string };
type Settings = { accounts?: AccountCfg[]; poll_seconds?: number; due_days?: number };
type Account = AccountCfg & { auth: string; me?: { name: string; displayName: string; accountId?: string } };
type Issue = {
  key: string;
  fields: {
    summary: string; updated: string; duedate: string | null;
    status: { name: string; statusCategory: { key: string } };
    priority?: { name: string } | null; issuetype?: { name: string } | null;
    assignee?: { displayName: string; name?: string; accountId?: string } | null;
  };
};

// Cloudflare na frente de alguns Jira Server barra User-Agent que nao seja de navegador.
const UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
let settings: Settings = {};
let accounts: Account[] = [];
const tasks = new Map<string, Issue[]>();
const watched = new Map<string, Issue[]>();
const reported = new Map<string, Issue[]>();
const snap = new Map<string, { updated: string; status: string }>(); // "conta|KEY"
const errors = new Map<string, string>();
let first = true;

// ---------------------------------------------------------------- HTTP

function base(a: Account) { return a.url.replace(/\/$/, ""); }
function api3(a: Account) { return a.type === "cloud"; }

async function call(a: Account, path: string, init: RequestInit & { key?: string } = {}): Promise<any> {
  const write = init.method && init.method !== "GET";
  const headers: Record<string, string> = { Authorization: a.auth, Accept: "application/json", "User-Agent": UA };
  if (write) {
    // Sem estes o Jira Server responde 403 "XSRF check failed", mesmo com token valido.
    headers["X-Atlassian-Token"] = "no-check";
    headers["Origin"] = base(a);
    headers["Referer"] = init.key ? `${base(a)}/browse/${init.key}` : base(a);
    headers["Content-Type"] = "application/json";
  }
  let r: Response;
  try { r = await fetch(base(a) + path, { ...init, headers }); }
  catch (e) { throw new Error(`sem acesso a ${base(a)} (rede ou Jira fora do ar)`); }
  const text = await r.text();
  if (!r.ok) {
    let msg = text.slice(0, 160);
    try { const j = JSON.parse(text); msg = (j.errorMessages ?? []).join("; ") || j.message || msg; } catch { /* html */ }
    throw new Error(`${r.status} ${msg}`);
  }
  return text ? JSON.parse(text) : null;
}

const FIELDS = "summary,status,priority,issuetype,duedate,updated,assignee";

async function jql(a: Account, q: string, max = 100): Promise<Issue[]> {
  if (api3(a)) {
    const r = await call(a, `/rest/api/3/search/jql?jql=${encodeURIComponent(q)}&maxResults=${max}&fields=${FIELDS}`);
    return r.issues ?? [];
  }
  const r = await call(a, `/rest/api/2/search?jql=${encodeURIComponent(q)}&maxResults=${max}&fields=${FIELDS}`);
  return r.issues ?? [];
}

// ---------------------------------------------------------------- texto

function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 3600) return `ha ${Math.max(1, Math.floor(s / 60))}min`;
  if (s < 86400) return `ha ${Math.floor(s / 3600)}h`;
  return `ha ${Math.floor(s / 86400)}d`;
}
function ddmm(d: string) { const [y, m, dd] = d.split("-"); return `${dd}/${m}`; }
function daysTo(d: string) {
  const today = new Date(); today.setHours(0, 0, 0, 0);
  return Math.round((Date.parse(d + "T00:00:00") - today.getTime()) / 86400000);
}
/** Comentario do Cloud vem em ADF (JSON): junta os nos de texto. */
function adf(n: any): string {
  if (!n) return "";
  if (typeof n === "string") return n;
  if (n.type === "text") return n.text ?? "";
  const inner = (n.content ?? []).map(adf).join("");
  return ["paragraph", "heading", "listItem", "codeBlock", "blockquote"].includes(n.type) ? inner + "\n" : inner;
}
function txt(x: any): string { return (typeof x === "string" ? x : adf(x)).replace(/\r/g, "").trim(); }

function icon(t?: string | null): string {
  const n = (t ?? "").toLowerCase();
  if (n.includes("bug") || n.includes("defeito")) return "bug";
  if (n.includes("hist") || n.includes("story")) return "list";
  if (n.includes("epic") || n.includes("épico")) return "rocket";
  return "check";
}
function color(i: Issue): string {
  const c = i.fields.status.statusCategory.key;
  return c === "done" ? "done" : c === "indeterminate" ? "working" : "muted";
}

// ---------------------------------------------------------------- dados

async function loadAccounts(): Promise<Account[]> {
  const out: Account[] = [];
  for (const c of settings.accounts ?? []) {
    const token = await duck.secret(c.secret ?? `jira/${c.name}`);
    if (!token) { errors.set(c.name, `sem token: echo <token> | duck secret set ${c.secret ?? "jira/" + c.name}`); continue; }
    const auth = c.type === "cloud" ? "Basic " + Buffer.from(`${c.email}:${token}`).toString("base64") : `Bearer ${token}`;
    const a: Account = { ...c, auth };
    try { a.me = await call(a, api3(a) ? "/rest/api/3/myself" : "/rest/api/2/myself"); } catch (e) { errors.set(c.name, String((e as Error).message)); }
    out.push(a);
  }
  return out;
}

async function poll(): Promise<void> {
  for (const a of accounts) {
    try {
      const [t, w, r] = await Promise.all([
        jql(a, "assignee = currentUser() AND statusCategory != Done ORDER BY duedate ASC, updated DESC"),
        jql(a, "watcher = currentUser() AND (assignee != currentUser() OR assignee is EMPTY) AND statusCategory != Done ORDER BY updated DESC", 50),
        jql(a, "reporter = currentUser() AND (assignee != currentUser() OR assignee is EMPTY) AND statusCategory != Done ORDER BY updated DESC", 30),
      ]);
      tasks.set(a.name, t); watched.set(a.name, w); reported.set(a.name, r);
      errors.delete(a.name);
      await detectChanges(a, [...t, ...w, ...r], new Set(t.map((i) => i.key)));
    } catch (e) { errors.set(a.name, String((e as Error).message)); duck.log("poll", a.name, e); }
  }
  first = false;
  render();
}

/** Compara com a leitura anterior: atribuida a mim, status novo, comentario de outra pessoa. */
async function detectChanges(a: Account, all: Issue[], mine: Set<string>): Promise<void> {
  for (const i of all) {
    const k = `${a.name}|${i.key}`;
    const before = snap.get(k);
    snap.set(k, { updated: i.fields.updated, status: i.fields.status.name });
    if (first) continue;
    const item = `j|${a.name}|${i.key}`;
    const acts: Action[] = [{ id: "open", title: "Abrir" }, { id: "web", title: "Navegador" }];
    if (!before) {
      if (mine.has(i.key)) duck.notify({ title: `Jira · atribuida a voce: ${i.key}`, body: i.fields.summary, item, actions: acts, color: "accent" });
      continue;
    }
    if (before.updated === i.fields.updated) continue;
    if (before.status !== i.fields.status.name) {
      duck.notify({ title: `Jira · ${i.key}: ${before.status} → ${i.fields.status.name}`, body: i.fields.summary, item, actions: acts, color: "working" });
      continue;
    }
    // Atualizou sem mudar status: ve se foi comentario de outra pessoa.
    try {
      const full = await call(a, `/rest/api/${api3(a) ? 3 : 2}/issue/${i.key}?fields=comment`);
      const list = full.fields.comment?.comments ?? [];
      const last = list[list.length - 1];
      const fromMe = last && (last.author?.name === a.me?.name || (a.me?.accountId && last.author?.accountId === a.me.accountId));
      if (last && !fromMe && Date.parse(last.created) > Date.parse(before.updated)) {
        duck.notify({ title: `Jira · ${i.key}: comentario de ${last.author?.displayName}`, body: txt(last.body).split("\n")[0].slice(0, 140), item, actions: acts, color: "accent" });
      }
    } catch (e) { duck.log("comentarios", i.key, e); }
  }
}

function issueItem(a: Account, i: Issue): Item {
  const f = i.fields;
  const due = f.duedate ? daysTo(f.duedate) : null;
  const badge = due === null ? "" : due < 0 ? `atrasada ${-due}d` : due === 0 ? "vence hoje" : `prazo ${ddmm(f.duedate!)}`;
  return {
    id: `j|${a.name}|${i.key}`, title: `${i.key}  ${f.summary}`, icon: icon(f.issuetype?.name),
    color: due !== null && due < 0 ? "blocked" : color(i), badge,
    subtitle: [f.status.name, f.priority?.name, f.assignee ? f.assignee.displayName : "", ago(f.updated)].filter(Boolean).join(" · "),
    actions: [{ id: "open", title: "Abrir", primary: true }, { id: "transition", title: "Status" }, { id: "comment", title: "Comentar" }, { id: "web", title: "Navegador" }],
  };
}

function render(): void {
  const multi = accounts.length > 1;
  const days = settings.due_days ?? 3;
  const tSecs: Section[] = [];
  const wSecs: Section[] = [];
  let open = 0, late = 0;
  for (const a of accounts) {
    const p = multi ? `${a.name} · ` : "";
    const t = tasks.get(a.name) ?? [];
    open += t.length;
    const lateL = t.filter((i) => i.fields.duedate && daysTo(i.fields.duedate) < 0);
    const soon = t.filter((i) => i.fields.duedate && daysTo(i.fields.duedate) >= 0 && daysTo(i.fields.duedate) <= days);
    const rest = t.filter((i) => !lateL.includes(i) && !soon.includes(i));
    late += lateL.length;
    if (lateL.length) tSecs.push({ title: `${p}Atrasadas (${lateL.length})`, items: lateL.map((i) => issueItem(a, i)) });
    if (soon.length) tSecs.push({ title: `${p}Vencem em ate ${days} dias (${soon.length})`, items: soon.map((i) => issueItem(a, i)) });
    const doing = rest.filter((i) => i.fields.status.statusCategory.key === "indeterminate");
    const todo = rest.filter((i) => !doing.includes(i));
    tSecs.push({ title: `${p}Em andamento (${doing.length})`, items: doing.map((i) => issueItem(a, i)) });
    tSecs.push({ title: `${p}A fazer (${todo.length})`, items: todo.map((i) => issueItem(a, i)) });
    wSecs.push({ title: `${p}Acompanhando (${watched.get(a.name)?.length ?? 0})`, items: (watched.get(a.name) ?? []).map((i) => issueItem(a, i)) });
    wSecs.push({ title: `${p}Criadas por mim (${reported.get(a.name)?.length ?? 0})`, items: (reported.get(a.name) ?? []).map((i) => issueItem(a, i)) });
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("tasks", tSecs, { error: err });
  duck.view("watch", wSecs, { error: err });
  duck.status("tasks", ` ${open}${late ? ` · ${late} atrasada${late > 1 ? "s" : ""}` : ""}`, { color: late ? "blocked" : open ? "accent" : "muted", view: "tasks" });
}

// ---------------------------------------------------------------- detalhe e acoes

async function showIssue(a: Account, key: string): Promise<void> {
  const v = api3(a) ? 3 : 2;
  const i = await call(a, `/rest/api/${v}/issue/${key}?fields=summary,status,assignee,reporter,priority,duedate,description,comment,issuetype,created,labels`);
  const f = i.fields;
  const comments = (f.comment?.comments ?? []).slice(-10);
  const body = [
    txt(f.description) || "(sem descricao)",
    "",
    `── Comentarios (${f.comment?.total ?? comments.length}${(f.comment?.total ?? 0) > comments.length ? `, ultimos ${comments.length}` : ""}) ──`,
    ...comments.flatMap((c: any) => ["", `# ${c.author?.displayName} · ${ago(c.created)}`, ...txt(c.body).split("\n")]),
  ].join("\n");
  const due = f.duedate ? ` · prazo ${ddmm(f.duedate)}` : "";
  duck.detail({
    title: `${key} · ${f.summary}`, item: `j|${a.name}|${key}`,
    subtitle: [f.issuetype?.name, f.status?.name, f.priority?.name, f.assignee ? `com ${f.assignee.displayName}` : "sem responsavel", `por ${f.reporter?.displayName}`].filter(Boolean).join(" · ") + due + (f.labels?.length ? ` · ${f.labels.join(", ")}` : ""),
    body,
    actions: [{ id: "comment", title: "Comentar" }, { id: "transition", title: "Mudar status" }, { id: "web", title: "Navegador" }],
  });
}

const pending = new Map<string, (v: string) => Promise<void>>();

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [, name, key] = item.split("|");
  const a = accounts.find((x) => x.name === name);
  if (!a || !key) return;
  const v = api3(a) ? 3 : 2;
  switch (action) {
    case "open": await showIssue(a, key); break;
    case "web": duck.openUrl(`${base(a)}/browse/${key}`); break;
    case "comment":
      pending.set(item, async (text) => {
        const body = v === 3 ? { body: { type: "doc", version: 1, content: [{ type: "paragraph", content: [{ type: "text", text }] }] } } : { body: text };
        await call(a, `/rest/api/${v}/issue/${key}/comment`, { method: "POST", body: JSON.stringify(body), key });
        duck.flash(`Comentario enviado em ${key}`);
        await showIssue(a, key);
      });
      duck.prompt(item, `Comentario em ${key}`);
      break;
    case "transition": {
      const t = await call(a, `/rest/api/${v}/issue/${key}/transitions`);
      const opts = (t.transitions ?? []).map((x: any) => ({ id: String(x.id), title: x.name, hint: x.to?.name ? `vai para ${x.to.name}` : "" }));
      if (!opts.length) { duck.flash(`Nenhuma transicao disponivel para ${key}`); return; }
      pending.set(`t|${item}`, async (id) => {
        await call(a, `/rest/api/${v}/issue/${key}/transitions`, { method: "POST", body: JSON.stringify({ transition: { id } }), key });
        duck.flash(`${key}: status alterado`);
        await poll();
      });
      duck.choose(`t|${item}`, `Mudar status de ${key}`, opts);
      break;
    }
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register(
      [{ id: "tasks", title: "Jira · Minhas tarefas", icon: "jira" }, { id: "watch", title: "Jira · Acompanhando", icon: "eye" }],
      [{ id: "tasks", title: "Jira: minhas tarefas" }, { id: "watch", title: "Jira: acompanhando" }, { id: "goto", title: "Jira: abrir issue (CHAVE-123)" }],
    );
    if (!settings.accounts?.length) {
      duck.view("tasks", [], { error: "Configure extensions.jira.accounts no ~/.duck.lua" });
      return;
    }
    accounts = await loadAccounts();
    await poll();
    setInterval(() => void poll(), Math.max(60, settings.poll_seconds ?? 120) * 1000);
  },
  async viewOpen(m) { if (m.view) render(); },
  async command(m) {
    if (m.command === "goto") { duck.prompt("goto", "Issue do Jira (CHAVE-123)"); return; }
    duck.showView(m.command);
  },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    if (m.id === "goto") {
      const key = m.value.trim().toUpperCase();
      if (accounts[0] && /^[A-Z][A-Z0-9]+-\d+$/.test(key)) await showIssue(accounts[0], key); else duck.flash("Formato: CHAVE-123");
      return;
    }
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value.trim()) await fn(m.value.trim());
  },
});
