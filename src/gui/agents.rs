//! Painel dos agentes (Ctrl+Shift+I, badge "Agentes"): todos os agentes do
//! workspace (ou de todos), agrupados pela conta do Claude (perfil do
//! CLAUDE_CONFIG_DIR) e ordenados por quem precisa de voce: pede, terminou
//! (nao visto), trabalhando, ocioso. Cada linha tem glifo com forma propria
//! (nao so cor), aba, passo do plano ou o pedido, tempo no estado, contexto e
//! fila de recados. Embaixo, o "why" e a fila do selecionado.
//!
//! Com o painel em foco (abrir, clicar nele): Enter vai para o pane, y/n
//! aprova/nega, m manda recado, b/w/d/i/a filtram, / busca, t alterna
//! workspace atual/todos, Tab entra na fila (x cancela, u sobe, e entrega
//! agora), c recolhe em bolinhas, Esc devolve o foco ao terminal.

use super::*;
use super::ui::{inside, mt, uit, Hit};
use crate::render::Shape;

#[derive(Clone, Default, Debug)]
pub(super) struct AgentRow {
    pub pane: u64, pub ws: String, pub tab: String, pub profile: String, pub name: String,
    pub state: String, pub since: u64,
    pub step: Option<String>, pub msg: Option<String>, pub prompt: Option<String>,
    pub todo: Option<(u64, u64)>, pub tokens: Option<u64>, pub session_tokens: Option<u64>,
    pub queue: Vec<String>, pub why: Vec<String>,
    pub config_dir: String, pub session: Option<String>,
}

impl AgentRow {
    pub fn from_json(v: &Value) -> AgentRow {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let o = |k: &str| v[k].as_str().map(String::from).filter(|x| !x.is_empty());
        let list = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
        AgentRow {
            pane: v["pane"].as_u64().unwrap_or(0), ws: s("workspace"), tab: s("tab"), profile: s("profile"), name: s("name"),
            state: s("state"), since: v["since"].as_u64().unwrap_or(0),
            step: o("step"), msg: o("msg"), prompt: o("prompt"),
            todo: v["todo"][0].as_u64().zip(v["todo"][1].as_u64()),
            tokens: v["tokens"].as_u64(), session_tokens: v["session_tokens"].as_u64(),
            queue: list("queue"), why: list("why"),
            config_dir: s("config_dir"), session: o("session"),
        }
    }
}

/// Ordem dentro do grupo: quem pede voce primeiro.
pub(super) fn rank(state: &str) -> u8 {
    match state { "blocked" => 0, "done" => 1, "working" => 2, "idle" => 3, _ => 4 }
}

/// Filtro por tecla: b pede, w trabalhando, d terminou, i ocioso, a todos.
fn filter_state(f: char) -> Option<&'static str> {
    match f { 'b' => Some("blocked"), 'w' => Some("working"), 'd' => Some("done"), 'i' => Some("idle"), _ => None }
}

pub(super) fn row_matches(r: &AgentRow, ws: Option<&str>, filter: char, query: &str) -> bool {
    if ws.is_some_and(|w| r.ws != w) { return false; }
    if filter_state(filter).is_some_and(|s| r.state != s) { return false; }
    if query.is_empty() { return true; }
    let hay = fold_str(&[&r.tab, &r.name, &r.profile, &r.ws, r.step.as_deref().unwrap_or(""), r.msg.as_deref().unwrap_or(""), r.prompt.as_deref().unwrap_or("")].join(" "));
    fold_str(query).split_whitespace().all(|w| hay.contains(w))
}

/// Grupos por conta: (perfil, indices em `rows` na ordem de exibicao). O
/// grupo com o agente mais urgente vem primeiro; empate pelo nome.
pub(super) fn group_rows(rows: &[AgentRow], ws: Option<&str>, filter: char, query: &str) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, r) in rows.iter().enumerate().filter(|(_, r)| row_matches(r, ws, filter, query)) {
        match groups.iter_mut().find(|g| g.0 == r.profile) {
            Some(g) => g.1.push(i),
            None => groups.push((r.profile.clone(), vec![i])),
        }
    }
    // Mesmo estado: quem esta nele ha mais tempo primeiro (esperando voce ha mais tempo).
    for g in &mut groups { g.1.sort_by_key(|&i| (rank(&rows[i].state), rows[i].since, rows[i].pane)); }
    groups.sort_by(|a, b| {
        let best = |g: &(String, Vec<usize>)| g.1.first().map(|&i| rank(&rows[i].state)).unwrap_or(9);
        best(a).cmp(&best(b)).then_with(|| a.0.cmp(&b.0))
    });
    groups
}

/// Glifo com forma propria por estado (daltonico tambem distingue).
pub(super) fn glyph(theme: &Theme, state: &str) -> (&'static str, Rgb) {
    match state {
        "blocked" => ("▲", theme.working),
        "done" => ("✓", theme.done),
        "working" => ("◉", theme.accent),
        "idle" => ("○", theme.idle),
        _ => ("·", theme.muted),
    }
}

fn age(s: u64) -> String {
    if s < 60 { format!("{s}s") } else if s < 3600 { format!("{}m", s / 60) } else if s < 86400 { format!("{}h", s / 3600) } else { format!("{}d", s / 86400) }
}

/// Fracao do contexto usada: janela de 200k (1M quando ja passou disso).
fn ctx_frac(t: u64) -> f32 { (t as f32 / if t > 200_000 { 1_000_000.0 } else { 200_000.0 }).min(1.0) }

/// Segunda linha da linha do agente: o pedido, o passo do plano ou ha quanto parou.
fn row_text(r: &AgentRow, secs: u64) -> String {
    let plan = r.todo.filter(|t| t.1 > 0).map(|(d, t)| format!("{d}/{t}"));
    match r.state.as_str() {
        "blocked" => format!("pede: {}", r.msg.clone().unwrap_or_else(|| "aprovacao".into())),
        "done" => {
            let mut s = format!("terminou ha {}", age(secs));
            if let Some(p) = plan { s += &format!(" · {p}"); }
            s
        }
        "working" => {
            let what = r.step.clone().or_else(|| r.msg.clone()).or_else(|| r.prompt.clone()).unwrap_or_else(|| "trabalhando".into());
            match plan { Some(p) => format!("{p} {what}"), None => what }
        }
        _ => match &r.msg { Some(m) => format!("ocioso ha {} · {m}", age(secs)), None => format!("ocioso ha {}", age(secs)) },
    }
}

pub(super) struct AgentPanel {
    pub rows: Vec<AgentRow>,
    /// Relogio do server na ultima leitura (as idades saem dele).
    pub now: u64,
    pub fetched: Instant,
    /// Pane selecionado (a selecao segue o agente quando a ordem muda).
    pub pane: u64,
    pub filter: char,
    pub query: String,
    pub searching: bool,
    pub all_ws: bool,
    pub collapsed: bool,
    /// Teclas vao para o painel (e nao para o terminal).
    pub focused: bool,
    pub folded: HashSet<String>,
    /// Navegando na fila do selecionado (Tab): recado escolhido.
    pub queue_sel: Option<usize>,
    pub scroll: usize,
    pub ws: String,
    /// Detalhe do selecionado: plano do transcript e arquivos mexidos (git).
    pub plan: Vec<(String, String)>,
    pub files: Vec<(String, String, i64, i64)>,
    pub detail_pane: u64,
    pub detail_at: Instant,
}

impl AgentPanel {
    pub fn new() -> AgentPanel {
        let old = Instant::now() - Duration::from_secs(60);
        AgentPanel {
            rows: vec![], now: 0, fetched: old, pane: 0, filter: 'a', query: String::new(), searching: false, all_ws: false,
            collapsed: false, focused: true, folded: HashSet::new(), queue_sel: None, scroll: 0, ws: String::new(),
            plan: vec![], files: vec![], detail_pane: 0, detail_at: old,
        }
    }
    pub fn groups(&self) -> Vec<(String, Vec<usize>)> {
        group_rows(&self.rows, (!self.all_ws).then_some(self.ws.as_str()), self.filter, &self.query)
    }
    /// Linhas navegaveis (grupos recolhidos ficam de fora), em ordem.
    pub fn visible(&self) -> Vec<usize> {
        self.groups().into_iter().filter(|g| !self.folded.contains(&g.0)).flat_map(|g| g.1).collect()
    }
    pub fn selected(&self) -> Option<&AgentRow> { self.rows.iter().find(|r| r.pane == self.pane) }
    /// Selecionado sumiu do filtro: vai para o primeiro visivel.
    pub fn fix_sel(&mut self) {
        let vis = self.visible();
        if !vis.iter().any(|&i| self.rows[i].pane == self.pane) { self.pane = vis.first().map(|&i| self.rows[i].pane).unwrap_or(0); }
        let n = self.selected().map(|r| r.queue.len()).unwrap_or(0);
        if n == 0 { self.queue_sel = None; } else if let Some(q) = self.queue_sel.as_mut() { *q = (*q).min(n - 1); }
    }
    fn step(&mut self, d: i64) {
        if let Some(q) = self.queue_sel {
            let n = self.selected().map(|r| r.queue.len()).unwrap_or(0) as i64;
            if n > 0 { self.queue_sel = Some((q as i64 + d).clamp(0, n - 1) as usize); }
            return;
        }
        let vis = self.visible();
        if vis.is_empty() { return; }
        let cur = vis.iter().position(|&i| self.rows[i].pane == self.pane).unwrap_or(0) as i64;
        let k = (cur + d).rem_euclid(vis.len() as i64) as usize;
        self.pane = self.rows[vis[k]].pane;
    }
}

impl Gui {
    pub(super) fn agents_reload(&mut self) {
        if let Some(a) = self.agent_panel.as_mut() { a.fetched = Instant::now() - Duration::from_secs(60); }
        self.refresh_agent_panel(false);
    }

    /// Teclas com o painel em foco. false = a tecla segue o caminho normal
    /// (atalhos com Ctrl/Alt continuam valendo).
    pub(super) fn agents_key(&mut self, ev: &KeyEvent) -> bool {
        // "/" como digitado (no ABNT2 ele pode vir com AltGr = Ctrl+Alt).
        let slash = ev.text.as_deref() == Some("/");
        if !slash && (self.mods.control_key() || self.mods.alt_key() || self.mods.super_key()) { return false; }
        let name = if slash { "/".to_string() } else { key_name(ev) };
        let Some(a) = self.agent_panel.as_mut() else { return false };
        if a.searching {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => { a.searching = false; a.query.clear(); }
                Key::Named(NamedKey::Enter) => a.searching = false,
                Key::Named(NamedKey::Backspace) => { a.query.pop(); }
                Key::Named(NamedKey::ArrowDown) => a.step(1),
                Key::Named(NamedKey::ArrowUp) => a.step(-1),
                _ => if let Some(t) = &ev.text { if t.chars().all(|c| !c.is_control()) { a.query.push_str(t); } },
            }
            a.fix_sel();
            self.redraw();
            return true;
        }
        let pane = a.pane;
        let row = a.selected().cloned();
        let in_queue = a.queue_sel;
        match name.as_str() {
            "escape" => {
                if a.queue_sel.take().is_none() {
                    if !a.query.is_empty() { a.query.clear(); } else { a.focused = false; }
                }
            }
            "arrowdown" | "j" => a.step(1),
            "arrowup" | "k" => a.step(-1),
            "/" => { a.searching = true; a.queue_sel = None; }
            "t" => { a.all_ws = !a.all_ws; a.scroll = 0; }
            "b" | "w" | "d" | "i" | "a" => {
                let f = name.chars().next().unwrap_or('a');
                a.filter = if a.filter == f { 'a' } else { f };
                a.scroll = 0;
            }
            "tab" => { a.queue_sel = if a.queue_sel.is_some() || row.as_ref().is_none_or(|r| r.queue.is_empty()) { None } else { Some(0) }; }
            "c" => { a.collapsed = !a.collapsed; self.send_size(); }
            "x" | "delete" | "u" | "e" if in_queue.is_some() => {
                let op = match name.as_str() { "u" => "up", "e" => "now", _ => "rm" };
                self.agents_builtin("agents_queue", &[json!(pane), json!(op), json!(in_queue.unwrap_or(0))]);
                return true;
            }
            "enter" if in_queue.is_some() => { self.agents_builtin("agents_queue", &[json!(pane), json!("now"), json!(in_queue.unwrap_or(0))]); return true; }
            "enter" => { if pane != 0 { self.agents_builtin("agents_go", &[json!(pane)]); } return true; }
            "y" | "n" => { if pane != 0 { self.agents_builtin("agents_approve", &[json!(pane), json!(name == "y")]); } return true; }
            "m" => { if pane != 0 { self.builtin("queue_message", &[json!(pane)]); } return true; }
            _ => {}
        }
        if let Some(a) = self.agent_panel.as_mut() { a.fix_sel(); }
        self.redraw();
        true
    }

    /// Acoes do painel (teclas e cliques).
    pub(super) fn agents_builtin(&mut self, name: &str, args: &[Value]) {
        let n = |i: usize| args.get(i).and_then(|v| v.as_u64()).unwrap_or(0);
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "agents_sel" => if let Some(a) = self.agent_panel.as_mut() { a.pane = n(0); a.queue_sel = None; a.focused = true; },
            "agents_go" => {
                // Clique na linha ja selecionada (ou Enter) vai para o pane; o foco volta ao terminal.
                self.cmd(json!({"cmd": "focus", "pane": n(0)}));
                if let Some(a) = self.agent_panel.as_mut() { a.pane = n(0); a.focused = false; }
            }
            "agents_pick" => {
                let again = self.agent_panel.as_ref().is_some_and(|a| a.pane == n(0) && a.focused);
                if again { self.agents_builtin("agents_go", args); } else { self.agents_builtin("agents_sel", args); }
            }
            "agents_approve" => {
                let ok = args.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
                let blocked = self.agent_panel.as_ref().and_then(|a| a.rows.iter().find(|r| r.pane == n(0))).is_some_and(|r| r.state == "blocked");
                if !blocked { return self.flash("Este agente nao esta pedindo nada"); }
                // O mesmo dos toasts: "1" escolhe Yes no menu do Claude Code, Esc nega.
                self.cmd(json!({"cmd": "send", "pane": n(0), "text": if ok { "1" } else { "\x1b" }}));
                self.flash(if ok { "Aprovado no agente" } else { "Negado no agente" });
                self.agents_reload();
            }
            "agents_queue" => {
                let op = s(1);
                match self.call(json!({"cmd": format!("queue-{op}"), "pane": n(0), "index": n(2)})) {
                    Ok(_) => self.flash(match op.as_str() { "now" => "Recado entregue agora", "up" => "Recado subiu na fila", _ => "Recado cancelado" }),
                    Err(e) => self.flash(&e),
                }
                self.agents_reload();
            }
            "agents_filter" => if let Some(a) = self.agent_panel.as_mut() {
                let f = s(0).chars().next().unwrap_or('a');
                a.filter = if a.filter == f { 'a' } else { f };
                a.focused = true;
                a.fix_sel();
            },
            "agents_scope" => if let Some(a) = self.agent_panel.as_mut() { a.all_ws = !a.all_ws; a.scroll = 0; a.fix_sel(); },
            "agents_fold" => if let Some(a) = self.agent_panel.as_mut() {
                let g = s(0);
                if !a.folded.remove(&g) { a.folded.insert(g); }
                a.fix_sel();
            },
            "agents_collapse" => {
                if let Some(a) = self.agent_panel.as_mut() { a.collapsed = !a.collapsed; }
                self.send_size();
            }
            "agents_search" => if let Some(a) = self.agent_panel.as_mut() { a.searching = true; a.focused = true; },
            _ => {}
        }
        self.redraw();
    }
}

/// Uso do Claude por perfil ("5h: 62%  7d: 31%"), da visao da extensao
/// claude-usage (itens "perfil|5h" com o % no badge). Sem a extensao, vazio.
pub(super) fn usage_by_profile(exts: &super::ext::Exts) -> HashMap<String, String> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for it in exts.view("claude-usage/usage").into_iter().flat_map(|v| v.sections.iter()).flat_map(|s| s.items.iter()) {
        let Some((prof, kind)) = it.id.rsplit_once('|') else { continue };
        if (kind == "5h" || kind == "7d") && !it.badge.is_empty() { out.entry(prof.to_string()).or_default().push(format!("{kind}: {}", it.badge)); }
    }
    out.into_iter().map(|(k, v)| (k, v.join("  "))).collect()
}

/// Desenha o painel em `rect`. true = tem animacao (agente trabalhando).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_agent_panel(ap: &mut AgentPanel, r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, theme: &Theme, ui: &config::Ui,
    usage: &HashMap<String, String>, rect: [f32; 4], mouse: (f32, f32), alpha: f32, t_now: f32) -> bool {
    let [x0, y0, pw, ph] = rect;
    let ch = r.cell_h;
    let mut pulse = false;
    l.shapes.push(Shape::rect(x0, y0, pw, ph, lin(theme.tab_bar, alpha), 0.0));
    l.quads.push(Quad { x: x0, y: y0, w: 1.0, h: ph, color: lin(if ap.focused { theme.accent } else { theme.border }, if ap.focused { 0.9 } else { 0.6 }) });
    let hover = |rr: [f32; 4]| inside(rr, mouse);
    let bg_hover = lin(mix(theme.tab_bar, theme.fg, 0.08), 1.0);
    hits.push(Hit { r: rect, action: Action::Builtin("agents_sel".into(), vec![json!(ap.pane)]) });
    let groups = ap.groups();
    let secs = |r: &AgentRow| ap.now.saturating_sub(r.since);
    // Recolhido: uma coluna de bolinhas com a cor (e a forma) do estado.
    if ap.collapsed {
        let br = [x0 + 4.0, y0 + 6.0, pw - 8.0, 22.0];
        l.texts.push(uit(x0 + (pw - r.measure("«", true, 0.9)) / 2.0, br[1] + 2.0, "«", if hover(br) { theme.fg } else { theme.muted }, 0.9));
        hits.push(Hit { r: br, action: Action::Builtin("agents_collapse".into(), vec![]) });
        let mut y = y0 + 34.0;
        for &i in groups.iter().flat_map(|g| g.1.iter()) {
            if y > y0 + ph - 20.0 { break; }
            let row = &ap.rows[i];
            let (g, c) = glyph(theme, &row.state);
            let a = if row.state == "working" { pulse = true; 0.45 + 0.55 * (t_now * 3.2).sin().abs() } else { 1.0 };
            let rr = [x0 + 3.0, y - 2.0, pw - 6.0, 20.0];
            if row.pane == ap.pane { l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(theme.accent, 0.2), 5.0)); }
            let gw = r.measure(g, false, 0.85);
            l.texts.push(TextItem { alpha: a, size: 0.85, ..mt(x0 + (pw - gw) / 2.0, y, g, c) });
            hits.push(Hit { r: rr, action: Action::Builtin("agents_go".into(), vec![json!(row.pane)]) });
            y += 22.0;
        }
        return pulse;
    }
    let (lx, rx) = (x0 + 14.0, x0 + pw - 12.0);
    let mut y = y0 + 12.0;
    // Cabecalho: titulo, escopo (workspace atual / todos), recolher e fechar.
    let xr = [rx - 18.0, y - 2.0, 20.0, 20.0];
    l.texts.push(uit(xr[0] + 4.0, y, "✕", if hover(xr) { theme.fg } else { theme.muted }, 0.8));
    hits.push(Hit { r: xr, action: Action::Builtin("agent_panel".into(), vec![json!("close")]) });
    let cr = [xr[0] - 22.0, y - 2.0, 20.0, 20.0];
    l.texts.push(uit(cr[0] + 5.0, y - 1.0, "»", if hover(cr) { theme.fg } else { theme.muted }, 0.9));
    hits.push(Hit { r: cr, action: Action::Builtin("agents_collapse".into(), vec![]) });
    let scope = if ap.all_ws { "todos ▾" } else { "ws atual ▾" };
    let sw = r.measure(scope, true, 0.75) + 14.0;
    let sr = [cr[0] - sw - 6.0, y - 3.0, sw, ch * 0.75 + 8.0];
    l.shapes.push(Shape::rect(sr[0], sr[1], sr[2], sr[3], lin(mix(theme.tab_bar, theme.fg, if hover(sr) { 0.16 } else { 0.08 }), 1.0), 6.0));
    l.texts.push(uit(sr[0] + 7.0, y, scope, theme.fg, 0.75));
    hits.push(Hit { r: sr, action: Action::Builtin("agents_scope".into(), vec![]) });
    let title = if ap.all_ws { "AGENTES · todos os workspaces".to_string() } else { format!("AGENTES · workspace \"{}\"", ap.ws) };
    l.texts.push(TextItem { bold: true, clip_right: Some(sr[0] - 6.0), ..uit(lx, y, &title, theme.fg, 0.85) });
    y += ch * 1.35;
    // Contadores por estado (clique filtra); o filtro ativo fica sublinhado.
    let scoped: Vec<&AgentRow> = ap.rows.iter().filter(|r| ap.all_ws || r.ws == ap.ws).collect();
    let mut x = lx;
    for (st, label, f) in [("blocked", "pede", 'b'), ("working", "trabalhando", 'w'), ("done", "terminou", 'd'), ("idle", "ociosos", 'i')] {
        let n = scoped.iter().filter(|r| r.state == st).count();
        let (g, c) = glyph(theme, st);
        let t = format!("{n} {label}");
        let tw = r.measure(&t, true, 0.75);
        let rr = [x - 2.0, y - 2.0, tw + 20.0, ch * 0.75 + 6.0];
        if hover(rr) { l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], bg_hover, 4.0)); }
        let on = ap.filter == f;
        l.texts.push(TextItem { size: 0.75, ..mt(x, y, g, if n > 0 || on { c } else { theme.muted }) });
        l.texts.push(uit(x + 14.0, y, &t, if n > 0 || on { theme.fg } else { theme.muted }, 0.75));
        if on { l.quads.push(Quad { x, y: y + ch * 0.8, w: tw + 14.0, h: 1.5, color: lin(c, 1.0) }); }
        hits.push(Hit { r: rr, action: Action::Builtin("agents_filter".into(), vec![json!(f.to_string())]) });
        x += tw + 26.0;
        if x > rx - 40.0 { break; }
    }
    y += ch * 1.2;
    if ap.searching || !ap.query.is_empty() {
        let q = format!("/ {}{}", ap.query, if ap.searching { "▏" } else { "" });
        l.shapes.push(Shape::rect(lx - 4.0, y - 3.0, rx - lx + 4.0, ch * 0.8 + 6.0, lin(theme.bg, 0.6), 5.0));
        l.texts.push(TextItem { clip_right: Some(rx), ..uit(lx, y, &q, if ap.searching { theme.fg } else { theme.muted }, 0.8) });
        y += ch * 1.2;
    }
    l.quads.push(Quad { x: x0 + 8.0, y: y - 2.0, w: pw - 16.0, h: 1.0, color: lin(theme.border, 0.5) });
    y += 4.0;
    // Lista: grupos por conta. Embaixo fica o detalhe do selecionado.
    let foot = y0 + ph - ch * 1.1 - 8.0;
    let detail_h = (ph * 0.36).clamp(ch * 6.0, ch * 16.0);
    let list_end = foot - detail_h;
    let row_h = ch * 2.2;
    if groups.is_empty() {
        let t = if ap.rows.is_empty() { "Nenhum agente rodando" } else if ap.query.is_empty() && ap.filter == 'a' { "Nenhum agente neste workspace (t mostra todos)" } else { "Nada com este filtro" };
        l.texts.push(uit(lx, y + 4.0, t, theme.muted, 0.8));
    }
    // Rolagem: o selecionado sempre a vista.
    let vis = ap.visible();
    let cap = (((list_end - y) / row_h).floor() as usize).saturating_sub(groups.len()).max(1);
    if let Some(k) = vis.iter().position(|&i| ap.rows[i].pane == ap.pane) {
        if k < ap.scroll { ap.scroll = k; }
        if k >= ap.scroll + cap { ap.scroll = k + 1 - cap; }
    }
    ap.scroll = ap.scroll.min(vis.len().saturating_sub(1));
    let mut k = 0usize;
    'groups: for (prof, idx) in &groups {
        let folded = ap.folded.contains(prof);
        // Grupo inteiro acima da rolagem: nao desenha nem o cabecalho.
        if !folded && k + idx.len() <= ap.scroll { k += idx.len(); continue; }
        if y > list_end - ch { break; }
        let gr = [x0 + 6.0, y - 2.0, pw - 12.0, ch * 1.1];
        if hover(gr) { l.shapes.push(Shape::rect(gr[0], gr[1], gr[2], gr[3], bg_hover, 5.0)); }
        let name = if prof.is_empty() { "pessoal" } else { prof.as_str() };
        let head = match usage.get(name) { Some(u) => format!("{} {name} · {u}", if folded { "▸" } else { "▾" }), None => format!("{} {name}", if folded { "▸" } else { "▾" }) };
        let cnt = format!("{} agente{}", idx.len(), if idx.len() == 1 { "" } else { "s" });
        let cw = r.measure(&cnt, true, 0.72);
        l.texts.push(uit(rx - cw, y + 1.0, &cnt, theme.muted, 0.72));
        l.texts.push(TextItem { bold: true, clip_right: Some(rx - cw - 8.0), ..uit(lx - 4.0, y, &head, super::ui::avatar_color(theme, ui, name), 0.8) });
        hits.push(Hit { r: gr, action: Action::Builtin("agents_fold".into(), vec![json!(prof)]) });
        y += ch * 1.2;
        if folded { continue; }
        for &i in idx {
            if k < ap.scroll { k += 1; continue; }
            k += 1;
            if y + row_h > list_end { break 'groups; }
            let row = &ap.rows[i];
            let sel = row.pane == ap.pane;
            let rr = [x0 + 6.0, y - 3.0, pw - 12.0, row_h - 2.0];
            if sel { l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(theme.accent, if ap.focused { 0.22 } else { 0.1 }), 6.0)); }
            else if hover(rr) { l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], bg_hover, 6.0)); }
            hits.push(Hit { r: rr, action: Action::Builtin("agents_pick".into(), vec![json!(row.pane)]) });
            let (g, c) = glyph(theme, &row.state);
            let a = if row.state == "working" { pulse = true; 0.45 + 0.55 * (t_now * 3.2).sin().abs() } else { 1.0 };
            l.texts.push(TextItem { alpha: a, size: 0.85, ..mt(lx, y, g, c) });
            // Linha 1: aba (e o workspace, vendo todos) e o tempo no estado.
            let t = secs(row);
            let when = if row.state == "idle" || row.state == "done" { String::new() } else { age(t) };
            let ww = r.measure(&when, true, 0.75);
            if !when.is_empty() { l.texts.push(uit(rx - ww, y + 1.0, &when, theme.muted, 0.75)); }
            let mut right = rx - ww - 8.0;
            // Fila de recados: ✉ n.
            if !row.queue.is_empty() {
                let q = format!("{}", row.queue.len());
                let qw = r.measure(&q, true, 0.75);
                l.texts.push(uit(right - qw, y + 1.0, &q, theme.accent, 0.75));
                l.texts.push(TextItem { size: 0.75, ..mt(right - qw - 14.0, y + 1.0, "\u{f0e0}", theme.accent) });
                right -= qw + 22.0;
            }
            let tab = if ap.all_ws { format!("{} · {}", row.tab, row.ws) } else { row.tab.clone() };
            l.texts.push(TextItem { bold: sel, clip_right: Some(right), ..uit(lx + 18.0, y, &tab, theme.fg, 0.85) });
            // Linha 2: pedido / passo do plano / parado ha quanto; a direita Y/N, contexto ou "novo".
            let y2 = y + ch * 0.98;
            let mut right = rx;
            if row.state == "blocked" {
                for (lbl, ok, col) in [("N", false, theme.blocked), ("Y", true, theme.done)] {
                    let br = [right - 20.0, y2 - 2.0, 20.0, ch * 0.78 + 4.0];
                    l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(col, if hover(br) { 0.55 } else { 0.28 }), 4.0));
                    let lw = r.measure(lbl, true, 0.72);
                    l.texts.push(TextItem { bold: true, ..uit(br[0] + (20.0 - lw) / 2.0, y2, lbl, theme.fg, 0.72) });
                    hits.push(Hit { r: br, action: Action::Builtin("agents_approve".into(), vec![json!(row.pane), json!(ok)]) });
                    right -= 24.0;
                }
            } else if row.state == "done" {
                let t = "● novo";
                let tw = r.measure(t, true, 0.72);
                l.texts.push(uit(right - tw, y2, t, theme.done, 0.72));
                right -= tw + 8.0;
            } else if let Some(tk) = row.tokens {
                let f = ctx_frac(tk);
                let pct = format!("{:.0}%", f * 100.0);
                let pw2 = r.measure(&pct, true, 0.72);
                l.texts.push(uit(right - pw2, y2, &pct, theme.muted, 0.72));
                let (bw, bx) = (34.0, right - pw2 - 40.0);
                let col = if f >= 0.85 { theme.blocked } else if f >= 0.6 { theme.working } else { theme.done };
                l.shapes.push(Shape::rect(bx, y2 + ch * 0.3, bw, 4.0, lin(theme.muted, 0.25), 2.0));
                l.shapes.push(Shape::rect(bx, y2 + ch * 0.3, bw * f, 4.0, lin(col, 1.0), 2.0));
                right = bx - 8.0;
            }
            l.texts.push(TextItem { clip_right: Some(right), ..uit(lx + 18.0, y2, &row_text(row, t), if row.state == "blocked" { theme.working } else { theme.muted }, 0.75) });
            y += row_h;
        }
    }
    // Detalhe do selecionado: o "why", a fila (editavel) e o plano.
    let mut y = list_end + 4.0;
    l.quads.push(Quad { x: x0 + 8.0, y: y - 4.0, w: pw - 16.0, h: 1.0, color: lin(theme.border, 0.5) });
    if let Some(row) = ap.selected().cloned() {
        let lim = foot - ch * 2.6;
        let (g, c) = glyph(theme, &row.state);
        l.texts.push(TextItem { size: 0.8, ..mt(lx, y, g, c) });
        let head = format!("{} · {} {} · pane {}", row.tab, row.name, super::ui::state_label(&row.state), row.pane);
        l.texts.push(TextItem { bold: true, clip_right: Some(rx), ..uit(lx + 16.0, y, &head, theme.fg, 0.8) });
        y += ch * 1.1;
        for w in row.why.iter().skip(1) {
            if y > lim { break; }
            l.texts.push(TextItem { clip_right: Some(rx), ..uit(lx, y, w, theme.muted, 0.72) });
            y += ch * 0.95;
        }
        if !row.queue.is_empty() && y < lim {
            y += 4.0;
            l.texts.push(TextItem { bold: true, ..uit(lx, y, &format!("Fila ({})  Tab escolhe · x cancela · u sobe · e entrega agora", row.queue.len()), theme.fg, 0.72) });
            y += ch * 1.0;
            for (qi, q) in row.queue.iter().enumerate() {
                if y > lim { break; }
                let rr = [x0 + 6.0, y - 2.0, pw - 12.0, ch * 0.95 + 2.0];
                if ap.queue_sel == Some(qi) { l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(theme.accent, 0.22), 4.0)); }
                let mut bx = rx;
                for (lbl, op) in [("✕", "rm"), ("agora", "now"), ("↑", "up")] {
                    if op == "up" && qi == 0 { continue; }
                    let bw = r.measure(lbl, true, 0.7) + 10.0;
                    let br = [bx - bw, y - 1.0, bw, ch * 0.85];
                    if hover(br) { l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(theme.accent, 0.35), 4.0)); }
                    l.texts.push(uit(br[0] + 5.0, y, lbl, if op == "rm" { theme.blocked } else { theme.fg }, 0.7));
                    hits.push(Hit { r: br, action: Action::Builtin("agents_queue".into(), vec![json!(row.pane), json!(op), json!(qi)]) });
                    bx -= bw + 4.0;
                }
                let text: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
                l.texts.push(TextItem { clip_right: Some(bx - 4.0), ..uit(lx, y, &format!("{}. {text}", qi + 1), theme.fg, 0.72) });
                y += ch * 0.95 + 2.0;
            }
        }
        // Plano do transcript (Claude Code), se couber.
        if ap.detail_pane == row.pane && !ap.plan.is_empty() && y < lim - ch {
            y += 4.0;
            let done = ap.plan.iter().filter(|p| p.0 == "completed").count();
            l.texts.push(TextItem { bold: true, ..uit(lx, y, &format!("Plano  {done}/{}", ap.plan.len()), theme.fg, 0.72) });
            y += ch * 1.0;
            for (st, subject) in &ap.plan {
                if y > lim { break; }
                let (g, c) = match st.as_str() { "completed" => ("\u{f058}", theme.done), "in_progress" => ("\u{f192}", theme.accent), _ => ("\u{f10c}", theme.muted) };
                l.texts.push(TextItem { size: 0.72, ..mt(lx, y, g, c) });
                l.texts.push(TextItem { clip_right: Some(rx), ..uit(lx + 16.0, y, subject, if st == "completed" { theme.muted } else { theme.fg }, 0.72) });
                y += ch * 0.95;
            }
        }
        let mut extra = Vec::new();
        let k = |n: u64| if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) } else { format!("{}k", n / 1000) };
        if let Some(t) = row.tokens { extra.push(format!("contexto {}", k(t))); }
        if let Some(t) = row.session_tokens {
            extra.push(format!("sessao {}", k(t)));
            if ui.token_price > 0.0 { extra.push(format!("US$ {:.2}", t as f64 / 1e6 * ui.token_price)); }
        }
        if ap.detail_pane == row.pane && !ap.files.is_empty() {
            let (ta, td) = ap.files.iter().fold((0, 0), |s, f| (s.0 + f.2, s.1 + f.3));
            extra.push(format!("{} arquivo(s) +{ta} −{td}", ap.files.len()));
        }
        if !extra.is_empty() && y < lim {
            y += 4.0;
            l.texts.push(TextItem { clip_right: Some(rx), ..uit(lx, y, &extra.join(" · "), theme.muted, 0.72) });
        }
        // Botoes: os do painel antigo continuam.
        let mut bx = lx - 2.0;
        let by = foot - ch * 1.6;
        for (label, act, args) in [("Ir", "agents_go", vec![json!(row.pane)]), ("Recado", "queue_message", vec![json!(row.pane)]), ("Linha do tempo", "agent_timeline", vec![]), ("Diff", "agent_diff", vec![])] {
            let bw = r.measure(label, true, 0.75) + 16.0;
            let br = [bx, by, bw, ch * 1.3];
            let hv = hover(br);
            l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(if hv { theme.accent } else { mix(theme.tab_bar, theme.fg, 0.1) }, if hv { 0.35 } else { 1.0 }), 6.0));
            l.texts.push(uit(bx + 8.0, by + (ch * 1.3 - ch * 0.75) / 2.0, label, theme.fg, 0.75));
            hits.push(Hit { r: br, action: Action::Builtin(act.into(), args) });
            bx += bw + 5.0;
        }
    }
    let hint = if ap.focused { "Enter ir · y/n aprovar · m recado · b/w/d/i/a filtro · / busca · t todos · Esc sai" } else { "clique no painel para usar o teclado" };
    l.texts.push(TextItem { clip_right: Some(rx), ..uit(lx, foot + 2.0, hint, theme.muted, 0.68) });
    pulse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pane: u64, ws: &str, profile: &str, state: &str, since: u64) -> AgentRow {
        AgentRow { pane, ws: ws.into(), tab: format!("aba{pane}"), profile: profile.into(), name: "claude".into(), state: state.into(), since, ..Default::default() }
    }

    #[test]
    fn agrupa_por_conta_e_ordena_por_urgencia() {
        let rows = vec![
            row(1, "w", "pessoal", "idle", 10),
            row(2, "w", "totvs", "working", 50),
            row(3, "w", "totvs", "done", 40),
            row(4, "w", "totvs", "blocked", 30),
            row(5, "w", "totvs", "working", 20),
            row(6, "outro", "totvs", "blocked", 1),
        ];
        let g = group_rows(&rows, Some("w"), 'a', "");
        assert_eq!(g.len(), 2);
        // O grupo com quem pede vem primeiro; dentro: pede, terminou, trabalhando (mais antigo antes), ocioso.
        assert_eq!(g[0].0, "totvs");
        assert_eq!(g[0].1.iter().map(|&i| rows[i].pane).collect::<Vec<_>>(), vec![4, 3, 5, 2]);
        assert_eq!(g[1].0, "pessoal");
        // Todos os workspaces: o bloqueado do outro workspace (ha mais tempo) entra primeiro.
        let g = group_rows(&rows, None, 'a', "");
        assert_eq!(rows[g[0].1[0]].pane, 6);
    }

    #[test]
    fn filtro_e_busca() {
        let mut rows = vec![row(1, "w", "totvs", "blocked", 1), row(2, "w", "pessoal", "idle", 1)];
        rows[1].step = Some("Ajustando testes".into());
        let g = group_rows(&rows, Some("w"), 'b', "");
        assert_eq!(g.len(), 1);
        assert_eq!(rows[g[0].1[0]].pane, 1);
        let g = group_rows(&rows, Some("w"), 'a', "ajustando");
        assert_eq!(rows[g[0].1[0]].pane, 2);
        assert!(group_rows(&rows, Some("w"), 'd', "").is_empty());
        // Busca sem acento e por varias palavras.
        rows[0].tab = "migração".into();
        assert_eq!(group_rows(&rows, Some("w"), 'a', "migracao totvs").len(), 1);
    }

    #[test]
    fn selecao_segue_filtro_e_grupo_recolhido() {
        let mut ap = AgentPanel::new();
        ap.ws = "w".into();
        ap.rows = vec![row(1, "w", "a", "blocked", 1), row(2, "w", "b", "idle", 1)];
        ap.pane = 2;
        ap.filter = 'b';
        ap.fix_sel();
        assert_eq!(ap.pane, 1);
        ap.filter = 'a';
        ap.step(1);
        assert_eq!(ap.pane, 2);
        ap.folded.insert("b".into());
        ap.fix_sel();
        assert_eq!(ap.pane, 1);
    }

    #[test]
    fn texto_da_linha() {
        let mut r = row(1, "w", "p", "blocked", 0);
        r.msg = Some("Bash(npm test)".into());
        assert_eq!(row_text(&r, 40), "pede: Bash(npm test)");
        r.state = "working".into();
        r.todo = Some((3, 8));
        r.step = Some("Ajustando testes".into());
        assert_eq!(row_text(&r, 5), "3/8 Ajustando testes");
        r.state = "idle".into();
        r.msg = None;
        assert_eq!(row_text(&r, 7200), "ocioso ha 2h");
        assert!((ctx_frac(122_000) - 0.61).abs() < 0.01);
    }
}
