//! Extensoes: programas separados (qualquer linguagem; o SDK oficial e em
//! TypeScript) que conversam com o Duck por JSON, uma mensagem por linha, no
//! stdin/stdout. Protocolo: docs/EXTENSIONS.md.
//!
//! Os processos sao do server, nao da janela: continuam rodando com a janela
//! fechada (agendamentos, polling, avisos). A janela so desenha: recebe as
//! mensagens pelo `subscribe` e manda as acoes pelo comando `ext-send`. Sem
//! janela aberta, o server mesmo cuida do que da (toast, abrir pane, segredo).
//!
//! Descoberta: ~/.duck/extensions/<nome>/duck-extension.json
//!   { "name": "github", "title": "GitHub", "icon": "github",
//!     "command": ["node", "dist/index.js"] }
//! Config opcional em ~/.duck.lua: extensions = { github = { enabled = true, ... } }
//! (a tabela inteira vai para a extensao no "init").

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

/// Extensao encontrada no disco (ativa ou nao).
#[derive(Clone)]
pub struct ExtInfo {
    pub name: String, pub title: String, pub description: String, pub version: String, pub category: String,
    pub dir: PathBuf, pub manifest: Value,
    /// ~/.duck/extensions (do usuario) ou da instalacao (de todos).
    pub personal: bool,
    /// O comando existe (ex.: dist/index.js compilado).
    pub built: bool,
}

pub fn extensions_dir() -> PathBuf { crate::config::home().join(".duck").join("extensions") }

/// Pastas de extensoes da instalacao: `<raiz>/extensions`, subindo a partir do executavel.
fn shared_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = crate::proto::current_exe() {
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

/// Nome curto no badge da barra (manifesto "short" ou "title").
pub fn label(i: &ExtInfo) -> String { i.manifest["short"].as_str().map(String::from).unwrap_or_else(|| i.title.clone()) }

fn is_node(i: &ExtInfo) -> bool { i.manifest["command"][0].as_str().is_some_and(|c| c == "node") }

/// sdk/host.js da instalacao (ou do ~/.duck/extensions/sdk).
fn host_js() -> Option<PathBuf> {
    shared_dirs().into_iter().chain([extensions_dir()]).map(|d| d.join("sdk").join("host.js")).find(|p| p.is_file())
}

/// Extensoes internas: badge na barra desenhado pelo proprio Duck (sem processo).
pub const BUILTIN: &[(&str, &str, &str, &str)] = &[
    ("agents", "Agentes", "Agentes do Claude/Codex: bloqueados, trabalhando e prontos; abre o painel do agente", "\u{f06a9}"),
    ("system", "Sistema", "CPU e RAM da maquina; abre o painel com nucleos, swap e processos", "\u{f4bc}"),
];

/// Todas as extensoes: as da instalacao e as pessoais (mesmo nome: a pessoal vence).
pub fn catalog() -> Vec<ExtInfo> {
    let mut out: Vec<ExtInfo> = Vec::new();
    let mut roots: Vec<(PathBuf, bool)> = shared_dirs().into_iter().map(|d| (d, false)).collect();
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
            category: "Duck".into(), dir: PathBuf::new(), manifest: json!({"builtin": true, "icon": icon}), personal: true, built: true });
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    out
}

/// Tabela `extensions` do ~/.duck.lua (+ ~/.duck-local.lua), como JSON.
pub fn settings() -> Value {
    use mlua::LuaSerdeExt;
    let (lua, t, _) = crate::config::load();
    t.get::<mlua::Value>("extensions").ok().and_then(|v| lua.from_value::<Value>(v).ok()).filter(|v| v.is_object()).unwrap_or(json!({}))
}

/// Ativa: a pessoal (~/.duck/extensions) vale ate ser desligada; a da
/// instalacao so roda depois de ligada no menu (nao gasta memoria a toa).
pub fn enabled(info: &ExtInfo, settings: &Value) -> bool {
    match settings[&info.name]["enabled"].as_bool() { Some(b) => b, None => info.personal }
}

fn strings(man: &Value, k: &str) -> Vec<String> {
    man[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect()
}

/// Recebe cada mensagem de uma extensao: (extensao, mensagem, geracao do processo).
pub type Sink = Arc<dyn Fn(String, Value, u64) + Send + Sync>;

struct Proc {
    name: String,
    stdin: Option<std::process::ChildStdin>,
    child: Option<std::process::Child>,
    /// Roda dentro do hospedeiro compartilhado (um Node para todas).
    hosted: bool,
    label: String,
    /// Segredos extras que o manifesto pede ("secrets": ["jira/trabalho"]); os "<nome>/..." sempre valem.
    secrets: Vec<String>,
    /// "context", "paste", "clipboard".
    permissions: Vec<String>,
    /// O "exited" de um processo antigo (reinicio) nao derruba o novo.
    gen: u64,
}

/// As extensoes que rodam no server e o que a janela precisa para desenhar.
#[derive(Default)]
pub struct Host {
    procs: Vec<Proc>,
    /// Hospedeiro das extensoes em Node (sdk/host.js).
    host: Option<(Option<std::process::ChildStdin>, std::process::Child)>,
    gen: u64,
    /// Ultimo register/view/status de cada extensao: a janela que conecta
    /// depois (ou reabre) recebe tudo de uma vez.
    sticky: Vec<(String, Value)>,
    /// Mensagens para as janelas: (seq, extensao, mensagem).
    pub out: VecDeque<(u64, String, Value)>,
    pub seq: u64,
    /// Pane ativo da janela, para as extensoes com permissao "context".
    context: Option<Value>,
}

impl Host {
    /// Sobe cada extensao ativa.
    pub fn start_all(&mut self, sink: &Sink) {
        let settings = settings();
        let mut hosted = Vec::new();
        for info in catalog() {
            if info.manifest["builtin"] == true || !(enabled(&info, &settings) && info.built) { continue; }
            if is_node(&info) && host_js().is_some() { hosted.push(info); } else { self.start_one(&info, &settings, sink); }
        }
        self.start_host(hosted, &settings, sink);
        self.announce();
    }

    /// Reinicia uma extensao (ou todas) com a config atual: ativar, desativar e mudar ajustes passam por aqui.
    pub fn restart(&mut self, name: Option<&str>, sink: &Sink) {
        let settings = settings();
        let Some(name) = name else {
            let names: Vec<String> = self.procs.iter().map(|p| p.name.clone()).collect();
            for n in &names { self.kill(n); }
            self.kill_host();
            return self.start_all(sink);
        };
        let info = catalog().into_iter().find(|i| i.name == name);
        // Internas (agentes, sistema) sao desenhadas pela janela: nao ha processo.
        if info.as_ref().is_some_and(|i| i.manifest["builtin"] == true) { return; }
        let in_host = info.as_ref().is_some_and(|i| is_node(i) && host_js().is_some());
        if in_host || self.procs.iter().any(|p| p.name == name && p.hosted) {
            if self.procs.iter().any(|p| p.name == name && !p.hosted) { self.kill(name); }
            self.restart_host(&settings, sink);
        } else {
            self.kill(name);
            if let Some(i) = info.filter(|i| enabled(i, &settings) && i.built) { self.start_one(&i, &settings, sink); }
        }
        self.announce();
    }

    fn kill(&mut self, name: &str) {
        if let Some(i) = self.procs.iter().position(|p| p.name == name) {
            let mut p = self.procs.remove(i);
            drop(p.stdin.take());
            if let Some(mut c) = p.child.take() { let _ = c.kill(); }
        }
        self.forget(name);
    }

    fn kill_host(&mut self) {
        let hosted: Vec<String> = self.procs.iter().filter(|p| p.hosted).map(|p| p.name.clone()).collect();
        for n in &hosted { self.forget(n); }
        self.procs.retain(|p| !p.hosted);
        if let Some((stdin, mut child)) = self.host.take() { drop(stdin); let _ = child.kill(); }
    }

    fn restart_host(&mut self, settings: &Value, sink: &Sink) {
        self.kill_host();
        let list: Vec<ExtInfo> = catalog().into_iter().filter(|i| i.manifest["builtin"] != true && is_node(i) && i.built && enabled(i, settings)).collect();
        self.start_host(list, settings, sink);
    }

    /// Tira das janelas tudo o que a extensao pos na tela.
    fn forget(&mut self, name: &str) {
        self.sticky.retain(|(e, _)| e != name);
        self.push(name, json!({"type": "removed"}));
    }

    /// Lista das extensoes rodando, para a janela (menu, permissoes, badge).
    fn announce(&mut self) {
        let msg = self.running();
        self.push("", msg);
    }

    fn running(&self) -> Value {
        json!({"type": "running", "list": self.procs.iter().map(|p| json!({"name": p.name, "label": p.label, "permissions": p.permissions})).collect::<Vec<_>>()})
    }

    fn reader(&self, stdout: std::process::ChildStdout, members: Vec<String>, hosted: bool, gen: u64, sink: &Sink) {
        let sink = sink.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let name = if hosted { v["ext"].as_str().map(String::from) } else { members.first().cloned() };
                if let Some(n) = name { sink(n, v, gen); }
            }
            for n in members { sink(n, json!({"type": "exited"}), gen); }
        });
    }

    /// Um Node so para todas as extensoes Node ativas.
    fn start_host(&mut self, list: Vec<ExtInfo>, settings: &Value, sink: &Sink) {
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
            Err(err) => { crate::proto::log(&format!("extensoes: o Node nao abriu ({err})")); return; }
        };
        self.gen += 1;
        let gen = self.gen;
        self.reader(child.stdout.take().unwrap(), list.iter().map(|i| i.name.clone()).collect(), true, gen, sink);
        self.host = Some((child.stdin.take(), child));
        for info in &list {
            let man = &info.manifest;
            self.procs.push(Proc { name: info.name.clone(), stdin: None, child: None, hosted: true, label: label(info), secrets: strings(man, "secrets"), permissions: strings(man, "permissions"), gen });
            self.init(&info.name, settings);
        }
    }

    fn start_one(&mut self, info: &ExtInfo, settings: &Value, sink: &Sink) {
        let name = info.name.clone();
        if self.procs.iter().any(|p| p.name == name) { return; }
        let man = &info.manifest;
        let cmd = strings(man, "command");
        let Some(prog) = cmd.first() else { return };
        let log = std::fs::File::create(crate::proto::data_dir().join(format!("ext-{name}.log"))).ok();
        let mut c = std::process::Command::new(prog);
        c.args(&cmd[1..]).current_dir(&info.dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(log.map(std::process::Stdio::from).unwrap_or_else(std::process::Stdio::null));
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(err) => { crate::proto::log(&format!("extensao {name}: nao abriu ({prog}: {err})")); return; }
        };
        self.gen += 1;
        let gen = self.gen;
        self.reader(child.stdout.take().unwrap(), vec![name.clone()], false, gen, sink);
        self.procs.push(Proc { name: name.clone(), stdin: child.stdin.take(), child: Some(child), hosted: false, label: label(info), secrets: strings(man, "secrets"), permissions: strings(man, "permissions"), gen });
        self.init(&name, settings);
    }

    fn init(&mut self, name: &str, settings: &Value) {
        self.send(name, json!({"type": "init", "settings": settings[name], "duck": {"version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS}}));
        if let Some(c) = self.context.clone() { self.send_perm(name, "context", c); }
    }

    /// Manda uma mensagem para a extensao (linha JSON).
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

    /// Manda so se a extensao tem a permissao; ext "*" = todas que tem.
    pub fn send_perm(&mut self, ext: &str, perm: &str, msg: Value) {
        if perm == "context" && ext == "*" { self.context = Some(msg.clone()); }
        let names: Vec<String> = self.procs.iter().filter(|p| (ext == "*" || p.name == ext) && p.permissions.iter().any(|x| x == perm)).map(|p| p.name.clone()).collect();
        for n in names { self.send(&n, msg.clone()); }
    }

    /// A ultima janela fechou: nenhuma visao esta aberta (a extensao para o polling de visao).
    pub fn views_closed(&mut self) {
        let views: Vec<(String, Value)> = self.sticky.iter().filter(|(_, m)| m["type"] == "view").map(|(e, m)| (e.clone(), m["id"].clone())).collect();
        for (e, id) in views { self.send(&e, json!({"type": "view-close", "view": id})); }
    }

    pub fn has_perm(&self, ext: &str, perm: &str) -> bool {
        self.procs.iter().any(|p| p.name == ext && p.permissions.iter().any(|x| x == perm))
    }

    /// Mensagem para as janelas.
    pub fn push(&mut self, ext: &str, msg: Value) {
        self.seq += 1;
        self.out.push_back((self.seq, ext.to_string(), msg));
        while self.out.len() > 512 { self.out.pop_front(); }
    }

    /// O que uma janela recem-conectada recebe antes das mensagens novas.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        let mut v = vec![(String::new(), json!({"type": "reset"})), (String::new(), self.running())];
        v.extend(self.sticky.iter().cloned());
        v
    }

    /// Mensagem que chegou de uma extensao. Devolve o que a janela deve
    /// receber (None = o server ja resolveu ou e de um processo antigo).
    pub fn incoming(&mut self, ext: &str, m: Value, gen: u64) -> Option<Value> {
        if !self.procs.iter().any(|p| p.name == ext && p.gen == gen) { return None; }
        match m["type"].as_str().unwrap_or("") {
            "secret" => {
                // So os segredos da propria extensao (ou os que o manifesto declara).
                let name = m["name"].as_str().unwrap_or("").to_string();
                let allowed = name.starts_with(&format!("{ext}/")) || self.procs.iter().any(|p| p.name == ext && p.secrets.contains(&name));
                let value = if allowed { crate::secret::get(&name) } else { None };
                if !allowed { crate::proto::log(&format!("extensao {ext} pediu o segredo {name} sem permissao")); }
                self.send(ext, json!({"type": "secret", "id": m["id"], "name": name, "value": value}));
                return None;
            }
            "register" => self.sticky.push((ext.to_string(), m.clone())),
            "view" => {
                let id = m["id"].clone();
                match self.sticky.iter_mut().find(|(e, x)| e == ext && x["type"] == "view" && x["id"] == id) {
                    Some((_, old)) => {
                        let sections = if m.get("sections").is_none() { old.get("sections").cloned() } else { None };
                        *old = m.clone();
                        if let Some(s) = sections { old["sections"] = s; }
                    }
                    None => self.sticky.push((ext.to_string(), m.clone())),
                }
            }
            "status" => {
                let id = m["id"].clone();
                self.sticky.retain(|(e, x)| !(e == ext && x["type"] == "status" && x["id"] == id));
                if m["text"].as_str().is_some_and(|t| !t.is_empty()) { self.sticky.push((ext.to_string(), m.clone())); }
            }
            "exited" => {
                crate::proto::log(&format!("extensao {ext} encerrou"));
                self.procs.retain(|p| p.name != ext);
                self.sticky.retain(|(e, x)| !(e == ext && x["type"] == "status"));
                for (_, x) in self.sticky.iter_mut().filter(|(e, x)| e == ext && x["type"] == "view") {
                    x["loading"] = json!(false);
                    x["error"] = json!("a extensao encerrou (veja ext-<nome>.log)");
                }
                self.announce();
            }
            _ => {}
        }
        Some(m)
    }
}
