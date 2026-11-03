//! Extensoes: programas separados (qualquer linguagem; o SDK oficial e em
//! TypeScript) que conversam com o Duck por JSON, uma mensagem por linha, no
//! stdin/stdout. A extensao descreve dados (listas, detalhes, avisos) e o Duck
//! desenha tudo no proprio visual. Protocolo: docs/EXTENSIONS.md.
//!
//! Descoberta: ~/.duck/extensions/<nome>/duck-extension.json
//!   { "name": "github", "title": "GitHub", "icon": "github",
//!     "command": ["node", "dist/index.js"] }
//! Config opcional em ~/.duck.lua: extensions = { github = { enabled = true, ... } }
//! (a tabela inteira vai para a extensao no "init").

use super::*;
use super::ui::{inside, mt, uit};
use crate::render::Shape;
use std::io::{BufRead, Write as _};

#[derive(Deserialize, Clone, Default)]
pub(super) struct ExtAction { pub id: String, pub title: String, #[serde(default)] pub primary: bool }

#[derive(Deserialize, Clone, Default)]
pub(super) struct ExtItem {
    pub id: String,
    #[serde(deserialize_with = "one_line")] pub title: String,
    #[serde(default, deserialize_with = "one_line")] pub subtitle: String,
    #[serde(default)] pub icon: String,
    #[serde(default)] pub badge: String,
    #[serde(default)] pub color: String,
    #[serde(default)] pub unread: bool,
    #[serde(default)] pub actions: Vec<ExtAction>,
}

#[derive(Deserialize, Clone, Default)]
pub(super) struct ExtSection { #[serde(default)] pub title: String, #[serde(default)] pub items: Vec<ExtItem> }

#[derive(Clone, Default)]
pub(super) struct ExtView {
    pub ext: String,
    pub id: String,
    pub title: String,
    pub icon: String,
    pub sections: Vec<ExtSection>,
    pub loading: bool,
    pub error: String,
}
impl ExtView { pub fn key(&self) -> String { format!("{}/{}", self.ext, self.id) } }

#[derive(Clone, Default)]
pub(super) struct ExtStatus { pub ext: String, pub id: String, pub text: String, pub color: String, pub view: String }

struct ExtProc {
    name: String, stdin: Option<std::process::ChildStdin>, _child: Option<std::process::Child>,
    /// Roda dentro do hospedeiro compartilhado (um Node para todas).
    hosted: bool,
    /// Nome curto no badge da barra (manifesto "short" ou "title").
    pub label: String,
    /// Segredos extras que o manifesto pede ("secrets": ["jira/trabalho"]); os "<nome>/..." sempre valem.
    secrets: Vec<String>,
    /// "context", "paste", "clipboard".
    permissions: Vec<String>,
}

/// Extensao encontrada no disco (ativa ou nao).
#[derive(Clone)]
pub(super) struct ExtInfo {
    pub name: String, pub title: String, pub description: String, pub version: String, pub category: String,
    pub dir: std::path::PathBuf, pub manifest: Value,
    /// ~/.duck/extensions (do usuario) ou da instalacao (de todos).
    pub personal: bool,
    /// O comando existe (ex.: dist/index.js compilado).
    pub built: bool,
}

/// Pastas de extensoes da instalacao: `<raiz>/extensions`, subindo a partir do executavel.
fn shared_dirs() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            let Some(cur) = d else { break };
            // <raiz>/extensions (Windows, repo) ou <prefixo>/share/duck/extensions (Linux/mac: ~/.local/bin/duck).
            let found = [cur.join("extensions"), cur.join("share").join("duck").join("extensions")].into_iter().find(|c| c.is_dir() && *c != extensions_dir());
            if let Some(c) = found { out.push(c); break; }
            d = cur.parent().map(|p| p.to_path_buf());
        }
    }
    out
}

fn label(i: &ExtInfo) -> String { i.manifest["short"].as_str().map(String::from).unwrap_or_else(|| i.title.clone()) }

/// Texto de uma linha so: mensagens com quebra de linha (Rollbar) nao invadem a linha de baixo.
fn one_line<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Ok(s.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn is_node(i: &ExtInfo) -> bool { i.manifest["command"][0].as_str().is_some_and(|c| c == "node") }

/// sdk/host.js da instalacao (ou do ~/.duck/extensions/sdk).
fn host_js() -> Option<std::path::PathBuf> {
    shared_dirs().into_iter().chain([extensions_dir()]).map(|d| d.join("sdk").join("host.js")).find(|p| p.is_file())
}

/// Todas as extensoes: as da instalacao e as pessoais (mesmo nome: a pessoal vence).
pub(super) fn catalog() -> Vec<ExtInfo> {
    let mut out: Vec<ExtInfo> = Vec::new();
    let mut roots: Vec<(std::path::PathBuf, bool)> = shared_dirs().into_iter().map(|d| (d, false)).collect();
    roots.push((extensions_dir(), true));
    for (root, personal) in roots {
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let dir = e.path();
            let Ok(text) = std::fs::read_to_string(dir.join("duck-extension.json")) else { continue };
            let Ok(man) = serde_json::from_str::<Value>(&text) else { continue };
            let name = man["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() { continue; }
            let s = |k: &str| man[k].as_str().unwrap_or("").to_string();
            // Comando "node dist/index.js": compilada se o arquivo existir.
            let built = man["command"].as_array().map(|c| c.iter().skip(1).filter_map(|v| v.as_str()).filter(|a| a.contains('.')).all(|a| dir.join(a).exists())).unwrap_or(false);
            let info = ExtInfo { title: if s("title").is_empty() { name.clone() } else { s("title") }, description: s("description"), version: s("version"), category: s("category"), dir, manifest: man, personal, built, name };
            out.retain(|x| x.name != info.name);
            out.push(info);
        }
    }
    for (name, title, desc, icon) in BUILTIN {
        if out.iter().any(|x| x.name == *name) { continue; }
        out.push(ExtInfo { name: name.to_string(), title: title.to_string(), description: desc.to_string(), version: env!("CARGO_PKG_VERSION").into(),
            category: "Duck".into(), dir: std::path::PathBuf::new(), manifest: json!({"builtin": true, "icon": icon}), personal: true, built: true });
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Extensoes internas: badge na barra desenhado pelo proprio Duck (sem processo).
pub(super) const BUILTIN: &[(&str, &str, &str, &str)] = &[
    ("agents", "Agentes", "Agentes do Claude/Codex: bloqueados, trabalhando e prontos; abre o painel do agente", "\u{f06a9}"),
    ("system", "Sistema", "CPU e RAM da maquina; abre o painel com nucleos, swap e processos", "\u{f4bc}"),
];

#[derive(Default)]
pub(super) struct Exts {
    procs: Vec<ExtProc>,
    pub views: Vec<ExtView>,
    pub status: Vec<ExtStatus>,
    /// Comandos da paleta: (extensao, id, titulo).
    pub commands: Vec<(String, String, String)>,
    inbox: Arc<Mutex<Vec<(String, Value)>>>,
    /// Hospedeiro das extensoes em Node (sdk/host.js).
    host: Option<(Option<std::process::ChildStdin>, std::process::Child)>,
    /// Sobe a cada reinicio: o "exited" do hospedeiro antigo nao derruba o novo.
    host_gen: u64,
}

impl Exts {
    pub fn view(&self, key: &str) -> Option<&ExtView> { self.views.iter().find(|v| v.key() == key) }

    /// Manda so se a extensao tem a permissao.
    pub fn send_if(&mut self, ext: &str, perm: &str, msg: Value) {
        if self.procs.iter().any(|p| p.name == ext && p.permissions.iter().any(|x| x == perm)) { self.send(ext, msg); }
    }

    /// Manda uma mensagem para a extensao (linha JSON).
    /// Nome curto da extensao para a barra.
    pub fn label(&self, ext: &str) -> String { self.procs.iter().find(|p| p.name == ext).map(|p| p.label.clone()).unwrap_or_else(|| ext.to_string()) }

    pub fn send(&mut self, ext: &str, msg: Value) {
        let Some(p) = self.procs.iter_mut().find(|p| p.name == ext) else { return };
        if p.hosted {
            let mut m = msg;
            m["ext"] = json!(ext);
            if let Some((Some(s), _)) = self.host.as_mut() {
                if writeln!(s, "{m}").and_then(|_| s.flush()).is_err() { self.host.as_mut().unwrap().0 = None; }
            }
            return;
        }
        let ok = p.stdin.as_mut().is_some_and(|s| writeln!(s, "{msg}").and_then(|_| s.flush()).is_ok());
        if !ok { p.stdin = None; }
    }
}

pub(super) fn extensions_dir() -> std::path::PathBuf { config::home().join(".duck").join("extensions") }

/// Icones por nome (Nerd Font embutida); qualquer outro texto e usado como esta.
pub(super) fn icon(name: &str) -> String {
    match name {
        "github" => "\u{f09b}", "bell" => "\u{f0f3}", "issue" => "\u{f41b}", "issue-closed" => "\u{f41d}",
        "pr" => "\u{f407}", "pr-merged" => "\u{f419}", "pr-draft" => "\u{f4dd}", "repo" => "\u{f401}",
        "comment" => "\u{f075}", "check" => "\u{f00c}", "x" => "\u{f00d}", "star" => "\u{f005}",
        "tag" => "\u{f02b}", "eye" => "\u{f06e}", "clock" => "\u{f017}", "user" => "\u{f007}",
        "jira" => "\u{f0b0}", "azure" => "\u{f0c2}", "list" => "\u{f03a}", "warning" => "\u{f071}",
        "refresh" => "\u{f021}", "branch" => "\u{e725}", "rocket" => "\u{f135}", "bug" => "\u{f188}",
        "" => "\u{f111}",
        other => return other.to_string(),
    }.to_string()
}

impl Gui {
    /// Configuracao de todas as extensoes (tabela `extensions` do ~/.duck.lua).
    pub(super) fn ext_settings(&self) -> Value {
        self.cfg_table.as_ref().and_then(|t| t.get::<LuaValue>("extensions").ok())
            .and_then(|v| self.lua.from_value(v).ok()).filter(|v: &Value| v.is_object()).unwrap_or(json!({}))
    }

    /// Ativa: a pessoal (~/.duck/extensions) vale ate ser desligada; a da
    /// instalacao so roda depois de ligada no menu (nao gasta memoria a toa).
    pub(super) fn ext_enabled(&self, info: &ExtInfo, settings: &Value) -> bool {
        match settings[&info.name]["enabled"].as_bool() { Some(b) => b, None => info.personal }
    }

    /// Sobe cada extensao ativa.
    pub(super) fn start_extensions(&mut self) {
        let settings = self.ext_settings();
        let mut hosted = Vec::new();
        for info in catalog() {
            if !(self.ext_enabled(&info, &settings) && info.built) { continue; }
            if is_node(&info) && host_js().is_some() { hosted.push(info); } else { self.start_one(&info, &settings); }
        }
        self.start_host(hosted, &settings);
    }

    /// Um Node so para todas as extensoes Node ativas.
    fn start_host(&mut self, list: Vec<ExtInfo>, settings: &Value) {
        if list.is_empty() { return; }
        let Some(host) = host_js() else { return };
        let arg = json!(list.iter().map(|i| json!({"name": i.name, "dir": i.dir.display().to_string(),
            "main": i.manifest["command"][1].as_str().unwrap_or("dist/index.js")})).collect::<Vec<_>>()).to_string();
        let log = std::fs::File::create(crate::proto::data_dir().join("ext-host.log")).ok();
        let mut c = std::process::Command::new("node");
        c.arg(&host).arg(arg).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(log.map(std::process::Stdio::from).unwrap_or_else(std::process::Stdio::null));
        if let Some(d) = host.parent() { c.current_dir(d); }
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(err) => { self.flash(&format!("Extensoes: o Node nao abriu ({err})")); return; }
        };
        let stdout = child.stdout.take().unwrap();
        let members: Vec<String> = list.iter().map(|i| i.name.clone()).collect();
        self.exts.host_gen += 1;
        let host_gen = self.exts.host_gen;
        let (inbox, proxy) = (self.exts.inbox.clone(), self.proxy.clone());
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let Some(n) = v["ext"].as_str().map(String::from) else { continue };
                inbox.lock().unwrap().push((n, v));
                if proxy.send_event(UserEv::Ext).is_err() { return; }
            }
            for n in members { inbox.lock().unwrap().push((n, json!({"type": "exited", "host_gen": host_gen}))); }
            let _ = proxy.send_event(UserEv::Ext);
        });
        self.exts.host = Some((child.stdin.take(), child));
        for info in &list {
            let man = &info.manifest;
            let lst = |k: &str| man[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>();
            self.exts.procs.push(ExtProc { name: info.name.clone(), stdin: None, _child: None, hosted: true, label: label(info), secrets: lst("secrets"), permissions: lst("permissions") });
            let init = json!({"type": "init", "settings": settings[&info.name], "duck": {"version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS}});
            self.exts.send(&info.name, init);
            if let Some(c) = self.ext_context.clone() { self.exts.send_if(&info.name, "context", c); }
        }
    }

    /// Reinicia o hospedeiro com as extensoes Node ativas agora.
    fn restart_host(&mut self) {
        let hosted: Vec<String> = self.exts.procs.iter().filter(|p| p.hosted).map(|p| p.name.clone()).collect();
        for n in &hosted { self.clear_ext(n); }
        self.exts.procs.retain(|p| !p.hosted);
        if let Some((stdin, mut child)) = self.exts.host.take() { drop(stdin); let _ = child.kill(); }
        let settings = self.ext_settings();
        let list: Vec<ExtInfo> = catalog().into_iter().filter(|i| is_node(i) && i.built && self.ext_enabled(i, &settings)).collect();
        self.start_host(list, &settings);
    }

    fn clear_ext(&mut self, name: &str) {
        self.exts.views.retain(|v| v.ext != name);
        self.exts.status.retain(|s| s.ext != name);
        self.exts.commands.retain(|c| c.0 != name);
        if self.ext_panel.as_deref().is_some_and(|k| k.starts_with(&format!("{name}/"))) { self.ext_panel = None; self.send_size(); }
    }

    pub(super) fn start_one(&mut self, info: &ExtInfo, settings: &Value) {
        let name = info.name.clone();
        if self.exts.procs.iter().any(|p| p.name == name) { return; }
        let man = &info.manifest;
        let cmd: Vec<String> = man["command"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect();
        let Some(prog) = cmd.first() else { return };
        let log = std::fs::File::create(crate::proto::data_dir().join(format!("ext-{name}.log"))).ok();
        let mut c = std::process::Command::new(prog);
        c.args(&cmd[1..]).current_dir(&info.dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(log.map(std::process::Stdio::from).unwrap_or_else(std::process::Stdio::null));
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(err) => { self.flash(&format!("Extensao {name}: nao abriu ({prog}: {err})")); return; }
        };
        let stdout = child.stdout.take().unwrap();
        let (inbox, proxy, n) = (self.exts.inbox.clone(), self.proxy.clone(), name.clone());
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                inbox.lock().unwrap().push((n.clone(), v));
                if proxy.send_event(UserEv::Ext).is_err() { return; }
            }
            inbox.lock().unwrap().push((n.clone(), json!({"type": "exited"})));
            let _ = proxy.send_event(UserEv::Ext);
        });
        let list = |k: &str| man[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>();
        self.exts.procs.push(ExtProc { name: name.clone(), stdin: child.stdin.take(), _child: Some(child), hosted: false, label: label(info), secrets: list("secrets"), permissions: list("permissions") });
        let init = json!({"type": "init", "settings": settings[&name], "duck": {"version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS}});
        self.exts.send(&name, init);
        if let Some(c) = self.ext_context.clone() { self.exts.send_if(&name, "context", c); }
    }

    /// Encerra a extensao e tira tudo o que ela pos na tela.
    pub(super) fn stop_one(&mut self, name: &str) {
        if let Some(i) = self.exts.procs.iter().position(|p| p.name == name) {
            if self.exts.procs[i].hosted {
                // Dentro do hospedeiro: reinicia o hospedeiro sem ela (a config ja diz "desativada").
                self.load_config();
                self.restart_host();
                return;
            }
            let mut p = self.exts.procs.remove(i);
            drop(p.stdin.take());
            if let Some(mut c) = p._child.take() { let _ = c.kill(); }
        }
        self.clear_ext(name);
    }

    pub(super) fn restart_one(&mut self, name: &str) {
        // Config recarregada antes: as mudancas do menu entram no init.
        self.load_config();
        let settings = self.ext_settings();
        let Some(info) = catalog().into_iter().find(|i| i.name == name) else { return };
        if is_node(&info) && host_js().is_some() {
            // Some do processo proprio (se havia) e entra no hospedeiro.
            if let Some(i) = self.exts.procs.iter().position(|p| p.name == name && !p.hosted) {
                let mut p = self.exts.procs.remove(i);
                if let Some(mut c) = p._child.take() { let _ = c.kill(); }
            }
            self.restart_host();
            return;
        }
        self.stop_one(name);
        if self.ext_enabled(&info, &settings) && info.built { self.start_one(&info, &settings); }
    }

    /// Pane ativo mudou: avisa as extensoes com permissao "context".
    pub(super) fn ext_context_tick(&mut self) {
        let Some(p) = self.active() else { return };
        let ctx = json!({"type": "context", "cwd": p.cwd, "pane": p.id, "domain": p.domain, "title": p.title});
        if self.ext_context.as_ref() == Some(&ctx) { return; }
        self.ext_context = Some(ctx.clone());
        let names: Vec<String> = self.exts.procs.iter().map(|p| p.name.clone()).collect();
        for n in names { self.exts.send_if(&n, "context", ctx.clone()); }
    }

    /// Texto copiado no Duck: para as extensoes com permissao "clipboard".
    pub(super) fn ext_clipboard(&mut self, text: &str) {
        let names: Vec<String> = self.exts.procs.iter().map(|p| p.name.clone()).collect();
        for n in names { self.exts.send_if(&n, "clipboard", json!({"type": "clipboard", "text": text})); }
    }

    /// Mensagens que chegaram das extensoes.
    pub(super) fn ext_poll(&mut self) {
        let msgs: Vec<(String, Value)> = std::mem::take(&mut *self.exts.inbox.lock().unwrap());
        for (ext, m) in msgs { self.ext_handle(&ext, m); }
        self.redraw();
    }

    fn ext_handle(&mut self, ext: &str, m: Value) {
        let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
        match m["type"].as_str().unwrap_or("") {
            "register" => {
                for v in m["views"].as_array().into_iter().flatten() {
                    let id = v["id"].as_str().unwrap_or("").to_string();
                    if id.is_empty() || self.exts.views.iter().any(|x| x.ext == ext && x.id == id) { continue; }
                    self.exts.views.push(ExtView { ext: ext.into(), id, title: v["title"].as_str().unwrap_or("").into(), icon: v["icon"].as_str().unwrap_or("").into(), loading: true, ..Default::default() });
                }
                for c in m["commands"].as_array().into_iter().flatten() {
                    self.exts.commands.push((ext.into(), c["id"].as_str().unwrap_or("").into(), c["title"].as_str().unwrap_or("").into()));
                }
            }
            "view" => {
                let id = s("id");
                let sections: Vec<ExtSection> = serde_json::from_value(m["sections"].clone()).unwrap_or_default();
                match self.exts.views.iter_mut().find(|x| x.ext == ext && x.id == id) {
                    Some(v) => {
                        if m.get("sections").is_some() { v.sections = sections; }
                        if let Some(t) = m["title"].as_str() { v.title = t.into(); }
                        v.loading = m["loading"].as_bool().unwrap_or(false);
                        v.error = s("error");
                    }
                    None => self.exts.views.push(ExtView { ext: ext.into(), id, title: s("title"), icon: s("icon"), sections, loading: false, error: s("error") }),
                }
            }
            "status" => {
                let id = s("id");
                let st = ExtStatus { ext: ext.into(), id: id.clone(), text: s("text"), color: s("color"), view: s("view") };
                match self.exts.status.iter_mut().find(|x| x.ext == ext && x.id == id) { Some(x) => *x = st, None => self.exts.status.push(st) }
                self.exts.status.retain(|x| !x.text.is_empty());
            }
            "notify" => {
                let theme = self.theme().clone();
                let item = s("item");
                let acts: Vec<(String, String)> = m["actions"].as_array().into_iter().flatten()
                    .map(|a| (a["id"].as_str().unwrap_or("").to_string(), a["title"].as_str().unwrap_or("").to_string())).collect();
                if self.focused {
                    self.card_seq += 1;
                    let buttons = acts.iter().map(|(id, t)| (t.clone(), "ext_action".to_string(), vec![json!(ext), json!(id), json!(item)])).collect();
                    let color = if m["color"].is_string() { theme.named(&s("color")) } else { theme.accent };
                    self.cards.push(Card { id: self.card_seq, title: s("title"), body: s("body"), color, buttons, until: Instant::now() + Duration::from_secs(m["seconds"].as_u64().unwrap_or(15)) });
                } else {
                    let enc = |x: &str| x.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
                    let urls = acts.iter().map(|(id, t)| (t.clone(), format!("duck://ext/{ext}/{}/{}", enc(id), enc(&item)))).collect::<Vec<_>>();
                    toast_actions(&s("title"), &s("body"), &urls);
                }
            }
            "detail" => {
                let actions = m["actions"].as_array().into_iter().flatten().map(|a| (
                    a["id"].as_str().unwrap_or("").to_string(), a["title"].as_str().unwrap_or("").to_string(),
                    a["item"].as_str().map(String::from).unwrap_or_else(|| s("item")))).collect();
                let lines = s("body").replace("\r\n", "\n").lines().map(String::from).collect();
                self.overlay = Some(Overlay { kind: OverlayKind::Detail { ext: ext.into(), subtitle: s("subtitle"), lines, actions, scroll: 0 }, ..Overlay::list(&s("title"), vec![]) });
            }
            "prompt" => {
                self.overlay = Some(Overlay::prompt(&s("title"), &s("value"), "ext_input", vec![json!(ext), json!(s("id"))]));
            }
            "open-url" => open_url(&s("url")),
            "open-pane" => {
                let mut req = json!({"cmd": "spawn", "where": m["where"].as_str().unwrap_or("tab"), "argv": m["cmd"].clone(), "pane": self.frame.active_pane});
                if let Some(c) = m["cwd"].as_str() { req["cwd"] = json!(c); }
                if let Some(t) = m["title"].as_str() { req["title"] = json!(t); }
                if m["where"] == "float" { req = json!({"cmd": "float", "replace": true, "argv": m["cmd"].clone()}); }
                self.cmd(req);
            }
            "flash" => self.flash(&s("text")),
            "paste" => {
                // Cola no pane ativo; texto de varias linhas ou perigoso passa pela confirmacao.
                if !self.exts.procs.iter().any(|p| p.name == ext && p.permissions.iter().any(|x| x == "paste")) {
                    gui_log(&format!("extensao {ext} tentou colar sem a permissao paste"));
                    return;
                }
                self.paste(s("text"), false);
                if m["enter"] == true && self.overlay.is_none() { self.send_keys("\r"); }
            }
            "secret" => {
                // So os segredos da propria extensao (ou os que o manifesto declara).
                let name = s("name");
                let allowed = name.starts_with(&format!("{ext}/")) || self.exts.procs.iter().any(|p| p.name == ext && p.secrets.contains(&name));
                let value = if allowed { crate::secret::get(&name) } else { None };
                if !allowed { gui_log(&format!("extensao {ext} pediu o segredo {name} sem permissao")); }
                self.exts.send(ext, json!({"type": "secret", "id": m["id"], "name": name, "value": value}));
            }
            "choose" => {
                let id = s("id");
                // A 1a opcao nunca e uma acao: Enter distraido nao muda nada
                // (no Jira a 1a transicao pode ser "Cancelar" a issue).
                let mut items = vec![Item::new("Voltar sem escolher", "Esc", "", vec![])];
                items.extend(m["options"].as_array().into_iter().flatten().map(|o| Item::new(
                    o["title"].as_str().unwrap_or(""), o["hint"].as_str().unwrap_or(""), "ext_input",
                    vec![json!(ext), json!(id), json!(o["id"].as_str().unwrap_or(""))])));
                self.overlay = Some(Overlay::list(&s("title"), items));
            }
            "show-view" => {
                let key = format!("{ext}/{}", s("id"));
                if self.ext_panel.as_deref() != Some(key.as_str()) { self.ext_builtin("ext_view", &[json!(key)]); }
            }
            "exited" => {
                if m["host_gen"].as_u64().is_some_and(|g| g != self.exts.host_gen) { return; } // hospedeiro antigo (reinicio nosso)
                self.exts.procs.retain(|p| p.name != ext);
                self.exts.status.retain(|x| x.ext != ext);
                for v in self.exts.views.iter_mut().filter(|v| v.ext == ext) { v.loading = false; v.error = "a extensao encerrou (veja ext-<nome>.log)".into(); }
            }
            _ => {}
        }
    }

    /// Acoes que vem da interface: botao, clique na linha, comando, prompt.
    pub(super) fn ext_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "ext_view" => {
                let key = s(0);
                let open = self.ext_panel.as_deref() == Some(key.as_str());
                if let Some(old) = self.ext_panel.take() {
                    if let Some((e, v)) = old.split_once('/') { self.exts.send(e, json!({"type": "view-close", "view": v})); }
                }
                if !open {
                    self.agent_panel = None;
                    self.sys_panel = false;
                    self.ext_scroll = 0.0;
                    if let Some((e, v)) = key.split_once('/') { self.exts.send(e, json!({"type": "view-open", "view": v})); }
                    self.ext_panel = Some(key);
                }
                self.stats.lock().unwrap().want_procs = self.sys_panel;
                self.send_size();
            }
            "ext_action" => {
                let view = self.ext_panel.as_deref().and_then(|k| k.split_once('/')).map(|x| x.1.to_string()).unwrap_or_default();
                self.exts.send(&s(0), json!({"type": "action", "action": s(1), "item": s(2), "view": view}));
            }
            "ext_command" => self.exts.send(&s(0), json!({"type": "command", "command": s(1)})),
            "ext_input" => self.exts.send(&s(0), json!({"type": "input", "id": s(1), "value": s(2)})),
            _ => return self.ext_menu_builtin(name, args),
        }
        self.redraw();
        true
    }

    /// Paleta: comandos das extensoes e os itens das visoes abertas.
    pub(super) fn ext_palette(&self, items: &mut Vec<Item>) {
        for (e, id, t) in &self.exts.commands { items.push(Item::new(t.clone(), format!("extensao · {e}"), "ext_command", vec![json!(e), json!(id)])); }
        for v in &self.exts.views {
            items.push(Item::new(format!("{}: abrir painel", v.title), format!("extensao · {}", v.ext), "ext_view", vec![json!(v.key())]));
            for sec in &v.sections {
                for it in &sec.items {
                    let Some(a) = it.actions.iter().find(|a| a.primary).or(it.actions.first()) else { continue };
                    items.push(Item::new(format!("{} · {}", v.title, it.title), it.subtitle.clone(), "ext_action", vec![json!(v.ext), json!(a.id), json!(it.id)]));
                }
            }
        }
    }
}

/// Valor JSON como literal Lua (para gravar no ~/.duck-local.lua).
pub(super) fn lua_lit(v: &Value) -> String {
    match v {
        Value::Null => "nil".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{s:?}"),
        Value::Array(a) => format!("{{ {} }}", a.iter().map(lua_lit).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => {
            let ident = |k: &str| k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            format!("{{ {} }}", o.iter().map(|(k, v)| if ident(k) { format!("{k} = {}", lua_lit(v)) } else { format!("[{k:?}] = {}", lua_lit(v)) }).collect::<Vec<_>>().join(", "))
        }
    }
}

/// Troca {campo} pelos valores da conta (nome do segredo).
fn fill(template: &str, vals: &Value) -> String {
    let mut out = template.to_string();
    if let Some(o) = vals.as_object() { for (k, v) in o { out = out.replace(&format!("{{{k}}}"), v.as_str().unwrap_or("")); } }
    out
}

impl Gui {
    /// Ctrl+Shift+P -> "Extensoes": todas as extensoes, ativas ou nao.
    pub(super) fn open_ext_menu(&mut self) {
        let settings = self.ext_settings();
        let running: Vec<String> = self.exts.procs.iter().map(|p| p.name.clone()).collect();
        let items: Vec<Item> = catalog().into_iter().map(|i| {
            let on = self.ext_enabled(&i, &settings);
            let state = if !i.built { "nao compilada" } else if running.contains(&i.name) { "ativa" } else if on { "ativa (parada)" } else { "desativada" };
            let mark = if running.contains(&i.name) { "●" } else { "○" };
            let cat = if i.category.is_empty() { String::new() } else { format!(" · {}", i.category) };
            Item::new(format!("{mark}  {}  —  {}", i.title, i.description), format!("{state}{cat}"), "ext_menu", vec![json!(i.name)])
        }).collect();
        if items.is_empty() { return self.flash("Nenhuma extensao encontrada (D:\\Tools\\Duck\\extensions ou ~/.duck/extensions)"); }
        self.overlay = Some(Overlay::list("Extensoes (Enter abre as opcoes)", items));
    }

    fn ext_submenu(&mut self, name: &str) {
        let Some(info) = catalog().into_iter().find(|i| i.name == name) else { return };
        let settings = self.ext_settings();
        let cur = &settings[name];
        let on = self.ext_enabled(&info, &settings);
        let running = self.exts.procs.iter().any(|p| p.name == name);
        let man = &info.manifest;
        let mut items = vec![];
        if info.manifest["builtin"] == true {
            items.push(if on { Item::new("Desativar", "tira o badge da barra", "ext_toggle", vec![json!(name), json!(false)]) }
                       else { Item::new("Ativar", "mostra o badge na barra", "ext_toggle", vec![json!(name), json!(true)]) });
            let (label, act) = if name == "agents" { ("Abrir painel do agente", "agent_panel") } else { ("Abrir painel do sistema", "sys_panel") };
            items.push(Item::new(label, "", act, vec![]));
            self.overlay = Some(Overlay::list(&format!("{} · interna do Duck", info.title), items));
            return;
        }
        if !info.built {
            items.push(Item::new("Compilar (npm install + build)", info.dir.display().to_string(), "ext_build", vec![json!(name)]));
        }
        items.push(if on { Item::new("Desativar", "para e nao abre mais", "ext_toggle", vec![json!(name), json!(false)]) }
                   else { Item::new("Ativar", "comeca agora e ao abrir o Duck", "ext_toggle", vec![json!(name), json!(true)]) });
        for v in self.exts.views.iter().filter(|v| v.ext == name) {
            items.push(Item::new(format!("Abrir: {}", v.title), "painel lateral", "ext_view", vec![json!(v.key())]));
        }
        // Contas (assistente) e o token de cada uma.
        if let Some(acc) = man["accounts"].as_object() {
            let key = acc.get("key").and_then(|v| v.as_str()).unwrap_or("accounts");
            let secret = acc.get("secret").and_then(|v| v.as_str()).unwrap_or("");
            for (k, a) in cur[key].as_array().into_iter().flatten().enumerate() {
                let label = a["name"].as_str().map(String::from).unwrap_or_else(|| a.to_string());
                if !secret.is_empty() {
                    let sname = fill(secret, a);
                    let has = crate::secret::get(&sname).is_some();
                    items.push(Item::new(format!("Token da conta {label}"), if has { format!("{sname} · gravado") } else { format!("{sname} · FALTA") }, "ext_secret", vec![json!(name), json!(sname)]));
                }
                let addr = a["url"].as_str().unwrap_or("");
                items.push(Item::new(format!("Editar conta {label}"), addr, "ext_acc_edit", vec![json!(name), json!(k)]));
                items.push(Item::new(format!("Remover conta {label}"), "", "ext_acc_remove", vec![json!(name), json!(k)]));
            }
            items.push(Item::new(format!("Adicionar conta ({})", acc.get("title").and_then(|v| v.as_str()).unwrap_or("conta")), "assistente", "ext_acc_add", vec![json!(name)]));
        }
        for st in man["settings"].as_array().into_iter().flatten() {
            let key = st["key"].as_str().unwrap_or("");
            let ty = st["type"].as_str().unwrap_or("string");
            let val = if cur[key].is_null() { st["default"].clone() } else { cur[key].clone() };
            let shown = match &val { Value::Null => "(padrao)".to_string(), Value::String(s) => s.clone(), Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", "), v => v.to_string() };
            items.push(Item::new(format!("Configurar: {}", st["title"].as_str().unwrap_or(key)), shown, "ext_set", vec![json!(name), json!(key), json!(ty)]));
        }
        if running { items.push(Item::new("Recarregar", "reinicia o processo", "ext_reload", vec![json!(name)])); }
        items.push(Item::new("Ver log", format!("ext-{name}.log"), "ext_log", vec![json!(name)]));
        items.push(Item::new("Abrir pasta", info.dir.display().to_string(), "open_url", vec![json!(info.dir.display().to_string())]));
        let perms = man["permissions"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let title = format!("{} {} · {}{}", info.title, info.version, if info.personal { "pessoal" } else { "instalacao (todos os usuarios)" },
            if perms.is_empty() { String::new() } else { format!(" · permissoes: {perms}") });
        self.overlay = Some(Overlay::list(&title, items));
    }

    /// Grava config.extensions.<ext>.<key> e reinicia a extensao.
    fn ext_write(&mut self, name: &str, key: &str, lit: &str) {
        match set_local_setting(&format!("extensions.{name}.{key}"), lit) {
            Ok(_) => { self.restart_one(name); self.flash(&format!("{name}: {key} gravado")); }
            Err(e) => self.flash(&e),
        }
    }

    pub(super) fn ext_menu_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "extensions" => self.open_ext_menu(),
            "ext_menu" => self.ext_submenu(&s(0)),
            "ext_toggle" => {
                let on = args.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
                let n = s(0);
                // Grava antes: no hospedeiro, parar = reiniciar com a config nova.
                match set_local_setting(&format!("extensions.{n}.enabled"), if on { "true" } else { "false" }) {
                    Ok(_) => { if on { self.restart_one(&n); } else { self.stop_one(&n); } self.flash(&format!("{n} {}", if on { "ativada" } else { "desativada" })); }
                    Err(e) => self.flash(&e),
                }
            }
            "ext_reload" => { let n = s(0); self.restart_one(&n); self.flash(&format!("{n} recarregada")); }
            "ext_log" => {
                let path = crate::proto::data_dir().join(format!("ext-{}.log", s(0)));
                let cmd = if cfg!(windows) { json!(["pwsh", "-NoLogo", "-NoProfile", "-Command", format!("Get-Content -Wait -Tail 80 '{}'", path.display())]) }
                          else { json!(["tail", "-n", "80", "-f", path.display().to_string()]) };
                self.cmd(json!({"cmd": "float", "replace": true, "argv": cmd}));
            }
            "ext_build" => {
                let Some(info) = catalog().into_iter().find(|i| i.name == s(0)) else { return true };
                let line = "npm install --no-audit --no-fund; npm run build";
                self.cmd(json!({"cmd": "float", "replace": true, "cwd": info.dir.display().to_string(),
                    "argv": if cfg!(windows) { json!(["pwsh", "-NoLogo", "-NoProfile", "-NoExit", "-Command", line]) } else { json!(["sh", "-c", line]) }}));
                self.flash("Compilando; depois use Ativar no menu Extensoes");
            }
            "ext_set" => {
                let (n, key, ty) = (s(0), s(1), s(2));
                let cur = &self.ext_settings()[&n][&key];
                if ty == "bool" {
                    let now = cur.as_bool().unwrap_or_else(|| catalog().into_iter().find(|i| i.name == n)
                        .and_then(|i| i.manifest["settings"].as_array().and_then(|a| a.iter().find(|x| x["key"] == key.as_str()).and_then(|x| x["default"].as_bool()))).unwrap_or(false));
                    self.ext_write(&n, &key, if now { "false" } else { "true" });
                    return true;
                }
                if ty == "list" { self.ext_list_menu(&n, &key); return true; }
                let shown = match cur { Value::String(v) => v.clone(), Value::Null => String::new(), v => v.to_string() };
                self.overlay = Some(Overlay::prompt(&format!("{n}: {key}"), &shown, "ext_set_value", vec![json!(n), json!(key), json!(ty)]));
            }
            "ext_set_value" => {
                let (n, key, ty, raw) = (s(0), s(1), s(2), s(3));
                let v = match ty.as_str() {
                    "number" => match raw.trim().parse::<f64>() { Ok(x) => json!(x), Err(_) => { self.flash("Numero invalido"); return true; } },
                    "list" => json!(raw.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>()),
                    _ => json!(raw),
                };
                self.ext_write(&n, &key, &lua_lit(&v));
            }
            "ext_acc_add" => self.ext_wizard(&s(0), json!({}), 0),
            "ext_acc_edit" => {
                let k = args.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
                let man = catalog().into_iter().find(|x| x.name == s(0)).map(|x| x.manifest).unwrap_or(json!({}));
                let key = man["accounts"]["key"].as_str().unwrap_or("accounts").to_string();
                let mut vals = self.ext_settings()[&s(0)][&key][k as usize].clone();
                if !vals.is_object() { return true; }
                vals["__edit"] = json!(k);
                self.ext_wizard(&s(0), vals, 0);
            }
            "ext_list" => self.ext_list_menu(&s(0), &s(1)),
            "ext_list_add" => {
                let i = args.get(2).and_then(|v| v.as_i64()).unwrap_or(-1);
                let cur = if i >= 0 { self.ext_list_get(&s(0), &s(1)).get(i as usize).cloned().unwrap_or_default() } else { String::new() };
                let title = if i >= 0 { format!("{}: editar item", s(1)) } else { format!("{}: novo item", s(1)) };
                self.overlay = Some(Overlay::prompt(&title, &cur, "ext_list_value", vec![json!(s(0)), json!(s(1)), json!(i)]));
            }
            "ext_list_value" => {
                let (n, key, raw) = (s(0), s(1), s(3));
                let i = args.get(2).and_then(|v| v.as_i64()).unwrap_or(-1);
                let mut list = self.ext_list_get(&n, &key);
                let raw = raw.trim().to_string();
                if raw.is_empty() { return true; }
                if i >= 0 && (i as usize) < list.len() { list[i as usize] = raw; } else { list.push(raw); }
                self.ext_write(&n, &key, &lua_lit(&json!(list)));
                self.ext_list_menu(&n, &key);
            }
            "ext_list_del" => {
                let i = args.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let mut list = self.ext_list_get(&s(0), &s(1));
                if i < list.len() { list.remove(i); }
                self.ext_write(&s(0), &s(1), &lua_lit(&json!(list)));
                self.ext_list_menu(&s(0), &s(1));
            }
            "ext_acc_step" => {
                // args: [ext, estado(json), indice, valor digitado]
                let mut vals: Value = serde_json::from_str(&s(1)).unwrap_or(json!({}));
                let i = args.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let man = catalog().into_iter().find(|x| x.name == s(0)).map(|x| x.manifest).unwrap_or(json!({}));
                if let Some(f) = man["accounts"]["fields"].as_array().and_then(|a| a.get(i)) {
                    vals[f["key"].as_str().unwrap_or("")] = json!(s(3).trim());
                }
                self.ext_wizard(&s(0), vals, i + 1);
            }
            "ext_acc_secret" => {
                // args: [ext, estado(json), nome do segredo, valor]
                let (n, vals) = (s(0), serde_json::from_str::<Value>(&s(1)).unwrap_or(json!({})));
                if !s(3).is_empty() { if let Err(e) = crate::secret::set(&s(2), s(3).trim()) { self.flash(&e); return true; } }
                self.ext_save_account(&n, vals);
            }
            "ext_acc_remove" => {
                let items = vec![Item::new("Cancelar", "Esc", "", vec![]), Item::new("Remover a conta", "", "ext_acc_remove_ok", vec![json!(s(0)), args.get(1).cloned().unwrap_or(json!(0))])];
                self.overlay = Some(Overlay::dialog(&format!("Remover a conta {} da extensao {}? O token fica no Gerenciador de Credenciais.", args.get(1).cloned().unwrap_or(json!(0)).as_u64().unwrap_or(0) + 1, s(0)), items));
            }
            "ext_acc_remove_ok" => {
                let n = s(0);
                let k = args.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let man = catalog().into_iter().find(|x| x.name == n).map(|x| x.manifest).unwrap_or(json!({}));
                let key = man["accounts"]["key"].as_str().unwrap_or("accounts").to_string();
                let mut list = self.ext_settings()[&n][&key].as_array().cloned().unwrap_or_default();
                if k < list.len() { list.remove(k); }
                self.ext_write(&n, &key, &lua_lit(&json!(list)));
            }
            "ext_secret" => {
                let mut o = Overlay::prompt(&format!("Token para {} (fica no Gerenciador de Credenciais)", s(1)), "", "ext_secret_value", vec![json!(s(0)), json!(s(1))]);
                o.secret = true;
                self.overlay = Some(o);
            }
            "ext_secret_value" => {
                if s(2).trim().is_empty() { return true; }
                match crate::secret::set(&s(1), s(2).trim()) {
                    Ok(_) => { let n = s(0); self.restart_one(&n); self.flash(&format!("Token gravado: {}", s(1))); }
                    Err(e) => self.flash(&e),
                }
            }
            _ => return false,
        }
        self.redraw();
        true
    }

    /// Assistente de conta: um prompt por campo do manifesto, depois o token (mascarado).
    /// Lista atual (config ou padrao do manifesto).
    fn ext_list_get(&self, name: &str, key: &str) -> Vec<String> {
        let cur = self.ext_settings()[name][key].clone();
        let v = if cur.is_null() {
            catalog().into_iter().find(|i| i.name == name)
                .and_then(|i| i.manifest["settings"].as_array().and_then(|a| a.iter().find(|x| x["key"] == key).map(|x| x["default"].clone())))
                .unwrap_or(Value::Null)
        } else { cur };
        match v { Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), Value::String(s) if !s.is_empty() => vec![s], _ => vec![] }
    }

    fn ext_list_menu(&mut self, name: &str, key: &str) {
        let list = self.ext_list_get(name, key);
        let mut items = vec![Item::new("Adicionar item", "", "ext_list_add", vec![json!(name), json!(key), json!(-1)])];
        for (i, v) in list.iter().enumerate() {
            items.push(Item::new(format!("Editar: {v}"), "", "ext_list_add", vec![json!(name), json!(key), json!(i)]));
            items.push(Item::new(format!("Remover: {v}"), "", "ext_list_del", vec![json!(name), json!(key), json!(i)]));
        }
        items.push(Item::new("Voltar", "", "ext_menu", vec![json!(name)]));
        self.overlay = Some(Overlay::list(&format!("{name}: {key} ({} itens)", list.len()), items));
    }

    fn ext_wizard(&mut self, name: &str, vals: Value, i: usize) {
        let man = catalog().into_iter().find(|x| x.name == name).map(|x| x.manifest).unwrap_or(json!({}));
        let acc = &man["accounts"];
        let fields = acc["fields"].as_array().cloned().unwrap_or_default();
        if let Some(f) = fields.get(i) {
            let edit = vals.get("__edit").is_some();
            let title = format!("{} {} ({}/{}): {}", if edit { "Editar conta" } else { "Nova conta" }, name, i + 1, fields.len(), f["title"].as_str().unwrap_or(f["key"].as_str().unwrap_or("")));
            let key = f["key"].as_str().unwrap_or("");
            let initial = vals[key].as_str().map(String::from).unwrap_or_else(|| f["default"].as_str().unwrap_or("").to_string());
            self.overlay = Some(Overlay::prompt(&title, &initial, "ext_acc_step", vec![json!(name), json!(vals.to_string()), json!(i)]));
            return;
        }
        match acc["secret"].as_str().filter(|s| !s.is_empty()) {
            Some(tpl) => {
                let sname = fill(tpl, &vals);
                let st = acc["secret_title"].as_str().unwrap_or("Token");
                let keep = if vals.get("__edit").is_some() { "vazio = manter o atual" } else { "vazio = depois" };
                let mut o = Overlay::prompt(&format!("{st} para {sname} ({keep})"), "", "ext_acc_secret", vec![json!(name), json!(vals.to_string()), json!(sname)]);
                o.secret = true;
                self.overlay = Some(o);
            }
            None => self.ext_save_account(name, vals),
        }
    }

    fn ext_save_account(&mut self, name: &str, vals: Value) {
        let man = catalog().into_iter().find(|x| x.name == name).map(|x| x.manifest).unwrap_or(json!({}));
        let key = man["accounts"]["key"].as_str().unwrap_or("accounts").to_string();
        let mut list = self.ext_settings()[name][&key].as_array().cloned().unwrap_or_default();
        let mut vals = vals;
        match vals.as_object_mut().and_then(|o| o.remove("__edit")).and_then(|v| v.as_u64()) {
            Some(k) if (k as usize) < list.len() => list[k as usize] = vals,
            _ => list.push(vals),
        }
        self.ext_write(name, &key, &lua_lit(&json!(list)));
    }
}

/// Painel lateral de uma visao de extensao.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_view(r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, v: &ExtView, rect: [f32; 4], theme: &Theme, mouse: (f32, f32), scroll: f32, siblings: &[(String, String)]) -> f32 {
    let [x0, y0, pw, ph] = rect;
    let ch = r.cell_h;
    l.shapes.push(Shape::rect(x0, y0, pw, ph, lin(theme.tab_bar, 1.0), 0.0));
    l.quads.push(Quad { x: x0, y: y0, w: 1.0, h: ph, color: lin(theme.border, 0.6) });
    let lx = x0 + 14.0;
    // Cabecalho: icone, titulo, atualizar, fechar.
    l.texts.push(TextItem { size: 1.0, ..mt(lx, y0 + 12.0, &icon(&v.icon), theme.accent) });
    l.texts.push(TextItem { bold: true, clip_right: Some(x0 + pw - 70.0), ..uit(lx + r.cell_w * 1.6, y0 + 12.0, &v.title, theme.fg, 1.0) });
    let btn = |l: &mut Layer, hits: &mut Vec<Hit>, x: f32, glyph: &str, act: Action| {
        let br = [x, y0 + 9.0, 24.0, 24.0];
        if inside(br, mouse) { l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), 5.0)); }
        l.texts.push(TextItem { size: 0.85, ..mt(x + 6.0, y0 + 9.0 + (24.0 - ch * 0.85) / 2.0, glyph, theme.muted) });
        hits.push(Hit { r: br, action: act });
    };
    btn(l, hits, x0 + pw - 62.0, "\u{f021}", Action::Builtin("ext_action".into(), vec![json!(v.ext), json!("refresh"), json!("")]));
    btn(l, hits, x0 + pw - 34.0, "\u{f00d}", Action::Builtin("ext_view".into(), vec![json!(v.key())]));
    // Outras visoes da mesma extensao: abas pequenas abaixo do titulo.
    let mut top = y0 + 44.0;
    if siblings.len() > 1 {
        let mut x = lx;
        for (key, title) in siblings {
            let short = title.rsplit(" · ").next().unwrap_or(title);
            let tw = r.measure(short, true, 0.78) + 16.0;
            let tr = [x, top - 4.0, tw, ch * 1.2];
            let cur = *key == v.key();
            let hv = inside(tr, mouse);
            l.shapes.push(Shape::rect(tr[0], tr[1], tr[2], tr[3], lin(if cur { theme.accent } else { mix(theme.tab_bar, theme.fg, if hv { 0.14 } else { 0.07 }) }, if cur { 0.3 } else { 1.0 }), 6.0));
            l.texts.push(uit(x + 8.0, tr[1] + (tr[3] - ch * 0.78) / 2.0, short, if cur { theme.fg } else { theme.muted }, 0.78));
            if !cur { hits.push(Hit { r: tr, action: Action::Builtin("ext_view".into(), vec![json!(key)]) }); }
            x += tw + 4.0;
        }
        top += ch * 1.6;
    }
    let mut y = top - scroll;
    let bottom = y0 + ph;
    if v.loading && v.sections.is_empty() { l.texts.push(uit(lx, y, "carregando…", theme.muted, 0.85)); }
    if !v.error.is_empty() { l.texts.push(TextItem { clip_right: Some(x0 + pw - 10.0), ..uit(lx, y, &v.error, theme.blocked, 0.8) }); y += ch * 1.2; }
    let visible = |y: f32, h: f32| y + h > top && y < bottom;
    for sec in &v.sections {
        if !sec.title.is_empty() {
            if visible(y, ch) { l.texts.push(TextItem { bold: true, ..uit(lx, y + 4.0, &sec.title, theme.muted, 0.78) }); }
            y += ch * 1.35;
        }
        if sec.items.is_empty() {
            if visible(y, ch) { l.texts.push(uit(lx + 8.0, y, "nada aqui", theme.muted, 0.78)); }
            y += ch * 1.2;
        }
        for it in &sec.items {
            let rh = if it.subtitle.is_empty() { ch * 1.35 } else { ch * 2.25 };
            if visible(y, rh) {
                let rr = [x0 + 6.0, y, pw - 12.0, rh];
                let hv = inside(rr, mouse) && mouse.1 >= top;
                if hv { l.shapes.push(Shape::rect(rr[0], rr[1].max(top), rr[2], rr[3] - (top - rr[1]).max(0.0), lin(mix(theme.tab_bar, theme.fg, 0.07), 1.0), 6.0)); }
                let c = if it.color.is_empty() { theme.muted } else { theme.named(&it.color) };
                if y >= top - 2.0 {
                    l.texts.push(TextItem { size: 0.9, ..mt(lx, y + 4.0, &icon(&it.icon), c) });
                    if it.unread { l.shapes.push(Shape::rect(x0 + 4.0, y + rh / 2.0 - 3.0, 6.0, 6.0, lin(theme.accent, 1.0), 3.0)); }
                    let mut right = x0 + pw - 14.0;
                    if !it.badge.is_empty() {
                        let bw = r.measure(&it.badge, true, 0.72) + 12.0;
                        right -= bw;
                        l.shapes.push(Shape::rect(right, y + 5.0, bw, ch * 0.95, lin(c, 0.20), ch * 0.47));
                        l.texts.push(uit(right + 6.0, y + 5.0 + (ch * 0.95 - ch * 0.72) / 2.0, &it.badge, c, 0.72));
                        right -= 6.0;
                    }
                    // Botoes das acoes no hover (fora a primaria, que e o clique na linha).
                    if hv {
                        for a in it.actions.iter().filter(|a| !a.primary).rev().take(3) {
                            let aw = r.measure(&a.title, true, 0.72) + 14.0;
                            right -= aw;
                            let ar = [right, y + rh - ch * 1.05 - 3.0, aw, ch * 1.05];
                            let ah = inside(ar, mouse);
                            l.shapes.push(Shape::rect(ar[0], ar[1], ar[2], ar[3], lin(if ah { theme.accent } else { mix(theme.tab_bar, theme.fg, 0.14) }, if ah { 0.45 } else { 1.0 }), 5.0));
                            l.texts.push(uit(right + 7.0, ar[1] + (ar[3] - ch * 0.72) / 2.0, &a.title, theme.fg, 0.72));
                            hits.push(Hit { r: ar, action: Action::Builtin("ext_action".into(), vec![json!(v.ext), json!(a.id), json!(it.id)]) });
                            right -= 4.0;
                        }
                    }
                    let tx = lx + r.cell_w * 1.8;
                    l.texts.push(TextItem { bold: it.unread, clip_right: Some(if it.badge.is_empty() { x0 + pw - 14.0 } else { right }), ..uit(tx, y + 4.0, &it.title, theme.fg, 0.86) });
                    if !it.subtitle.is_empty() {
                        l.texts.push(TextItem { clip_right: Some(if hv { right } else { x0 + pw - 14.0 }), ..uit(tx, y + 4.0 + ch * 1.0, &it.subtitle, theme.muted, 0.74) });
                    }
                }
                if let Some(a) = it.actions.iter().find(|a| a.primary).or(it.actions.first()) {
                    let clip = [rr[0], rr[1].max(top), rr[2], (rr[1] + rr[3] - rr[1].max(top)).max(0.0)];
                    hits.insert(0, Hit { r: clip, action: Action::Builtin("ext_action".into(), vec![json!(v.ext), json!(a.id), json!(it.id)]) });
                }
            }
            y += rh + 2.0;
        }
        y += 6.0;
    }
    // Altura total do conteudo (para limitar a rolagem).
    y + scroll - top
}
