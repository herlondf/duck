// Extensao Google Play do Duck: faixas/releases e avaliacoes dos apps
// (Google Play Developer API, androidpublisher v3) com conta de servico.
//
// Config (~/.duck.lua):
//   extensions = { googleplay = { accounts = { { name = 'empresa' } }, packages = { 'com.empresa.app' } } }
// Credencial: Get-Content conta.json -Raw | duck secret set googleplay/<nome>

import { duck, run, Item, Section, Action } from "./duck";
import { signJwt, ServiceAccount } from "./jwt";

type Settings = { accounts?: { name: string }[]; packages?: string[] | string; poll_seconds?: number };
type Account = { name: string; sa: ServiceAccount; token?: string; exp?: number };
type Release = { name?: string; versionCodes?: string[]; status?: string; userFraction?: number };
type Track = { track: string; releases?: Release[] };
type UserComment = { text?: string; starRating?: number; appVersionName?: string; appVersionCode?: number; device?: string; androidOsVersion?: number; lastModified?: { seconds: string } };
type Review = { reviewId: string; authorName?: string; comments?: { userComment?: UserComment; developerComment?: { text?: string; lastModified?: { seconds: string } } }[] };

const API = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications";
const SCOPE = "https://www.googleapis.com/auth/androidpublisher";
const CONSOLE = "https://play.google.com/console";

let settings: Settings = {};
let accounts: Account[] = [];
const owner = new Map<string, Account>();      // pacote -> conta que conseguiu ler
const tracks = new Map<string, Track[]>();
const reviews = new Map<string, Review[]>();
const errors = new Map<string, string>();
const relSnap = new Map<string, string>();     // "pkg|faixa|versoes" -> status
const seenReviews = new Set<string>();
let first = true;
let noCreds = "";

function packages(): string[] {
  const p = settings.packages ?? [];
  return (Array.isArray(p) ? p : p.split(",")).map((s) => s.trim()).filter(Boolean);
}

// ---------------------------------------------------------------- HTTP

async function token(a: Account): Promise<string> {
  if (a.token && a.exp && Date.now() < a.exp) return a.token;
  const r = await fetch(a.sa.token_uri ?? "https://oauth2.googleapis.com/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer", assertion: signJwt(a.sa, SCOPE) }),
  });
  const j: any = await r.json().catch(() => ({}));
  if (!r.ok || !j.access_token) throw new Error(`token recusado (${r.status} ${j.error_description ?? j.error ?? ""})`.trim());
  a.token = j.access_token;
  a.exp = Date.now() + (Number(j.expires_in ?? 3600) - 60) * 1000;
  return a.token!;
}

async function call(a: Account, path: string, method = "GET", body?: unknown): Promise<any> {
  const headers: Record<string, string> = { Authorization: `Bearer ${await token(a)}`, Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  let r: Response;
  try { r = await fetch(API + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) }); }
  catch { throw new Error("sem acesso a androidpublisher.googleapis.com (rede)"); }
  const text = await r.text();
  if (!r.ok) {
    let msg = text.slice(0, 160);
    try { msg = JSON.parse(text).error?.message ?? msg; } catch { /* nao e json */ }
    const e = new Error(`${r.status} ${msg}`) as Error & { status: number };
    e.status = r.status;
    throw e;
  }
  return text ? JSON.parse(text) : null;
}

/** Faixas via edit temporario: cria, le e apaga sempre (o edit e so leitura). */
async function readTracks(a: Account, pkg: string): Promise<Track[]> {
  const edit = await call(a, `/${pkg}/edits`, "POST", {});
  try { return (await call(a, `/${pkg}/edits/${edit.id}/tracks`)).tracks ?? []; }
  finally { await call(a, `/${pkg}/edits/${edit.id}`, "DELETE").catch((e) => duck.log("delete edit", pkg, e)); }
}

/** Tenta as contas em ordem ate uma ter acesso ao app; lembra qual foi. */
async function withOwner<T>(pkg: string, fn: (a: Account) => Promise<T>): Promise<T> {
  const known = owner.get(pkg);
  const order = known ? [known, ...accounts.filter((x) => x !== known)] : accounts;
  let last: unknown;
  for (const a of order) {
    try { const r = await fn(a); owner.set(pkg, a); return r; }
    catch (e) { last = e; const s = (e as { status?: number }).status; if (s !== 403 && s !== 404 && s !== 401) throw e; }
  }
  throw last ?? new Error("nenhuma conta configurada");
}

// ---------------------------------------------------------------- texto

const stars = (n = 0) => "★".repeat(n) + "☆".repeat(Math.max(0, 5 - n));
function ago(sec?: string): string {
  if (!sec) return "";
  const s = Math.max(0, Date.now() / 1000 - Number(sec));
  if (s < 3600) return `há ${Math.max(1, Math.floor(s / 60))}min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)}h`;
  return `há ${Math.floor(s / 86400)}d`;
}
const STATUS: Record<string, string> = { completed: "concluída", inProgress: "em andamento", halted: "interrompida", draft: "rascunho" };
const statusColor = (s?: string) => s === "completed" ? "done" : s === "inProgress" ? "working" : s === "halted" ? "blocked" : "muted";
const TRACK: Record<string, string> = { production: "Produção", beta: "Beta (aberto)", alpha: "Alpha (fechado)", internal: "Teste interno" };
const trackOrder = (t: string) => { const i = ["production", "beta", "alpha", "internal"].indexOf(t); return i < 0 ? 4 : i; };
const versions = (r: Release) => (r.versionCodes ?? []).join(", ") || "sem versão";
const userComment = (r: Review) => r.comments?.find((c) => c.userComment)?.userComment ?? {};
const devReply = (r: Review) => r.comments?.find((c) => c.developerComment)?.developerComment;

function helpText(): string {
  return [
    "Sem credencial da conta de serviço. Para configurar:",
    "",
    "1. No Google Cloud Console (console.cloud.google.com), escolha um projeto,",
    "   ative a \"Google Play Android Developer API\" e crie uma conta de serviço",
    "   em IAM e administrador → Contas de serviço.",
    "2. Na conta de serviço, Chaves → Adicionar chave → JSON; baixe o arquivo.",
    "3. No Play Console → Usuários e permissões → Convidar novos usuários,",
    "   informe o e-mail da conta de serviço (…@….iam.gserviceaccount.com) e dê",
    "   acesso aos apps: \"Ver informações do app\" e, para responder,",
    "   \"Responder a avaliações\".",
    "4. Grave o JSON no Duck (a conta precisa estar em Extensões → Google Play → Contas):",
    "",
    "   Get-Content conta.json -Raw | duck secret set googleplay/<nome>",
    "",
    "5. Configure os apps em \"packages\" (applicationId, ex.: com.empresa.app) e reabra a janela.",
  ].join("\n");
}

function helpSections(): Section[] {
  const act: Action[] = [{ id: "help", title: "Como configurar", primary: true }, { id: "console", title: "Abrir no Play Console" }];
  return [{ title: "Configurar o Google Play", items: [
    { id: "help", title: "Como criar a conta de serviço", subtitle: "Google Cloud → conta de serviço → chave JSON", icon: "warning", color: "working", actions: act },
    { id: "help", title: "Dar acesso no Play Console", subtitle: "Usuários e permissões → convidar o e-mail da conta", icon: "user", actions: act },
    { id: "help", title: "Get-Content conta.json -Raw | duck secret set googleplay/<nome>", subtitle: "grava o JSON da conta de serviço", icon: "list", actions: act },
  ] }];
}

// ---------------------------------------------------------------- dados

async function loadAccounts(): Promise<void> {
  accounts = [];
  const missing: string[] = [];
  for (const c of settings.accounts ?? []) {
    if (!c?.name) continue;
    const raw = await duck.secret(`googleplay/${c.name}`);
    if (!raw) { missing.push(c.name); continue; }
    try {
      const sa = JSON.parse(raw);
      if (!sa.client_email || !sa.private_key) throw new Error("JSON sem client_email/private_key");
      accounts.push({ name: c.name, sa });
    } catch (e) { errors.set(c.name, `credencial inválida em googleplay/${c.name}: ${(e as Error).message}`); }
  }
  noCreds = accounts.length ? "" : missing.length
    ? `sem credencial: ${missing.map((n) => `googleplay/${n}`).join(", ")}`
    : (settings.accounts?.length ? "nenhuma credencial válida" : "nenhuma conta configurada");
}

async function poll(): Promise<void> {
  if (!accounts.length) { render(); return; }
  for (const pkg of packages()) {
    try {
      const t = await withOwner(pkg, (a) => readTracks(a, pkg));
      tracks.set(pkg, t);
      const rv = (await withOwner(pkg, (a) => call(a, `/${pkg}/reviews?maxResults=50`))).reviews ?? [];
      reviews.set(pkg, rv);
      errors.delete(pkg);
      detectChanges(pkg, t, rv);
    } catch (e) { errors.set(pkg, (e as Error).message); duck.log("poll", pkg, e); }
  }
  first = false;
  render();
}

function detectChanges(pkg: string, t: Track[], rv: Review[]): void {
  for (const tr of t) for (const r of tr.releases ?? []) {
    const k = `${pkg}|${tr.track}|${versions(r)}`;
    const before = relSnap.get(k);
    relSnap.set(k, r.status ?? "");
    if (first || before === undefined || before === r.status) continue;
    duck.notify({
      title: `Google Play · ${pkg} ${TRACK[tr.track] ?? tr.track}: ${STATUS[before] ?? before} → ${STATUS[r.status ?? ""] ?? r.status}`,
      body: `${r.name ?? ""} (${versions(r)})`.trim(), item: `t|${pkg}|${tr.track}`, color: statusColor(r.status),
      actions: [{ id: "console", title: "Abrir no Play Console" }],
    });
  }
  for (const r of rv) {
    const k = `${pkg}|${r.reviewId}`;
    if (seenReviews.has(k)) continue;
    seenReviews.add(k);
    const u = userComment(r);
    if (first || (u.starRating ?? 5) > 3) continue;
    duck.notify({
      title: `Google Play · ${pkg}: ${stars(u.starRating)} de ${r.authorName ?? "anônimo"}`,
      body: (u.text ?? "").trim().slice(0, 160), item: `r|${pkg}|${r.reviewId}`, color: "blocked",
      actions: [{ id: "open", title: "Ver" }, { id: "reply", title: "Responder" }],
    });
  }
}

function render(): void {
  if (!accounts.length) {
    duck.view("releases", helpSections(), { error: noCreds });
    duck.view("reviews", helpSections(), { error: noCreds });
    duck.status("gp", "", {});
    return;
  }
  const rel: Section[] = [];
  const rev: Section[] = [];
  let low = 0;
  if (!packages().length) rel.push({ title: "Nenhum app", items: [{ id: "none", title: "Configure \"packages\" com os applicationId dos apps", icon: "warning" }] });
  for (const pkg of packages()) {
    const items: Item[] = [];
    for (const tr of [...(tracks.get(pkg) ?? [])].sort((a, b) => trackOrder(a.track) - trackOrder(b.track))) {
      const rs = tr.releases ?? [];
      if (!rs.length) items.push({ id: `t|${pkg}|${tr.track}`, title: TRACK[tr.track] ?? tr.track, subtitle: "sem release", icon: "tag", color: "muted", actions: [{ id: "console", title: "Abrir no Play Console", primary: true }] });
      for (const r of rs) items.push({
        id: `t|${pkg}|${tr.track}`, title: `${TRACK[tr.track] ?? tr.track} · ${r.name ?? versions(r)}`,
        subtitle: [`versão ${versions(r)}`, r.userFraction != null ? `${Math.round(r.userFraction * 1000) / 10}% dos usuários` : ""].filter(Boolean).join(" · "),
        badge: STATUS[r.status ?? ""] ?? r.status, color: statusColor(r.status), icon: r.status === "halted" ? "warning" : "rocket",
        actions: [{ id: "console", title: "Abrir no Play Console", primary: true }],
      });
    }
    rel.push({ title: pkg, items });
    const rv = reviews.get(pkg) ?? [];
    rev.push({ title: `${pkg} (${rv.length})`, items: rv.map((r) => {
      const u = userComment(r);
      const replied = !!devReply(r);
      if ((u.starRating ?? 5) <= 3 && !replied) low++;
      return {
        id: `r|${pkg}|${r.reviewId}`, title: `${stars(u.starRating)}  ${(u.text ?? "").trim().replace(/\s+/g, " ").slice(0, 90) || "(sem texto)"}`,
        subtitle: [r.authorName, u.appVersionName ? `v${u.appVersionName}` : "", ago(u.lastModified?.seconds), replied ? "respondida" : ""].filter(Boolean).join(" · "),
        icon: "star", color: (u.starRating ?? 5) <= 2 ? "blocked" : (u.starRating ?? 5) === 3 ? "working" : "done", unread: !replied && (u.starRating ?? 5) <= 3,
        actions: [{ id: "open", title: "Ver", primary: true }, { id: "reply", title: "Responder" }, { id: "console", title: "Play Console" }],
      };
    }) });
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("releases", rel, { error: err });
  duck.view("reviews", rev, { error: err });
  duck.status("gp", low ? ` ${low}` : "", { color: "blocked", view: "reviews" });
}

// ---------------------------------------------------------------- detalhe e acoes

function findReview(pkg: string, id: string): Review | undefined { return reviews.get(pkg)?.find((r) => r.reviewId === id); }

function showReview(pkg: string, id: string): void {
  const r = findReview(pkg, id);
  if (!r) { duck.flash("Avaliação não encontrada (atualize a lista)"); return; }
  const u = userComment(r);
  const d = devReply(r);
  duck.detail({
    title: `${stars(u.starRating)} · ${r.authorName ?? "anônimo"}`, item: `r|${pkg}|${id}`,
    subtitle: [pkg, u.appVersionName ? `versão ${u.appVersionName} (${u.appVersionCode})` : "", u.device, u.androidOsVersion ? `Android API ${u.androidOsVersion}` : "", ago(u.lastModified?.seconds)].filter(Boolean).join(" · "),
    body: [(u.text ?? "(sem texto)").trim(), ...(d ? ["", `── Sua resposta · ${ago(d.lastModified?.seconds)} ──`, "", (d.text ?? "").trim()] : [])].join("\n"),
    actions: [{ id: "reply", title: d ? "Responder de novo" : "Responder" }, { id: "console", title: "Abrir no Play Console" }],
  });
}

const pending = new Map<string, (v: string) => Promise<void>>();

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await loadAccounts(); await poll(); return; }
  if (action === "help") { duck.detail({ title: "Google Play · como configurar", body: helpText(), actions: [{ id: "console", title: "Abrir no Play Console" }] }); return; }
  if (action === "console") { duck.openUrl(CONSOLE); return; }
  const [kind, pkg, id] = item.split("|");
  if (kind !== "r" || !pkg || !id) return;
  if (action === "open") { showReview(pkg, id); return; }
  if (action === "reply") {
    pending.set(`reply|${item}`, async (text) => {
      // Confirmacao: a 1a opcao do choose e sempre "Voltar sem escolher".
      pending.set(`confirm|${item}`, async (v) => {
        if (v !== "send") return;
        await withOwner(pkg, (a) => call(a, `/${pkg}/reviews/${id}:reply`, "POST", { replyText: text }));
        duck.flash("Resposta enviada");
        const r = findReview(pkg, id);
        if (r) r.comments = [...(r.comments ?? []).filter((c) => !c.developerComment), { developerComment: { text, lastModified: { seconds: String(Math.floor(Date.now() / 1000)) } } }];
        render();
        showReview(pkg, id);
      });
      duck.choose(`confirm|${item}`, "Enviar esta resposta publicamente?", [{ id: "send", title: "Enviar resposta", hint: text.slice(0, 80) }]);
    });
    duck.prompt(`reply|${item}`, "Resposta à avaliação (pública)", devReply(findReview(pkg, id) ?? { reviewId: id })?.text ?? "");
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register(
      [{ id: "releases", title: "Google Play · Publicações", icon: "rocket" }, { id: "reviews", title: "Google Play · Avaliações", icon: "star" }],
      [{ id: "releases", title: "Google Play: publicações" }, { id: "reviews", title: "Google Play: avaliações" }, { id: "console", title: "Google Play: abrir o Play Console" }],
    );
    await loadAccounts();
    await poll();
    setInterval(() => void poll(), Math.max(60, Number(settings.poll_seconds ?? 900)) * 1000);
  },
  viewOpen() { render(); },
  command(m) { if (m.command === "console") duck.openUrl(CONSOLE); else duck.showView(m.command); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value?.trim()) await fn(m.value.trim());
  },
});
