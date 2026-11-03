// Extensao Home Assistant do Duck: luzes, interruptores, cortinas, clima,
// fechaduras e avisos (binary_sensor e notificacoes persistentes) pela API REST.
//
// Config (~/.duck.lua):
//   extensions = { homeassistant = {
//     accounts = { { name = 'casa', url = 'http://homeassistant.local:8123' } },
//     favorites = { 'light.sala', 'switch.cafeteira' },
//     alerts = { 'binary_sensor.porta_entrada' },
//     poll_seconds = 30,
//   } }
// Token: echo <token> | duck secret set homeassistant/<name>

import { duck, run, Item, Section, Action } from "./duck";

type AccountCfg = { name: string; url: string; secret?: string };
type Settings = { accounts?: AccountCfg[]; favorites?: string[]; alerts?: string[]; poll_seconds?: number };
type Account = AccountCfg & { token: string };
type State = { entity_id: string; state: string; last_changed: string; attributes: Record<string, any> };

const DEFAULT: AccountCfg = { name: "casa", url: "http://homeassistant.local:8123" };
const DOMAINS: [string, string][] = [
  ["light", "Luzes"], ["switch", "Interruptores"], ["fan", "Ventiladores"], ["cover", "Cortinas e portões"],
  ["climate", "Clima"], ["lock", "Fechaduras"], ["alarm_control_panel", "Alarme"], ["media_player", "Mídia"],
  ["input_boolean", "Chaves virtuais"], ["sensor", "Sensores"],
];
const SENSOR_CLASSES = ["temperature", "humidity", "power", "battery"];
const TOGGLE = ["light", "switch", "fan", "input_boolean"];
const ICON: Record<string, string> = {
  light: "\u{F0335}", switch: "\u{F0521}", fan: "\u{F0210}", cover: "\u{F111B}", climate: "\u{F0510}",
  lock: "\u{F033E}", alarm_control_panel: "\u{F0498}", media_player: "\u{F075A}", input_boolean: "\u{F0521}",
  sensor: "\u{F050F}", binary_sensor: "\u{F0026}", persistent_notification: "bell",
};
const PT: Record<string, string> = {
  on: "ligado", off: "desligado", open: "aberto", closed: "fechado", opening: "abrindo", closing: "fechando",
  locked: "trancado", unlocked: "destrancado", locking: "trancando", unlocking: "destrancando", jammed: "travado",
  unavailable: "indisponível", unknown: "desconhecido", playing: "tocando", paused: "pausado", idle: "ocioso",
  standby: "em espera", heat: "aquecendo", cool: "resfriando", auto: "automático", dry: "secando", fan_only: "ventilando",
  armed_away: "armado (fora)", armed_home: "armado (em casa)", armed_night: "armado (noite)", disarmed: "desarmado",
  triggered: "disparado", pending: "aguardando", arming: "armando",
};
const ON = new Set(["on", "open", "opening", "playing", "unlocked", "heat", "cool", "auto", "dry", "fan_only", "heat_cool", "triggered"]);

let settings: Settings = {};
let accounts: Account[] = [];
const states = new Map<string, State[]>();
const errors = new Map<string, string>();
const prev = new Map<string, string>(); // "conta|entity_id" -> estado anterior (alertas)
const seenNotif = new Set<string>();    // "conta|notification_id"
const pending = new Map<string, () => Promise<void>>();
let first = true;
let viewOpen = false;
let timer: NodeJS.Timeout | undefined;
let noNotifApi = new Set<string>();

// ---------------------------------------------------------------- HTTP

function base(a: AccountCfg) { return a.url.replace(/\/$/, ""); }

async function call(a: Account, path: string, body?: unknown): Promise<any> {
  let r: Response;
  try {
    r = await fetch(base(a) + path, {
      method: body === undefined ? "GET" : "POST",
      headers: { Authorization: `Bearer ${a.token}`, "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(10000),
    });
  } catch { throw new Error(`sem acesso a ${base(a)} (rede ou Home Assistant fora do ar)`); }
  if (r.status === 401) throw new Error("token recusado (401): gere outro em Perfil → Segurança");
  const text = await r.text();
  if (!r.ok) { const e: any = new Error(`${r.status} ${text.slice(0, 160)}`); e.status = r.status; throw e; }
  return text ? JSON.parse(text) : null;
}

// ---------------------------------------------------------------- texto

function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "agora";
  if (s < 3600) return `há ${Math.floor(s / 60)}min`;
  if (s < 86400) return `há ${Math.floor(s / 3600)}h`;
  return `há ${Math.floor(s / 86400)}d`;
}
const domain = (id: string) => id.split(".")[0];
const name = (s: State) => s.attributes.friendly_name ?? s.entity_id;

function stateText(s: State): string {
  const a = s.attributes;
  if (domain(s.entity_id) === "climate") {
    const cur = a.current_temperature != null ? `${a.current_temperature}°` : "";
    const tgt = a.temperature != null ? ` → ${a.temperature}°` : "";
    return [PT[s.state] ?? s.state, cur + tgt].filter(Boolean).join(" · ");
  }
  if (domain(s.entity_id) === "light" && s.state === "on" && a.brightness != null) return `ligado ${Math.round(a.brightness / 2.55)}%`;
  if (domain(s.entity_id) === "media_player" && a.media_title) return `${PT[s.state] ?? s.state} · ${a.media_title}`;
  const v = PT[s.state] ?? s.state;
  return a.unit_of_measurement ? `${v} ${a.unit_of_measurement}` : v;
}

// ---------------------------------------------------------------- dados

async function loadAccounts(): Promise<void> {
  accounts = [];
  const cfgs = settings.accounts?.length ? settings.accounts : [DEFAULT];
  for (const c of cfgs) {
    const n = c.name || DEFAULT.name;
    const cfg = { ...c, name: n, url: c.url || DEFAULT.url };
    const sec = c.secret ?? `homeassistant/${n}`;
    const token = await duck.secret(sec);
    if (!token) { errors.set(n, `sem token: echo <token> | duck secret set ${sec}`); continue; }
    accounts.push({ ...cfg, token: token.trim() });
  }
}

async function poll(): Promise<void> {
  for (const a of accounts) {
    try {
      const list: State[] = await call(a, "/api/states");
      states.set(a.name, list);
      errors.delete(a.name);
      checkAlerts(a, list);
      await checkNotifications(a, list);
    } catch (e) { errors.set(a.name, String((e as Error).message)); duck.log("poll", a.name, (e as Error).message); }
  }
  first = false;
  render();
}

function checkAlerts(a: Account, list: State[]): void {
  const alerts = new Set(settings.alerts ?? []);
  for (const s of list) {
    if (!alerts.has(s.entity_id)) continue;
    const k = `${a.name}|${s.entity_id}`;
    const before = prev.get(k);
    prev.set(k, s.state);
    if (!first && before !== undefined && before !== "on" && s.state === "on") {
      duck.notify({ title: `⌂ ${name(s)}`, body: `${stateText(s)} · ${a.name}`, item: `ha|${a.name}|${s.entity_id}`, color: "blocked",
        actions: [{ id: "detail", title: "Detalhe" }] });
    }
  }
}

async function checkNotifications(a: Account, list: State[]): Promise<void> {
  // Versoes antigas: entidades persistent_notification.*; algumas expoem GET /api/persistent_notification.
  const found: { id: string; title: string; message: string }[] = list
    .filter((s) => domain(s.entity_id) === "persistent_notification")
    .map((s) => ({ id: s.entity_id, title: s.attributes.title ?? "Notificação", message: s.attributes.message ?? "" }));
  if (!noNotifApi.has(a.name)) {
    try {
      const r = await call(a, "/api/persistent_notification");
      for (const n of Array.isArray(r) ? r : Object.values(r ?? {}) as any[]) {
        found.push({ id: n.notification_id, title: n.title ?? "Notificação", message: n.message ?? "" });
      }
    } catch (e) { if ((e as any).status === 404 || (e as any).status === 405) noNotifApi.add(a.name); }
  }
  for (const n of found) {
    const k = `${a.name}|${n.id}`;
    if (seenNotif.has(k)) continue;
    seenNotif.add(k);
    if (!first) duck.notify({ title: `Home Assistant · ${n.title}`, body: n.message.slice(0, 300), color: "accent",
      actions: [{ id: "web", title: "Abrir no navegador" }], item: `ha|${a.name}|` });
  }
}

function actions(s: State): Action[] {
  const d = domain(s.entity_id);
  const out: Action[] = [];
  if (TOGGLE.includes(d)) out.push({ id: "toggle", title: "Alternar", primary: true });
  else if (d === "cover") out.push({ id: "open_cover", title: "Abrir" }, { id: "close_cover", title: "Fechar" });
  else if (d === "lock") out.push(s.state === "locked" ? { id: "unlock", title: "Destrancar" } : { id: "lock", title: "Trancar" });
  else if (d === "alarm_control_panel") out.push(s.state === "disarmed" ? { id: "alarm_arm_away", title: "Armar" } : { id: "alarm_disarm", title: "Desarmar" });
  out.push({ id: "detail", title: "Detalhe", primary: !TOGGLE.includes(d) });
  return out;
}

function item(a: Account, s: State): Item {
  const on = ON.has(s.state);
  const off = s.state === "unavailable" || s.state === "unknown";
  return {
    id: `ha|${a.name}|${s.entity_id}`, title: name(s), icon: ICON[domain(s.entity_id)],
    subtitle: `${stateText(s)} · ${ago(s.last_changed)}`,
    color: off ? "muted" : on ? (domain(s.entity_id) === "light" ? "accent" : "done") : "fg",
    actions: actions(s),
  };
}

function shown(s: State): boolean {
  const d = domain(s.entity_id);
  if (d === "sensor") return SENSOR_CLASSES.includes(s.attributes.device_class) && s.state !== "unavailable";
  return DOMAINS.some(([x]) => x === d);
}

function render(): void {
  const secs: Section[] = [];
  const multi = accounts.length > 1;
  const fav = settings.favorites ?? [];
  let lit = 0;
  for (const a of accounts) {
    const list = states.get(a.name) ?? [];
    const by = new Map(list.map((s) => [s.entity_id, s]));
    const p = multi ? `${a.name} · ` : "";
    lit += list.filter((s) => (domain(s.entity_id) === "light" || domain(s.entity_id) === "switch") && s.state === "on").length;
    if (fav.length) {
      secs.push({ title: `${p}Favoritos`, items: fav.map((id) => by.get(id)).filter((s): s is State => !!s).map((s) => item(a, s)) });
    } else {
      for (const [d, title] of DOMAINS) {
        const items = list.filter((s) => domain(s.entity_id) === d && shown(s)).sort((x, y) => name(x).localeCompare(name(y))).slice(0, 40);
        if (items.length) secs.push({ title: `${p}${title} (${items.length})`, items: items.map((s) => item(a, s)) });
      }
    }
    const al = (settings.alerts ?? []).map((id) => by.get(id)).filter((s): s is State => !!s);
    if (al.length) secs.push({ title: `${p}Alertas`, items: al.map((s) => item(a, s)) });
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("casa", secs, { error: err || undefined });
  const ok = accounts.some((a) => states.has(a.name));
  duck.status("casa", ok ? `⌂ ${lit}` : "⌂ !", { color: !ok ? "blocked" : lit ? "accent" : "muted", view: "casa" });
}

function schedule(): void {
  clearTimeout(timer);
  const open = Math.max(5, settings.poll_seconds ?? 30);
  timer = setTimeout(async () => { await poll(); schedule(); }, (viewOpen ? open : Math.max(open, 120)) * 1000);
}

// ---------------------------------------------------------------- acoes

async function service(a: Account, s: State, svc: string): Promise<void> {
  const d = svc === "toggle" ? "homeassistant" : domain(s.entity_id);
  await call(a, `/api/services/${d}/${svc}`, { entity_id: s.entity_id });
  duck.flash(`${name(s)}: ok`);
  setTimeout(() => void poll(), 800);
}

const CONFIRM: Record<string, string> = { lock: "Trancar", unlock: "Destrancar", alarm_arm_away: "Armar", alarm_disarm: "Desarmar" };

async function onAction(action: string, itemId: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [, acc, eid] = itemId.split("|");
  const a = accounts.find((x) => x.name === acc) ?? accounts[0];
  if (action === "web") { duck.openUrl(base(a ?? DEFAULT)); return; }
  if (!a || !eid) return;
  const s = states.get(a.name)?.find((x) => x.entity_id === eid);
  if (!s) { duck.flash(`${eid} não encontrado`); return; }
  if (action === "detail") {
    const attrs = Object.entries(s.attributes).map(([k, v]) => `${k}: ${typeof v === "object" ? JSON.stringify(v) : v}`);
    duck.detail({
      title: name(s), subtitle: `${s.entity_id} · ${stateText(s)} · mudou ${ago(s.last_changed)}`, item: itemId,
      body: attrs.join("\n") || "(sem atributos)",
      actions: [...actions(s).filter((x) => x.id !== "detail"), { id: "web", title: "Abrir no navegador" }],
    });
    return;
  }
  try {
    if (CONFIRM[action]) {
      const id = `c|${itemId}|${action}`;
      pending.set(id, () => service(a, s, action));
      duck.choose(id, `${CONFIRM[action]} ${name(s)}?`, [{ id: "yes", title: `Sim, ${CONFIRM[action].toLowerCase()}` }, { id: "no", title: "Cancelar" }]);
      return;
    }
    if (["toggle", "open_cover", "close_cover"].includes(action)) await service(a, s, action);
  } catch (e) { duck.flash(`Home Assistant: ${(e as Error).message}`); }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register([{ id: "casa", title: "Casa", icon: ICON.light }], [{ id: "casa", title: "Casa: abrir" }, { id: "web", title: "Casa: abrir Home Assistant no navegador" }]);
    duck.view("casa", [], { loading: true });
    await loadAccounts();
    await poll();
    schedule();
  },
  viewOpen(m) { if (m.view === "casa") { viewOpen = true; void poll(); schedule(); } },
  viewClose(m) { if (m.view === "casa") { viewOpen = false; schedule(); } },
  async command(m) {
    if (m.command === "web") duck.openUrl(base(accounts[0] ?? settings.accounts?.[0] ?? DEFAULT));
    else duck.showView("casa");
  },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value === "yes") { try { await fn(); } catch (e) { duck.flash(`Home Assistant: ${(e as Error).message}`); } }
  },
});
