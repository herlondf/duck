//! Mapa dos agentes: a orquestracao como arvore, da esquerda para a direita.
//!
//! Nos: cada pane com agente; filhos dele os panes que ele abriu pela API
//! (`parent`) e os subagentes da sessao do Claude Code, lidos de
//! `<perfil>/projects/*/<sessao>/subagents/*.meta.json` (descricao, tipo,
//! terminou). Estado ao vivo, plano (feitas/total) e o loop com verificador.
//! Enter foca o pane; num subagente, mostra o pedido e a ultima resposta.

use super::*;
use super::ui::{uit, Hit};
use crate::render::Shape;

#[derive(Clone)]
pub(super) struct GNode {
    pub parent: Option<usize>,
    pub depth: usize,
    pub title: String,
    pub sub: String,
    pub state: String,
    pub pane: Option<u64>,
    /// Subagente: transcript (.jsonl) e o pedido.
    pub transcript: Option<std::path::PathBuf>,
}

pub(super) struct Graph { pub nodes: Vec<GNode>, pub sel: usize, pub fetched: Instant, pub scroll: usize }

/// Pasta da sessao (projects/<projeto>/<sessao>) no perfil, se existir.
fn session_dir(config_dir: &str, session: &str) -> Option<std::path::PathBuf> {
    let base = if config_dir.is_empty() { config::home().join(".claude") } else { std::path::PathBuf::from(config_dir) };
    for proj in std::fs::read_dir(base.join("projects")).ok()?.flatten() {
        let d = proj.path().join(session);
        if d.is_dir() { return Some(d); }
    }
    None
}

fn age(secs: u64) -> String {
    if secs < 60 { "agora".into() } else if secs < 3600 { format!("{} min", secs / 60) } else if secs < 86400 { format!("{} h", secs / 3600) } else { format!("{} d", secs / 86400) }
}

/// Subagentes da sessao, mais novos primeiro (ate `max`), e quantos ficaram de fora.
fn subagents(dir: &std::path::Path, max: usize) -> (Vec<GNode>, usize) {
    let mut found: Vec<(u64, GNode)> = Vec::new();
    let now = SystemTime::now();
    for f in std::fs::read_dir(dir.join("subagents")).into_iter().flatten().flatten() {
        let p = f.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = name.strip_suffix(".meta.json") else { continue };
        let Ok(meta) = std::fs::read_to_string(&p).map(|t| serde_json::from_str::<Value>(&t).unwrap_or_default()) else { continue };
        let jsonl = dir.join("subagents").join(format!("{stem}.jsonl"));
        let modified = std::fs::metadata(&jsonl).and_then(|m| m.modified()).ok().or_else(|| f.metadata().and_then(|m| m.modified()).ok());
        let idle = modified.and_then(|t| now.duration_since(t).ok()).map(|d| d.as_secs()).unwrap_or(u64::MAX);
        let finished = meta["finished"].as_bool().unwrap_or(false);
        let state = if finished { "done" } else if idle < 120 { "working" } else { "idle" };
        let mut sub = meta["agentType"].as_str().unwrap_or("subagente").to_string();
        if meta["requestShape"] == "background" { sub += " · em segundo plano"; }
        if let Some(d) = meta["spawnDepth"].as_u64().filter(|d| *d > 1) { sub += &format!(" · nível {d}"); }
        sub += &format!(" · {}", if finished { format!("terminou há {}", age(idle)) } else if idle < 120 { "trabalhando".into() } else { format!("parado há {}", age(idle)) });
        let stamp = modified.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        found.push((stamp, GNode {
            parent: None, depth: 0, title: meta["description"].as_str().unwrap_or(stem).to_string(), sub, state: state.into(),
            pane: None, transcript: Some(jsonl),
        }));
    }
    // Trabalhando primeiro, depois os mais recentes.
    found.sort_by(|a, b| (b.1.state == "working").cmp(&(a.1.state == "working")).then(b.0.cmp(&a.0)));
    let extra = found.len().saturating_sub(max);
    (found.into_iter().take(max).map(|x| x.1).collect(), extra)
}

impl Gui {
    pub(super) fn open_graph(&mut self) {
        self.graph = Some(Graph { nodes: vec![], sel: 0, fetched: Instant::now() - Duration::from_secs(10), scroll: 0 });
        self.refresh_graph();
        if self.graph.as_ref().is_some_and(|g| g.nodes.is_empty()) {
            self.graph = None;
            self.flash("Nenhum agente rodando");
        }
    }

    pub(super) fn refresh_graph(&mut self) {
        let Some(g) = self.graph.as_ref() else { return };
        if g.fetched.elapsed() < Duration::from_secs(2) { return; }
        let Ok(v) = self.call(json!({"cmd": "list"})) else { return };
        // Panes com agente, na ordem das abas.
        struct P { id: u64, parent: Option<u64>, title: String, sub: String, state: String, session: String, config_dir: String }
        let mut ps: Vec<P> = Vec::new();
        for w in v["workspaces"].as_array().into_iter().flatten() {
            for (ti, t) in w["tabs"].as_array().into_iter().flatten().enumerate() {
                for p in t["panes"].as_array().into_iter().flatten() {
                    let a = &p["agent"];
                    if a.is_null() { continue; }
                    let tab = t["title"].as_str().filter(|x| !x.is_empty()).map(String::from)
                        .unwrap_or_else(|| p["title"].as_str().unwrap_or("").trim_start_matches(|c: char| !c.is_alphanumeric()).to_string());
                    let mut sub = format!("{} · {} aba {}", a["name"].as_str().unwrap_or("agente"), w["name"].as_str().unwrap_or(""), ti + 1);
                    if let (Some(d), Some(n)) = (a["todo_done"].as_u64(), a["todo_total"].as_u64()) { sub += &format!(" · plano {d}/{n}"); }
                    if let Some(l) = p["loop"].as_str().filter(|l| !l.is_empty()) { sub += &format!(" · loop {l}"); }
                    ps.push(P {
                        id: p["id"].as_u64().unwrap_or(0), parent: p["parent"].as_u64(), title: if tab.is_empty() { format!("pane {}", p["id"]) } else { tab },
                        sub, state: a["state"].as_str().unwrap_or("").to_string(),
                        session: a["session"].as_str().unwrap_or("").to_string(), config_dir: a["config_dir"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
        }
        let ids: Vec<u64> = ps.iter().map(|p| p.id).collect();
        let mut nodes: Vec<GNode> = Vec::new();
        // Arvore em profundidade: raiz = agente sem pai entre os agentes.
        fn add(ps: &[P], ids: &[u64], i: usize, parent: Option<usize>, depth: usize, nodes: &mut Vec<GNode>, seen: &mut Vec<u64>) {
            let p = &ps[i];
            if seen.contains(&p.id) { return; }
            seen.push(p.id);
            let me = nodes.len();
            nodes.push(GNode { parent, depth, title: p.title.clone(), sub: format!("{} · pane {}", p.sub, p.id), state: p.state.clone(), pane: Some(p.id), transcript: None });
            for (j, c) in ps.iter().enumerate() {
                if c.parent == Some(p.id) && ids.contains(&c.id) { add(ps, ids, j, Some(me), depth + 1, nodes, seen); }
            }
            // WSL: o perfil e um caminho do Linux, fora do alcance daqui.
            if !p.session.is_empty() && !p.config_dir.starts_with('/') {
                if let Some(dir) = session_dir(&p.config_dir, &p.session) {
                    let (subs, extra) = subagents(&dir, 12);
                    for mut s in subs { s.parent = Some(me); s.depth = depth + 1; nodes.push(s); }
                    if extra > 0 {
                        nodes.push(GNode { parent: Some(me), depth: depth + 1, title: format!("+{extra} subagentes mais antigos"), sub: String::new(), state: "muted".into(), pane: None, transcript: None });
                    }
                }
            }
        }
        let mut seen = Vec::new();
        for i in 0..ps.len() {
            let root = ps[i].parent.is_none_or(|pp| !ids.contains(&pp));
            if root { add(&ps, &ids, i, None, 0, &mut nodes, &mut seen); }
        }
        if let Some(g) = self.graph.as_mut() {
            g.sel = g.sel.min(nodes.len().saturating_sub(1));
            g.nodes = nodes;
            g.fetched = Instant::now();
        }
    }

    pub(super) fn graph_key(&mut self, ev: &KeyEvent) {
        let k = key_name(ev);
        let Some(g) = self.graph.as_mut() else { return };
        let n = g.nodes.len().max(1);
        match k.as_str() {
            "escape" => { self.graph = None; }
            "arrowdown" | "tab" => g.sel = (g.sel + 1) % n,
            "arrowup" => g.sel = (g.sel + n - 1) % n,
            "arrowleft" => if let Some(p) = g.nodes.get(g.sel).and_then(|x| x.parent) { g.sel = p },
            "arrowright" => if let Some(c) = g.nodes.iter().position(|x| x.parent == Some(g.sel)) { g.sel = c },
            "enter" => { let i = g.sel; self.graph_pick(i); }
            _ => {}
        }
        self.redraw();
    }

    pub(super) fn graph_pick(&mut self, i: usize) {
        let Some(node) = self.graph.as_ref().and_then(|g| g.nodes.get(i)).cloned() else { return };
        if let Some(g) = self.graph.as_mut() { g.sel = i; }
        if let Some(id) = node.pane {
            self.graph = None;
            self.cmd(json!({"cmd": "focus", "pane": id}));
            return;
        }
        let Some(path) = node.transcript else { return };
        // Pedido (1a linha) e a ultima resposta de texto do subagente.
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut ask = String::new();
        let mut last = String::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let c = &v["message"]["content"];
            let t = c.as_str().map(String::from).unwrap_or_else(|| c.as_array().into_iter().flatten().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"));
            if t.trim().is_empty() { continue; }
            if v["type"] == "user" && ask.is_empty() { ask = t; } else if v["type"] == "assistant" { last = t; }
        }
        let body = format!("Pedido:\n{}\n\nÚltima resposta:\n{}", ask.trim(), if last.trim().is_empty() { "(ainda nada)" } else { last.trim() });
        let lines = body.lines().map(String::from).collect();
        self.overlay = Some(Overlay { kind: OverlayKind::Detail { ext: String::new(), subtitle: node.sub.clone(), lines, actions: vec![], scroll: 0 }, ..Overlay::list(&node.title, vec![]) });
    }
}

/// Desenha o mapa por cima de tudo (como a grade de agentes).
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_graph(g: &mut Graph, r: &mut Renderer, modal: &mut Layer, hits: &mut Vec<Hit>, theme: &Theme, canvas: Rgb, w: f32, h: f32, ch: f32, rad: f32) {
    modal.shapes.push(Shape::rect(0.0, 0.0, w, h, lin(canvas, 0.97), 0.0));
    let title = "Mapa dos agentes";
    modal.texts.push(TextItem { bold: true, ..uit(20.0, 14.0, title, theme.fg, 1.1) });
    let working = g.nodes.iter().filter(|n| n.state == "working").count();
    let blocked = g.nodes.iter().filter(|n| n.state == "blocked").count();
    let summary = format!("{} nós · {working} trabalhando · {blocked} esperando você", g.nodes.len());
    modal.texts.push(uit(28.0 + r.measure(title, true, 1.1), 16.0, &summary, theme.muted, 0.9));
    let max_depth = g.nodes.iter().map(|n| n.depth).max().unwrap_or(0) as f32;
    // Recuo de arvore: o filho fica um pouco a direita do pai, nao numa coluna inteira.
    let indent = 44.0;
    let card_w = (w - 40.0 - indent * max_depth).clamp(240.0, 560.0);
    let card_h = ch * 2.7;
    let row_h = card_h + 10.0;
    let top = 14.0 + ch * 2.2;
    let rows_fit = (((h - top - ch - 26.0) / row_h).floor() as usize).max(1);
    // Mantem o selecionado visivel.
    if g.sel < g.scroll { g.scroll = g.sel; }
    if g.sel >= g.scroll + rows_fit { g.scroll = g.sel + 1 - rows_fit; }
    let pos = |i: usize, n: &GNode| (20.0 + n.depth as f32 * indent, top + (i as f32 - g.scroll as f32) * row_h);
    // Ligacoes: do pai desce uma linha e entra no filho pela esquerda.
    for (i, n) in g.nodes.iter().enumerate() {
        let Some(pi) = n.parent else { continue };
        let (x, y) = pos(i, n);
        let (px, py) = pos(pi, &g.nodes[pi]);
        let lx = px + 18.0;
        let (y0, y1) = ((py + card_h).max(top - 4.0), (y + card_h / 2.0).min(h));
        if y1 < top || y0 > h { continue; }
        let c = lin(if n.state == "working" { theme.working } else { theme.muted }, 0.55);
        modal.shapes.push(Shape::rect(lx - 1.0, y0, 2.0, (y1 - y0).max(0.0), c, 0.0));
        modal.shapes.push(Shape::rect(lx - 1.0, y1 - 1.0, x - lx + 1.0, 2.0, c, 0.0));
    }
    for (i, n) in g.nodes.iter().enumerate() {
        if i < g.scroll || i >= g.scroll + rows_fit { continue; }
        let (x, y) = pos(i, n);
        let sel = i == g.sel;
        let sc = match n.state.as_str() { "working" | "blocked" | "done" => theme.named(&n.state), "muted" => theme.muted, _ => theme.muted };
        modal.shapes.push(Shape::shadow(x, y + 3.0, card_w, card_h, [0.0, 0.0, 0.0, 0.35], rad, 6.0));
        modal.shapes.push(Shape::rect(x, y, card_w, card_h, lin(theme.bg, 1.0), rad));
        modal.shapes.push(Shape::rect(x, y + 4.0, 3.0, card_h - 8.0, lin(sc, 1.0), 1.5));
        modal.shapes.push(Shape::ring(x, y, card_w, card_h, lin(if sel { theme.accent } else { sc }, if sel { 1.0 } else { 0.35 }), rad, if sel { 2.0 } else { 1.0 }));
        let dot = match n.state.as_str() { "working" => "◐", "blocked" => "⏸", "done" => "✓", _ if n.pane.is_none() && n.transcript.is_none() => "…", _ => "○" };
        modal.texts.push(TextItem { bold: true, clip_right: Some(x + card_w - 10.0), ..uit(x + 12.0, y + ch * 0.35, &format!("{dot} {}", n.title), if n.state == "muted" { theme.muted } else { theme.fg }, 0.95) });
        if !n.sub.is_empty() {
            modal.texts.push(TextItem { clip_right: Some(x + card_w - 10.0), ..uit(x + 12.0, y + ch * 1.45, &n.sub, theme.muted, 0.82) });
        }
        hits.push(Hit { r: [x, y, card_w, card_h], action: Action::Builtin("graph_pick".into(), vec![json!(i)]) });
    }
    if g.nodes.len() > rows_fit {
        let more = format!("{}–{} de {}", g.scroll + 1, (g.scroll + rows_fit).min(g.nodes.len()), g.nodes.len());
        modal.texts.push(uit(w - 24.0 - r.measure(&more, true, 0.85), 16.0, &more, theme.muted, 0.85));
    }
    let help = "Enter foca o pane ou abre o subagente · setas navegam · Esc fecha";
    let hw = r.measure(help, true, 0.85) + 24.0;
    modal.shapes.push(Shape::rect((w - hw) / 2.0, h - ch - 16.0, hw, ch + 10.0, lin(theme.accent, 1.0), (ch + 10.0) / 2.0));
    modal.texts.push(uit((w - hw) / 2.0 + 12.0, h - ch - 11.0, help, theme.bg, 0.85));
}
