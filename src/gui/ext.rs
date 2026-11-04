//! Extensoes na janela: so o desenho. Os processos rodam no server
//! (src/ext.rs) e continuam com a janela fechada; as mensagens chegam pelo
//! `subscribe` ({"t":"ext"}) e as acoes voltam pelo comando `ext-send`.
//! Protocolo: docs/EXTENSIONS.md.

use super::*;
use super::ui::{inside, mt, uit};
use crate::render::Shape;
pub(super) use crate::ext::{catalog, ExtInfo};

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

/// Extensao rodando no server (a lista vem dele na mensagem "running").
#[derive(Deserialize, Clone, Default)]
struct Running {
    name: String,
    #[serde(default)] label: String,
    #[serde(default)] permissions: Vec<String>,
}

/// Texto de uma linha so: mensagens com quebra de linha (Rollbar) nao invadem a linha de baixo.
fn one_line<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Ok(s.split_whitespace().collect::<Vec<_>>().join(" "))
}

#[derive(Default)]
pub(super) struct Exts {
    running: Vec<Running>,
    pub views: Vec<ExtView>,
    pub status: Vec<ExtStatus>,
    /// Comandos da paleta: (extensao, id, titulo).
    pub commands: Vec<(String, String, String)>,
    /// Mensagens que chegaram do server: (extensao, mensagem).
    pub inbox: Arc<Mutex<Vec<(String, Value)>>>,
    /// Conexao com o server (a mesma da janela).
    pub conn: Arc<Mutex<Option<Conn>>>,
}

impl Exts {
    pub fn view(&self, key: &str) -> Option<&ExtView> { self.views.iter().find(|v| v.key() == key) }

    /// Nome curto da extensao para a barra.
    pub fn label(&self, ext: &str) -> String { self.running.iter().find(|p| p.name == ext).map(|p| p.label.clone()).unwrap_or_else(|| ext.to_string()) }

    fn has_perm(&self, ext: &str, perm: &str) -> bool { self.running.iter().any(|p| p.name == ext && p.permissions.iter().any(|x| x == perm)) }

    fn is_running(&self, ext: &str) -> bool { self.running.iter().any(|p| p.name == ext) }

    fn call(&self, req: Value) {
        if let Err(e) = with_conn(&self.conn, |c| c.call(req.clone())) { gui_log(&format!("extensoes: {e}")); }
    }

    /// Manda uma mensagem para a extensao (pelo server).
    pub fn send(&mut self, ext: &str, msg: Value) { self.call(json!({"cmd": "ext-send", "ext": ext, "msg": msg})); }

    /// Manda so para as extensoes com a permissao (o server confere); ext "*" = todas.
    pub fn send_perm(&mut self, ext: &str, perm: &str, msg: Value) { self.call(json!({"cmd": "ext-send", "ext": ext, "perm": perm, "msg": msg})); }

    /// Reinicia no server com a config atual (ativar/desativar/ajuste); "" = todas.
    pub fn restart(&mut self, ext: &str) { self.call(json!({"cmd": "ext-restart", "ext": ext})); }
}

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

    pub(super) fn ext_enabled(&self, info: &ExtInfo, settings: &Value) -> bool { crate::ext::enabled(info, settings) }

    fn clear_ext(&mut self, name: &str) {
        self.exts.views.retain(|v| v.ext != name);
        self.exts.status.retain(|s| s.ext != name);
        self.exts.commands.retain(|c| c.0 != name);
        if self.ext_panel.as_deref().is_some_and(|k| k.starts_with(&format!("{name}/"))) { self.ext_panel = None; self.send_size(); }
    }

    /// Para, sobe ou reinicia no server conforme a config (ja gravada).
    pub(super) fn restart_one(&mut self, name: &str) {
        self.load_config();
        self.exts.restart(name);
    }

    /// Pane ativo mudou: avisa as extensoes com permissao "context".
    pub(super) fn ext_context_tick(&mut self) {
        let Some(p) = self.active() else { return };
        let ctx = json!({"type": "context", "cwd": p.cwd, "pane": p.id, "domain": p.domain, "title": p.title});
        if self.ext_context.as_ref() == Some(&ctx) { return; }
        self.ext_context = Some(ctx.clone());
        self.exts.send_perm("*", "context", ctx);
    }

    /// Texto copiado no Duck: para as extensoes com permissao "clipboard".
    pub(super) fn ext_clipboard(&mut self, text: &str) {
        self.exts.send_perm("*", "clipboard", json!({"type": "clipboard", "text": text}));
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
                if !self.exts.has_perm(ext, "paste") {
                    gui_log(&format!("extensao {ext} tentou colar sem a permissao paste"));
                    return;
                }
                self.paste(s("text"), false);
                if m["enter"] == true && self.overlay.is_none() { self.send_keys("\r"); }
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
                self.exts.status.retain(|x| x.ext != ext);
                for v in self.exts.views.iter_mut().filter(|v| v.ext == ext) { v.loading = false; v.error = "a extensao encerrou (veja ext-<nome>.log)".into(); }
            }
            // Do server: janela (re)conectada, extensoes rodando, extensao parada.
            "reset" => {
                self.exts.views.clear();
                self.exts.status.clear();
                self.exts.commands.clear();
                if self.ext_panel.take().is_some() { self.send_size(); }
            }
            "running" => self.exts.running = serde_json::from_value(m["list"].clone()).unwrap_or_default(),
            "removed" => self.clear_ext(ext),
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
        let running: Vec<String> = self.exts.running.iter().map(|p| p.name.clone()).collect();
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
        let running = self.exts.is_running(name);
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
                    Ok(_) => { self.restart_one(&n); self.flash(&format!("{n} {}", if on { "ativada" } else { "desativada" })); }
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
