// Partes puras da extensao (sem rede): arquivo de consultas, tabela, comandos Redis.

export type Query = { name: string; text: string };

/** Blocos que comecam com "-- name: Titulo"; o que vem antes do 1o bloco e ignorado. */
export function parseQueries(src: string): Query[] {
  const out: Query[] = [];
  let cur: Query | null = null;
  for (const line of src.replace(/\r/g, "").split("\n")) {
    const m = /^\s*--\s*name:\s*(.+?)\s*$/i.exec(line);
    if (m) { cur = { name: m[1], text: "" }; out.push(cur); continue; }
    if (cur) cur.text += line + "\n";
  }
  for (const q of out) q.text = q.text.trim();
  return out.filter((q) => q.text);
}

export function formatQuery(name: string, text: string): string {
  return `\n-- name: ${name.replace(/[\r\n]+/g, " ").trim()}\n${text.trim()}\n`;
}

const MAX_ROWS = 200;
const MAX_WIDTH = 40;

export function cell(v: unknown): string {
  if (v === null || v === undefined) return "NULL";
  if (v instanceof Date) return isNaN(v.getTime()) ? String(v) : v.toISOString();
  if (Buffer.isBuffer(v)) return "\\x" + v.toString("hex");
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

/** Tabela monoespacada: colunas alinhadas, largura limitada, no maximo 200 linhas. */
export function table(cols: string[], rows: unknown[][], ms: number): string {
  const shown = rows.slice(0, MAX_ROWS).map((r) => r.map((v) => cell(v).replace(/\s*[\r\n]+\s*/g, " ⏎ ").replace(/\t/g, " ")));
  const w = cols.map((c, i) => Math.min(MAX_WIDTH, Math.max(c.length, ...shown.map((r) => (r[i] ?? "").length))));
  const fit = (s: string, n: number) => (s.length > n ? s.slice(0, n - 1) + "…" : s.padEnd(n));
  const line = (r: string[]) => r.map((s, i) => fit(s ?? "", w[i])).join(" │ ").trimEnd();
  const out = cols.length ? [line(cols), w.map((n) => "─".repeat(n)).join("─┼─"), ...shown.map(line)] : [];
  if (rows.length > MAX_ROWS) out.push(`… mostrando ${MAX_ROWS} de ${rows.length}`);
  out.push("", `${rows.length} ${rows.length === 1 ? "linha" : "linhas"} em ${ms} ms`);
  return out.join("\n");
}

/** Divide uma linha de comando Redis respeitando aspas simples/duplas. */
export function tokenize(line: string): string[] {
  const out: string[] = [];
  const re = /"((?:\\.|[^"\\])*)"|'([^']*)'|(\S+)/g;
  for (let m; (m = re.exec(line));) out.push(m[1] !== undefined ? m[1].replace(/\\(.)/g, "$1") : m[2] ?? m[3]);
  return out;
}

export const REDIS_READ = new Set([
  "GET", "MGET", "HGETALL", "HGET", "LRANGE", "SMEMBERS", "ZRANGE", "TTL", "TYPE", "EXISTS",
  "DBSIZE", "INFO", "SCAN", "KEYS", "STRLEN", "LLEN", "SCARD", "ZCARD",
]);

/** Uma linha por comando; linhas vazias e comentarios (-- ou #) ficam de fora. */
export function redisCommands(text: string): string[][] {
  return text.replace(/\r/g, "").split("\n").map((l) => l.trim()).filter((l) => l && !l.startsWith("--") && !l.startsWith("#")).map(tokenize);
}
