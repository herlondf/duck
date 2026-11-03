// Extensão Grafana do Duck: consultas LogQL no Loki (via proxy do datasource
// do Grafana), com contagem na janela, últimas linhas em ordem cronológica e
// aviso quando aparece ocorrência nova.
//
// Config (~/.duck.lua):
//   extensions = { grafana = {
//     accounts = { { name = 'prod', url = 'https://grafana.empresa.com', datasource_uid = '<uid do Loki>' } },
//     queries = { 'Erros da API={service_name="api"} |= "ERROR"' },
//     window_hours = 24, poll_seconds = 300,
//   } }
// Credencial (Basic): echo usuario:senha | duck secret set grafana/<name>

import { duck, run, Section } from "./duck";

type AccountCfg = { name: string; url?: string; datasource_uid?: string };
type Settings = { accounts?: AccountCfg[]; queries?: string[] | string; window_hours?: number; poll_seconds?: number };
type Account = AccountCfg & { auth: string };
type Query = { name: string; expr: string };
type Result = { count: number; lastNs: number };
type Line = { ns: number; text: string; group: string };

const DEFAULT_QUERY = 'Erros={service_name="api"} |= "ERROR"';
let settings: Settings = {};
let accounts: Account[] = [];
let queries: Query[] = [];
const results = new Map<string, Result>(); // "conta|idx"
const errors = new Map<string, string>();
let first = true;

function hours() { return Math.max(1, settings.window_hours ?? 24); }

// ---------------------------------------------------------------- consultas

/** O tipo "list" do Duck separa por vírgula, e LogQL tem vírgula dentro de {...}:
 *  junta de volta os pedaços enquanto chaves/aspas estiverem abertas. */
export function parseQueries(raw: string[] | string | undefined): Query[] {
  const parts = Array.isArray(raw) ? raw : raw ? [raw] : [];
  const joined: string[] = [];
  const open = (s: string) => (s.split("{").length - s.split("}").length) !== 0 || (s.replace(/\\"/g, "").split('"').length - 1) % 2 === 1;
  for (const p of parts) {
    if (joined.length && open(joined[joined.length - 1])) joined[joined.length - 1] += "," + p;
    else joined.push(p);
  }
  const out = joined.map((s) => s.trim()).filter(Boolean).map((s, i) => {
    const eq = s.indexOf("=");
    const name = eq > 0 ? s.slice(0, eq).trim() : "";
    // "Nome=LogQL": nome não tem chave nem aspas; sem nome, a linha inteira é a consulta.
    return name && !/[{}"~|]/.test(name) ? { name, expr: s.slice(eq + 1).trim() } : { name: `Consulta ${i + 1}`, expr: s };
  });
  return out.length ? out : parseQueries([DEFAULT_QUERY]);
}

// ---------------------------------------------------------------- HTTP

function base(a: Account) {
  if (!a.url) throw new Error(`conta ${a.name} sem url`);
  return a.url.replace(/\/$/, "");
}
function uid(a: Account) {
  if (!a.datasource_uid) throw new Error(`conta ${a.name} sem datasource_uid`);
  return a.datasource_uid;
}

async function loki(a: Account, path: string, params: Record<string, string>): Promise<any> {
  const url = `${base(a)}/api/datasources/proxy/uid/${uid(a)}/loki/api/v1/${path}?${new URLSearchParams(params)}`;
  let r: Response;
  try { r = await fetch(url, { headers: { Authorization: a.auth, Accept: "application/json" } }); }
  catch { throw new Error(`sem acesso a ${base(a)} (rede ou Grafana fora do ar)`); }
  const text = await r.text();
  if (!r.ok) {
    let msg = text.slice(0, 160);
    try { msg = JSON.parse(text).message ?? msg; } catch { /* texto */ }
    throw new Error(r.status === 401 ? "401 usuário ou senha inválidos" : `${r.status} ${msg}`);
  }
  return JSON.parse(text).data;
}

const ns = (ms: number) => `${BigInt(Math.floor(ms)) * 1000000n}`;

/** Linhas da consulta em ordem cronológica. O envelope JSON do vector/S3 traz .log e .container_id. */
async function lines(a: Account, expr: string, fromMs: number, toMs: number, limit: number): Promise<Line[]> {
  const d = await loki(a, "query_range", { query: expr, start: ns(fromMs), end: ns(toMs), limit: String(limit), direction: "backward" });
  const out: Line[] = [];
  for (const s of d?.result ?? []) {
    const labels = s.stream ?? {};
    for (const [t, raw] of s.values ?? []) {
      let text = String(raw), group = labels.container_id ?? labels.instance ?? labels.service_name ?? "";
      try { const j = JSON.parse(text); if (typeof j.log === "string") { text = j.log; group = j.container_id ?? group; } } catch { /* texto puro */ }
      out.push({ ns: Number(BigInt(t) / 1000000n), text: text.replace(/\s+$/, ""), group });
    }
  }
  return out.sort((x, y) => x.ns - y.ns);
}

async function measure(a: Account, q: Query): Promise<Result> {
  const now = Date.now();
  const [c, last] = await Promise.all([
    loki(a, "query", { query: `sum(count_over_time(${q.expr} [${hours()}h]))`, time: ns(now) }),
    lines(a, q.expr, now - hours() * 3600e3, now, 1),
  ]);
  return { count: Number(c?.result?.[0]?.value?.[1] ?? 0), lastNs: last[0]?.ns ?? 0 };
}

// ---------------------------------------------------------------- texto

function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 3600) return `há ${Math.max(1, Math.floor(s / 60))}min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)}h`;
  return `há ${Math.floor(s / 86400)}d`;
}
function when(ms: number) {
  return new Date(ms).toLocaleString("pt-BR", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

/** Agrupa por instância (container) quando há mais de uma; dentro do grupo, cronológico. */
function format(ls: Line[]): string {
  if (!ls.length) return "(nenhuma linha na janela)";
  const groups = new Map<string, Line[]>();
  for (const l of ls) { if (!groups.has(l.group)) groups.set(l.group, []); groups.get(l.group)!.push(l); }
  const one = groups.size === 1;
  return [...groups.entries()].flatMap(([g, gl]) => [
    ...(one ? [] : ["", `── ${g || "(sem instância)"} · ${gl.length} linha(s) · ${when(gl[0].ns)} ──`]),
    ...gl.map((l) => `${when(l.ns)}  ${l.text}`),
  ]).join("\n").replace(/^\n/, "");
}

// ---------------------------------------------------------------- dados

async function loadAccounts(): Promise<Account[]> {
  const out: Account[] = [];
  for (const c of settings.accounts ?? []) {
    const cred = await duck.secret(`grafana/${c.name}`);
    if (!cred || !cred.includes(":")) {
      errors.set(c.name, `sem senha: echo usuario:senha | duck secret set grafana/${c.name}`);
      continue;
    }
    out.push({ ...c, auth: "Basic " + Buffer.from(cred.trim()).toString("base64") });
  }
  return out;
}

async function poll(): Promise<void> {
  for (const a of accounts) {
    try {
      for (let i = 0; i < queries.length; i++) {
        const q = queries[i], k = `${a.name}|${i}`;
        const r = await measure(a, q);
        const before = results.get(k);
        results.set(k, r);
        // Compara a última ocorrência e não a contagem: na janela móvel a contagem cai sozinha.
        if (!first && before && r.lastNs > before.lastNs && r.count > 0) {
          duck.notify({
            title: `Grafana · ${q.name}: ocorrência nova`, body: `${r.count} na janela de ${hours()}h · última às ${when(r.lastNs)}`,
            item: `q|${a.name}|${i}`, actions: [{ id: "open", title: "Ver linhas" }, { id: "web", title: "Abrir no Grafana" }], color: "blocked",
          });
        }
      }
      errors.delete(a.name);
    } catch (e) { errors.set(a.name, (e as Error).message); duck.log("poll", a.name, (e as Error).message); }
  }
  first = false;
  render();
}

function render(): void {
  const multi = (settings.accounts?.length ?? 0) > 1;
  const secs: Section[] = [];
  let total = 0;
  for (const a of accounts) {
    secs.push({
      title: `${multi ? `${a.name} · ` : ""}Últimas ${hours()}h`,
      items: queries.map((q, i) => {
        const r = results.get(`${a.name}|${i}`);
        total += r?.count ?? 0;
        return {
          id: `q|${a.name}|${i}`, title: q.name, icon: "list",
          badge: r ? String(r.count) : "…", color: r?.count ? "blocked" : "muted",
          subtitle: !r ? (errors.has(a.name) ? "sem dados" : "carregando") : r.lastNs ? `última ${when(r.lastNs)} · ${ago(r.lastNs)}` : "nenhuma ocorrência na janela",
          actions: [{ id: "open", title: "Ver linhas", primary: true }, { id: "incident", title: "Último incidente" }, { id: "web", title: "Abrir no Grafana" }, { id: "refresh", title: "Atualizar" }],
        };
      }),
    });
  }
  duck.view("queries", secs, { error: [...errors.entries()].map(([n, e]) => n === "config" ? e : `${n}: ${e}`).join(" | ") });
  duck.status("queries", accounts.length ? ` ${total}` : "", { color: total ? "blocked" : "muted", view: "queries" });
}

// ---------------------------------------------------------------- detalhe e ações

function exploreUrl(a: Account, expr: string, from = `now-${hours()}h`, to = "now"): string {
  const panes = { duck: { datasource: uid(a), queries: [{ refId: "A", expr, queryType: "range", datasource: { type: "loki", uid: uid(a) } }], range: { from, to } } };
  return `${base(a)}/explore?schemaVersion=1&orgId=1&panes=${encodeURIComponent(JSON.stringify(panes))}`;
}

const acts = [{ id: "incident", title: "Último incidente" }, { id: "web", title: "Abrir no Grafana" }, { id: "open", title: "Atualizar" }];

async function showLines(a: Account, i: number): Promise<void> {
  const q = queries[i], now = Date.now();
  const ls = await lines(a, q.expr, now - hours() * 3600e3, now, 200);
  duck.detail({
    title: `${q.name} · últimas ${ls.length} linha(s)`, item: `q|${a.name}|${i}`,
    subtitle: `${a.name} · janela de ${hours()}h · horário local · ${q.expr}`,
    body: format(ls), actions: acts,
  });
}

/** Como o passo 2 da skill: todas as linhas da instância do último hit, de -2s a +20s. */
async function showIncident(a: Account, i: number): Promise<void> {
  const q = queries[i], now = Date.now();
  const [hit] = (await lines(a, q.expr, now - hours() * 3600e3, now, 1));
  if (!hit) { duck.flash(`${q.name}: nenhuma ocorrência em ${hours()}h`); return; }
  const selector = q.expr.slice(0, q.expr.indexOf("}") + 1);
  if (!hit.group || !selector) { await showLines(a, i); return; }
  const expr = `${selector} |= ${JSON.stringify(hit.group)}`;
  const ls = await lines(a, expr, hit.ns - 2000, hit.ns + 20000, 5000);
  duck.detail({
    title: `${q.name} · incidente de ${when(hit.ns)}`, item: `q|${a.name}|${i}`,
    subtitle: `${a.name} · instância ${hit.group} · ${ls.length} linha(s), ${ls.filter((l) => /\[0x[0-9a-f]+\]/.test(l.text)).length} frame(s)`,
    body: ls.map((l) => `${when(l.ns)}  ${l.text}`).join("\n") || "(sem linhas)",
    actions: [{ id: "web", title: "Abrir no Grafana" }],
  });
}

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [, name, idx] = item.split("|");
  const a = accounts.find((x) => x.name === name);
  const i = Number(idx);
  if (!a || !queries[i]) return;
  switch (action) {
    case "open": await showLines(a, i); break;
    case "incident": await showIncident(a, i); break;
    case "web": duck.openUrl(exploreUrl(a, queries[i].expr)); break;
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    queries = parseQueries(settings.queries);
    duck.register([{ id: "queries", title: "Grafana · Consultas", icon: "eye" }], [{ id: "queries", title: "Grafana: consultas" }]);
    if (!settings.accounts?.length) {
      errors.set("config", "Configure extensions.grafana.accounts (Extensões → Grafana → Contas)");
      render();
      return;
    }
    accounts = await loadAccounts();
    await poll();
    setInterval(() => void poll(), Math.max(60, settings.poll_seconds ?? 300) * 1000);
  },
  async viewOpen(m) { if (m.view) render(); },
  async command(m) { duck.showView(m.command); },
  async action(m) {
    try { await onAction(m.action, m.item); }
    catch (e) { duck.flash(`Grafana: ${(e as Error).message}`); }
  },
});
