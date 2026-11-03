// Extensao Agenda do Duck: compromissos por URL secreta iCal (ICS) do Google
// Agenda ("endereco secreto no formato iCal") ou do Outlook/Microsoft 365
// ("calendario publicado"). Sem OAuth. Aceita tambem file:// (testes).
//
// Config (~/.duck.lua):
//   extensions = { calendar = { accounts = { { name = 'trabalho' }, { name = 'pessoal' } },
//     remind_minutes = 5, refresh_minutes = 15, days = 7 } }
// URL: echo <url .ics> | duck secret set calendar/<name>

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { duck, run, Item, Section, Action } from "./duck";

type AccountCfg = { name: string; secret?: string };
type Settings = { accounts?: AccountCfg[]; remind_minutes?: number; refresh_minutes?: number; days?: number };
export type Ev = {
  id: string; uid: string; cal: string; title: string; start: number; end: number; allDay: boolean;
  location: string; description: string; url: string;
};

const DAY = 86400000;
const HORIZON_DAYS = 14;

// ---------------------------------------------------------------- fuso horario

// Nomes do Windows que o Outlook manda em TZID.
const WINDOWS_TZ: Record<string, string> = {
  "E. South America Standard Time": "America/Sao_Paulo",
  "SA Eastern Standard Time": "America/Cayenne",
  "Central Brazilian Standard Time": "America/Cuiaba",
  "SA Western Standard Time": "America/La_Paz",
  "SA Pacific Standard Time": "America/Bogota",
  "Bahia Standard Time": "America/Bahia",
  "Tocantins Standard Time": "America/Araguaina",
  "Argentina Standard Time": "America/Buenos_Aires",
  "Pacific SA Standard Time": "America/Santiago",
  "Eastern Standard Time": "America/New_York",
  "Central Standard Time": "America/Chicago",
  "Mountain Standard Time": "America/Denver",
  "Pacific Standard Time": "America/Los_Angeles",
  "GMT Standard Time": "Europe/London",
  "W. Europe Standard Time": "Europe/Berlin",
  "Romance Standard Time": "Europe/Paris",
  "Central Europe Standard Time": "Europe/Budapest",
  "GTB Standard Time": "Europe/Bucharest",
  "India Standard Time": "Asia/Kolkata",
  "China Standard Time": "Asia/Shanghai",
  "Tokyo Standard Time": "Asia/Tokyo",
  "UTC": "UTC", "Coordinated Universal Time": "UTC", "Greenwich Standard Time": "UTC",
};
const fmtCache = new Map<string, Intl.DateTimeFormat | null>();

function zoneFmt(tzid: string): Intl.DateTimeFormat | null {
  if (fmtCache.has(tzid)) return fmtCache.get(tzid)!;
  const tz = WINDOWS_TZ[tzid] ?? tzid.replace(/^\/+/, "");
  let f: Intl.DateTimeFormat | null = null;
  try {
    f = new Intl.DateTimeFormat("en-US", { timeZone: tz, hourCycle: "h23", year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "numeric", second: "numeric" });
  } catch { duck.log("fuso desconhecido, usando o local:", tzid); }
  fmtCache.set(tzid, f);
  return f;
}

/** Diferenca (ms) entre a hora de parede na zona e UTC no instante t. */
function offset(f: Intl.DateTimeFormat, t: number): number {
  const p: Record<string, number> = {};
  for (const x of f.formatToParts(new Date(t))) if (x.type !== "literal") p[x.type] = +x.value;
  return Date.UTC(p.year, p.month - 1, p.day, p.hour % 24, p.minute, p.second) - Math.floor(t / 1000) * 1000;
}

// ---------------------------------------------------------------- datas ICS

/** Hora de parede + zona: "UTC", nome TZID, ou null (flutuante = local). */
type DT = { y: number; m: number; d: number; h: number; mi: number; s: number; tz: string | null; date: boolean };

function parseDT(v: string, params: Record<string, string>): DT | null {
  const r = /^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2})(\d{2})?(Z)?)?$/.exec(v.trim());
  if (!r) return null;
  const date = !r[4] || params.VALUE === "DATE";
  return { y: +r[1], m: +r[2], d: +r[3], h: +(r[4] ?? 0), mi: +(r[5] ?? 0), s: +(r[6] ?? 0), date,
    tz: date ? null : r[7] ? "UTC" : params.TZID ?? null };
}

function instant(t: DT): number {
  if (t.tz === "UTC") return Date.UTC(t.y, t.m - 1, t.d, t.h, t.mi, t.s);
  const f = t.tz ? zoneFmt(t.tz) : null;
  if (!f) return new Date(t.y, t.m - 1, t.d, t.h, t.mi, t.s).getTime();
  const wall = Date.UTC(t.y, t.m - 1, t.d, t.h, t.mi, t.s);
  const guess = wall - offset(f, wall);
  return wall - offset(f, guess); // segunda passada acerta a virada de horario de verao
}

/** Mesma hora de parede, outro dia de calendario (aritmetica pura em UTC). */
function withDay(t: DT, dayNum: number): DT {
  const x = new Date(dayNum * DAY);
  return { ...t, y: x.getUTCFullYear(), m: x.getUTCMonth() + 1, d: x.getUTCDate() };
}
const dayNum = (t: { y: number; m: number; d: number }) => Math.floor(Date.UTC(t.y, t.m - 1, t.d) / DAY);
const WD = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
const weekday = (n: number) => (n + 4) % 7; // dia 0 (1970-01-01) foi quinta

function parseDuration(v: string): number {
  const r = /^([+-])?P(?:(\d+)W)?(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?)?$/.exec(v.trim());
  if (!r) return 0;
  const ms = (((+(r[2] ?? 0) * 7 + +(r[3] ?? 0)) * 24 + +(r[4] ?? 0)) * 60 + +(r[5] ?? 0)) * 60000 + +(r[6] ?? 0) * 1000;
  return r[1] === "-" ? -ms : ms;
}

// ---------------------------------------------------------------- parser ICS

type Prop = { name: string; params: Record<string, string>; value: string };
type Raw = { props: Prop[] };

function unescape(v: string): string {
  return v.replace(/\\([nN,;\\])/g, (_, c) => (c === "n" || c === "N" ? "\n" : c));
}

function parseLine(line: string): Prop | null {
  // NOME;PARAM=valor;PARAM="com:dois pontos":valor
  let i = 0, inQ = false;
  for (; i < line.length; i++) {
    const c = line[i];
    if (c === '"') inQ = !inQ;
    else if (c === ":" && !inQ) break;
  }
  if (i >= line.length) return null;
  const head = line.slice(0, i).split(/;(?=(?:[^"]*"[^"]*")*[^"]*$)/);
  const params: Record<string, string> = {};
  for (const p of head.slice(1)) {
    const eq = p.indexOf("=");
    if (eq > 0) params[p.slice(0, eq).toUpperCase()] = p.slice(eq + 1).replace(/^"|"$/g, "");
  }
  return { name: head[0].toUpperCase(), params, value: line.slice(i + 1) };
}

function parseICS(text: string): Raw[] {
  const lines = text.replace(/\r\n/g, "\n").replace(/\n[ \t]/g, "").split("\n");
  const out: Raw[] = [];
  let cur: Raw | null = null;
  let depth = 0; // componentes dentro do VEVENT (VALARM) sao ignorados
  for (const l of lines) {
    if (!l) continue;
    const p = parseLine(l);
    if (!p) continue;
    if (p.name === "BEGIN") {
      if (cur) depth++;
      else if (p.value.toUpperCase() === "VEVENT") { cur = { props: [] }; depth = 0; }
      continue;
    }
    if (p.name === "END") {
      if (cur && depth) depth--;
      else if (cur && p.value.toUpperCase() === "VEVENT") { out.push(cur); cur = null; }
      continue;
    }
    if (cur && !depth) cur.props.push(p);
  }
  return out;
}

const get = (r: Raw, n: string) => r.props.find((p) => p.name === n);
const text = (r: Raw, n: string) => unescape(get(r, n)?.value ?? "").trim();

type Rule = { freq: string; interval: number; count?: number; until?: number; byday: { n: number; wd: number }[]; bymonthday: number[] };

function parseRule(v: string): Rule | null {
  const kv: Record<string, string> = {};
  for (const part of v.split(";")) { const [k, x] = part.split("="); if (k && x) kv[k.toUpperCase()] = x; }
  if (!["DAILY", "WEEKLY", "MONTHLY"].includes(kv.FREQ)) return null;
  const until = kv.UNTIL ? parseDT(kv.UNTIL, {}) : null;
  return {
    freq: kv.FREQ, interval: Math.max(1, +(kv.INTERVAL ?? 1)), count: kv.COUNT ? +kv.COUNT : undefined,
    until: until ? instant(until.date ? { ...until, h: 23, mi: 59, s: 59 } : until) : undefined,
    byday: (kv.BYDAY ?? "").split(",").filter(Boolean).map((x) => {
      const r = /^([+-]?\d+)?([A-Z]{2})$/.exec(x.toUpperCase());
      return { n: r?.[1] ? +r[1] : 0, wd: WD.indexOf(r?.[2] ?? "") };
    }).filter((x) => x.wd >= 0),
    bymonthday: (kv.BYMONTHDAY ?? "").split(",").filter(Boolean).map(Number),
  };
}

/** Dias (dayNum) de cada periodo da regra, em ordem, a partir do inicio. */
function* periods(rule: Rule, start: DT): Generator<number[]> {
  const s = dayNum(start);
  if (rule.freq === "DAILY") {
    for (let d = s; ; d += rule.interval) {
      if (!rule.byday.length || rule.byday.some((b) => b.wd === weekday(d))) yield [d];
      else yield [];
    }
  }
  if (rule.freq === "WEEKLY") {
    const days = rule.byday.length ? rule.byday.map((b) => b.wd) : [weekday(s)];
    const monday = s - ((weekday(s) + 6) % 7); // WKST=MO
    for (let w = monday; ; w += 7 * rule.interval) {
      yield days.map((wd) => w + ((wd + 6) % 7)).sort((a, b) => a - b);
    }
  }
  // MONTHLY
  for (let k = 0; ; k += rule.interval) {
    const y = start.y + Math.floor((start.m - 1 + k) / 12), m = (start.m - 1 + k) % 12;
    const first = Math.floor(Date.UTC(y, m, 1) / DAY), len = Math.floor(Date.UTC(y, m + 1, 1) / DAY) - first;
    const out: number[] = [];
    if (rule.byday.length) {
      for (const b of rule.byday) {
        const all: number[] = [];
        for (let d = first; d < first + len; d++) if (weekday(d) === b.wd) all.push(d);
        if (b.n === 0) out.push(...all);
        else { const x = b.n > 0 ? all[b.n - 1] : all[all.length + b.n]; if (x !== undefined) out.push(x); }
      }
    } else {
      for (const md of rule.bymonthday.length ? rule.bymonthday : [start.d]) {
        const d = md > 0 ? md : len + md + 1;
        if (d >= 1 && d <= len) out.push(first + d - 1);
      }
    }
    yield out.sort((a, b) => a - b);
  }
}

/** Expande os eventos do ICS em ocorrencias no intervalo [from, to). */
export function expand(ics: string, cal: string, from: number, to: number): Ev[] {
  const raws = parseICS(ics);
  const overrides = new Map<string, Map<number, Raw>>(); // uid -> RECURRENCE-ID -> evento
  for (const r of raws) {
    const rid = get(r, "RECURRENCE-ID");
    const t = rid && parseDT(rid.value, rid.params);
    if (!t) continue;
    const uid = text(r, "UID");
    if (!overrides.has(uid)) overrides.set(uid, new Map());
    overrides.get(uid)!.set(instant(t), r);
  }
  const out: Ev[] = [];
  const push = (r: Raw, start: number, dur: number, allDay: boolean) => {
    if (text(r, "STATUS").toUpperCase() === "CANCELLED") return;
    const end = start + dur;
    if (end <= from || start >= to) return;
    const uid = text(r, "UID");
    out.push({
      id: `${cal}|${uid}|${start}`, uid, cal, title: text(r, "SUMMARY") || "(sem título)", start, end, allDay,
      location: text(r, "LOCATION"), description: text(r, "DESCRIPTION"), url: text(r, "URL"),
    });
  };
  for (const r of raws) {
    const sp = get(r, "DTSTART");
    const st = sp && parseDT(sp.value, sp.params);
    if (!st) continue;
    const ep = get(r, "DTEND");
    const et = ep && parseDT(ep.value, ep.params);
    const start = instant(st);
    const durP = get(r, "DURATION");
    const dur = et ? instant(et) - start : durP ? parseDuration(durP.value) : st.date ? DAY : 0;
    if (get(r, "RECURRENCE-ID")) { push(r, start, dur, st.date); continue; }
    const rr = get(r, "RRULE");
    const rule = rr && parseRule(rr.value);
    if (!rule) { push(r, start, dur, st.date); continue; }
    const ex = new Set<number>();
    for (const p of r.props.filter((x) => x.name === "EXDATE")) {
      for (const v of p.value.split(",")) { const t = parseDT(v, p.params); if (t) ex.add(instant(t.date ? { ...st, y: t.y, m: t.m, d: t.d } : t)); }
    }
    const ov = overrides.get(text(r, "UID"));
    let n = 0, steps = 0;
    // ponytail: varre do DTSTART ate o fim da janela; serie diaria de decadas = alguns milhares de passos.
    outer: for (const days of periods(rule, st)) {
      for (const d of days) {
        if (d < dayNum(st)) continue;
        const t = instant(withDay(st, d));
        if (t > to || (rule.until !== undefined && t > rule.until)) break outer;
        if (rule.count !== undefined && n >= rule.count) break outer;
        n++;
        if (ex.has(t) || ov?.has(t)) continue;
        push(r, t, dur, st.date);
      }
      if (++steps > 200000) break; // trava de seguranca
    }
  }
  return out.sort((a, b) => a.start - b.start);
}

// ---------------------------------------------------------------- estado

let settings: Settings = {};
const urls = new Map<string, string>(); // nunca logar: e segredo
const events = new Map<string, Ev[]>();
const errors = new Map<string, string>();
const notified = new Set<string>();
let byId = new Map<string, Ev>();

const MEETING = /https?:\/\/(?:[\w-]+\.)*(?:teams\.microsoft\.com|teams\.live\.com|meet\.google\.com|zoom\.us|zoom\.com|webex\.com)\/[^\s<>"'\])]*/i;
function meetingLink(e: Ev): string | null {
  for (const f of [e.location, e.description, e.url]) { const m = MEETING.exec(f); if (m) return m[0]; }
  return null;
}

async function fetchICS(url: string): Promise<string> {
  if (url.startsWith("file://")) return readFile(fileURLToPath(url), "utf8").catch(() => { throw new Error("não consegui ler o arquivo da agenda"); });
  const u = url.replace(/^webcals?:\/\//i, "https://");
  let r: Response;
  try { r = await fetch(u, { signal: AbortSignal.timeout(20000), headers: { Accept: "text/calendar" } }); }
  catch { throw new Error("sem acesso à agenda (rede fora do ar?)"); }
  if (!r.ok) throw new Error(`a agenda respondeu ${r.status}${r.status === 404 ? " (URL secreta trocada?)" : ""}`);
  return r.text();
}

async function refresh(): Promise<void> {
  const today = startOfDay(Date.now());
  for (const [cal, url] of urls) {
    try {
      const ics = await fetchICS(url);
      if (!/BEGIN:VCALENDAR/i.test(ics)) throw new Error("o endereço não devolveu um arquivo iCal");
      events.set(cal, expand(ics, cal, today, today + HORIZON_DAYS * DAY));
      errors.delete(cal);
    } catch (e) { errors.set(cal, (e as Error).message); duck.log("agenda", cal, (e as Error).message); }
  }
  render();
}

// ---------------------------------------------------------------- texto

function startOfDay(t: number) { const d = new Date(t); d.setHours(0, 0, 0, 0); return d.getTime(); }
const hhmm = (t: number) => new Date(t).toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
function until(ms: number): string {
  const m = Math.max(0, Math.round(ms / 60000));
  return m < 60 ? `em ${m}min` : `em ${Math.floor(m / 60)}h${m % 60 ? String(m % 60).padStart(2, "0") : ""}`;
}
function when(e: Ev): string {
  if (e.allDay) return "Dia inteiro";
  return `${hhmm(e.start)}–${hhmm(e.end)}`;
}

function all(): Ev[] { return [...events.values()].flat().sort((a, b) => a.start - b.start); }

function actions(e: Ev): Action[] {
  const a: Action[] = [];
  if (meetingLink(e)) a.push({ id: "join", title: "Entrar", primary: true });
  a.push({ id: "detail", title: "Detalhes", primary: !a.length });
  return a;
}

function item(e: Ev, now: number, withDate: boolean): Item {
  const live = !e.allDay && e.start <= now && e.end > now;
  const date = withDate ? new Date(e.start).toLocaleDateString("pt-BR", { weekday: "short", day: "2-digit", month: "2-digit" }) + " " : "";
  return {
    id: e.id, title: `${date}${when(e)} ${e.title}`, icon: meetingLink(e) ? "\u{F0567}" : "clock",
    subtitle: [e.location.split("\n")[0].slice(0, 80), e.cal].filter(Boolean).join(" · "),
    badge: live ? "agora" : !e.allDay && e.start > now && e.start - now < 3 * 3600000 ? until(e.start - now) : undefined,
    color: e.end <= now ? "muted" : live ? "accent" : undefined,
    actions: actions(e),
  };
}

function render(): void {
  const now = Date.now();
  const t0 = startOfDay(now), t1 = startOfDay(t0 + 36 * 3600000), t2 = startOfDay(t1 + 36 * 3600000);
  const days = Math.max(2, settings.days ?? 7);
  const tEnd = startOfDay(t0 + (days + 0.5) * DAY);
  const list = all();
  byId = new Map(list.map((e) => [e.id, e]));
  const inDay = (e: Ev, a: number, b: number) => e.start < b && e.end > a;
  const secs: Section[] = [
    { title: "Hoje", items: list.filter((e) => inDay(e, t0, t1)).map((e) => item(e, now, false)) },
    { title: "Amanhã", items: list.filter((e) => inDay(e, t1, t2)).map((e) => item(e, now, false)) },
    { title: `Próximos ${days} dias`, items: list.filter((e) => e.start >= t2 && e.start < tEnd).map((e) => item(e, now, true)) },
  ];
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("agenda", secs, { error: err || undefined });

  const next = list.find((e) => !e.allDay && e.end > now && e.start < t1);
  const label = next ? `📅 ${hhmm(next.start)} ${next.title.slice(0, 30)} (${next.start <= now ? "agora" : until(next.start - now)})` : "";
  duck.status("agenda", label, { color: next && next.start - now < 15 * 60000 ? "working" : "accent", view: "agenda" });
}

function remind(): void {
  const now = Date.now();
  const lead = (settings.remind_minutes ?? 5) * 60000;
  for (const e of all()) {
    if (e.allDay || notified.has(e.id) || e.start <= now || e.start - now > lead) continue;
    notified.add(e.id);
    duck.notify({
      title: `📅 ${hhmm(e.start)} ${e.title}`, body: [until(e.start - now), e.location.split("\n")[0], e.cal].filter(Boolean).join(" · "),
      item: e.id, color: "accent", actions: actions(e),
    });
  }
}

function detail(e: Ev): void {
  const link = meetingLink(e);
  const date = new Date(e.start).toLocaleDateString("pt-BR", { weekday: "long", day: "2-digit", month: "long" });
  duck.detail({
    title: e.title, item: e.id,
    subtitle: [`${date} · ${when(e)}`, e.location, e.cal].filter(Boolean).join(" · "),
    body: [e.description || "(sem descrição)", link ? `\nReunião: ${link}` : "", e.url && e.url !== link ? `Link: ${e.url}` : ""].filter(Boolean).join("\n"),
    actions: link ? [{ id: "join", title: "Entrar" }] : [],
  });
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register([{ id: "agenda", title: "Agenda", icon: "clock" }], [{ id: "agenda", title: "Agenda: abrir" }, { id: "join", title: "Agenda: entrar na próxima reunião" }]);
    if (!settings.accounts?.length) {
      duck.view("agenda", [], { error: "Adicione uma agenda em Extensões → Agenda (ou extensions.calendar.accounts no ~/.duck.lua)" });
      return;
    }
    duck.view("agenda", [], { loading: true });
    for (const a of settings.accounts) {
      const sec = a.secret ?? `calendar/${a.name}`;
      const url = (await duck.secret(sec))?.trim();
      if (url) urls.set(a.name, url);
      else errors.set(a.name, `sem URL: echo <url .ics> | duck secret set ${sec}`);
    }
    await refresh();
    remind();
    setInterval(() => void refresh(), Math.max(1, settings.refresh_minutes ?? 15) * 60000);
    setInterval(() => { remind(); render(); }, 30000);
  },
  viewOpen(m) { if (m.view === "agenda") render(); },
  command(m) {
    if (m.command !== "join") { duck.showView("agenda"); return; }
    const now = Date.now();
    const e = all().find((x) => !x.allDay && x.end > now && x.start - now < 3600000 && meetingLink(x));
    if (e) duck.openUrl(meetingLink(e)!); else duck.flash("Nenhuma reunião com link na próxima hora");
  },
  async action(m) {
    if (m.action === "refresh") { await refresh(); return; }
    const e = byId.get(m.item);
    if (!e) { duck.flash("Compromisso não encontrado (agenda atualizada?)"); return; }
    if (m.action === "join") { const l = meetingLink(e); if (l) duck.openUrl(l); else duck.flash("Sem link de reunião"); }
    else detail(e);
  },
});
