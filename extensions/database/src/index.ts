// Extensao Banco de dados do Duck: consultas salvas em Postgres e Redis,
// resultado em tabela no detail, somente leitura por padrao.
//
// Config (~/.duck.lua):
//   extensions = { database = { allow_write = false, accounts = {
//     { name = 'local', kind = 'postgres', host = 'localhost', port = 5432, database = 'app', user = 'postgres' },
//     { name = 'cache', kind = 'redis', host = 'localhost' },
//   } } }
// Senha: echo <senha> | duck secret set database/<name>
// Consultas: ~/.duck/queries/<name>.sql, blocos "-- name: Titulo".

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { Client } from "pg";
import { createClient } from "redis";
import { duck, run, Item, Section } from "./duck";
import { parseQueries, formatQuery, table, cell, redisCommands, REDIS_READ, Query } from "./lib";

type Conn = { name: string; kind: "postgres" | "redis"; host: string; port: number; database?: string; user?: string };
type Settings = { accounts?: Record<string, unknown>[]; allow_write?: boolean };
type Redis = ReturnType<typeof createClient>;
type Live = { pg?: Client; redis?: Redis; timer?: NodeJS.Timeout };

const TIMEOUT = 30_000;
const IDLE = 120_000;
const QDIR = path.join(os.homedir(), ".duck", "queries");

let settings: Settings = {};
let conns: Conn[] = [];
const live = new Map<string, Promise<Live>>();
const locks = new Map<string, Promise<unknown>>();
const adhoc = new Map<string, string>();   // chave -> texto da "Nova consulta…" para salvar depois
let seq = 0;

const writable = () => settings.allow_write === true || (settings.allow_write as unknown) === "true";

function loadConns(): Conn[] {
  return (settings.accounts ?? []).filter((a) => a && a.name).map((a) => {
    const kind = String(a.kind ?? "postgres").toLowerCase().trim() === "redis" ? "redis" : "postgres";
    const port = Number(a.port) || (kind === "redis" ? 6379 : 5432);
    return {
      name: String(a.name), kind, host: String(a.host || "localhost"), port,
      database: a.database ? String(a.database) : undefined, user: a.user ? String(a.user) : undefined,
    };
  });
}

// ---------------------------------------------------------------- arquivo de consultas

const qfile = (c: Conn) => path.join(QDIR, `${c.name.replace(/[^\w.-]/g, "_")}.sql`);

function queries(c: Conn): Query[] {
  try { return parseQueries(fs.readFileSync(qfile(c), "utf8")); } catch { return []; }
}

function ensureFile(c: Conn): string {
  const f = qfile(c);
  if (!fs.existsSync(f)) {
    fs.mkdirSync(QDIR, { recursive: true });
    const sample = c.kind === "redis"
      ? "-- Consultas do Redis: cada bloco comeca com \"-- name: Titulo\"; um comando por linha.\n\n-- name: Tamanho\nDBSIZE\n"
      : "-- Consultas do Postgres: cada bloco comeca com \"-- name: Titulo\".\n\n-- name: Versão\nselect version();\n";
    fs.writeFileSync(f, sample);
  }
  return f;
}

// ---------------------------------------------------------------- conexoes

function errMsg(e: unknown): string {
  const x = e as { message?: string; code?: string; errors?: { message?: string }[] };
  return x?.message || x?.errors?.map((m) => m.message).filter(Boolean).join("; ") || x?.code || String(e);
}

function close(name: string): void {
  const p = live.get(name);
  live.delete(name);
  p?.then((l) => {
    clearTimeout(l.timer);
    l.pg?.end().catch(() => {});
    l.redis?.disconnect().catch(() => {});
  }, () => {});
}

/** Conecta sob demanda; fecha depois de 2 min sem uso. */
async function connect(c: Conn): Promise<Live> {
  let p = live.get(c.name);
  if (!p) {
    p = (async () => {
      const password = (await duck.secret(`database/${c.name}`)) ?? undefined;
      if (c.kind === "postgres") {
        const pg = new Client({
          host: c.host, port: c.port, database: c.database, user: c.user, password,
          connectionTimeoutMillis: 10_000, statement_timeout: TIMEOUT, query_timeout: TIMEOUT + 2_000,
          application_name: "duck",
        });
        pg.on("error", (e) => { duck.log("pg", c.name, e); close(c.name); });
        await pg.connect();
        return { pg };
      }
      const redis = createClient({ socket: { host: c.host, port: c.port, connectTimeout: 10_000, reconnectStrategy: false }, password });
      redis.on("error", (e) => duck.log("redis", c.name, errMsg(e)));
      await redis.connect();
      return { redis };
    })();
    live.set(c.name, p);
    p.catch(() => { if (live.get(c.name) === p) live.delete(c.name); });
  }
  const l = await p;
  clearTimeout(l.timer);
  l.timer = setTimeout(() => close(c.name), IDLE);
  l.timer.unref();
  return l;
}

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  let t: NodeJS.Timeout;
  return Promise.race([p, new Promise<T>((_, rej) => { t = setTimeout(() => rej(new Error(`tempo esgotado (${ms / 1000} s)`)), ms); })])
    .finally(() => clearTimeout(t));
}

/** Uma consulta por vez em cada conexao (BEGIN/ROLLBACK nao podem se misturar). */
function serial<T>(name: string, fn: () => Promise<T>): Promise<T> {
  const prev = locks.get(name) ?? Promise.resolve();
  const next = prev.then(fn, fn);
  locks.set(name, next.catch(() => {}));
  return next;
}

// ---------------------------------------------------------------- execucao

async function runPg(c: Conn, sql: string): Promise<string> {
  const { pg } = await connect(c);
  const t0 = Date.now();
  let res: any;
  try {
    if (writable()) {
      res = await pg!.query({ text: sql, rowMode: "array" });
    } else {
      await pg!.query("BEGIN TRANSACTION READ ONLY");
      // Protocolo estendido: um comando so, entao "COMMIT; DELETE ..." nao escapa da transacao.
      try { res = await pg!.query({ text: sql, rowMode: "array", queryMode: "extended" } as any); }
      finally { await pg!.query("ROLLBACK").catch(() => {}); }
    }
  } catch (e) {
    if (/timeout|terminated|ECONN/i.test(errMsg(e))) close(c.name);
    throw e;
  }
  const ms = Date.now() - t0;
  const list = Array.isArray(res) ? res : [res];
  const r = [...list].reverse().find((x) => x.fields?.length) ?? list[list.length - 1];
  if (r?.fields?.length) return table(r.fields.map((f: { name: string }) => f.name), r.rows, ms);
  return `${r?.command ?? "OK"}${r?.rowCount != null ? ` ${r.rowCount}` : ""}\n\n${r?.rowCount ?? 0} ${r?.rowCount === 1 ? "linha" : "linhas"} em ${ms} ms`;
}

function redisOut(args: string[], v: unknown, ms: number): string {
  const cmd = args[0].toUpperCase();
  if (v === null || v === undefined) return `(nil)\n\n0 linhas em ${ms} ms`;
  if (!Array.isArray(v)) return `${cell(v)}\n\n1 linha em ${ms} ms`;
  const pairs = (a: unknown[]) => { const out: unknown[][] = []; for (let i = 0; i < a.length; i += 2) out.push([a[i], a[i + 1]]); return out; };
  if (cmd === "HGETALL") return table(["campo", "valor"], pairs(v), ms);
  if (cmd === "ZRANGE" && args.some((a) => a.toUpperCase() === "WITHSCORES")) return table(["membro", "score"], pairs(v), ms);
  if (cmd === "SCAN" && Array.isArray(v[1])) return `cursor: ${v[0]}${v[0] === "0" ? " (fim)" : ""}\n\n` + table(["chave"], (v[1] as unknown[]).map((k) => [k]), ms);
  if (cmd === "MGET") return table(["chave", "valor"], v.map((x, i) => [args[i + 1], x === null ? "(nil)" : x]), ms);
  return table(["#", "valor"], v.map((x, i) => [i + 1, x]), ms);
}

async function runRedis(c: Conn, text: string): Promise<string> {
  const cmds = redisCommands(text);
  if (!cmds.length) throw new Error("nenhum comando");
  const warn: string[] = [];
  for (const a of cmds) {
    const n = a[0].toUpperCase();
    if (!writable() && !REDIS_READ.has(n)) throw new Error(`comando não permitido no modo somente leitura: ${n} (ligue allow_write para liberar)`);
    if (n === "KEYS") warn.push("Aviso: KEYS percorre todas as chaves e trava o Redis em bases grandes; prefira SCAN 0 MATCH <padrão> COUNT 100.");
  }
  const { redis } = await connect(c);
  const out: string[] = [...warn, ...(warn.length ? [""] : [])];
  for (const a of cmds) {
    const t0 = Date.now();
    let v: unknown;
    try { v = await withTimeout(redis!.sendCommand(a), TIMEOUT); }
    catch (e) { if (/tempo esgotado|closed|ECONN/i.test(errMsg(e))) close(c.name); throw e; }
    if (cmds.length > 1) out.push(`> ${a.join(" ")}`);
    out.push(redisOut(a, v, Date.now() - t0), "");
  }
  return out.join("\n").trimEnd();
}

function describe(c: Conn): string {
  return `${c.kind} ${c.host}:${c.port}${c.database ? "/" + c.database : ""}${c.user ? ` como ${c.user}` : ""} · ${writable() ? "escrita liberada" : "somente leitura"}`;
}

async function execute(c: Conn, title: string, text: string, actions: { id: string; title: string; item?: string }[], item: string): Promise<void> {
  duck.flash(`Executando ${title}…`);
  let body: string;
  try {
    body = await serial(c.name, () => (c.kind === "redis" ? runRedis(c, text) : runPg(c, text)));
  } catch (e) {
    body = `Erro: ${errMsg(e)}`;
    duck.log("consulta", c.name, errMsg(e));
  }
  const preview = text.replace(/\s+/g, " ").trim();
  duck.detail({ title: `${c.name} · ${title}`, subtitle: `${describe(c)} · ${preview.length > 100 ? preview.slice(0, 99) + "…" : preview}`, body, item, actions });
}

// ---------------------------------------------------------------- visao

function render(): void {
  const secs: Section[] = conns.map((c) => {
    const items: Item[] = queries(c).map((q, i) => ({
      id: `q|${c.name}|${i}`, title: q.name, icon: c.kind === "redis" ? "tag" : "list",
      subtitle: q.text.replace(/\s+/g, " ").slice(0, 80),
      actions: [{ id: "run", title: "Executar", primary: true }, { id: "edit", title: "Editar arquivo" }],
    }));
    items.push({ id: `n|${c.name}`, title: "Nova consulta…", icon: "star", color: "muted",
      actions: [{ id: "new", title: "Nova consulta", primary: true }, { id: "edit", title: "Editar arquivo" }] });
    return { title: `${c.name} · ${describe(c)}`, items };
  });
  if (!conns.length) secs.push({ title: "Sem conexões", items: [{ id: "none", title: "Adicione uma conexão em Extensões → Banco de dados → Conexões", subtitle: "nome, tipo (postgres/redis), servidor, porta, banco, usuário", icon: "warning" }] });
  duck.view("db", secs);
}

// ---------------------------------------------------------------- acoes

const pending = new Map<string, (v: string) => Promise<void>>();

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { render(); return; }
  const [kind, name, ref] = item.split("|");
  const c = conns.find((x) => x.name === name);
  if (!c) return;
  if (action === "edit") { duck.openUrl(ensureFile(c)); return; }
  if (action === "run" && kind === "q") {
    const q = queries(c)[Number(ref)];
    if (!q) { duck.flash("Consulta não encontrada (o arquivo mudou?)"); render(); return; }
    await execute(c, q.name, q.text, [{ id: "run", title: "Executar de novo" }, { id: "edit", title: "Editar arquivo" }], item);
    return;
  }
  if (action === "new") {
    pending.set(`new|${c.name}`, async (text) => {
      const key = String(++seq);
      adhoc.set(key, text);
      const it = `s|${c.name}|${key}`;
      await execute(c, "Nova consulta", text, [{ id: "save", title: "Salvar consulta", item: it }, { id: "again", title: "Executar de novo", item: it }], it);
    });
    duck.prompt(`new|${c.name}`, c.kind === "redis" ? `Comando Redis (${c.name})` : `SQL (${c.name})`);
    return;
  }
  const text = adhoc.get(ref);
  if (kind !== "s" || !text) return;
  if (action === "again") {
    await execute(c, "Nova consulta", text, [{ id: "save", title: "Salvar consulta", item }, { id: "again", title: "Executar de novo", item }], item);
  } else if (action === "save") {
    pending.set(`save|${item}`, async (title) => {
      ensureFile(c);
      fs.appendFileSync(qfile(c), formatQuery(title, text));
      duck.flash(`Consulta "${title}" salva em ${qfile(c)}`);
      render();
    });
    duck.prompt(`save|${item}`, "Nome da consulta");
  }
}

run({
  init(m) {
    settings = (m.settings ?? {}) as Settings;
    conns = loadConns();
    duck.register([{ id: "db", title: "Banco", icon: "list" }], [{ id: "db", title: "Banco: consultas salvas" }]);
    render();
  },
  viewOpen() { render(); },
  command() { duck.showView("db"); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value?.trim()) await fn(m.value.trim());
  },
});
