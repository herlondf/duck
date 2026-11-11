//! Linux: avisos do desktop com botoes (org.freedesktop.Notifications) e a
//! tecla global do quick terminal (portal GlobalShortcuts no Wayland, XGrabKey
//! no X11). D-Bus em Rust puro (zbus, API bloqueante, sem libdbus).
//!
//! Os botoes voltam como URLs duck:// (as mesmas dos cartoes do canto): a
//! janela registra `on_action` e trata no loop do winit; sem ela (server),
//! vai pelo `duck url`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MsgType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

const NOTE: &str = "org.freedesktop.Notifications";
const NOTE_PATH: &str = "/org/freedesktop/Notifications";
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const GS: &str = "org.freedesktop.portal.GlobalShortcuts";

// ------------------------------------------------------------------ avisos

struct Notes {
    conn: Connection,
    actions: bool,
    markup: bool,
    /// id do aviso -> (dono no D-Bus, urls dos botoes).
    sent: Mutex<HashMap<u32, (String, Vec<String>)>>,
}

static NOTES: OnceLock<Option<Notes>> = OnceLock::new();
type Handler = Box<dyn Fn(String) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

/// Quem trata o clique num botao (a janela: manda para o loop do winit).
pub fn on_action(f: Handler) { let _ = HANDLER.set(f); }

/// Abre a conexao e le as capacidades fora do caminho quente (o server avisa
/// com o lock na mao).
pub fn warm() { std::thread::spawn(|| { notes(); }); }

fn notes() -> Option<&'static Notes> {
    NOTES.get_or_init(|| {
        let conn = Connection::session().ok()?;
        let caps: Vec<String> = conn.call_method(Some(NOTE), NOTE_PATH, Some(NOTE), "GetCapabilities", &()).ok()?.body().deserialize().ok()?;
        // Assina os sinais antes do primeiro aviso: nenhum clique se perde.
        let mut actions = caps.iter().any(|c| c == "actions");
        if actions {
            let it = MatchRule::builder().msg_type(MsgType::Signal).interface(NOTE).and_then(|b| b.path(NOTE_PATH)).map(|b| b.build())
                .and_then(|r| MessageIterator::for_match_rule(r, &conn, Some(64)));
            match it { Ok(it) => { std::thread::spawn(move || listen(it)); } Err(_) => actions = false }
        }
        Some(Notes { conn, actions, markup: caps.iter().any(|c| c == "body-markup"), sent: Mutex::new(HashMap::new()) })
    }).as_ref()
}

fn listen(it: MessageIterator) {
    for msg in it {
        let Ok(msg) = msg else { continue };
        let Some(n) = NOTES.get().and_then(Option::as_ref) else { continue };
        let h = msg.header();
        let from = h.sender().map(|s| s.to_string()).unwrap_or_default();
        match h.member().map(|m| m.as_str()) {
            Some("ActionInvoked") => {
                let Ok((id, key)) = msg.body().deserialize::<(u32, String)>() else { continue };
                // So o servidor que mostrou o aviso, e so um botao que ele tinha.
                let url = {
                    let sent = n.sent.lock().unwrap();
                    let Some((_, urls)) = sent.get(&id).filter(|(o, _)| *o == from) else { continue };
                    if key == "default" { urls.iter().find(|u| u.starts_with("duck://focus/")).cloned() } else { urls.iter().find(|u| **u == key).cloned() }
                };
                if let Some(u) = url { act(u); }
            }
            Some("NotificationClosed") => if let Ok((id, _)) = msg.body().deserialize::<(u32, u32)>() { n.sent.lock().unwrap().remove(&id); },
            _ => {}
        }
    }
}

fn act(url: String) {
    match HANDLER.get() {
        Some(h) => h(url),
        None => { std::thread::spawn(move || crate::cli::url(&[url])); }
    }
}

/// Aviso do desktop com botoes (rotulo, url duck://). Clique no corpo = o
/// botao de focar. Servidor sem botoes: aviso simples. false: sem servidor.
pub fn notify(title: &str, body: &str, actions: &[(String, String)]) -> bool {
    let Some(n) = notes() else { return false };
    let esc = |s: String| if n.markup { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") } else { s };
    let title = crate::server::clean(title, 200);
    let body = esc(body.lines().map(|l| crate::server::clean(l, 300)).collect::<Vec<_>>().join("\n"));
    let mut acts: Vec<String> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    if n.actions {
        if actions.iter().any(|(_, u)| u.starts_with("duck://focus/")) { acts.extend(["default".into(), "Abrir".into()]); }
        for (l, u) in actions { acts.extend([u.clone(), l.clone()]); urls.push(u.clone()); }
    }
    // Pedido de aprovacao fica na tela ate alguem responder.
    let urgent = actions.iter().any(|(_, u)| u.starts_with("duck://approve/") || u.starts_with("duck://approval/"));
    let conn = n.conn.clone();
    std::thread::spawn(move || {
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from("duck"));
        hints.insert("urgency", Value::from(if urgent { 2u8 } else { 1u8 }));
        let a: Vec<&str> = acts.iter().map(String::as_str).collect();
        let Ok(r) = conn.call_method(Some(NOTE), NOTE_PATH, Some(NOTE), "Notify", &("Duck", 0u32, "duck", title.as_str(), body.as_str(), a, hints, -1i32)) else { return };
        let Ok(id) = r.body().deserialize::<u32>() else { return };
        if urls.is_empty() { return; }
        let owner = r.header().sender().map(|s| s.to_string()).unwrap_or_default();
        if let Some(n) = NOTES.get().and_then(Option::as_ref) { n.sent.lock().unwrap().insert(id, (owner, urls)); }
    });
    true
}

/// Fecha os avisos deste processo com algum botao que casa (ja respondido na janela).
pub fn close_where(f: impl Fn(&str) -> bool) {
    let Some(n) = NOTES.get().and_then(Option::as_ref) else { return };
    let ids: Vec<u32> = n.sent.lock().unwrap().iter().filter(|(_, (_, u))| u.iter().any(|u| f(u))).map(|(id, _)| *id).collect();
    if ids.is_empty() { return; }
    let conn = n.conn.clone();
    std::thread::spawn(move || for id in ids { let _ = conn.call_method(Some(NOTE), NOTE_PATH, Some(NOTE), "CloseNotification", &id); });
}

/// `duck doctor`: quem mostra os avisos e se tem botoes.
pub fn notify_probe() -> Result<String, String> {
    let conn = Connection::session().map_err(|e| format!("sem D-Bus de sessao ({e})"))?;
    let call = |m: &str| conn.call_method(Some(NOTE), NOTE_PATH, Some(NOTE), m, &()).map_err(|e| format!("nenhum servidor de avisos ({e})"));
    let (name, _, ver, _): (String, String, String, String) = call("GetServerInformation")?.body().deserialize().map_err(|e| e.to_string())?;
    let caps: Vec<String> = call("GetCapabilities")?.body().deserialize().map_err(|e| e.to_string())?;
    Ok(format!("{name} {ver}, {}", if caps.iter().any(|c| c == "actions") { "com botoes (Aprovar/Negar/Abrir)" } else { "sem botoes (aviso simples)" }))
}

// ------------------------------------------------------------------ tecla global

/// "CTRL+SHIFT+'" separado.
#[derive(Debug, PartialEq, Default)]
struct Spec { ctrl: bool, alt: bool, shift: bool, logo: bool, key: String }

fn parse(spec: &str) -> Spec {
    let mut s = Spec::default();
    for p in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match p.to_uppercase().as_str() {
            "CTRL" | "CONTROL" => s.ctrl = true,
            "ALT" => s.alt = true,
            "SHIFT" => s.shift = true,
            "WIN" | "SUPER" | "LOGO" | "META" => s.logo = true,
            _ => s.key = p.to_string(),
        }
    }
    s
}

/// Nome da tecla no xkb (o que o portal espera no preferred_trigger).
fn xkb_name(key: &str) -> Option<String> {
    let k = key.to_uppercase();
    if k.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| (1..=35).contains(&n)) { return Some(k); }
    let named = match k.as_str() {
        "SPACE" => "space", "ENTER" | "RETURN" => "Return", "TAB" => "Tab", "ESC" | "ESCAPE" => "Escape", "BACKSPACE" => "BackSpace",
        "'" => "apostrophe", "`" => "grave", "-" => "minus", "=" => "equal", "," => "comma", "." => "period", "/" => "slash",
        ";" => "semicolon", "\\" => "backslash", "[" => "bracketleft", "]" => "bracketright",
        _ => {
            let mut c = key.chars();
            return match (c.next(), c.next()) { (Some(ch), None) if ch.is_ascii_alphanumeric() => Some(ch.to_ascii_lowercase().to_string()), _ => None };
        }
    };
    Some(named.into())
}

/// Keysym X11 da tecla (Latin-1 = o proprio codigo).
fn keysym(key: &str) -> Option<u32> {
    let k = key.to_uppercase();
    if let Some(n) = k.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=35).contains(n)) { return Some(0xffbe + n - 1); }
    match k.as_str() {
        "SPACE" => Some(0x20), "ENTER" | "RETURN" => Some(0xff0d), "TAB" => Some(0xff09), "ESC" | "ESCAPE" => Some(0xff1b), "BACKSPACE" => Some(0xff08),
        _ => { let mut c = key.chars(); match (c.next(), c.next()) { (Some(ch), None) if (' '..='~').contains(&ch) => Some(ch.to_ascii_lowercase() as u32), _ => None } }
    }
}

fn trigger(s: &Spec) -> Option<String> {
    let mut t: Vec<String> = [(s.ctrl, "CTRL"), (s.alt, "ALT"), (s.shift, "SHIFT"), (s.logo, "LOGO")].iter().filter(|m| m.0).map(|m| m.1.to_string()).collect();
    t.push(xkb_name(&s.key)?);
    Some(t.join("+"))
}

static TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// Token de ativacao (xdg-activation) do ultimo acionamento: a janela nova pega o foco com ele.
pub fn set_token(t: Option<String>) { if let Some(t) = t.filter(|t| !t.is_empty()) { *TOKEN.lock().unwrap() = Some(t); } }
pub fn take_token() -> Option<String> { TOKEN.lock().unwrap().take() }

fn status_path() -> std::path::PathBuf { crate::proto::data_dir().join("hotkey.txt") }

fn status(msg: &str) {
    crate::proto::log(&format!("quick terminal: {msg}"));
    let _ = std::fs::write(status_path(), msg);
}

/// O que o quick terminal anotou da ultima vez (para o `duck doctor`).
pub fn hotkey_status() -> Option<String> { std::fs::read_to_string(status_path()).ok().filter(|s| !s.trim().is_empty()) }

pub const HOTKEY_HINT: &str = "ligue `duck quick` a um atalho nas configuracoes de teclado do sistema";

/// Tecla global: portal no Wayland, XGrabKey no X11. `on` recebe o token de
/// ativacao quando o portal manda um.
pub fn hotkey(spec: &str, wayland: bool, on: impl Fn(Option<String>) + Send + 'static) {
    let spec = spec.to_string();
    std::thread::spawn(move || {
        let s = parse(&spec);
        let r = if wayland { portal(&spec, &s, &on) } else if std::env::var_os("DISPLAY").is_some() { x11(&spec, &s, &on) } else { Err("sem Wayland nem X11".into()) };
        match r {
            Ok(()) => status("atalho global parou: a conexao caiu"),
            Err(e) => status(&format!("atalho global indisponivel: {e}; {HOTKEY_HINT}")),
        }
    });
}

/// `duck doctor`: da para ter a tecla global nesta sessao?
pub fn hotkey_probe() -> Result<String, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
        let conn = Connection::session().map_err(|e| format!("sem D-Bus de sessao ({e})"))?;
        return portal_version(&conn).map(|v| format!("portal GlobalShortcuts v{v} (Wayland)"));
    }
    if std::env::var_os("DISPLAY").is_none() { return Err("sem Wayland nem X11".into()); }
    x11rb::connect(None).map(|_| "XGrabKey (X11)".to_string()).map_err(|e| format!("X11 nao conecta ({e})"))
}

fn portal_version(conn: &Connection) -> Result<u32, String> {
    let r = conn.call_method(Some(PORTAL), PORTAL_PATH, Some("org.freedesktop.DBus.Properties"), "Get", &(GS, "version")).map_err(|e| {
        if e.to_string().contains("ServiceUnknown") { "xdg-desktop-portal nao esta rodando".to_string() } else { "portal sem GlobalShortcuts".to_string() }
    })?;
    let v: OwnedValue = r.body().deserialize().map_err(|_| "portal sem GlobalShortcuts".to_string())?;
    match &*v { Value::U32(n) => Ok(*n), _ => Err("portal sem GlobalShortcuts".into()) }
}

/// Caminho do Request que o portal vai criar para este token (assina antes de chamar).
fn request_path(conn: &Connection, token: &str) -> String {
    let me = conn.unique_name().map(|n| n.as_str().trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    format!("{PORTAL_PATH}/request/{me}/{token}")
}

fn subscribe(conn: &Connection, iface: &str, member: &str, path: &str) -> Result<MessageIterator, String> {
    let rule = MatchRule::builder().msg_type(MsgType::Signal).interface(iface).and_then(|b| b.member(member)).and_then(|b| b.path(path))
        .map_err(|e| e.to_string())?.build();
    MessageIterator::for_match_rule(rule, conn, Some(16)).map_err(|e| e.to_string())
}

/// Espera a resposta do Request (o portal pode abrir um dialogo e esperar o usuario).
fn response(conn: &Connection, it: MessageIterator, reply: zbus::Result<zbus::message::Message>, expected: &str) -> Result<HashMap<String, OwnedValue>, String> {
    let reply = reply.map_err(|e| format!("portal: {e}"))?;
    let handle: OwnedObjectPath = reply.body().deserialize().map_err(|e| e.to_string())?;
    // Portal antigo devolve outro caminho: assina o certo.
    let mut it = if handle.as_str() == expected { it } else { subscribe(conn, "org.freedesktop.portal.Request", "Response", handle.as_str())? };
    let msg = it.next().ok_or("portal nao respondeu")?.map_err(|e| e.to_string())?;
    let (code, res): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|e| e.to_string())?;
    match code { 0 => Ok(res), 1 => Err("o pedido do atalho foi recusado".into()), _ => Err("o portal recusou o atalho".into()) }
}

fn create_session(conn: &Connection, tok: &str) -> Result<ObjectPath<'static>, String> {
    let path = request_path(conn, tok);
    let it = subscribe(conn, "org.freedesktop.portal.Request", "Response", &path)?;
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(tok));
    opts.insert("session_handle_token", Value::from(tok));
    let r = conn.call_method(Some(PORTAL), PORTAL_PATH, Some(GS), "CreateSession", &(opts,));
    let res = response(conn, it, r, &path)?;
    let session = match res.get("session_handle").map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        Some(Value::ObjectPath(p)) => p.to_string(),
        _ => return Err("portal nao abriu a sessao".into()),
    };
    ObjectPath::try_from(session).map_err(|e| e.to_string())
}

fn portal(spec: &str, s: &Spec, on: &dyn Fn(Option<String>)) -> Result<(), String> {
    let conn = Connection::session().map_err(|e| format!("sem D-Bus de sessao ({e})"))?;
    portal_version(&conn)?;
    let tok = format!("duck{}", std::process::id());
    let session = create_session(&conn, &tok)?;
    // Activated antes do Bind: o primeiro acionamento ja chega.
    let activated = subscribe(&conn, GS, "Activated", PORTAL_PATH)?;
    let tok2 = format!("{tok}b");
    let path2 = request_path(&conn, &tok2);
    let it = subscribe(&conn, "org.freedesktop.portal.Request", "Response", &path2)?;
    let mut sc: HashMap<&str, Value> = HashMap::new();
    sc.insert("description", Value::from("Abrir/esconder o quick terminal do Duck"));
    if let Some(t) = trigger(s) { sc.insert("preferred_trigger", Value::from(t)); }
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(tok2.as_str()));
    let r = conn.call_method(Some(PORTAL), PORTAL_PATH, Some(GS), "BindShortcuts", &(&session, vec![("quick", sc)], "", opts));
    let res = response(&conn, it, r, &path2)?;
    if res.get("shortcuts").is_some_and(|v| matches!(&**v, Value::Array(a) if a.is_empty())) { return Err("o desktop nao ligou o atalho".into()); }
    status(&format!("ok: portal GlobalShortcuts (Wayland), pedido {spec}; a tecla final fica nas configuracoes do sistema"));
    for msg in activated {
        let Ok(msg) = msg else { continue };
        let Ok((sess, id, _, o)) = msg.body().deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>() else { continue };
        if id != "quick" || sess.as_str() != session.as_str() { continue; }
        on(o.get("activation_token").and_then(|v| match &**v { Value::Str(t) => Some(t.to_string()), _ => None }));
    }
    Ok(())
}

fn x11(spec: &str, s: &Spec, on: &dyn Fn(Option<String>)) -> Result<(), String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
    use x11rb::protocol::Event;
    let sym = keysym(&s.key).ok_or_else(|| format!("tecla '{}' desconhecida em {spec}", s.key))?;
    let (c, screen) = x11rb::connect(None).map_err(|e| format!("X11 nao conecta ({e})"))?;
    let (root, min, max) = { let st = c.setup(); (st.roots[screen].root, st.min_keycode, st.max_keycode) };
    let map = c.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let per = (map.keysyms_per_keycode as usize).max(1);
    let code = map.keysyms.chunks(per).position(|k| k.contains(&sym)).map(|i| min + i as u8).ok_or_else(|| format!("{spec}: tecla fora do layout atual"))?;
    let mut mods = 0u16;
    for (on, m) in [(s.ctrl, ModMask::CONTROL), (s.alt, ModMask::M1), (s.shift, ModMask::SHIFT), (s.logo, ModMask::M4)] { if on { mods |= u16::from(m); } }
    // Com Caps Lock e Num Lock ligados a tecla tambem vale.
    for extra in [0, u16::from(ModMask::LOCK), u16::from(ModMask::M2), u16::from(ModMask::LOCK) | u16::from(ModMask::M2)] {
        c.grab_key(false, root, ModMask::from(mods | extra), code, GrabMode::ASYNC, GrabMode::ASYNC).map_err(|e| e.to_string())?
            .check().map_err(|_| format!("tecla {spec} ja esta em uso por outro programa"))?;
    }
    c.flush().map_err(|e| e.to_string())?;
    status(&format!("ok: XGrabKey (X11), tecla {spec}"));
    // Autorepeat do X: soltar+apertar com o mesmo horario. So o primeiro aperto conta.
    let mut released = None;
    loop {
        match c.wait_for_event() {
            Ok(Event::KeyPress(e)) if e.detail == code => if released != Some(e.time) { on(None) },
            Ok(Event::KeyRelease(e)) if e.detail == code => released = Some(e.time),
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_spec() {
        let s = parse("CTRL+'");
        assert_eq!(s, Spec { ctrl: true, key: "'".into(), ..Default::default() });
        assert_eq!(trigger(&s).as_deref(), Some("CTRL+apostrophe"));
        assert_eq!(keysym("'"), Some(0x27));
        let s = parse("alt+shift+F12");
        assert_eq!(trigger(&s).as_deref(), Some("ALT+SHIFT+F12"));
        assert_eq!(keysym(&s.key), Some(0xffc9));
        assert_eq!(trigger(&parse("SUPER+`")).as_deref(), Some("LOGO+grave"));
        assert_eq!(trigger(&parse("CTRL+SPACE")).as_deref(), Some("CTRL+space"));
        assert_eq!(keysym("A"), Some(0x61));
        assert_eq!(trigger(&parse("CTRL+?Z")), None);
    }

    // Ao vivo (precisa da sessao grafica): cargo test -- --ignored live_
    #[test]
    #[ignore]
    fn live_notify() {
        let wait = |f: &dyn Fn(usize) -> bool| (0..50).any(|_| { std::thread::sleep(std::time::Duration::from_millis(100)); f(NOTES.get().and_then(Option::as_ref).map_or(0, |n| n.sent.lock().unwrap().len())) });
        assert!(notify("Duck: teste", "aviso com <botao> & Abrir", &[("Abrir".into(), "duck://focus/0".into())]));
        assert!(wait(&|n| n == 1), "o servidor nao devolveu o id");
        close_where(|u| u == "duck://focus/0");
        // NotificationClosed chega pelo ouvinte e tira o id.
        assert!(wait(&|n| n == 0), "NotificationClosed nao chegou");
    }

    #[test]
    #[ignore]
    fn live_portal_session() {
        // So a sessao (o BindShortcuts abre dialogo e grava o atalho no desktop).
        let conn = Connection::session().unwrap();
        portal_version(&conn).unwrap();
        let s = create_session(&conn, "ducktest").unwrap();
        assert!(s.as_str().starts_with("/org/freedesktop/portal/desktop/session/"), "{s}");
    }

    #[test]
    #[ignore]
    fn live_x11_grab() {
        let spec = "CTRL+ALT+SHIFT+F11";
        std::thread::spawn(move || x11(spec, &parse(spec), &|_| {}));
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(hotkey_status().is_some_and(|s| s.starts_with("ok")), "{:?}", hotkey_status());
    }
}
