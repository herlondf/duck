//! Sessao do celular (docs/MOBILE.md): pareamento pelo convite do QR, pedidos
//! com `id`, eventos assinados, ping e push pelo relay.
//!
//! Cada conexao tem uma thread que le pedidos e manda eventos; cada `call`
//! roda numa thread propria (no maximo 4 por sessao), e o lock do Mux so e
//! pego dentro de `handle`/para copiar o estado. A escrita para o celular e
//! sempre fora do lock e com tempo maximo: celular lento derruba a propria
//! sessao, nao segura o server nem os outros clientes.
//!
//! Estado proprio (convites, sessoes abertas) num lock folha: nunca pega o
//! Mux segurando ele.

use super::*;
use crate::mobile as mb;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::LazyLock;

struct Session { key: String, kill: Arc<AtomicBool> }

#[derive(Default)]
struct State { invites: mb::Invites, sessions: Vec<Session> }

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);
fn state() -> MutexGuard<'static, State> { STATE.lock().unwrap_or_else(|e| e.into_inner()) }

/// Pedidos rodando ao mesmo tempo numa sessao.
const MAX_CALLS: usize = 4;
/// Intervalo minimo entre duas telas de `screen:<pane>` (4 por segundo).
const SCREEN_EVERY: Duration = Duration::from_millis(250);
/// Os outros eventos (agents, approvals, inbox): no maximo 1 por segundo.
const LIST_EVERY: Duration = Duration::from_secs(1);

pub(super) fn online(key: &str) -> bool { state().sessions.iter().any(|s| s.key == key) }

/// Derruba as sessoes abertas de um celular (revogado). Devolve quantas.
pub(super) fn kill_sessions(key: &str) -> usize {
    let st = state();
    let v: Vec<&Session> = st.sessions.iter().filter(|s| s.key == key).collect();
    for s in &v { s.kill.store(true, Ordering::SeqCst); }
    v.len()
}

pub(super) fn cancel_invites() { state().invites = mb::Invites::default(); }

/// Convite novo e o conteudo do QR (`mobile-pair`).
pub(super) fn invite(srv: &Arc<Server>) -> Result<Value, String> {
    let (name, listen, relay, secret) = { let m = srv.lock(); (m.cfg.peer_name.clone(), m.cfg.peer_listen.clone(), m.cfg.peer_relay.clone(), m.cfg.peer_relay_secret.clone()) };
    let mut addrs: Vec<String> = vec![];
    if !listen.is_empty() {
        let port = crate::peer::port_of(&listen);
        if crate::peer::listens_all(&listen) {
            if let Some(ip) = crate::peer::local_ip() { addrs.push(format!("{ip}:{port}")); }
            for i in crate::peer::interfaces() { let a = format!("{}:{port}", i.ip); if !addrs.contains(&a) { addrs.push(a); } }
        } else if let Some(a) = crate::peer::advertised(&listen) { addrs.push(a); }
    }
    if addrs.is_empty() && relay.is_empty() { return Err("este computador nao escuta nem usa relay: ligue no painel Computadores".into()); }
    let code = crate::secure::random_hex(8);
    let exp = mb::now() + mb::INVITE_SECS;
    state().invites.add(&code, exp);
    let p = mb::Pairing { name, key: crate::secure::my_key(), addrs, relay, relay_secret: secret, invite: code, exp };
    Ok(proto::ok(json!({"url": p.url(), "payload": p.to_json(), "minutes": mb::INVITE_SECS / 60, "exp": exp})))
}

fn err(id: &Value, code: &str, msg: &str) -> Value {
    let mut v = json!({"ok": false, "error": {"code": code, "message": msg}});
    if !id.is_null() { v["id"] = id.clone(); }
    v
}

fn ok(id: &Value, data: Value) -> Value {
    let mut v = json!({"ok": true, "data": data});
    if !id.is_null() { v["id"] = id.clone(); }
    v
}

/// Codigo do erro pelo texto do server.
fn code_of(e: &str) -> &'static str {
    if e.contains("nao existe") || e.contains("nao encontrad") || e.contains("nenhum") { "not_found" }
    else if e.contains("nao esta ligad") || e.contains("inacessivel") || e.contains("nao rodou") || e.contains("timeout") || e.contains("Tradutor") { "unavailable" }
    else { "invalid" }
}

/// Primeira mensagem de uma conexao cifrada com `op` (peer_conn): pareia ou
/// abre a sessao. Chave desconhecida sem convite valido: erro e fecha.
pub(super) fn conn(srv: &Arc<Server>, mut c: crate::secure::Chan, first: Value) {
    let key = c.remote().to_string();
    let id = first["id"].clone();
    let mut pending = Some(first.clone());
    if first["op"] == "pair" {
        match pair(srv, &c, &key, &first) {
            Ok(reply) => { if c.send_json(&reply).is_err() { return; } pending = None; }
            Err(e) => {
                proto::log(&format!("celular: pareamento recusado: {e}"));
                std::thread::sleep(Duration::from_millis(500));
                let _ = c.send_json(&err(&id, "forbidden", &e));
                return;
            }
        }
    }
    let Some(dev) = mb::by_key(&key) else {
        proto::log("celular: chave desconhecida");
        std::thread::sleep(Duration::from_millis(500));
        let _ = c.send_json(&err(&id, "forbidden", "celular nao pareado com este Duck: leia o QR de novo"));
        return;
    };
    session(srv, c, dev, pending);
}

fn pair(srv: &Arc<Server>, c: &crate::secure::Chan, key: &str, v: &Value) -> Result<Value, String> {
    if v["kind"] != "mobile" { return Err("pair: kind deve ser mobile".into()); }
    if key == crate::secure::my_key() { return Err("e este mesmo computador".into()); }
    let invite = v["invite"].as_str().unwrap_or("");
    if !state().invites.take(invite, mb::now()) { return Err("convite invalido, ja usado ou vencido: gere outro QR no Duck".into()); }
    let all = mb::list();
    let dev = match all.iter().find(|d| d.key == key) {
        // Mesmo celular de novo (app reinstalado com a mesma chave): mantem as permissoes.
        Some(d) => d.clone(),
        None => {
            let taken: Vec<String> = all.iter().map(|d| d.name.clone()).collect();
            let name = mb::free_name(&taken, v["name"].as_str().unwrap_or("celular"));
            mb::Device { name, key: key.into(), scopes: mb::DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect(), paired_at: mb::now(), ..Default::default() }
        }
    };
    let dev = mb::Device { last_seen: mb::now(), ..dev };
    mb::save(&dev)?;
    let confirm = c.confirm().replace(' ', "-");
    proto::log(&format!("celular: {} pareado (numero {confirm})", dev.name));
    let me = {
        let mut m = srv.lock();
        m.inbox.push(json!({"kind": "mobile", "pane": 0, "title": format!("Celular {} conectado", dev.name),
            "body": format!("Número de confirmação: {confirm}. Se não for o mesmo do celular, revogue em Celulares."), "color": "done"}));
        m.cfg.peer_name.clone()
    };
    srv.notify();
    #[cfg(not(test))]
    crate::gui::toast_card(&format!("Celular {} pareado", dev.name), &format!("Número de confirmação: {confirm}\nConfira se é o mesmo que aparece no celular. Se não for, revogue em Celulares."), &[], Some("done"));
    // Formato do contrato: os campos na raiz da resposta.
    let mut r = json!({"ok": true, "scopes": dev.scopes, "confirm": confirm, "name": dev.name, "duck": me});
    if !v["id"].is_null() { r["id"] = v["id"].clone(); }
    Ok(r)
}

/// Assinatura de evento: o ultimo enviado (hash) e quando.
struct Sub { name: String, pane: Option<Id>, hash: u64, last: Option<Instant>, fresh: bool }

fn hash(v: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.to_string().hash(&mut h);
    h.finish()
}

fn session(srv: &Arc<Server>, mut c: crate::secure::Chan, dev: mb::Device, mut pending: Option<Value>) {
    let kill = Arc::new(AtomicBool::new(false));
    state().sessions.push(Session { key: dev.key.clone(), kill: kill.clone() });
    mb::touch(&dev.name);
    proto::log(&format!("celular: {} conectou", dev.name));
    c.set_write_timeout(Duration::from_secs(20));
    let (tx, rx) = channel::<Value>();
    let busy = Arc::new(AtomicUsize::new(0));
    let mut subs: Vec<Sub> = vec![];
    let mut last_in = Instant::now();
    let mut last_gen = u64::MAX;
    let mut last_scan = Instant::now();
    let mut touched = Instant::now();
    let why = loop {
        if kill.load(Ordering::SeqCst) { let _ = c.send_json(&json!({"ev": "revoked", "data": {}})); break "revogado"; }
        if last_in.elapsed() > Duration::from_secs(mb::IDLE_SECS) { break "5 min sem nada"; }
        let msg = match pending.take() {
            Some(v) => Some(v),
            None => match c.readable(Duration::from_millis(100), Duration::from_secs(30)) {
                Ok(true) => match c.recv_json() { Ok(v) => Some(v), Err(e) => { proto::log(&format!("celular: {}: {e}", dev.name)); break "erro de leitura"; } },
                Ok(false) => None,
                Err(_) => break "fechou",
            },
        };
        if let Some(v) = msg {
            last_in = Instant::now();
            if touched.elapsed() > Duration::from_secs(60) { mb::touch(&dev.name); touched = Instant::now(); }
            if let Some(r) = on_msg(srv, &dev, &v, &tx, &busy, &mut subs) { if c.send_json(&r).is_err() { break "escrita falhou"; } }
        }
        if !drain(&mut c, &rx) { break "escrita falhou"; }
        let gen = *srv.gen.lock().unwrap_or_else(|e| e.into_inner());
        if !subs.is_empty() && (subs.iter().any(|s| s.fresh) || (gen != last_gen && last_scan.elapsed() >= SCREEN_EVERY)) {
            last_gen = gen;
            last_scan = Instant::now();
            let mut failed = false;
            for e in events(srv, &mut subs) { if c.send_json(&e).is_err() { failed = true; break; } }
            if failed { break "escrita falhou"; }
        }
    };
    state().sessions.retain(|s| !Arc::ptr_eq(&s.kill, &kill));
    proto::log(&format!("celular: {} desconectou ({why})", dev.name));
}

fn drain(c: &mut crate::secure::Chan, rx: &Receiver<Value>) -> bool {
    while let Ok(r) = rx.try_recv() { if c.send_json(&r).is_err() { return false; } }
    true
}

/// Uma mensagem do celular. Resposta na hora (Some) ou pela fila (`call`).
fn on_msg(srv: &Arc<Server>, dev: &mb::Device, v: &Value, tx: &Sender<Value>, busy: &Arc<AtomicUsize>, subs: &mut Vec<Sub>) -> Option<Value> {
    let id = &v["id"];
    match v["op"].as_str().unwrap_or("") {
        "ping" => { let mut r = json!({"op": "pong"}); if !id.is_null() { r["id"] = id.clone(); } Some(r) }
        "pair" => Some(ok(id, json!({"scopes": mb::by_key(&dev.key).map(|d| d.scopes).unwrap_or_default()}))),
        "subscribe" => Some(subscribe(srv, dev, id, v, subs)),
        "call" => {
            let cmd = v["cmd"].as_str().unwrap_or("").to_string();
            if busy.load(Ordering::SeqCst) >= MAX_CALLS { return Some(err(id, "unavailable", "pedidos demais ao mesmo tempo")); }
            busy.fetch_add(1, Ordering::SeqCst);
            let (srv, key, args, id, tx, busy) = (srv.clone(), dev.key.clone(), v["args"].clone(), id.clone(), tx.clone(), busy.clone());
            std::thread::spawn(move || {
                let r = std::panic::catch_unwind(AssertUnwindSafe(|| call(&srv, &key, &cmd, &args)))
                    .unwrap_or_else(|_| Err(("invalid", "erro interno (detalhes em server.log)".into())));
                let _ = tx.send(match r { Ok(d) => ok(&id, d), Err((code, msg)) => err(&id, code, &msg) });
                busy.fetch_sub(1, Ordering::SeqCst);
                srv.notify();
            });
            None
        }
        op => Some(err(id, "invalid", &format!("op desconhecida: {op}"))),
    }
}

/// Pane que o celular pode ver: existe e nao e privado.
fn visible(m: &Mux, id: Id) -> bool { m.panes.get(&id).is_some_and(|p| !p.private) }

fn subscribe(srv: &Arc<Server>, dev: &mb::Device, id: &Value, v: &Value, subs: &mut Vec<Sub>) -> Value {
    let Some(d) = mb::by_key(&dev.key) else { return err(id, "forbidden", "celular revogado") };
    if !d.has("view") { return err(id, "forbidden", "sem a permissao view"); }
    let mut new: Vec<Sub> = vec![];
    let mut rejected = vec![];
    for e in v["events"].as_array().into_iter().flatten().filter_map(|e| e.as_str()) {
        let pane = e.strip_prefix("screen:").map(|p| p.parse::<Id>().ok());
        let ok = match pane { Some(Some(p)) => visible(&srv.lock(), p), Some(None) => false, None => ["agents", "approvals", "inbox"].contains(&e) };
        if !ok { rejected.push(e.to_string()); continue; }
        if new.iter().any(|s| s.name == e) { continue; }
        // A mesma assinatura de antes continua de onde estava (sem reenviar).
        let old = subs.iter().position(|s| s.name == e).map(|i| subs.remove(i));
        new.push(old.unwrap_or(Sub { name: e.to_string(), pane: pane.flatten(), hash: 0, last: None, fresh: true }));
    }
    *subs = new;
    ok(id, json!({"events": subs.iter().map(|s| s.name.clone()).collect::<Vec<_>>(), "rejected": rejected}))
}

/// Eventos que mudaram desde o ultimo envio. Uma tela no maximo a cada 250 ms.
fn events(srv: &Arc<Server>, subs: &mut Vec<Sub>) -> Vec<Value> {
    let mut out = vec![];
    let res = srv.lock().secrets.clone();
    let mut gone = vec![];
    for (i, s) in subs.iter_mut().enumerate() {
        let every = if s.pane.is_some() { SCREEN_EVERY } else { LIST_EVERY };
        if s.last.is_some_and(|t| t.elapsed() < every) && !s.fresh { continue; }
        let data = match s.name.as_str() {
            // Sem o "why" (tempos relativos que mudam todo segundo): esta no call agents.
            "agents" => handle(srv, &json!({"cmd": "agents"}), Caller(None)).ok().map(|r| {
                let mut r = filter(srv, "agents", r);
                for x in r["agents"].as_array_mut().into_iter().flatten() { if let Some(o) = x.as_object_mut() { o.remove("why"); } }
                r
            }),
            "inbox" => handle(srv, &json!({"cmd": "inbox"}), Caller(None)).ok().map(|r| filter(srv, "inbox", r)),
            "approvals" => Some(approvals(srv)),
            _ => match screen(srv, s.pane.unwrap_or(0)) {
                Some(v) => Some(v),
                None => { gone.push(i); Some(json!({"pane": s.pane, "closed": true})) }
            },
        };
        let Some(mut data) = data else { continue };
        if let Some(o) = data.as_object_mut() { o.remove("ok"); }
        mask(&res, &mut data);
        // "now" (relogio do server) sozinho nao e mudanca.
        let h = hash(&data.as_object().map(|o| { let mut o = o.clone(); o.remove("now"); Value::Object(o) }).unwrap_or(Value::Null));
        s.fresh = false;
        if h == s.hash { continue; }
        s.hash = h;
        s.last = Some(Instant::now());
        out.push(json!({"ev": s.name, "data": data}));
    }
    for i in gone.into_iter().rev() { subs.remove(i); }
    out
}

/// Pedidos do Duck esperando o usuario (sem os de pane privado).
fn approvals(srv: &Arc<Server>) -> Value {
    let m = srv.lock();
    let list: Vec<Value> = m.approvals.iter().filter(|a| a.decided.is_none() && (a.pane == 0 || visible(&m, a.pane)))
        .map(|a| json!({"id": a.id, "kind": a.kind, "text": a.text, "pane": a.pane})).collect();
    json!({"approvals": list})
}

/// Tela visivel do pane em texto (sem cores), com cursor e tamanho.
fn screen(srv: &Arc<Server>, id: Id) -> Option<Value> {
    let m = srv.lock();
    let p = m.panes.get(&id).filter(|p| !p.private)?;
    let vt = p.vt();
    let t = &vt.term;
    let n = t.screen_lines();
    let lines: Vec<String> = (0..n as i32).map(|l| line_text(t, l)).collect();
    let cur = t.grid().cursor.point;
    Some(json!({"pane": id, "title": p.title, "cols": t.columns(), "rows": n, "cursor": [cur.line.0, cur.column.0], "lines": lines,
        "state": p.agent.as_ref().map(|a| a.state.clone())}))
}

/// Segredos mascarados em todo texto (menos o conteudo de arquivo, `data`).
fn mask(res: &[regex::Regex], v: &mut Value) {
    match v {
        Value::String(s) => {
            let t = res.iter().fold(std::mem::take(s), |t, re| re.replace_all(&t, "***").into_owned());
            *s = config::screen_secrets().replace_all(&t, "***").into_owned();
        }
        Value::Array(a) => for x in a { mask(res, x) },
        Value::Object(o) => for (k, x) in o.iter_mut() { if k != "data" { mask(res, x) } },
        _ => {}
    }
}

/// Tira os panes privados das listas (e as abas que so tinham eles).
fn filter(srv: &Arc<Server>, cmd: &str, mut r: Value) -> Value {
    let (private, name): (HashSet<Id>, String) = { let m = srv.lock(); (m.panes.iter().filter(|(_, p)| p.private).map(|(id, _)| *id).collect(), m.cfg.peer_name.clone()) };
    let hide = |v: &Value, k: &str| v[k].as_u64().is_some_and(|id| private.contains(&id));
    match cmd {
        "agents" => if let Some(a) = r["agents"].as_array_mut() {
            a.retain(|x| !hide(x, "pane"));
            for x in a.iter_mut() { x["computer"] = json!(name); }
        },
        "inbox" => if let Some(a) = r["items"].as_array_mut() { a.retain(|x| !hide(x, "pane")) },
        "loops" => if let Some(a) = r["loops"].as_array_mut() { a.retain(|x| !hide(x, "pane")) },
        "list" => for w in r["workspaces"].as_array_mut().into_iter().flatten() {
            if let Some(tabs) = w["tabs"].as_array_mut() {
                for t in tabs.iter_mut() {
                    if let Some(ps) = t["panes"].as_array_mut() { ps.retain(|p| !hide(p, "id")); }
                    if hide(t, "active_pane") { t["active_pane"] = t["panes"][0]["id"].clone(); }
                }
                tabs.retain(|t| t["panes"].as_array().is_some_and(|p| !p.is_empty()));
            }
        },
        _ => {}
    }
    r
}

type CallErr = (&'static str, String);

/// Um `call` do celular: permissao, pane privado, comando e mascara.
fn call(srv: &Arc<Server>, key: &str, cmd: &str, args: &Value) -> Result<Value, CallErr> {
    let dev = mb::by_key(key).ok_or(("forbidden", "celular revogado".to_string()))?;
    let scope = mb::scope_of(cmd).ok_or(("forbidden", format!("comando nao permitido ao celular: {cmd}")))?;
    if !dev.has(scope) { return Err(("forbidden", format!("sem a permissao {scope}: ligue no painel Celulares do Duck"))); }
    let mut a = if args.is_object() { args.clone() } else { json!({}) };
    if let Some(o) = a.as_object_mut() { o.remove("client"); o.remove("via"); }
    a["cmd"] = json!(cmd);
    a["via"] = json!(format!("mobile:{}", dev.name));
    {
        let m = srv.lock();
        let hidden = |id: Id| -> Result<(), CallErr> { if visible(&m, id) { Ok(()) } else { Err(("not_found", format!("pane {id} nao existe"))) } };
        if mb::PANE_CMDS.contains(&cmd) { hidden(arg_id(&a, "pane").ok_or(("invalid", "falta pane".to_string()))?)?; }
        match cmd {
            "approve" => {
                let aid = a["id"].as_u64().ok_or(("invalid", "falta id".to_string()))?;
                let ap = m.approvals.iter().find(|x| x.id == aid).ok_or(("not_found", "pedido nao existe".to_string()))?;
                if ap.pane != 0 { hidden(ap.pane)?; }
            }
            "spawn" => if let Some(p) = arg_id(&a, "pane") { hidden(p)?; },
            _ => {}
        }
    }
    match cmd {
        "push-token" => {
            let t = a["token"].as_str().map(str::trim).unwrap_or("");
            if t.len() > 4096 || t.chars().any(|c| c.is_whitespace() || c.is_control()) { return Err(("invalid", "token invalido".into())); }
            let d = mb::Device { token: t.to_string(), ..dev };
            mb::save(&d).map_err(|e| ("unavailable", e))?;
            audit_mobile(srv, &a);
            return Ok(json!({"push": !t.is_empty()}));
        }
        "spawn" => spawn_args(&mut a).map_err(|e| ("invalid", e))?,
        _ => {}
    }
    let r = handle(srv, &a, Caller(None)).map_err(|e| (code_of(&e), e))?;
    let mut r = filter(srv, cmd, r);
    if let Some(o) = r.as_object_mut() { o.remove("ok"); }
    let res = srv.lock().secrets.clone();
    mask(&res, &mut r);
    Ok(r)
}

/// O audit de um pedido que nao passa por `handle`.
fn audit_mobile(srv: &Arc<Server>, a: &Value) { let m = srv.lock(); audit(&m, Caller(None), a); }

/// `spawn` do celular: `{cwd, agent?, task?, worktree?, title?}` vira o spawn
/// do server (aba nova, agente digitado no shell quando ele subir).
fn spawn_args(a: &mut Value) -> Result<(), String> {
    let mut cwd = arg_str(a, "cwd").unwrap_or("").to_string();
    if let Some(branch) = arg_str(a, "worktree").map(str::trim).filter(|b| !b.is_empty()).map(String::from) {
        if !branch.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) || branch.starts_with('-') { return Err("worktree: nome de branch invalido".into()); }
        if cwd.is_empty() { return Err("worktree precisa de cwd".into()); }
        let git = |args: &[&str]| -> Result<String, String> {
            let mut c = std::process::Command::new("git");
            c.arg("-C").arg(&cwd).args(args).stdin(std::process::Stdio::null());
            #[cfg(windows)]
            { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
            let o = c.output().map_err(|e| format!("git: {e}"))?;
            if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()) } else { Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())) }
        };
        let top = git(&["rev-parse", "--show-toplevel"])?;
        let dest = format!("{top}-{}", branch.replace('/', "-"));
        git(&["worktree", "add", "-b", &branch, &dest])?;
        cwd = dest;
        a["cwd"] = json!(cwd);
    }
    if let Some(agent) = arg_str(a, "agent").map(str::trim).filter(|x| !x.is_empty()).map(String::from) {
        if !agent.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("agent: so o nome do programa (claude, codex, gemini)".into()); }
        let task = arg_str(a, "task").map(|t| clean(&t.replace(['\n', '\r'], " "), 4000)).unwrap_or_default();
        let line = if task.trim().is_empty() { agent } else { format!("{agent} {}", shell_quote(&task)) };
        a["type"] = json!(line);
    }
    if a.get("where").is_none() { a["where"] = json!("tab"); }
    if a.get("focus").is_none() { a["focus"] = json!(false); }
    if a.get("title").is_none() && !cwd.is_empty() { a["title"] = json!(cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("")); }
    if let Some(o) = a.as_object_mut() { for k in ["agent", "task", "worktree", "argv"] { o.remove(k); } }
    Ok(())
}

/// Texto entre aspas simples para o shell do pane (pwsh e sh aceitam '').
fn shell_quote(s: &str) -> String {
    if cfg!(windows) { format!("'{}'", s.replace('\'', "''")) } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

// ---------------------------------------------------------------- push

/// Push pelo relay nos eventos do contrato (approval, done, loop, command).
/// Sem relay ou sem celular com token, nao faz nada.
pub(super) fn push_loop(srv: Arc<Server>) {
    let mut last = srv.lock().event_seq;
    let mut warned = false;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let (pushes, relay, secret) = {
            let m = srv.lock();
            let new: Vec<Value> = m.events.iter().filter(|(s, _)| *s > last).map(|(_, e)| e.clone()).collect();
            last = m.event_seq;
            if new.is_empty() { continue; }
            let n = m.approvals.iter().filter(|a| a.decided.is_none() && (a.pane == 0 || visible(&m, a.pane))).count()
                + m.panes.values().filter(|p| !p.private && p.agent.as_ref().is_some_and(|a| a.state == "blocked")).count();
            let list: Vec<Value> = new.iter().filter_map(|e| push_data(e, &|id| visible(&m, id), &m.cfg.peer_name, n)).collect();
            (list, m.cfg.peer_relay.clone(), m.cfg.peer_relay_secret.clone())
        };
        if pushes.is_empty() { continue; }
        let tokens: Vec<String> = mb::list().into_iter().map(|d| d.token).filter(|t| !t.is_empty()).collect();
        if tokens.is_empty() { continue; }
        if relay.is_empty() {
            if !warned { proto::log("celular: push precisa de relay (painel Computadores > Relay)"); warned = true; }
            continue;
        }
        for d in pushes {
            if let Err(e) = crate::relay::push(&relay, &secret, &tokens, &d) { proto::log(&format!("celular: push: {e}")); }
        }
    }
}

/// Dados do push de um evento do server (None: evento sem push). Nada de
/// titulo, comando ou texto: so o tipo e os numeros.
pub(super) fn push_data(e: &Value, visible: &dyn Fn(Id) -> bool, duck: &str, n: usize) -> Option<Value> {
    let pane = e["pane"].as_u64().unwrap_or(0);
    if pane != 0 && !visible(pane) { return None; }
    let kind = match (e["event"].as_str()?, e["state"].as_str().unwrap_or("")) {
        ("approval", _) => "approval",
        ("attention", "blocked") => "approval",
        ("attention", "done") => "done",
        ("loop", _) if matches!(e["phase"].as_str(), Some("passed" | "failed")) => "loop",
        ("command-finished", _) => "command",
        _ => return None,
    };
    let mut d = json!({"duck": duck, "kind": kind, "pane": pane, "n": n});
    if let Some(a) = e["approval"].as_u64() { d["approval"] = json!(a); }
    if kind == "loop" { d["ok"] = json!(e["phase"] == "passed"); }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privado_some_das_listas_e_recusa() {
        let srv = testkit::server((80, 24));
        let ids = testkit::add_panes(&srv, 3);
        let (pub_id, priv_id) = (ids[0], ids[1]);
        {
            let mut m = srv.lock();
            for id in [pub_id, priv_id] { set_state(&mut m, id, "working", Some("claude"), "hook"); }
            m.panes.get_mut(&priv_id).unwrap().private = true;
            m.inbox.push(json!({"kind": "agent", "pane": priv_id, "title": "segredo"}));
            m.inbox.push(json!({"kind": "agent", "pane": pub_id, "title": "ok"}));
            ask(&mut m, "action", priv_id, "x".into(), String::new());
            m.cfg.secret_patterns.clear();
        }
        let key = "c1".repeat(32);
        mb::save(&mb::Device { name: "teste-privado".into(), key: key.clone(), scopes: mb::SCOPES.iter().map(|s| s.to_string()).collect(), ..Default::default() }).unwrap();
        let ag = call(&srv, &key, "agents", &json!({})).unwrap();
        let panes: Vec<u64> = ag["agents"].as_array().unwrap().iter().map(|a| a["pane"].as_u64().unwrap()).collect();
        assert_eq!(panes, vec![pub_id]);
        assert!(ag["agents"][0]["computer"].is_string());
        let l = call(&srv, &key, "list", &json!({})).unwrap();
        let listed: Vec<u64> = l["workspaces"][0]["tabs"][0]["panes"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect();
        assert!(!listed.contains(&priv_id) && listed.contains(&pub_id));
        let ib = call(&srv, &key, "inbox", &json!({})).unwrap();
        assert!(ib["items"].as_array().unwrap().iter().all(|i| i["pane"] != priv_id));
        for cmd in ["read", "send", "paste", "queue", "close", "file-get", "timeline", "agent-answer", "interrupt", "translate"] {
            let e = call(&srv, &key, cmd, &json!({"pane": priv_id, "text": "x", "path": "a", "yes": true})).unwrap_err();
            assert_eq!(e.0, "not_found", "{cmd}: {}", e.1);
        }
        assert!(approvals(&srv)["approvals"].as_array().unwrap().is_empty());
        let aid = srv.lock().approval_seq;
        assert_eq!(call(&srv, &key, "approve", &json!({"id": aid, "ok": true})).unwrap_err().0, "not_found");
        assert!(screen(&srv, priv_id).is_none() && screen(&srv, pub_id).is_some());
        assert!(call(&srv, &key, "read", &json!({"pane": pub_id})).is_ok());
        // Sem pane, comando de pane e recusado (nada de "o pane ativo").
        assert_eq!(call(&srv, &key, "read", &json!({})).unwrap_err().0, "invalid");
        // Fora da lista e recusado; sem a permissao tambem.
        assert_eq!(call(&srv, &key, "kill-server", &json!({})).unwrap_err().0, "forbidden");
        mb::save(&mb::Device { name: "teste-privado".into(), key: key.clone(), scopes: vec!["view".into()], ..Default::default() }).unwrap();
        assert_eq!(call(&srv, &key, "send", &json!({"pane": pub_id, "text": "ls"})).unwrap_err().0, "forbidden");
        // Push: evento de pane privado nao sai; sem texto.
        let vis = |id: Id| id != priv_id;
        assert!(push_data(&json!({"event": "attention", "pane": priv_id, "state": "blocked"}), &vis, "pc", 1).is_none());
        let d = push_data(&json!({"event": "approval", "pane": pub_id, "approval": 7, "title": "segredo"}), &vis, "pc", 2).unwrap();
        assert_eq!(d, json!({"duck": "pc", "kind": "approval", "pane": pub_id, "n": 2, "approval": 7}));
        assert_eq!(push_data(&json!({"event": "attention", "pane": pub_id, "state": "done"}), &vis, "pc", 0).unwrap()["kind"], "done");
        assert!(push_data(&json!({"event": "bell", "pane": pub_id}), &vis, "pc", 0).is_none());
        mb::remove("teste-privado").unwrap();
        assert_eq!(call(&srv, &key, "agents", &json!({})).unwrap_err().0, "forbidden");
    }

    #[test]
    fn mascara_e_spawn() {
        let mut v = json!({"text": "token ghp_0123456789abcdef0123456789abcdef0123 ok", "data": "ghp_0123456789abcdef0123456789abcdef0123"});
        let re = vec![regex::Regex::new("ok$").unwrap()];
        mask(&re, &mut v);
        assert!(!v["text"].as_str().unwrap().contains("ghp_0123"), "{v}");
        assert!(v["text"].as_str().unwrap().ends_with("***"));
        assert!(v["data"].as_str().unwrap().starts_with("ghp_"), "conteudo de arquivo nao muda");
        let mut a = json!({"cwd": "/home/u/proj", "agent": "claude", "task": "arrume o 'build'"});
        spawn_args(&mut a).unwrap();
        assert_eq!(a["where"], "tab");
        assert_eq!(a["title"], "proj");
        assert!(a["type"].as_str().unwrap().starts_with("claude '"));
        assert!(a.get("agent").is_none() && a.get("task").is_none());
        assert!(spawn_args(&mut json!({"agent": "claude; rm -rf /"})).is_err());
        assert!(spawn_args(&mut json!({"cwd": "/x", "worktree": "--force"})).is_err());
    }

    /// Pareamento de ponta a ponta: convite de uso unico, numero de
    /// confirmacao e a sessao respondendo (ping, call, permissao).
    #[test]
    fn pareia_e_conversa() {
        let srv = testkit::server((80, 24));
        testkit::add_panes(&srv, 1);
        srv.lock().cfg.peer_relay = "relay.exemplo.com:7879".into();
        let inv = invite(&srv).unwrap();
        let p = mb::Pairing::parse(inv["url"].as_str().unwrap()).unwrap();
        assert_eq!(p.relay, "relay.exemplo.com:7879");
        assert_eq!(p.invite.len(), 16);
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let s2 = srv.clone();
        std::thread::spawn(move || for s in l.incoming().flatten() {
            let s2 = s2.clone();
            std::thread::spawn(move || peer_conn(&s2, s, false));
        });
        let kp = snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap()).generate_keypair().unwrap();
        let dial = || crate::secure::Chan::connect_with(std::net::TcpStream::connect(addr).unwrap(), &kp.private).unwrap();
        let mut c = dial();
        let r = c.call(&json!({"op": "pair", "kind": "mobile", "name": "Pixel Teste", "invite": p.invite})).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["scopes"], json!(["view", "approve", "message"]));
        assert_eq!(r["confirm"], c.confirm().replace(' ', "-"));
        let name = r["name"].as_str().unwrap().to_string();
        assert!(name.starts_with("pixelteste"));
        assert_eq!(c.call(&json!({"op": "ping"})).unwrap(), json!({"op": "pong"}));
        let r = c.call(&json!({"id": 1, "op": "call", "cmd": "list", "args": {}})).unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["ok"], true);
        let r = c.call(&json!({"id": 2, "op": "call", "cmd": "send", "args": {"pane": 1, "text": "ls"}})).unwrap();
        assert_eq!(r["error"]["code"], "forbidden");
        let r = c.call(&json!({"id": 3, "op": "subscribe", "events": ["agents", "screen:1", "screen:99", "foo"]})).unwrap();
        assert_eq!(r["data"]["events"], json!(["agents", "screen:1"]));
        assert_eq!(r["data"]["rejected"], json!(["screen:99", "foo"]));
        let mut evs = vec![];
        for _ in 0..2 { evs.push(c.recv_json().unwrap()["ev"].as_str().unwrap().to_string()); }
        evs.sort();
        assert_eq!(evs, ["agents", "screen:1"]);
        // O mesmo convite nao serve de novo (outra chave).
        let kp2 = snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap()).generate_keypair().unwrap();
        let mut c2 = crate::secure::Chan::connect_with(std::net::TcpStream::connect(addr).unwrap(), &kp2.private).unwrap();
        let r = c2.call(&json!({"op": "pair", "kind": "mobile", "name": "outro", "invite": p.invite})).unwrap();
        assert_eq!(r["error"]["code"], "forbidden");
        // Chave desconhecida sem convite: erro e fecha.
        let mut c3 = crate::secure::Chan::connect_with(std::net::TcpStream::connect(addr).unwrap(), &kp2.private).unwrap();
        assert_eq!(c3.call(&json!({"op": "ping"})).unwrap()["error"]["code"], "forbidden");
        // Revogar derruba a sessao aberta.
        mb::remove(&name).unwrap();
        assert_eq!(kill_sessions(&crate::secure::hex(&kp.public)), 1);
        let mut got_revoked = false;
        for _ in 0..20 {
            match c.recv_json() { Ok(v) if v["ev"] == "revoked" => { got_revoked = true; break; } Ok(_) => continue, Err(_) => break }
        }
        assert!(got_revoked);
    }
}
