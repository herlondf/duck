//! Visual e interacao da janela: barra de titulo propria, abas com estado
//! animado (ou barra lateral), panes como cartoes, blocos de comando com
//! botoes, painel do agente, cartoes de aviso, editor de comando,
//! autocompletar, realce do comando, previa de arquivo, galeria de temas,
//! sliders, tela inicial e busca com contador.
//!
//! Tudo que e clicavel entra em `hits` durante o desenho; o clique resolve
//! contra o ultimo quadro (o que foi desenhado por ultimo fica por cima).

use super::*;
use crate::render::Shape;
use winit::window::ResizeDirection;

/// `bottom` soma, de cima para baixo: abas embaixo (`tabs_h`), rodape dos
/// badges (`foot_h`) e barra de atalhos (`keys_h`).
pub(super) struct Chrome { pub top: f32, pub bottom: f32, pub left: f32, pub right: f32, pub bar_h: f32, pub margin: f32, pub tabs_h: f32, pub foot_h: f32, pub keys_h: f32 }

#[derive(Clone)]
pub(super) struct Hit { pub r: [f32; 4], pub action: Action }

pub(super) struct AgentPanel {
    pub pane: u64, pub name: String, pub profile: String, pub state: String,
    pub ctx: Option<u64>, pub session_tokens: Option<u64>, pub plan: Vec<(String, String)>, pub fetched: Instant,
    pub todo: Option<(u64, u64)>,
}

pub(super) struct Composer {
    pub pane: u64, pub text: Vec<char>, pub cur: usize, pub anchor: Option<usize>,
    pub undo: Vec<(Vec<char>, usize)>, pub redo: Vec<(Vec<char>, usize)>,
    /// Onde o texto foi desenhado (x, y, largura, altura): clique posiciona o cursor.
    pub geom: [f32; 4],
}

pub(super) struct Ac { pub typed: String, pub items: Vec<AcItem>, pub sel: Option<usize> }
pub(super) struct AcItem { pub label: String, pub insert: String, pub delete: usize, pub kind: &'static str }

/// Pato da apresentacao (assets/duck.txt, gerado do SVG por assets/make_duck.py).
const DUCK_ART: &str = include_str!("../../assets/duck.txt");

fn ease(t: f32) -> f32 { let t = t.clamp(0.0, 1.0); 1.0 - (1.0 - t).powi(3) }
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t] }
pub(super) fn inside(r: [f32; 4], p: (f32, f32)) -> bool { p.0 >= r[0] && p.0 < r[0] + r[2] && p.1 >= r[1] && p.1 < r[1] + r[3] }
/// "nome|subtitulo": o que vem depois do primeiro '|' e desenhado menor, em cinza.
pub(super) fn split_subtitle(label: &str) -> (&str, &str) {
    match label.split_once('|') {
        Some((a, b)) if !a.trim().is_empty() => (a.trim(), b.trim()),
        Some((_, b)) => (b.trim(), ""),
        None => (label, ""),
    }
}

pub(super) fn uit(x: f32, y: f32, text: &str, color: Rgb, size: f32) -> TextItem {
    TextItem { x, y, text: text.into(), color, alpha: 1.0, bold: false, italic: false, clip_right: None, ui: true, size }
}
pub(super) fn mt(x: f32, y: f32, text: &str, color: Rgb) -> TextItem {
    TextItem { x, y, text: text.into(), color, alpha: 1.0, bold: false, italic: false, clip_right: None, ui: false, size: 1.0 }
}
fn shift(l: &mut Layer, from: (usize, usize, usize), dx: f32) {
    for s in &mut l.shapes[from.0..] { s.x += dx; }
    for q in &mut l.quads[from.1..] { q.x += dx; }
    for t in &mut l.texts[from.2..] { t.x += dx; }
}
fn bit(b: bool) -> f32 { if b { 1.0 } else { 0.0 } }

/// Cor do avatar do perfil do agente: da config ou derivada do nome.
fn avatar_color(theme: &Theme, ui: &config::Ui, profile: &str) -> Rgb {
    if let Some(c) = ui.agent_colors.get(profile) { return *c; }
    let opts = [theme.accent, theme.done, theme.working, theme.palette[5], theme.palette[6], theme.blocked];
    let h = profile.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    opts[h as usize % opts.len()]
}

fn draw_avatar(r: &mut Renderer, l: &mut Layer, x: f32, y: f32, d: f32, color: Rgb, profile: &str) {
    l.shapes.push(Shape::rect(x, y, d, d, lin(color, 1.0), d / 2.0));
    let letter: String = profile.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "A".into());
    let size = (d / r.cell_h * 0.78).max(0.4);
    let tw = r.measure(&letter, true, size);
    l.texts.push(TextItem { bold: true, ..uit(x + (d - tw) / 2.0, y + (d - r.cell_h * size) / 2.0, &letter, Rgb(24, 24, 32), size) });
}

/// Icone do pane pelo dominio (Nerd Font embutida).
fn pane_icon(domain: &str, wsl: bool) -> &'static str {
    if wsl { "\u{f17c}" } else if !domain.is_empty() { "\u{f0c2}" } else { "\u{e795}" }
}

/// Sino da caixa de entrada: icone centralizado e, com avisos nao lidos, o
/// numero numa bolinha no canto do icone (vermelha quando um agente espera).
/// `flat`: na barra de titulo, igual aos botoes da janela (altura toda, sem cantos).
#[allow(clippy::too_many_arguments)]
fn draw_bell(r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, theme: &Theme, mouse: (f32, f32), br: [f32; 4], n: u64, blocked: bool, flat: bool) {
    let hv = inside(br, mouse);
    if hv { l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), if flat { 0.0 } else { 6.0 })); }
    let ch = r.cell_h;
    let size = 0.85;
    let icon = super::ext::icon("bell");
    let gw = r.measure(&icon, false, size);
    // O glifo da Nerd Font fica alto na celula: desce um pouco para alinhar com os botoes.
    let (gx, gy) = (br[0] + (br[2] - gw) / 2.0, br[1] + (br[3] - ch * size) / 2.0 + ch * 0.12);
    let c = if blocked { theme.blocked } else if n > 0 || hv { theme.fg } else { theme.muted };
    l.texts.push(TextItem { size, ..mt(gx, gy, &icon, c) });
    if n > 0 {
        let t = if n > 99 { "99+".to_string() } else { n.to_string() };
        let tw = r.measure(&t, true, 0.6);
        let dh = ch * 0.6 + 3.0;
        let dw = (tw + 8.0).max(dh);
        let (dx, dy) = (gx + gw * 1.05, gy - dh * 0.3);
        l.shapes.push(Shape::rect(dx, dy, dw, dh, lin(if blocked { theme.blocked } else { theme.accent }, 1.0), dh / 2.0));
        l.texts.push(TextItem { bold: true, ..uit(dx + (dw - tw) / 2.0, dy + (dh - ch * 0.6) / 2.0, &t, theme.bg, 0.6) });
    }
    hits.push(Hit { r: br, action: Action::Builtin("inbox".into(), vec![]) });
}

fn is_shortcut(h: &str) -> bool { ["Ctrl+", "Alt+", "Shift+", "Win+"].iter().any(|k| h.contains(k)) }

/// Teclas como "keycaps" alinhadas a direita; devolve a largura usada.
fn keycaps(r: &mut Renderer, l: &mut Layer, right: f32, y: f32, row_h: f32, hint: &str, theme: &Theme) -> f32 {
    let parts: Vec<&str> = hint.split('+').collect();
    let mut x = right;
    for p in parts.iter().rev() {
        let tw = r.measure(p, true, 0.75) + 10.0;
        x -= tw;
        let kh = row_h * 0.7;
        l.shapes.push(Shape::rect(x, y + (row_h - kh) / 2.0, tw, kh, lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), 4.0));
        l.texts.push(uit(x + 5.0, y + (row_h - r.cell_h * 0.75) / 2.0, p, theme.fg, 0.75));
        x -= 3.0;
    }
    right - x
}


const SHADER_CRT: &str = r#"fn effect(uv: vec2<f32>, time: f32, size: vec2<f32>) -> vec4<f32> {
    let c = uv * 2.0 - 1.0;
    let k = c * (1.0 + 0.035 * dot(c, c));
    let q = k * 0.5 + 0.5;
    if (q.x < 0.0 || q.x > 1.0 || q.y < 0.0 || q.y > 1.0) { return vec4(0.0, 0.0, 0.0, 1.0); }
    let col = scene(q);
    let scan = 0.86 + 0.14 * sin(q.y * size.y * 3.14159);
    let vig = 1.0 - 0.32 * dot(c, c);
    return vec4(col.rgb * scan * vig, col.a);
}
"#;
const SHADER_GLOW: &str = r#"fn effect(uv: vec2<f32>, time: f32, size: vec2<f32>) -> vec4<f32> {
    let px = 2.0 / size;
    var acc = vec4(0.0);
    for (var i = -2; i <= 2; i++) {
        for (var j = -2; j <= 2; j++) { acc += scene(uv + vec2(f32(i), f32(j)) * px); }
    }
    let base = scene(uv);
    return vec4(base.rgb + (acc.rgb / 25.0) * 0.5, base.a);
}
"#;
const SHADER_GRAIN: &str = r#"fn hash(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }
fn effect(uv: vec2<f32>, time: f32, size: vec2<f32>) -> vec4<f32> {
    let c = scene(uv);
    let n = hash(floor(uv * size) + vec2(floor(time * 24.0) * 7.0, floor(time * 24.0) * 3.0)) - 0.5;
    return vec4(c.rgb + vec3(n * 0.07), c.a);
}
"#;

/// Registra o protocolo duck: (botoes dos toasts chamam `duck url ...`).
pub(super) fn register_protocol() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        use std::os::windows::process::CommandExt;
        let Some(exe) = crate::proto::current_exe().ok().map(|e| e.with_file_name("duck.exe")).filter(|e| e.is_file()) else { return };
        let cmd = format!("\"{}\" url \"%1\"", exe.display());
        for args in [
            vec!["add", r"HKCU\Software\Classes\duck", "/ve", "/d", "URL:Duck", "/f"],
            vec!["add", r"HKCU\Software\Classes\duck", "/v", "URL Protocol", "/d", "", "/f"],
            vec!["add", r"HKCU\Software\Classes\duck\shell\open\command", "/ve", "/d", cmd.as_str(), "/f"],
        ] {
            let _ = std::process::Command::new("reg.exe").args(&args).creation_flags(0x0800_0000).output();
        }
    });
}

impl Gui {
    // -------------------------------------------------------------- geometria

    pub(super) fn custom_titlebar(&self) -> bool { self.cfg.ui.titlebar == "custom" && !self.quick }

    pub(super) fn chrome(&self) -> Chrome {
        let ch = self.r.as_ref().map(|r| r.cell_h).unwrap_or(16.0);
        let bar_h = (ch + 16.0).max(32.0).round();
        let margin = if self.cfg.ui.cards { 8.0 } else { self.cfg.padding };
        if self.focus_mode { return Chrome { top: 0.0, bottom: 0.0, left: 0.0, right: 0.0, bar_h, margin, tabs_h: 0.0, foot_h: 0.0, keys_h: 0.0 }; }
        let tb = self.cfg.tab_bar.as_str();
        let tabs_h = if tb == "bottom" { bar_h } else { 0.0 };
        let foot_h = if self.cfg.ui.footer && !self.quick { (ch + 10.0).max(28.0).round() } else { 0.0 };
        let keys_h = if self.cfg.ui.shortcuts && !self.quick && !self.shortcuts.is_empty() { (ch * 0.8 + 10.0).max(22.0).round() } else { 0.0 };
        Chrome {
            top: if tb == "top" || self.custom_titlebar() { bar_h } else { 0.0 },
            bottom: tabs_h + foot_h + keys_h, tabs_h, foot_h, keys_h,
            left: if tb == "left" { if self.sidebar_collapsed { 52.0 } else { self.cfg.ui.sidebar_width } } else { 0.0 },
            right: if self.ext_panel.is_some() || self.agent_panel.is_some() || self.sys_panel { self.ext_w } else { 0.0 },
            bar_h, margin,
        }
    }

    pub(super) fn grid_origin(&self) -> (f32, f32) {
        let c = self.chrome();
        (c.left + c.margin, c.top + c.margin)
    }

    /// Mouse na borda esquerda do painel das extensoes (arrasta para redimensionar).
    fn ext_panel_edge(&self, x: f32, y: f32) -> bool {
        let Some(r) = &self.r else { return false };
        let c = self.chrome();
        (self.ext_panel.is_some() || self.agent_panel.is_some() || self.sys_panel) && self.overlay.is_none() && y > c.top && y < r.height() - c.bottom && (x - (r.width() - c.right)).abs() <= 5.0
    }

    pub(super) fn send_size(&mut self) {
        let Some(r) = &self.r else { return };
        let c = self.chrome();
        let cols = ((r.width() - c.left - c.right - 2.0 * c.margin) / r.cell_w).floor().max(2.0) as u64;
        let rows = ((r.height() - c.top - c.bottom - 2.0 * c.margin) / r.cell_h).floor().max(2.0) as u64;
        self.origin = self.grid_origin();
        self.cmd(json!({"cmd": "resize", "cols": cols, "rows": rows}));
    }

    fn tab_at(&self, x: f32, y: f32) -> Option<usize> {
        if y < self.tab_row.0 || y >= self.tab_row.0 + self.tab_row.1 { return None; }
        self.tab_rects.iter().find(|(a, b, _)| x >= *a && x < *b).map(|t| t.2)
    }

    /// Divisor entre panes sob o mouse: (pane a esquerda/acima, vertical?).
    fn divider_at(&self, x: f32, y: f32) -> Option<(u64, bool)> {
        let r = self.r.as_ref()?;
        let (ox, oy) = self.origin;
        let (cw, ch) = (r.cell_w, r.cell_h);
        for p in self.frame.panes.iter().filter(|p| !p.floating) {
            let px = ox + p.x as f32 * cw;
            let py = oy + p.y as f32 * ch;
            let (pw, ph) = (p.w as f32 * cw, p.h as f32 * ch);
            let hdr = if p.header { ch } else { 0.0 };
            let xr = ox + (p.x + p.w) as f32 * cw + cw * 0.5;
            if p.x + p.w < self.frame.cols && (x - xr).abs() <= 4.0 && y >= py - hdr && y < py + ph { return Some((p.id, true)); }
            let yb = oy + (p.y + p.h) as f32 * ch + ch * 0.5;
            if p.y + p.h < self.frame.rows && (y - yb).abs() <= 4.0 && x >= px && x < px + pw { return Some((p.id, false)); }
        }
        None
    }

    // -------------------------------------------------------------- desenho

    /// Previa da aba sob o mouse: pede o texto ao server (antes de emprestar o renderer).
    fn fetch_tab_preview(&mut self) {
        let Some((idx, t0)) = self.tab_hover else { self.tab_preview = None; return };
        if t0.elapsed() < Duration::from_millis(450) || self.tab_drag.is_some() { return; }
        if self.tab_preview.as_ref().is_some_and(|p| p.0 == idx) { return; }
        let Some(t) = self.frame.tabs.iter().find(|t| t.index == idx && !t.active).cloned() else { return };
        if let Ok(v) = self.call(json!({"cmd": "read", "pane": t.pane, "lines": 16})) {
            let lines = v["text"].as_str().unwrap_or("").lines().map(|l| l.trim_end().to_string()).collect::<Vec<_>>();
            let start = lines.iter().rposition(|l| !l.trim().is_empty()).map(|e| (e + 1).saturating_sub(12)).unwrap_or(0);
            self.tab_preview = Some((idx, lines.into_iter().skip(start).take(12).collect()));
        }
    }

    pub(super) fn draw(&mut self) {
        if self.r.is_none() { return; }
        // Limite de FPS (bateria): adia o desenho em vez de pular.
        if self.cfg.max_fps > 0 {
            let min = Duration::from_secs_f64(1.0 / self.cfg.max_fps as f64);
            let since = self.last_draw.elapsed();
            if since < min { self.redraw_at = Some(Instant::now() + (min - since)); return; }
        }
        self.last_draw = Instant::now();
        let now = Instant::now();
        self.fetch_tab_preview();
        let theme = self.theme().clone();
        let ui = self.cfg.ui.clone();
        let opacity = self.cfg.opacity.clamp(0.1, 1.0);
        let status = self.status_segments();
        let tabs = self.frame.tabs.clone();
        let branches: Vec<String> = tabs.iter().map(|t| self.branch_of(&t.cwd)).collect();
        let tab_labels: Vec<(String, TabInfo, Option<Rgb>)> = tabs.iter().zip(&branches).map(|(t, b)| (self.tab_label(t, b), t.clone(), self.tab_color(t))).collect();
        let hp: Vec<(u64, String)> = self.frame.panes.iter().filter(|p| p.header).map(|p| (p.id, p.cwd.clone())).collect();
        let pane_branch: HashMap<u64, String> = hp.into_iter().map(|(id, c)| { let b = self.branch_of(&c); (id, b) }).collect();
        let syntax = self.syntax_spans();
        let wsl: HashSet<String> = self.cfg.domains.iter().filter(|d| d.cmd.first().is_some_and(|c| c.to_lowercase().contains("wsl"))).map(|d| d.name.clone()).collect();
        let ghost = self.ghost.clone();
        let st = if self.sys_panel { Some(self.stats.lock().unwrap().clone()) } else { None };
        let ext_view = self.ext_panel.as_ref().and_then(|k| self.exts.view(k).cloned());
        // Um badge por extensao: icone + nome curto + contadores (status) dela.
        // A chave "duck:<acao>" e de extensao interna: o clique roda a acao.
        let mut ext_badges: Vec<(String, String, String, Vec<(String, String)>)> = Vec::new();
        let ext_cfg = self.ext_settings();
        let builtin_on = |n: &str| ext_cfg[n]["enabled"].as_bool().unwrap_or(true);
        if builtin_on("system") {
            let st = self.stats.lock().unwrap();
            let chips = vec![(format!("CPU {}%", st.cpu.round()), if st.cpu >= 85.0 { "blocked".into() } else { String::new() }),
                             (format!("RAM {:.1}/{:.0}G", st.ram, st.ram_total), if st.ram_total > 0.0 && st.ram / st.ram_total >= 0.9 { "blocked".into() } else { String::new() })];
            ext_badges.push(("duck:sys_panel".into(), "\u{f4bc}".into(), "Sistema".into(), chips));
        }
        if builtin_on("agents") {
            let g = |k: &str| self.frame.agents.get(k).copied().unwrap_or(0);
            let chips: Vec<(String, String)> = [("blocked", "bloqueado"), ("working", "trabalhando"), ("done", "pronto")].iter()
                .filter(|(k, _)| g(k) > 0).map(|(k, t)| (format!("{} {t}", g(k)), k.to_string())).collect();
            ext_badges.push(("duck:agent_panel".into(), "\u{f06a9}".into(), "Agentes".into(), chips));
        }
        // A caixa de entrada nao e badge: o sino fica fixo ao lado do minimizar.
        let inbox_on = builtin_on("inbox");
        let inbox_n = self.frame.inbox_unread;
        let inbox_blocked = self.frame.agents.get("blocked").copied().unwrap_or(0) > 0;
        let shortcut_items: Vec<(String, String, Action)> = self.shortcuts.iter().map(|(label, a)| {
            let hint = match a { Action::Builtin(n, args) => self.hint_for(n, args.first().and_then(|v| v.as_str())), _ => String::new() };
            (label.clone(), hint, a.clone())
        }).collect();
        for v in &self.exts.views {
            if ext_badges.iter().any(|b| b.0.starts_with(&format!("{}/", v.ext))) { continue; }
            let label = self.exts.label(&v.ext);
            let sts: Vec<&super::ext::ExtStatus> = self.exts.status.iter().filter(|s| s.ext == v.ext).collect();
            // O status abre a visao dele; sem status, a 1a visao.
            let key = sts.first().filter(|s| !s.view.is_empty()).map(|s| format!("{}/{}", s.ext, s.view)).unwrap_or_else(|| v.key());
            // Tira o glifo do inicio ("⌂ 57" -> "57"): o icone ja esta no badge.
            let chips = sts.iter().map(|s| {
                let t = s.text.trim();
                let t = match t.split_once(' ') { Some((a, rest)) if !a.chars().any(|c| c.is_alphanumeric()) => rest.trim(), _ => t };
                (t.to_string(), s.color.clone())
            }).filter(|c| !c.0.is_empty() && c.0 != "0").collect();
            ext_badges.push((key, v.icon.clone(), label, chips));
        }
        let rank = |k: &str| self.ext_order.iter().position(|n| n == badge_name(k)).unwrap_or(usize::MAX);
        ext_badges.sort_by_key(|b| rank(&b.0)); // estavel: as sem posicao mantem a ordem
        let ext_siblings: Vec<(String, String)> = ext_view.as_ref().map(|cv| self.exts.views.iter().filter(|v| v.ext == cv.ext).map(|v| (v.key(), v.title.clone())).collect()).unwrap_or_default();
        let ext_open = self.ext_panel.clone();
        let (sys_open, agent_open) = (self.sys_panel, self.agent_panel.is_some());
        let chrome = self.chrome();
        let (ox, oy) = self.grid_origin();
        self.origin = (ox, oy);
        let domain_bg: HashMap<String, Rgb> = self.cfg.domains.iter().filter_map(|d| d.background.map(|b| (d.name.clone(), b))).collect();
        let gradient = self.cfg.background.gradient.clone();
        let mouse = self.mouse;
        let focused = self.focused;
        let custom = self.custom_titlebar();
        let tab_bar = self.cfg.tab_bar.clone();
        let anim_on = ui.animations;
        let t_now = self.started.elapsed().as_secs_f32();
        let ws_name = self.frame.workspace.clone();
        let mut animating = false;
        let mut pulse = false;
        self.hits.clear();
        self.tab_rects.clear();
        let r = self.r.as_mut().unwrap();
        let maximized = r.window.is_maximized();
        let (cw, ch) = (r.cell_w, r.cell_h);
        let (w, h) = (r.width(), r.height());
        let alpha = if r.transparent { opacity } else { 1.0 };
        let mut base = Layer::default();
        let mut flt = Layer::default();
        let mut over = Layer::default();
        let mut modal = Layer::default();
        let txt = |x: f32, y: f32, text: &str, color: Rgb| mt(x, y, text, color);
        // Com cartoes, o fundo da janela e um tom abaixo dos panes.
        let canvas = if ui.cards { mix(theme.bg, Rgb(0, 0, 0), 0.30) } else { theme.bg };
        if gradient.len() >= 2 {
            let n = 48;
            for i in 0..n {
                let t = i as f32 / (n - 1) as f32 * (gradient.len() - 1) as f32;
                let (a, b) = (t.floor() as usize, (t.ceil() as usize).min(gradient.len() - 1));
                let c = config::mix_rgb(gradient[a], gradient[b], t - t.floor());
                base.quads.push(Quad { x: 0.0, y: h * i as f32 / n as f32, w, h: h / n as f32 + 1.0, color: lin(c, alpha) });
            }
        }

        // ---- panes
        let active = self.frame.active_pane;
        let multi = self.frame.panes.iter().filter(|p| !p.floating).count() > 1;
        let has_float = self.frame.panes.iter().any(|q| q.floating);
        let glow_c = tab_labels.iter().find(|t| t.1.active).and_then(|t| t.2).unwrap_or(theme.accent);
        let pix = (cw * 0.5 - 3.0).clamp(1.0, 6.0);
        let piy = (ch * 0.5 - 3.0).clamp(1.0, 6.0);
        let rad = ui.radius;
        let tab_key = format!("{}#{}", ws_name, tabs.iter().position(|t| t.active).unwrap_or(0));
        let same_tab = tab_key == self.anim_tab_key;
        let mut new_rects: HashMap<u64, [f32; 4]> = HashMap::new();
        let mark_base = (base.shapes.len(), base.quads.len(), base.texts.len());
        let mark_over = (over.shapes.len(), over.quads.len(), over.texts.len());
        let idle_cursor = self.last_key.elapsed() > Duration::from_millis(600);
        let mut blinking = false;
        for p in &self.frame.panes {
            let px = ox + p.x as f32 * cw;
            let py = oy + p.y as f32 * ch;
            let (pw, ph) = (p.w as f32 * cw, p.h as f32 * ch);
            let hdr = if p.header { ch } else { 0.0 };
            let card = if p.floating { [px - cw, py - ch * 0.5 - hdr, pw + 2.0 * cw, ph + ch + hdr] } else { [px - pix, py - piy - hdr, pw + 2.0 * pix, ph + 2.0 * piy + hdr] };
            new_rects.insert(p.id, card);
            // Animacao: o cartao desliza do lugar antigo; pane novo cresce e aparece.
            if anim_on && same_tab {
                match self.prev_rects.get(&p.id) {
                    Some(prev) if *prev != card => {
                        let from = self.rect_anim.get(&p.id).map(|a| lerp4(a.0, a.1, ease(a.2.elapsed().as_secs_f32() / 0.14))).unwrap_or(*prev);
                        self.rect_anim.insert(p.id, (from, card, now, false));
                    }
                    None if !self.prev_rects.is_empty() => {
                        let c = [card[0] + card[2] * 0.03, card[1] + card[3] * 0.03, card[2] * 0.94, card[3] * 0.94];
                        self.rect_anim.insert(p.id, (c, card, now, true));
                    }
                    _ => {}
                }
            }
            let mut shown = card;
            let mut veil = 0.0;
            if let Some(a) = self.rect_anim.get(&p.id).copied() {
                let k = a.2.elapsed().as_secs_f32() / 0.14;
                if k >= 1.0 || a.1 != card { self.rect_anim.remove(&p.id); }
                else { shown = lerp4(a.0, a.1, ease(k)); if a.3 { veil = 1.0 - ease(k); } animating = true; }
            }
            let layer = if p.floating { &mut flt } else { &mut base };
            let pane_bg = domain_bg.get(&p.domain).copied().unwrap_or(theme.bg);
            let is_active = p.id == active;
            if ui.cards || p.floating {
                let [cx, cy, cwid, chei] = shown;
                layer.shapes.push(Shape::shadow(cx, cy + 3.0, cwid, chei, [0.0, 0.0, 0.0, if p.floating { 0.55 } else { 0.35 }], rad, if p.floating { 16.0 } else { 8.0 }));
                let glow = ui.glow && is_active && (multi || p.floating) && focused;
                if glow { layer.shapes.push(Shape::shadow(cx, cy, cwid, chei, lin(glow_c, 0.32), rad, 7.0)); }
                layer.shapes.push(Shape::rect(cx, cy, cwid, chei, lin(pane_bg, if p.floating { 1.0 } else { alpha }), rad));
                if glow { layer.shapes.push(Shape::ring(cx, cy, cwid, chei, lin(glow_c, 0.85), rad, 1.5)); }
                else if p.floating { layer.shapes.push(Shape::ring(cx, cy, cwid, chei, lin(theme.accent, 0.8), rad, 1.0)); }
            } else if pane_bg != theme.bg {
                layer.quads.push(Quad { x: px, y: py, w: pw, h: ph, color: lin(pane_bg, alpha) });
            }

            // Blocos de comando como cartoes (antes do texto).
            let cursor_row = p.cursor.as_ref().map(|c| c.1 as i64);
            let last_row = (0..p.h as usize).rev().find(|r| !p.row_string(*r).trim().is_empty()).map(|r| r as i64).unwrap_or(0);
            let block_span = |i: usize| -> Option<(i64, i64)> {
                let b = &p.blocks[i];
                if b.cmd.is_empty() && b.exit.is_none() && b.out.is_none() { return None; }
                let top = b.row.max(0);
                let bottom = b.end.map(|e| e - 1).or_else(|| p.blocks.get(i + 1).map(|n| n.row - 1))
                    .unwrap_or_else(|| cursor_row.unwrap_or(last_row)).min(p.h as i64 - 1);
                (bottom >= top).then_some((top, bottom))
            };
            if ui.cards && !p.alt_screen {
                for i in 0..p.blocks.len() {
                    let Some((top, bottom)) = block_span(i) else { continue };
                    let b = &p.blocks[i];
                    let c = match b.exit { Some(0) => mix(pane_bg, theme.done, 0.05), Some(_) => mix(pane_bg, theme.blocked, 0.07), None => mix(pane_bg, theme.fg, 0.04) };
                    layer.shapes.push(Shape::rect(px - pix * 0.6, py + top as f32 * ch + 1.0, pw + pix * 1.2, (bottom - top + 1) as f32 * ch - 2.0, lin(c, 1.0), 5.0));
                }
            }

            for (row, runs) in p.lines().iter().enumerate() {
                let y = py + row as f32 * ch;
                for Run(col, text, fg, bg, fl, link) in runs {
                    let x = px + *col as f32 * cw;
                    let step = if fl & 32 != 0 { 2.0 } else { 1.0 };
                    let wpx = text.chars().count() as f32 * cw * step;
                    if *bg != -2 { layer.quads.push(Quad { x, y, w: wpx, h: ch, color: lin(theme.resolve(*bg), alpha) }); }
                    let mut color = theme.resolve(*fg);
                    if fl & 16 != 0 { color = mix(color, theme.bg, 0.45); }
                    if fl & 4 != 0 || link.is_some() { layer.quads.push(Quad { x, y: y + ch - 2.0, w: wpx, h: 1.0, color: lin(color, if link.is_some() { 0.6 } else { 1.0 }) }); }
                    if fl & 8 != 0 { layer.quads.push(Quad { x, y: y + ch / 2.0, w: wpx, h: 1.0, color: lin(color, 1.0) }); }
                    if !text.trim().is_empty() {
                        if step > 1.0 {
                            for (i, c) in text.chars().enumerate() {
                                layer.texts.push(TextItem { bold: fl & 1 != 0, italic: fl & 2 != 0, ..txt(x + i as f32 * cw * 2.0, y, &c.to_string(), color) });
                            }
                        } else {
                            layer.texts.push(TextItem { bold: fl & 1 != 0, italic: fl & 2 != 0, ..txt(x, y, text, color) });
                        }
                    }
                }
            }
            // Realce do comando digitado: cobre o texto e redesenha colorido.
            for (sp, row, spans) in &syntax {
                if *sp != p.id { continue; }
                let y = py + *row as f32 * ch;
                for (col, text, color, underline) in spans {
                    let x = px + *col as f32 * cw;
                    let wpx = text.chars().count() as f32 * cw;
                    over.quads.push(Quad { x, y, w: wpx, h: ch, color: lin(pane_bg, 1.0) });
                    over.texts.push(txt(x, y, text, *color));
                    if *underline { over.quads.push(Quad { x, y: y + ch - 2.0, w: wpx, h: 1.0, color: lin(*color, 0.7) }); }
                }
            }

            // Cabecalho do pane: pasta, branch, comando rodando, agente.
            if p.header {
                let hy = py - ch;
                let size = 0.85;
                let ty = hy + (ch - ch * size) / 2.0;
                let mut x = px + 2.0;
                let dim = if is_active { theme.fg } else { theme.muted };
                if !p.profile.is_empty() {
                    draw_avatar(r, &mut over, x, hy + (ch - 14.0) / 2.0, 14.0, avatar_color(&theme, &ui, &p.profile), &p.profile);
                    x += 20.0;
                }
                let folder = p.cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").to_string();
                over.texts.push(TextItem { size, ..txt(x, ty, "\u{f07b}", theme.muted) });
                x += cw * size + 6.0;
                over.texts.push(TextItem { clip_right: Some(px + pw), ..uit(x, ty, &folder, dim, size) });
                x += r.measure(&folder, true, size) + 10.0;
                if let Some(b) = pane_branch.get(&p.id).filter(|b| !b.is_empty()) {
                    over.texts.push(TextItem { size, ..txt(x, ty, "\u{e725}", theme.accent) });
                    x += cw * size + 4.0;
                    over.texts.push(TextItem { clip_right: Some(px + pw), ..uit(x, ty, b, theme.accent, size) });
                    x += r.measure(b, true, size) + 10.0;
                }
                let right = match (&p.running, p.state.as_str()) {
                    (Some((c, secs)), _) => Some((format!("▶ {} · {}", c.chars().take(40).collect::<String>(), fmt_ms(secs * 1000)), theme.working)),
                    (None, "working") => Some(("trabalhando…".to_string(), theme.accent)),
                    (None, "blocked") => Some(("pede aprovacao".to_string(), theme.working)),
                    (None, "done") => Some(("terminou ✓".to_string(), theme.done)),
                    _ => None,
                };
                if let Some((label, c)) = right {
                    let lw = r.measure(&label, true, size);
                    let lx = (px + pw - lw - 4.0).max(x);
                    over.texts.push(TextItem { clip_right: Some(px + pw), ..uit(lx, ty, &label, if p.attention { theme.blocked } else { c }, size) });
                }
                if p.state == "working" { pulse = true; }
                if let Some((st, v)) = p.progress {
                    let c = match st { 2 => theme.blocked, 4 => theme.working, _ => theme.done };
                    let f = if st == 3 { pulse = true; (t_now * 0.8).fract() } else { v as f32 / 100.0 };
                    over.shapes.push(Shape::rect(px, py - 2.0, pw, 2.0, lin(theme.muted, 0.2), 1.0));
                    over.shapes.push(Shape::rect(px, py - 2.0, pw * f, 2.0, lin(c, 1.0), 1.0));
                }
            }

            // Gutter, selo de exit/duracao e botoes de cada bloco.
            let last_done = p.blocks.iter().rev().find(|b| b.exit.is_some()).map(|b| b.abs);
            for (i, b) in p.blocks.iter().enumerate() {
                let span = block_span(i);
                if b.row >= 0 && !p.row_string(b.row as usize).trim().is_empty() {
                    let y = py + b.row as f32 * ch;
                    let c = match b.exit { Some(0) => theme.done, Some(_) => theme.blocked, None => theme.muted };
                    over.quads.push(Quad { x: px - 4.0, y: y + 1.0, w: 2.0, h: ch - 2.0, color: lin(c, 0.9) });
                    let label = match b.exit {
                        Some(0) => b.ms.filter(|ms| *ms >= 1000).map(|ms| format!("✓ {}", fmt_ms(ms))).unwrap_or_else(|| "✓".into()),
                        Some(e) => format!("✗ {e}{}", b.ms.map(|ms| format!(" · {}", fmt_ms(ms))).unwrap_or_default()),
                        None => String::new(),
                    };
                    let used = p.row_string(b.row as usize).trim_end().chars().count() as f32 * cw;
                    let mut right_x = px + pw - 4.0;
                    if !label.is_empty() {
                        let lw = r.measure(&label, true, 0.8) + 14.0;
                        if used + lw + 8.0 < pw {
                            right_x -= lw;
                            over.shapes.push(Shape::rect(right_x, y + 2.0, lw, ch - 4.0, lin(c, 0.20), (ch - 4.0) / 2.0));
                            over.texts.push(uit(right_x + 7.0, y + (ch - ch * 0.8) / 2.0, &label, c, 0.8));
                            right_x -= 4.0;
                        }
                    }
                    // Botoes ao passar o mouse sobre o cartao do bloco.
                    let hovering = span.is_some_and(|(t, bo)| mouse.0 >= px && mouse.0 < px + pw && mouse.1 >= py + t as f32 * ch && mouse.1 < py + (bo + 1) as f32 * ch);
                    if hovering && b.out.is_some() && !p.alt_screen {
                        let default = ui.collapse_lines > 0 && b.n as u32 > ui.collapse_lines && Some(b.abs) != last_done;
                        let col = self.collapsed.get(&(p.id, b.abs)).copied().unwrap_or(default);
                        let buttons: [(&str, &str, Vec<Value>); 5] = [
                            ("\u{f08d}", "block_pin", vec![json!(p.id), json!(b.abs)]),
                            ("\u{f06a9}", "block_agent", vec![json!(p.id), json!(b.abs)]),
                            (if col { "\u{f054}" } else { "\u{f078}" }, "block_toggle", vec![json!(p.id), json!(b.abs), json!(col)]),
                            ("\u{f01e}", "block_rerun", vec![json!(p.id), json!(b.cmd)]),
                            ("\u{f0c5}", "block_copy", vec![json!(p.id), json!(b.abs)]),
                        ];
                        let bs = ch - 2.0;
                        for (icon, act, args) in buttons {
                            let bx = right_x - bs;
                            if bx < px + used + 4.0 { break; }
                            let br = [bx, y + 1.0, bs, bs];
                            let hv = inside(br, mouse);
                            over.shapes.push(Shape::rect(bx, y + 1.0, bs, bs, lin(if hv { theme.accent } else { mix(pane_bg, theme.fg, 0.12) }, if hv { 0.35 } else { 0.9 }), 5.0));
                            over.texts.push(TextItem { size: 0.8, ..txt(bx + (bs - cw * 0.8) / 2.0, y + 1.0 + (bs - ch * 0.8) / 2.0, icon, theme.fg) });
                            self.hits.push(Hit { r: br, action: Action::Builtin(act.into(), args) });
                            right_x = bx - 3.0;
                        }
                    }
                }
                // Saida longa recolhida: veu com o total de linhas.
                if let (Some(out), Some(end)) = (b.out, b.end) {
                    let default = ui.collapse_lines > 0 && b.n as u32 > ui.collapse_lines && Some(b.abs) != last_done;
                    if self.collapsed.get(&(p.id, b.abs)).copied().unwrap_or(default) && !p.alt_screen {
                        let vs = (out + 3).max(0);
                        let ve = (end - 1).min(p.h as i64 - 1);
                        if ve >= vs {
                            let vr = [px - 2.0, py + vs as f32 * ch, pw + 4.0, (ve - vs + 1) as f32 * ch];
                            over.shapes.push(Shape::rect(vr[0], vr[1], vr[2], vr[3], lin(mix(pane_bg, theme.fg, 0.05), 1.0), 5.0));
                            let label = format!("▸ {} linhas recolhidas · clique para abrir", b.n);
                            let lw = r.measure(&label, true, 0.85);
                            over.texts.push(uit(px + (pw - lw) / 2.0, vr[1] + (ch - ch * 0.85) / 2.0, &label, theme.muted, 0.85));
                            self.hits.push(Hit { r: vr, action: Action::Builtin("block_toggle".into(), vec![json!(p.id), json!(b.abs), json!(true)]) });
                        }
                    }
                }
            }

            // Minimapa: barra de rolagem com os comandos (verde ok, vermelho falhou) e a busca.
            if p.history > 0 {
                let total = (p.history + p.h as i64) as f32;
                let track = [px + pw - 2.0, py, 8.0, ph];
                let hover = inside([track[0] - 6.0, track[1], track[2] + 8.0, track[3]], mouse);
                let tw = if hover || p.scroll > 0 { 7.0 } else { 3.0 };
                let sx = px + pw + pix - tw - 1.0;
                let thumb_h = (ph * p.h as f32 / total).max(12.0);
                let top_frac = (p.history - p.scroll) as f32 / total;
                over.shapes.push(Shape::rect(sx, py, tw, ph, lin(theme.muted, if hover { 0.18 } else { 0.06 }), tw / 2.0));
                over.shapes.push(Shape::rect(sx, py + top_frac * ph, tw, thumb_h, lin(theme.muted, if hover || p.scroll > 0 { 0.55 } else { 0.25 }), tw / 2.0));
                for (frac, code) in p.ticks.iter().rev().take(400) {
                    let c = match code { 1 => theme.done, 2 => theme.blocked, _ => theme.muted };
                    over.quads.push(Quad { x: sx - 1.0, y: py + *frac as f32 * ph, w: tw + 2.0, h: 2.0, color: lin(c, 0.85) });
                }
                if let Some((sp, lines, cur, hist, tot)) = &self.search_hits {
                    if *sp == p.id && *tot > 0 {
                        for (k, l) in lines.iter().enumerate().take(2000) {
                            let f = (*l + *hist) as f32 / *tot as f32;
                            over.quads.push(Quad { x: sx - 2.0, y: py + f * ph, w: tw + 4.0, h: if k == *cur { 3.0 } else { 2.0 }, color: lin(theme.working, if k == *cur { 1.0 } else { 0.7 }) });
                        }
                    }
                }
                self.hits.push(Hit { r: [sx - 4.0, py, tw + 8.0, ph], action: Action::Builtin("minimap".into(), vec![json!(p.id), json!(py), json!(ph), json!(p.history), json!(p.h)]) });
            }
            // Selecao (mouse ou modo copia)
            let sel = if let Some(c) = self.copy.as_ref().filter(|c| c.pane == p.id) { c.anchor.map(|a| (a, c.cur)) }
                else { self.sel.as_ref().filter(|s| s.pane == p.id && s.a != s.b).map(|s| (s.a, s.b)) };
            if let Some((a, b)) = sel {
                let (s, e) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
                for row in s.row.max(0)..=e.row.min(p.h as i32 - 1) {
                    let c0 = if row == s.row { s.col } else { 0 };
                    let c1 = if row == e.row { e.col + 1 } else { p.w as i32 };
                    over.quads.push(Quad { x: px + c0 as f32 * cw, y: py + row as f32 * ch, w: (c1 - c0).max(0) as f32 * cw, h: ch, color: lin(theme.selection, 0.55) });
                }
            }
            // Busca: destaca o termo na tela visivel
            if let Some(sr) = self.search.as_ref().filter(|s| s.pane == p.id && !s.query.is_empty()) {
                let re = if sr.query.len() > 2 && sr.query.starts_with('/') && sr.query.ends_with('/') {
                    regex::RegexBuilder::new(&sr.query[1..sr.query.len() - 1]).case_insensitive(true).build().ok()
                } else { regex::RegexBuilder::new(&regex::escape(&sr.query)).case_insensitive(true).build().ok() };
                if let Some(re) = re {
                    for row in 0..p.h as usize {
                        let line = p.row_string(row);
                        for m in re.find_iter(&line) {
                            let c0 = line[..m.start()].chars().count();
                            over.shapes.push(Shape::rect(px + c0 as f32 * cw - 1.0, py + row as f32 * ch, m.as_str().chars().count() as f32 * cw + 2.0, ch, lin(theme.working, 0.40), 3.0));
                        }
                    }
                }
            }
            // Cursor: desliza ate a celula nova (e pisca suave, se ligado).
            if let Some((cc, cr, shape)) = &p.cursor {
                let tx = px + *cc as f32 * cw;
                let ty = py + *cr as f32 * ch;
                let (mut x, mut y) = (tx, ty);
                let is_act = is_active && focused;
                if is_act && ui.cursor_smooth && anim_on {
                    let target = [tx, ty];
                    match self.cursor_anim {
                        Some((from, to, t0)) if to == target => {
                            let k = t0.elapsed().as_secs_f32() / 0.08;
                            if k < 1.0 { let e = ease(k); x = from[0] + (tx - from[0]) * e; y = from[1] + (ty - from[1]) * e; animating = true; }
                        }
                        _ => {
                            let from = self.cursor_shown;
                            let far = (from[0] - tx).abs() > w * 0.5 || (from[1] - ty).abs() > h * 0.5 || from == [0.0, 0.0];
                            self.cursor_anim = Some((if far { target } else { from }, target, now));
                            if !far { x = from[0]; y = from[1]; animating = true; }
                        }
                    }
                    self.cursor_shown = [x, y];
                }
                let mut ca = 1.0;
                if is_act && ui.cursor_blink && idle_cursor {
                    ca = 0.55 + 0.45 * (t_now * std::f32::consts::TAU / 1.1).cos();
                    blinking = true;
                }
                let layer = if p.floating { &mut flt } else { &mut base };
                match (shape.as_str(), is_act) {
                    ("beam", true) => layer.shapes.push(Shape::rect(x, y + 1.0, 2.0, ch - 2.0, lin(theme.cursor, ca), 1.0)),
                    ("underline", true) => layer.shapes.push(Shape::rect(x, y + ch - 2.0, cw, 2.0, lin(theme.cursor, ca), 1.0)),
                    (_, true) => {
                        layer.shapes.push(Shape::rect(x, y, cw, ch, lin(theme.cursor, ca), 2.0));
                        let under = p.row_chars(*cr as usize).get(*cc as usize).copied().unwrap_or(' ');
                        if under != ' ' && x == tx && y == ty { over.texts.push(txt(x, y, &under.to_string(), theme.bg)); }
                    }
                    _ => layer.shapes.push(Shape::ring(x, y, cw, ch, lin(theme.muted, 1.0), 2.0, 1.0)),
                }
            }
            if let Some(c) = self.copy.as_ref().filter(|c| c.pane == p.id) {
                over.quads.push(Quad { x: px + c.cur.col as f32 * cw, y: py + c.cur.row as f32 * ch, w: cw, h: ch, color: lin(theme.accent, 0.6) });
            }
            // Sugestao inline (texto fantasma) depois do cursor.
            if let (Some(g), true, Some((cc, cr, _))) = (&ghost, is_active, &p.cursor) {
                let gx = px + (*cc as f32 + 1.0) * cw;
                let room = ((p.w as i32 - *cc as i32 - 1).max(0)) as usize;
                let shown: String = g.chars().take(room).collect();
                over.texts.push(TextItem { alpha: 0.45, italic: true, ..txt(gx - cw, py + *cr as f32 * ch, &shown, theme.muted) });
            }
            // Bloco fixado: as ultimas linhas da saida presas no topo do pane.
            if let Some((_, cmd, lines)) = self.pinned.get(&p.id) {
                let n = lines.len().min(8);
                let bh = (n as f32 + 1.0) * ch + 6.0;
                over.shapes.push(Shape::shadow(px, py + 2.0, pw, bh, [0.0, 0.0, 0.0, 0.35], 6.0, 6.0));
                over.shapes.push(Shape::rect(px, py, pw, bh, lin(mix(pane_bg, theme.fg, 0.07), 1.0), 6.0));
                over.shapes.push(Shape::rect(px, py, 3.0, bh, lin(theme.accent, 1.0), 1.5));
                over.texts.push(TextItem { size: 0.85, ..txt(px + 8.0, py + 2.0, "\u{f08d}", theme.accent) });
                over.texts.push(TextItem { clip_right: Some(px + pw - 30.0), ..uit(px + 8.0 + cw, py + 2.0, &format!("$ {cmd}"), theme.fg, 0.9) });
                let xr = [px + pw - 24.0, py + 2.0, 20.0, ch - 2.0];
                over.texts.push(uit(xr[0] + 5.0, py + 2.0, "✕", if inside(xr, mouse) { theme.fg } else { theme.muted }, 0.8));
                self.hits.push(Hit { r: xr, action: Action::Builtin("unpin".into(), vec![json!(p.id)]) });
                for (k, l) in lines[lines.len() - n..].iter().enumerate() {
                    over.texts.push(TextItem { clip_right: Some(px + pw - 4.0), ..txt(px + 8.0, py + 4.0 + ch * (k as f32 + 1.0), l, theme.fg) });
                }
            } else if let Some(cmd) = &p.sticky {
                over.shapes.push(Shape::rect(px, py, pw, ch, lin(mix(pane_bg, theme.fg, 0.08), 0.97), 5.0));
                over.texts.push(TextItem { clip_right: Some(px + pw - cw * 8.0), ..txt(px + cw * 0.5, py, &format!("▲ $ {cmd}"), theme.fg) });
            }
            let pill = |l: &mut Layer, r: &mut Renderer, x: f32, y: f32, label: &str, bg: Rgb, fg: Rgb| -> f32 {
                let lw = r.measure(label, true, 0.8) + 14.0;
                l.shapes.push(Shape::rect(x, y + 2.0, lw, ch - 4.0, lin(bg, 1.0), (ch - 4.0) / 2.0));
                l.texts.push(uit(x + 7.0, y + (ch - ch * 0.8) / 2.0, label, fg, 0.8));
                lw
            };
            if p.queued > 0 {
                let label = format!("{} recado(s) na fila", p.queued);
                let lw = r.measure(&label, true, 0.8) + 14.0;
                pill(&mut over, r, px + pw - lw - 10.0, py + ph - ch, &label, theme.working, theme.bg);
            }
            if self.bell_until.get(&p.id).is_some_and(|t| *t > Instant::now()) {
                over.shapes.push(Shape::rect(shown[0], shown[1], shown[2], shown[3], lin(theme.fg, 0.10), rad));
            }
            // Pane inativo apagado (ou todos fora o ativo, no modo foco).
            if !p.floating && ((!is_active && (multi || self.focus_mode)) || has_float) {
                let dim = if has_float { 0.45 } else if self.focus_mode { 0.70 } else { 0.30 };
                (if has_float { &mut base } else { &mut over }).shapes.push(Shape::rect(shown[0], shown[1], shown[2], shown[3], lin(canvas, dim), rad));
            }
            if veil > 0.0 { over.shapes.push(Shape::rect(shown[0], shown[1], shown[2], shown[3], lin(pane_bg, veil), rad)); }
            if !p.floating && !ui.cards {
                let border = if p.attention { theme.blocked } else { theme.border };
                if p.x + p.w < self.frame.cols { over.quads.push(Quad { x: px + pw + cw / 2.0, y: py, w: 1.0, h: ph, color: lin(border, 1.0) }); }
                if p.y + p.h < self.frame.rows { over.quads.push(Quad { x: px, y: py + ph + ch / 2.0, w: pw, h: 1.0, color: lin(border, 1.0) }); }
            } else if p.attention && !p.floating {
                over.shapes.push(Shape::ring(shown[0], shown[1], shown[2], shown[3], lin(theme.blocked, 0.9), rad, 1.5));
            }
            if p.scroll > 0 {
                let label = format!("{}/{}", p.scroll, p.history);
                let lw = r.measure(&label, true, 0.8) + 14.0;
                pill(&mut over, r, px + pw - lw - 12.0, py + if self.pinned.contains_key(&p.id) { 0.0 } else { 2.0 }, &label, theme.accent, theme.bg);
            }
            if p.private { pill(&mut over, r, px + 4.0, py, "privado", theme.working, theme.bg); }
        }
        // Divisor sob o mouse (ou sendo arrastado) engrossa.
        let div = self.div_drag.map(|(id, v, _)| (id, v)).or_else(|| if mouse.1 > chrome.top { None } else { None });
        let hover_div = div.or_else(|| {
            let (ox, oy) = (ox, oy);
            self.frame.panes.iter().filter(|p| !p.floating).find_map(|p| {
                let xr = ox + (p.x + p.w) as f32 * cw + cw * 0.5;
                let yb = oy + (p.y + p.h) as f32 * ch + ch * 0.5;
                let py = oy + p.y as f32 * ch;
                let px = ox + p.x as f32 * cw;
                if p.x + p.w < self.frame.cols && (mouse.0 - xr).abs() <= 4.0 && mouse.1 >= py && mouse.1 < py + p.h as f32 * ch { return Some((p.id, true)); }
                if p.y + p.h < self.frame.rows && (mouse.1 - yb).abs() <= 4.0 && mouse.0 >= px && mouse.0 < px + p.w as f32 * cw { return Some((p.id, false)); }
                None
            })
        });
        if let Some((id, vertical)) = hover_div {
            if let Some(p) = self.frame.panes.iter().find(|p| p.id == id) {
                let (px, py) = (ox + p.x as f32 * cw, oy + p.y as f32 * ch);
                if vertical { over.shapes.push(Shape::rect(ox + (p.x + p.w) as f32 * cw + cw * 0.5 - 1.5, py, 3.0, p.h as f32 * ch, lin(theme.accent, 0.9), 1.5)); }
                else { over.shapes.push(Shape::rect(px, oy + (p.y + p.h) as f32 * ch + ch * 0.5 - 1.5, p.w as f32 * cw, 3.0, lin(theme.accent, 0.9), 1.5)); }
            }
        }
        // Panes que fecharam: o cartao some encolhendo.
        if anim_on && same_tab {
            for (id, rc) in &self.prev_rects {
                if !new_rects.contains_key(id) { self.ghosts.push((*rc, now)); }
            }
        }
        self.ghosts.retain(|g| g.1.elapsed() < Duration::from_millis(140));
        for (rc, t0) in &self.ghosts {
            let k = ease(t0.elapsed().as_secs_f32() / 0.14);
            let s = 1.0 - 0.06 * k;
            over.shapes.push(Shape::rect(rc[0] + rc[2] * (1.0 - s) / 2.0, rc[1] + rc[3] * (1.0 - s) / 2.0, rc[2] * s, rc[3] * s, lin(theme.bg, 1.0 - k), rad));
            animating = true;
        }
        self.prev_rects = new_rects;
        self.anim_tab_key = tab_key;

        // Troca de workspace: o conteudo entra deslizando.
        if let Some((t0, dir)) = self.ws_anim {
            let k = t0.elapsed().as_secs_f32() / 0.18;
            if k >= 1.0 || !anim_on { self.ws_anim = None; } else {
                animating = true;
                let dx = (1.0 - ease(k)) * w * 0.06 * dir;
                shift(&mut base, mark_base, dx);
                shift(&mut over, mark_over, dx);
                over.shapes.push(Shape::rect(chrome.left, chrome.top, w - chrome.left - chrome.right, h - chrome.top - chrome.bottom, lin(canvas, (1.0 - ease(k)) * 0.85), 0.0));
            }
        }

        // ---- hints: rotulo sobre cada alvo
        if let Some((hits, typed)) = &self.hints {
            for hnt in hits.iter().filter(|h| h.label.starts_with(typed.as_str())) {
                let Some(p) = self.frame.panes.iter().find(|p| p.id == hnt.pane) else { continue };
                let x = ox + (p.x as f32 + hnt.col as f32) * cw;
                let y = oy + (p.y as f32 + hnt.row as f32) * ch;
                over.quads.push(Quad { x, y, w: hnt.len as f32 * cw, h: ch, color: lin(theme.selection, 0.5) });
                over.shapes.push(Shape::rect(x, y, hnt.label.len() as f32 * cw + 4.0, ch, lin(theme.working, 1.0), 3.0));
                over.texts.push(TextItem { bold: true, ..txt(x + 2.0, y, &hnt.label, theme.bg) });
            }
        }

        // ---- barra de titulo: inicio, abas, sino e botoes da janela
        let bar_h = chrome.bar_h;
        let ctx = TabCtx { theme: &theme, mouse, t: t_now, drag: self.tab_drag };
        if chrome.top > 0.0 {
            base.shapes.push(Shape::rect(0.0, 0.0, w, chrome.top, lin(theme.tab_bar, alpha), 0.0));
            let mut x = 8.0;
            {
                let bw = 30.0;
                let br = [x, 7.0, bw, bar_h - 14.0];
                let hv = inside(br, mouse);
                if hv { base.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), 6.0)); }
                base.texts.push(TextItem { size: 0.9, ..txt(x + (bw - cw * 0.9) / 2.0, (bar_h - ch * 0.9) / 2.0, "\u{f015}", if hv { theme.fg } else { theme.muted }) });
                self.hits.push(Hit { r: br, action: Action::Builtin("home".into(), vec![]) });
                x += bw + 4.0;
            }
            // Chip do workspace (troca de workspace).
            let wsz = 0.85;
            let ww = r.measure(&ws_name, true, wsz) + 34.0;
            let wr = [x, 7.0, ww, bar_h - 14.0];
            let hv = inside(wr, mouse);
            base.shapes.push(Shape::rect(wr[0], wr[1], wr[2], wr[3], lin(mix(theme.tab_bar, theme.fg, if hv { 0.14 } else { 0.07 }), 1.0), 7.0));
            base.texts.push(TextItem { size: 0.85, ..txt(x + 8.0, (bar_h - ch * 0.85) / 2.0, "\u{f009}", theme.accent) });
            base.texts.push(uit(x + 24.0, (bar_h - ch * wsz) / 2.0, &ws_name, theme.fg, wsz));
            self.hits.push(Hit { r: wr, action: Action::Builtin("workspace_switch".into(), vec![]) });
            x += ww + 8.0;
            let mut rx = w;
            if custom {
                for (glyph, act) in [("✕", "win_close"), (if maximized { "❐" } else { "☐" }, "win_max"), ("─", "win_min")] {
                    let bw = 46.0;
                    rx -= bw;
                    let br = [rx, 0.0, bw, bar_h];
                    let hv = inside(br, mouse);
                    if hv { base.shapes.push(Shape::rect(rx, 0.0, bw, bar_h, lin(if act == "win_close" { Rgb(196, 43, 28) } else { mix(theme.tab_bar, theme.fg, 0.12) }, 1.0), 0.0)); }
                    let gw = r.measure(glyph, true, 0.85);
                    base.texts.push(uit(rx + (bw - gw) / 2.0, (bar_h - ch * 0.85) / 2.0, glyph, if hv && act == "win_close" { Rgb(255, 255, 255) } else { theme.fg }, 0.85));
                    self.hits.push(Hit { r: br, action: Action::Builtin(act.into(), vec![]) });
                }
            }
            // Sino da caixa de entrada: fixo ao lado do minimizar.
            if inbox_on {
                let bw = 46.0;
                rx -= bw;
                draw_bell(r, &mut base, &mut self.hits, &theme, mouse, [rx, 0.0, bw, bar_h], inbox_n as u64, inbox_blocked, true);
            }
            let mut tabs_end = x;
            if tab_bar == "top" && !self.focus_mode {
                self.tab_row = (0.0, bar_h);
                let (end, pl) = tabs_row(r, &mut base, &mut self.hits, &mut self.tab_rects, x, 0.0, rx - 60.0, bar_h, &tab_labels, &ctx);
                tabs_end = end;
                pulse |= pl;
            }
            if tabs_end == x {
                // Sem abas no topo: titulo do pane ativo no espaco livre.
                let title = self.frame.panes.iter().find(|p| p.id == active).map(|p| p.title.clone()).unwrap_or_default();
                let tw = r.measure(&title, true, 0.9);
                let free = (x, rx - 20.0);
                let tx = ((free.0 + free.1 - tw) / 2.0).max(free.0);
                base.texts.push(TextItem { clip_right: Some(free.1), ..uit(tx, (bar_h - ch * 0.9) / 2.0, &title, theme.muted, 0.9) });
            }
        }
        // ---- embaixo: abas (se a barra for embaixo), rodape dos badges e barra de atalhos
        let tabs_y = h - chrome.bottom;
        if chrome.tabs_h > 0.0 {
            base.shapes.push(Shape::rect(0.0, tabs_y, w, chrome.tabs_h, lin(theme.tab_bar, alpha), 0.0));
            self.tab_row = (tabs_y, bar_h);
            let (_, pl) = tabs_row(r, &mut base, &mut self.hits, &mut self.tab_rects, 8.0, tabs_y, w - 8.0, bar_h, &tab_labels, &ctx);
            pulse |= pl;
        }
        let foot_y = tabs_y + chrome.tabs_h;
        let fh = chrome.foot_h;
        self.badge_row = (foot_y, fh);
        self.badge_rects.clear();
        if fh + chrome.keys_h > 0.0 {
            base.shapes.push(Shape::rect(0.0, foot_y, w, fh + chrome.keys_h, lin(theme.tab_bar, alpha), 0.0));
            if chrome.tabs_h == 0.0 { base.quads.push(Quad { x: 0.0, y: foot_y, w, h: 1.0, color: lin(theme.border, 0.6) }); }
        }
        if fh > 0.0 {
            let (py, ph) = (foot_y + 4.0, fh - 8.0);
            let cy = |size: f32| foot_y + (fh - ch * size) / 2.0;
            // Sem barra de titulo, o workspace vem para o rodape.
            let mut x = 8.0;
            if chrome.top == 0.0 {
                let wsz = 0.78;
                let ww = r.measure(&ws_name, true, wsz) + 30.0;
                let wr = [x, py, ww, ph];
                let hv = inside(wr, mouse);
                base.shapes.push(Shape::rect(wr[0], wr[1], wr[2], wr[3], lin(mix(theme.tab_bar, theme.fg, if hv { 0.14 } else { 0.07 }), 1.0), ph / 2.0));
                base.texts.push(TextItem { size: 0.78, ..txt(x + 8.0, cy(0.78), "\u{f009}", theme.accent) });
                base.texts.push(uit(x + 22.0, cy(wsz), &ws_name, theme.fg, wsz));
                self.hits.push(Hit { r: wr, action: Action::Builtin("workspace_switch".into(), vec![]) });
                x += ww + 10.0;
            }
            let mut rx = w - 6.0;
            // Sem barra de titulo (barra nativa e abas fora do topo): o sino vem para ca.
            if inbox_on && chrome.top == 0.0 {
                let bw = 36.0;
                rx -= bw;
                draw_bell(r, &mut base, &mut self.hits, &theme, mouse, [rx, py, bw, ph], inbox_n as u64, inbox_blocked, false);
                rx -= 4.0;
            }
            // Extensoes: um badge por extensao (icone, nome e contadores).
            // Sem espaco (janela estreita): so icone + contador; o nome fica para quando couber.
            let (ls, cs) = (0.76, 0.72);
            let width = |r: &mut Renderer, label: &str, chips: &[(String, String)], named: bool| 8.0 + cw * 0.9 + 5.0
                + if named { r.measure(label, true, ls) } else { 0.0 } + chips.iter().map(|c| r.measure(&c.0, true, cs) + 14.0).sum::<f32>() + 8.0 + 4.0;
            let full: f32 = ext_badges.iter().map(|b| width(r, &b.2, &b.3, true)).sum();
            let named = full <= (rx - x - 220.0).max(0.0);
            let dragging = self.badge_drag.as_ref().filter(|d| d.2).map(|d| d.0.clone());
            for (key, icon, label, chips) in ext_badges.iter().rev() {
                let chip_w: Vec<f32> = chips.iter().map(|c| r.measure(&c.0, true, cs) + 10.0).collect();
                let label = if named { label.as_str() } else { "" };
                let bw = width(r, label, chips, named) - 4.0;
                if rx - bw - 4.0 < x { break; }
                rx -= bw + 4.0;
                let br = [rx, py, bw, ph];
                let open = match key.strip_prefix("duck:") {
                    Some("sys_panel") => sys_open,
                    Some("agent_panel") => agent_open,
                    Some(_) => false,
                    None => ext_open.as_deref().is_some_and(|k| k.split('/').next() == key.split('/').next()),
                };
                let hv = inside(br, mouse);
                base.shapes.push(Shape::rect(br[0], br[1], br[2], br[3],
                    lin(if open { theme.accent } else { mix(theme.tab_bar, theme.fg, if hv { 0.14 } else { 0.07 }) }, if open { 0.3 } else { 1.0 }), ph / 2.0));
                let mut bx = rx + 8.0;
                base.texts.push(TextItem { size: 0.82, ..txt(bx, cy(0.82), &super::ext::icon(icon), if open || hv { theme.fg } else { theme.muted }) });
                bx += cw * 0.9 + 5.0;
                if named {
                    base.texts.push(uit(bx, cy(ls), label, if open || hv { theme.fg } else { theme.muted }, ls));
                    bx += r.measure(label, true, ls) + 4.0;
                }
                for ((t, color), cwid) in chips.iter().zip(&chip_w) {
                    let c = if color.is_empty() { theme.accent } else { theme.named(color) };
                    let chh = (ch * 0.9).min(ph - 4.0);
                    base.shapes.push(Shape::rect(bx, foot_y + (fh - chh) / 2.0, *cwid, chh, lin(c, 0.22), chh / 2.0));
                    base.texts.push(uit(bx + 5.0, cy(cs), t, c, cs));
                    bx += cwid + 4.0;
                }
                let action = match key.strip_prefix("duck:") {
                    Some(a) => Action::Builtin(a.into(), vec![]),
                    None => Action::Builtin("ext_view".into(), vec![json!(key)]),
                };
                if dragging.as_deref() == Some(badge_name(key)) {
                    base.shapes.push(Shape::ring(br[0], br[1], br[2], br[3], lin(theme.accent, 1.0), ph / 2.0, 1.5));
                }
                // O clique e tratado pelo arrasto (press + release): sem Hit aqui.
                self.badge_rects.push((br[0], br[0] + br[2], badge_name(key).to_string(), action));
            }
            // Status da config (funcao `status`) no meio; corta pela esquerda se faltar espaco.
            let ssz = 0.8;
            let room = rx - x - 12.0;
            let mut segs: Vec<&(String, Rgb)> = status.iter().collect();
            while segs.len() > 1 && segs.iter().map(|(s, _)| s.chars().count() as f32 * cw * ssz).sum::<f32>() > room { segs.remove(0); }
            let mut sx = x;
            for (s, c) in segs {
                let sw = s.chars().count() as f32 * cw * ssz;
                if s.contains("CPU") || s.contains("RAM") {
                    let hr = [sx, py, sw, ph];
                    let on = inside(hr, mouse) || self.sys_panel;
                    if on { base.shapes.push(Shape::rect(hr[0], hr[1], hr[2], hr[3], lin(mix(theme.tab_bar, theme.fg, if self.sys_panel { 0.12 } else { 0.08 }), 1.0), 6.0)); }
                    self.hits.push(Hit { r: hr, action: Action::Builtin("sys_panel".into(), vec![]) });
                }
                base.texts.push(TextItem { size: ssz, clip_right: Some(rx - 6.0), ..txt(sx, cy(ssz), s, *c) });
                sx += sw;
            }
        }
        // Barra de atalhos: os principais, com as teclas (config `shortcuts`).
        if chrome.keys_h > 0.0 {
            let (ky, kh) = (foot_y + fh, chrome.keys_h);
            if fh > 0.0 { base.quads.push(Quad { x: 8.0, y: ky, w: w - 16.0, h: 1.0, color: lin(theme.border, 0.35) }); }
            let (ks, ls) = (0.68, 0.74);
            let cap_h = (kh - 8.0).max(12.0);
            let mut x = 10.0;
            for (label, hint, action) in &shortcut_items {
                let caps: Vec<&str> = if hint.is_empty() { vec![] } else { hint.split('+').collect() };
                let cap_w: Vec<f32> = caps.iter().map(|p| r.measure(p, true, ks) + 8.0).collect();
                let caps_total: f32 = cap_w.iter().map(|c| c + 2.0).sum();
                let lw = r.measure(label, true, ls);
                let iw = caps_total + 4.0 + lw;
                if x + iw > w - 8.0 { break; }
                let ir = [x - 5.0, ky + 2.0, iw + 10.0, kh - 4.0];
                let hv = inside(ir, mouse);
                if hv { base.shapes.push(Shape::rect(ir[0], ir[1], ir[2], ir[3], lin(mix(theme.tab_bar, theme.fg, 0.10), 1.0), 5.0)); }
                let mut cx = x;
                for (p, pw) in caps.iter().zip(&cap_w) {
                    base.shapes.push(Shape::rect(cx, ky + (kh - cap_h) / 2.0, *pw, cap_h, lin(mix(theme.tab_bar, theme.fg, 0.13), 1.0), 4.0));
                    base.texts.push(uit(cx + 4.0, ky + (kh - ch * ks) / 2.0, p, theme.fg, ks));
                    cx += pw + 2.0;
                }
                base.texts.push(uit(cx + 4.0, ky + (kh - ch * ls) / 2.0, label, if hv { theme.fg } else { theme.muted }, ls));
                self.hits.push(Hit { r: ir, action: action.clone() });
                x += iw + 18.0;
            }
        }
        // Prévia da aba sob o mouse.
        if let (Some((idx, lines)), Some((_, _, _))) = (&self.tab_preview, self.tab_hover.map(|t| (t.0, 0, 0))) {
            if let Some((a, b, _)) = self.tab_rects.iter().find(|t| t.2 == *idx).copied() {
                let pwid = (cw * 0.75 * 64.0).min(w - 20.0);
                let phei = ch * 0.75 * lines.len().max(1) as f32 + 34.0;
                let px = ((a + b) / 2.0 - pwid / 2.0).clamp(8.0, w - pwid - 8.0);
                let py = if self.tab_row.0 < h / 2.0 { self.tab_row.0 + self.tab_row.1 + 4.0 } else { self.tab_row.0 - phei - 4.0 };
                modal.shapes.push(Shape::shadow(px, py + 4.0, pwid, phei, [0.0, 0.0, 0.0, 0.5], 10.0, 12.0));
                modal.shapes.push(Shape::rect(px, py, pwid, phei, lin(theme.bg, 1.0), 10.0));
                modal.shapes.push(Shape::ring(px, py, pwid, phei, lin(theme.border, 1.0), 10.0, 1.0));
                let title = tab_labels.iter().find(|t| t.1.index == *idx).map(|t| t.0.clone()).unwrap_or_default();
                modal.texts.push(TextItem { bold: true, clip_right: Some(px + pwid - 10.0), ..uit(px + 12.0, py + 6.0, &title, theme.fg, 0.85) });
                for (k, l) in lines.iter().enumerate() {
                    modal.texts.push(TextItem { size: 0.75, clip_right: Some(px + pwid - 10.0), ..txt(px + 12.0, py + 26.0 + ch * 0.75 * k as f32, l, theme.fg) });
                }
            }
        }
        // Arrastando badge: marca onde ele vai cair.
        if let Some((name, _, true)) = &self.badge_drag {
            let from = self.badge_rects.iter().find(|b| &b.2 == name).map(|b| b.0);
            if let (Some(fx), Some(t)) = (from, self.badge_rects.iter().find(|b| mouse.0 >= b.0 && mouse.0 < b.1 && &b.2 != name)) {
                let bx = if t.0 > fx { t.1 + 1.0 } else { t.0 - 3.0 };
                base.shapes.push(Shape::rect(bx, self.badge_row.0 + 4.0, 2.0, self.badge_row.1 - 8.0, lin(theme.accent, 1.0), 1.0));
            }
        }
        // Arrastando aba: marca onde ela vai cair.
        if let Some((from, _, true)) = self.tab_drag {
            if let Some((a, b, idx)) = self.tab_rects.iter().find(|(a, b, _)| mouse.0 >= *a && mouse.0 < *b).copied() {
                if idx != from {
                    let x = if idx > from { b } else { a - 2.0 };
                    base.shapes.push(Shape::rect(x, self.tab_row.0 + 6.0, 3.0, self.tab_row.1 - 12.0, lin(theme.accent, 1.0), 1.5));
                }
            } else if mouse.1 > h || mouse.1 < 0.0 || mouse.0 < 0.0 || mouse.0 > w {
                let label = "Solte para abrir numa janela nova";
                let lw = r.measure(label, true, 0.9) + 20.0;
                modal.shapes.push(Shape::rect((w - lw) / 2.0, h / 2.0 - 16.0, lw, 32.0, lin(theme.accent, 1.0), 16.0));
                modal.texts.push(uit((w - lw) / 2.0 + 10.0, h / 2.0 - ch * 0.45, label, theme.bg, 0.9));
            }
        }

        // ---- barra lateral (abas verticais)
        if chrome.left > 0.0 {
            let (sy, sh, sw) = (chrome.top, h - chrome.top - chrome.bottom, chrome.left);
            let collapsed = self.sidebar_collapsed;
            base.shapes.push(Shape::rect(0.0, sy, sw, sh, lin(theme.tab_bar, alpha), 0.0));
            base.quads.push(Quad { x: sw - 1.0, y: sy, w: 1.0, h: sh, color: lin(theme.border, 0.6) });
            let row_h = (ch * 1.4).max(26.0);
            let mut y = sy + 8.0;
            let limit = sy + sh - row_h * 2.0 - 8.0;
            for wsn in self.frame.workspaces.clone() {
                if y > limit { break; }
                let cur = wsn == ws_name;
                let rr = [6.0, y, sw - 12.0, row_h];
                let hv = inside(rr, mouse);
                if hv { base.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(mix(theme.tab_bar, theme.fg, 0.08), 1.0), 6.0)); }
                base.texts.push(TextItem { size: 0.9, ..txt(14.0, y + (row_h - ch * 0.9) / 2.0, if cur { "\u{f07c}" } else { "\u{f07b}" }, if cur { theme.accent } else { theme.muted }) });
                if !collapsed {
                    base.texts.push(TextItem { bold: cur, clip_right: Some(sw - 10.0), ..uit(36.0, y + (row_h - ch * 0.9) / 2.0, &wsn, if cur { theme.fg } else { theme.muted }, 0.9) });
                }
                self.hits.push(Hit { r: rr, action: Action::Builtin("workspace".into(), vec![json!(wsn)]) });
                y += row_h + 2.0;
                if !cur { continue; }
                for (label, t, color) in &tab_labels {
                    if y > limit { break; }
                    let rr = if collapsed { [6.0, y, sw - 12.0, row_h] } else { [14.0, y, sw - 20.0, row_h] };
                    let hv = inside(rr, mouse);
                    if t.active || hv {
                        base.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(if t.active { theme.tab_active } else { mix(theme.tab_bar, theme.fg, 0.07) }, 1.0), 6.0));
                    }
                    if t.active { base.shapes.push(Shape::rect(rr[0], rr[1] + 5.0, 3.0, row_h - 10.0, lin(color.unwrap_or(theme.accent), 1.0), 1.5)); }
                    let ix = if collapsed { rr[0] + (rr[2] - 16.0) / 2.0 } else { rr[0] + 10.0 };
                    if !t.profile.is_empty() { draw_avatar(r, &mut base, ix, y + (row_h - 16.0) / 2.0, 16.0, avatar_color(&theme, &ui, &t.profile), &t.profile); }
                    else { base.texts.push(TextItem { size: 0.85, ..txt(ix, y + (row_h - ch * 0.85) / 2.0, pane_icon(&t.domain, wsl.contains(&t.domain)), if t.active { theme.accent } else { theme.muted }) }); }
                    if collapsed {
                        if !t.state.is_empty() { base.shapes.push(Shape::rect(rr[0] + rr[2] - 9.0, y + 5.0, 6.0, 6.0, lin(theme.named(&t.state), 1.0), 3.0)); }
                    } else {
                        let tx = rr[0] + 32.0;
                        let fg = if t.active { theme.fg } else { theme.muted };
                        let (name, sub) = split_subtitle(label);
                        let clip = rr[0] + rr[2] - 34.0;
                        base.texts.push(TextItem { clip_right: Some(clip), ..uit(tx, y + (row_h - ch * 0.9) / 2.0, name, fg, 0.9) });
                        let sx = tx + r.measure(name, true, 0.9) + 6.0;
                        if !sub.is_empty() && sx < clip {
                            base.texts.push(TextItem { italic: true, clip_right: Some(clip), ..uit(sx, y + (row_h - ch * 0.8) / 2.0, sub, mix(fg, theme.muted, 0.6), 0.8) });
                        }
                        let (glyph, c, a) = state_glyph(&theme, t, t_now);
                        if !glyph.is_empty() {
                            if t.state == "working" { pulse = true; }
                            base.texts.push(TextItem { alpha: a, ..uit(rr[0] + rr[2] - 30.0, y + (row_h - ch * 0.8) / 2.0, glyph, c, 0.8) });
                        }
                        if let Some((s, f)) = tab_progress(t) {
                            let (bx, bw) = (tx, rr[2] - 46.0);
                            base.shapes.push(Shape::rect(bx, y + row_h - 5.0, bw, 2.5, lin(theme.muted, 0.25), 1.25));
                            base.shapes.push(Shape::rect(bx, y + row_h - 5.0, bw * f, 2.5, lin(if s == 2 { theme.blocked } else { theme.done }, 1.0), 1.25));
                        }
                    }
                    self.hits.push(Hit { r: rr, action: Action::Builtin("tab".into(), vec![json!(t.index)]) });
                    if hv && !collapsed {
                        let cr = [rr[0] + rr[2] - 20.0, y + (row_h - 16.0) / 2.0, 16.0, 16.0];
                        base.texts.push(uit(cr[0] + 3.0, y + (row_h - ch * 0.7) / 2.0, "✕", if inside(cr, mouse) { theme.fg } else { theme.muted }, 0.7));
                        self.hits.push(Hit { r: cr, action: Action::Builtin("close_tab_idx".into(), vec![json!(t.index)]) });
                    }
                    y += row_h + 2.0;
                }
                // Nova aba
                if !collapsed && y <= limit {
                    let rr = [14.0, y, sw - 20.0, row_h];
                    let hv = inside(rr, mouse);
                    base.texts.push(uit(24.0, y + (row_h - ch * 0.85) / 2.0, "+  Nova aba", if hv { theme.fg } else { theme.muted }, 0.85));
                    self.hits.push(Hit { r: rr, action: Action::Builtin("new_tab".into(), vec![]) });
                    y += row_h + 6.0;
                }
            }
            let br = [6.0, sy + sh - row_h - 8.0, sw - 12.0, row_h];
            let hv = inside(br, mouse);
            if hv { base.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(mix(theme.tab_bar, theme.fg, 0.08), 1.0), 6.0)); }
            base.texts.push(uit(16.0, br[1] + (row_h - ch * 0.85) / 2.0, if collapsed { "»" } else { "«  recolher" }, theme.muted, 0.85));
            self.hits.push(Hit { r: br, action: Action::Builtin("toggle_sidebar".into(), vec![]) });
        }

        // ---- painel do agente (direita)
        if let Some(ap) = &self.agent_panel {
            let (x0, y0, pw, ph) = (w - chrome.right, chrome.top, chrome.right, h - chrome.top - chrome.bottom);
            base.shapes.push(Shape::rect(x0, y0, pw, ph, lin(theme.tab_bar, alpha), 0.0));
            base.quads.push(Quad { x: x0, y: y0, w: 1.0, h: ph, color: lin(theme.border, 0.6) });
            let mut y = y0 + 14.0;
            let ac = avatar_color(&theme, &ui, &ap.profile);
            draw_avatar(r, &mut base, x0 + 14.0, y, 30.0, ac, if ap.profile.is_empty() { &ap.name } else { &ap.profile });
            base.texts.push(TextItem { bold: true, ..uit(x0 + 54.0, y - 1.0, &format!("{} · {}", ap.name, if ap.profile.is_empty() { "pessoal" } else { &ap.profile }), theme.fg, 0.95) });
            let (sg, sc, sa) = state_glyph(&theme, &TabInfo { state: ap.state.clone(), ..Default::default() }, t_now);
            if ap.state == "working" { pulse = true; }
            base.texts.push(TextItem { alpha: sa, ..uit(x0 + 54.0, y + ch * 0.95, &format!("{sg} {}", state_label(&ap.state)), sc, 0.85) });
            let xr = [x0 + pw - 30.0, y0 + 10.0, 20.0, 20.0];
            base.texts.push(uit(xr[0] + 4.0, xr[1], "✕", if inside(xr, mouse) { theme.fg } else { theme.muted }, 0.8));
            self.hits.push(Hit { r: xr, action: Action::Builtin("agent_panel".into(), vec![]) });
            y += 46.0;
            let line = |r: &mut Renderer, l: &mut Layer, y: &mut f32, k: &str, v: String| {
                l.texts.push(uit(x0 + 16.0, *y, k, theme.muted, 0.85));
                let vw = r.measure(&v, true, 0.85);
                l.texts.push(uit(x0 + pw - 16.0 - vw, *y, &v, theme.fg, 0.85));
                *y += ch * 1.2;
            };
            let k = |n: u64| if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) } else { format!("{}k", n / 1000) };
            if let Some(c) = ap.ctx { line(r, &mut base, &mut y, "Contexto", format!("{} tokens", k(c))); }
            if let Some(s) = ap.session_tokens { line(r, &mut base, &mut y, "Sessao", format!("{} tokens", k(s))); }
            if ui.token_price > 0.0 {
                if let Some(s) = ap.session_tokens { line(r, &mut base, &mut y, "Custo estimado", format!("US$ {:.2}", s as f64 / 1e6 * ui.token_price)); }
            }
            y += 8.0;
            let (done, total) = if ap.plan.is_empty() { ap.todo.map(|(d, t)| (d as usize, t as usize)).unwrap_or((0, 0)) }
                else { (ap.plan.iter().filter(|p| p.0 == "completed").count(), ap.plan.len()) };
            base.texts.push(TextItem { bold: true, ..uit(x0 + 16.0, y, &format!("Plano  {done}/{total}"), theme.fg, 0.9) });
            y += ch * 1.3;
            if total > 0 {
                let f = done as f32 / total as f32;
                base.shapes.push(Shape::rect(x0 + 16.0, y - 6.0, pw - 32.0, 3.0, lin(theme.muted, 0.25), 1.5));
                base.shapes.push(Shape::rect(x0 + 16.0, y - 6.0, (pw - 32.0) * f, 3.0, lin(theme.done, 1.0), 1.5));
            }
            let bottom_buttons = y0 + ph - 46.0;
            for (st, subject) in &ap.plan {
                if y > bottom_buttons - ch * 1.3 { break; }
                let (g, c) = match st.as_str() { "completed" => ("\u{f058}", theme.done), "in_progress" => ("\u{f192}", theme.accent), _ => ("\u{f10c}", theme.muted) };
                base.texts.push(TextItem { size: 0.85, ..txt(x0 + 16.0, y, g, c) });
                base.texts.push(TextItem { clip_right: Some(x0 + pw - 12.0), ..uit(x0 + 36.0, y, subject, if st == "completed" { theme.muted } else { theme.fg }, 0.85) });
                y += ch * 1.25;
            }
            if ap.plan.is_empty() { base.texts.push(uit(x0 + 16.0, y, if total > 0 { "Itens do plano nao encontrados no transcript" } else { "Sem plano (TaskCreate/TodoWrite) nesta sessao" }, theme.muted, 0.8)); }
            let mut bx = x0 + 12.0;
            for (label, act, args) in [("Linha do tempo", "agent_timeline", vec![]), ("Recado", "queue_message", vec![json!(ap.pane)]), ("Diff", "agent_diff", vec![])] {
                let bw = r.measure(label, true, 0.8) + 18.0;
                let br = [bx, bottom_buttons + 8.0, bw, 28.0];
                let hv = inside(br, mouse);
                base.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(if hv { theme.accent } else { mix(theme.tab_bar, theme.fg, 0.1) }, if hv { 0.35 } else { 1.0 }), 7.0));
                base.texts.push(uit(bx + 9.0, br[1] + (28.0 - ch * 0.8) / 2.0, label, theme.fg, 0.8));
                self.hits.push(Hit { r: br, action: Action::Builtin(act.into(), args) });
                bx += bw + 6.0;
            }
        }

        // ---- painel de extensao (direita)
        if let Some(v) = &ext_view {
            let rect = [w - chrome.right, chrome.top, chrome.right, h - chrome.top - chrome.bottom];
            self.ext_content = super::ext::draw_view(r, &mut base, &mut self.hits, v, rect, &theme, mouse, self.ext_scroll, &ext_siblings);
        }

        // ---- painel do sistema (direita)
        if let Some(st) = &st {
            let (x0, y0, pw, ph) = (w - chrome.right, chrome.top, chrome.right, h - chrome.top - chrome.bottom);
            base.shapes.push(Shape::rect(x0, y0, pw, ph, lin(theme.tab_bar, alpha), 0.0));
            base.quads.push(Quad { x: x0, y: y0, w: 1.0, h: ph, color: lin(theme.border, 0.6) });
            let (lx, rw) = (x0 + 16.0, pw - 32.0);
            let pct_c = |p: f32| if p >= 85.0 { theme.blocked } else if p >= 60.0 { theme.working } else { theme.done };
            let mut y = y0 + 12.0;
            base.texts.push(TextItem { bold: true, ..uit(lx, y, "Sistema", theme.fg, 1.0) });
            let xr = [x0 + pw - 30.0, y0 + 10.0, 20.0, 20.0];
            base.texts.push(uit(xr[0] + 4.0, xr[1], "✕", if inside(xr, mouse) { theme.fg } else { theme.muted }, 0.8));
            self.hits.push(Hit { r: xr, action: Action::Builtin("sys_panel".into(), vec![]) });
            y += ch * 1.4;
            let up = st.uptime;
            base.texts.push(TextItem { clip_right: Some(x0 + pw - 10.0), ..uit(lx, y, &st.brand, theme.muted, 0.78) });
            y += ch * 1.05;
            base.texts.push(uit(lx, y, &format!("{} nucleos · {:.2} GHz · ligado ha {}d {}h {}min", st.cores.len(), st.freq_mhz as f32 / 1000.0, up / 86400, up / 3600 % 24, up / 60 % 60), theme.muted, 0.78));
            y += ch * 1.5;
            // Grafico de linha (barras finas) das ultimas 60 amostras.
            let spark = |l: &mut Layer, y: f32, hh: f32, hist: &std::collections::VecDeque<f32>, c: Rgb| {
                l.shapes.push(Shape::rect(lx, y, rw, hh, lin(theme.bg, 0.6), 6.0));
                let bw = rw / 60.0;
                for (i, v) in hist.iter().enumerate() {
                    let bh = (v / 100.0).clamp(0.0, 1.0) * (hh - 4.0);
                    let x = lx + (60 - hist.len() + i) as f32 * bw;
                    l.quads.push(Quad { x: x + 0.5, y: y + hh - 2.0 - bh, w: (bw - 1.0).max(1.0), h: bh, color: lin(c, 0.85) });
                }
            };
            let head = |r: &mut Renderer, l: &mut Layer, y: f32, k: &str, v: String, c: Rgb| {
                l.texts.push(TextItem { bold: true, ..uit(lx, y, k, theme.fg, 0.9) });
                let vw = r.measure(&v, true, 0.9);
                l.texts.push(uit(lx + rw - vw, y, &v, c, 0.9));
            };
            head(r, &mut base, y, "CPU", format!("{:.0}%", st.cpu), pct_c(st.cpu));
            y += ch * 1.3;
            spark(&mut base, y, 44.0, &st.cpu_hist, theme.accent);
            y += 52.0;
            // Um bloco por nucleo: barra vertical + numero.
            let n = st.cores.len().max(1);
            let per_row = if n > 16 { 8 } else { n.min(8) };
            let cell = rw / per_row as f32;
            let bar_h = 34.0;
            for (i, u) in st.cores.iter().enumerate() {
                let (cx, cy) = (lx + (i % per_row) as f32 * cell, y + (i / per_row) as f32 * (bar_h + ch * 0.95 + 6.0));
                let bw = (cell - 6.0).max(4.0);
                base.shapes.push(Shape::rect(cx, cy, bw, bar_h, lin(theme.bg, 0.6), 4.0));
                let fh = (u / 100.0).clamp(0.0, 1.0) * bar_h;
                if fh > 0.5 { base.shapes.push(Shape::rect(cx, cy + bar_h - fh, bw, fh, lin(pct_c(*u), 0.9), 4.0)); }
                let lbl = format!("{:.0}", u);
                let tw = r.measure(&lbl, true, 0.68);
                base.texts.push(uit(cx + (bw - tw) / 2.0, cy + bar_h + 1.0, &lbl, theme.muted, 0.68));
            }
            y += (n.div_ceil(per_row)) as f32 * (bar_h + ch * 0.95 + 6.0) + 8.0;
            // Memoria
            let pct = (st.ram / st.ram_total.max(0.001) * 100.0) as f32;
            head(r, &mut base, y, "Memoria", format!("{:.1} / {:.0} GB", st.ram, st.ram_total), pct_c(pct));
            y += ch * 1.3;
            spark(&mut base, y, 34.0, &st.ram_hist, theme.palette[5]);
            y += 42.0;
            let line = |r: &mut Renderer, l: &mut Layer, y: &mut f32, k: &str, v: String| {
                l.texts.push(uit(lx, *y, k, theme.muted, 0.82));
                let vw = r.measure(&v, true, 0.82);
                l.texts.push(uit(lx + rw - vw, *y, &v, theme.fg, 0.82));
                *y += ch * 1.15;
            };
            line(r, &mut base, &mut y, "Em uso", format!("{:.1} GB ({:.0}%)", st.ram, pct));
            line(r, &mut base, &mut y, "Disponivel", format!("{:.1} GB", st.avail));
            if st.swap_total > 0.0 {
                let sp = (st.swap / st.swap_total * 100.0) as f32;
                line(r, &mut base, &mut y, "Arquivo de paginacao", format!("{:.1} / {:.1} GB ({:.0}%)", st.swap, st.swap_total, sp));
            }
            y += 8.0;
            // Processos
            base.texts.push(TextItem { bold: true, ..uit(lx, y, "Processos (por CPU)", theme.fg, 0.9) });
            y += ch * 1.3;
            if st.top.is_empty() { base.texts.push(uit(lx, y, "lendo…", theme.muted, 0.8)); }
            for (name, cpu, mb, qty) in &st.top {
                if y > y0 + ph - ch * 1.3 { break; }
                let label = if *qty > 1 { format!("{name} ({qty})") } else { name.clone() };
                let mem = if *mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") };
                let cpu_s = format!("{cpu:.1}%");
                let (mw, cwid) = (r.measure(&mem, true, 0.8), r.measure(&cpu_s, true, 0.8));
                base.texts.push(TextItem { clip_right: Some(lx + rw - 130.0), ..uit(lx, y, &label, theme.fg, 0.8) });
                base.texts.push(uit(lx + rw - 70.0 - cwid, y, &cpu_s, pct_c(*cpu * 4.0), 0.8));
                base.texts.push(uit(lx + rw - mw, y, &mem, theme.muted, 0.8));
                y += ch * 1.1;
            }
        }

        // ---- autocompletar (popup junto do cursor)
        if let (Some(ac), Some(p)) = (&self.ac, self.frame.panes.iter().find(|p| p.id == active)) {
            if let Some((cc, cr, _)) = &p.cursor {
                let row_h = ch * 1.1;
                let n = ac.items.len();
                let wid = (ac.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(10).min(70) as f32 * cw * 0.9 + 110.0).max(300.0);
                let hh = n as f32 * row_h + 8.0 + ch;
                let ax = (ox + (p.x as f32 + *cc as f32) * cw - 8.0).min(w - wid - 8.0).max(4.0);
                let cy = oy + (p.y as f32 + *cr as f32) * ch;
                let ay = if cy + ch + hh < h - chrome.bottom { cy + ch + 2.0 } else { cy - hh - 2.0 };
                modal.shapes.push(Shape::shadow(ax, ay + 3.0, wid, hh, [0.0, 0.0, 0.0, 0.45], 8.0, 10.0));
                modal.shapes.push(Shape::rect(ax, ay, wid, hh, lin(theme.tab_bar, 1.0), 8.0));
                modal.shapes.push(Shape::ring(ax, ay, wid, hh, lin(theme.border, 1.0), 8.0, 1.0));
                for (k, it) in ac.items.iter().enumerate() {
                    let y = ay + 4.0 + k as f32 * row_h;
                    let rr = [ax + 4.0, y, wid - 8.0, row_h];
                    if ac.sel == Some(k) || inside(rr, mouse) { modal.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(theme.accent, 0.25), 5.0)); }
                    let icon = match it.kind { "pasta" => "\u{f07b}", "arquivo" => "\u{f15b}", "git" => "\u{e725}", "npm" => "\u{e71e}", _ => "\u{f1da}" };
                    modal.texts.push(TextItem { size: 0.85, ..txt(ax + 10.0, y + (row_h - ch * 0.85) / 2.0, icon, theme.muted) });
                    modal.texts.push(TextItem { size: 0.9, clip_right: Some(ax + wid - 80.0), ..txt(ax + 30.0, y + (row_h - ch * 0.9) / 2.0, &it.label, theme.fg) });
                    let kw = r.measure(it.kind, true, 0.75);
                    modal.texts.push(uit(ax + wid - kw - 12.0, y + (row_h - ch * 0.75) / 2.0, it.kind, theme.muted, 0.75));
                    self.hits.push(Hit { r: rr, action: Action::Builtin("ac_accept".into(), vec![json!(k)]) });
                }
                modal.texts.push(uit(ax + 10.0, ay + hh - ch - 2.0, "Alt+↑↓ escolhe · Alt+Enter aceita · Esc fecha", theme.muted, 0.72));
            }
        }

        // ---- editor de comando
        if let Some(c) = self.composer.as_mut() {
            if let Some(p) = self.frame.panes.iter().find(|p| p.id == c.pane) {
                let (px, py, pw, ph) = (ox + p.x as f32 * cw, oy + p.y as f32 * ch, p.w as f32 * cw, p.h as f32 * ch);
                let lines: Vec<String> = c.text.iter().collect::<String>().split('\n').map(String::from).collect();
                let n = lines.len().clamp(1, 10);
                let bh = n as f32 * ch + 44.0;
                let (bx, by, bw) = (px + 6.0, py + ph - bh - 6.0, pw - 12.0);
                modal.shapes.push(Shape::shadow(bx, by + 4.0, bw, bh, [0.0, 0.0, 0.0, 0.5], 10.0, 14.0));
                modal.shapes.push(Shape::rect(bx, by, bw, bh, lin(theme.tab_bar, 1.0), 10.0));
                modal.shapes.push(Shape::ring(bx, by, bw, bh, lin(theme.accent, 0.9), 10.0, 1.5));
                let tx = bx + 14.0;
                let ty = by + 10.0;
                c.geom = [tx, ty, bw - 28.0, n as f32 * ch];
                // posicao do cursor e da selecao em (linha, coluna)
                let pos = |i: usize| -> (usize, usize) {
                    let before: String = c.text[..i.min(c.text.len())].iter().collect();
                    let l = before.matches('\n').count();
                    (l, before.rsplit('\n').next().unwrap_or("").chars().count())
                };
                let first = pos(c.cur).0.saturating_sub(n - 1).min(lines.len().saturating_sub(n));
                if let Some(a) = c.anchor.filter(|a| *a != c.cur) {
                    let (s, e) = (pos(a.min(c.cur)), pos(a.max(c.cur)));
                    for l in s.0..=e.0 {
                        if l < first || l >= first + n { continue; }
                        let c0 = if l == s.0 { s.1 } else { 0 };
                        let c1 = if l == e.0 { e.1 } else { lines.get(l).map(|x| x.chars().count()).unwrap_or(0) + 1 };
                        modal.shapes.push(Shape::rect(tx + c0 as f32 * cw, ty + (l - first) as f32 * ch, (c1.saturating_sub(c0)) as f32 * cw, ch, lin(theme.selection, 0.7), 2.0));
                    }
                }
                for (k, l) in lines.iter().skip(first).take(n).enumerate() {
                    modal.texts.push(TextItem { clip_right: Some(bx + bw - 10.0), ..txt(tx, ty + k as f32 * ch, l, theme.fg) });
                }
                let (cl, ccol) = pos(c.cur);
                if cl >= first { modal.shapes.push(Shape::rect(tx + ccol as f32 * cw, ty + (cl - first) as f32 * ch + 1.0, 2.0, ch - 2.0, lin(theme.cursor, 1.0), 1.0)); }
                modal.texts.push(uit(tx, by + bh - ch - 6.0, "Enter envia · Shift+Enter quebra linha · Ctrl+Z desfaz · Esc fecha", theme.muted, 0.75));
            }
        }

        // ---- mensagem curta (erro de config, "copiado"...)
        if let Some((msg, at)) = &self.message {
            if at.elapsed() < Duration::from_secs(if self.cfg_error.as_deref() == Some(msg.as_str()) { 15 } else { 4 }) {
                let mw = (r.measure(msg, true, 0.9) + 28.0).min(w - 20.0);
                let mx = (w - chrome.right - mw - 12.0).max(4.0);
                let my = chrome.top + 10.0;
                modal.shapes.push(Shape::shadow(mx, my + 3.0, mw, ch + 14.0, [0.0, 0.0, 0.0, 0.45], (ch + 14.0) / 2.0, 8.0));
                modal.shapes.push(Shape::rect(mx, my, mw, ch + 14.0, lin(mix(theme.tab_bar, theme.fg, 0.08), 1.0), (ch + 14.0) / 2.0));
                modal.texts.push(TextItem { clip_right: Some(mx + mw - 10.0), ..uit(mx + 14.0, my + 7.0 + (ch - ch * 0.9) / 2.0, msg, theme.fg, 0.9) });
            } else {
                self.message = None;
            }
        }

        // ---- Agent Manager em grade
        if let Some(g) = &self.grid {
            let n = g.panes.len();
            let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
            let rows_n = n.div_ceil(cols);
            let (gx, gy, gw, gh) = (12.0, 12.0, w - 24.0, h - 24.0 - ch - 10.0);
            let (tw, th) = (gw / cols as f32, gh / rows_n as f32);
            modal.shapes.push(Shape::rect(0.0, 0.0, w, h, lin(canvas, 0.97), 0.0));
            for (i, (id, head, state)) in g.panes.iter().enumerate() {
                let (x, y) = (gx + (i % cols) as f32 * tw, gy + (i / cols) as f32 * th);
                let sel = i == g.sel;
                let border = if sel { theme.accent } else { theme.named(state) };
                modal.shapes.push(Shape::shadow(x + 6.0, y + 9.0, tw - 12.0, th - 12.0, [0.0, 0.0, 0.0, 0.4], rad, 8.0));
                modal.shapes.push(Shape::rect(x + 6.0, y + 6.0, tw - 12.0, th - 12.0, lin(theme.bg, 1.0), rad));
                modal.shapes.push(Shape::ring(x + 6.0, y + 6.0, tw - 12.0, th - 12.0, lin(border, if sel { 1.0 } else { 0.5 }), rad, if sel { 2.0 } else { 1.0 }));
                modal.texts.push(TextItem { bold: true, clip_right: Some(x + tw - 14.0), ..uit(x + 16.0, y + 12.0, &format!("● {head} · pane {id}"), theme.named(state), 0.9) });
                let max_lines = ((th - ch - 30.0) / ch).floor().max(1.0) as usize;
                let text = g.texts.get(id).cloned().unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                for (k, l) in lines[lines.len().saturating_sub(max_lines)..].iter().enumerate() {
                    modal.texts.push(TextItem { clip_right: Some(x + tw - 14.0), ..txt(x + 16.0, y + 18.0 + ch * (1.0 + k as f32), l, theme.fg) });
                }
                self.hits.push(Hit { r: [x + 6.0, y + 6.0, tw - 12.0, th - 12.0], action: Action::Builtin("grid_pick".into(), vec![json!(i)]) });
            }
            let help = "Enter foca · setas navegam · Ctrl+Y aprova · Ctrl+N nega · M recado · Esc fecha";
            let hw = r.measure(help, true, 0.85) + 24.0;
            modal.shapes.push(Shape::rect((w - hw) / 2.0, h - ch - 16.0, hw, ch + 10.0, lin(theme.accent, 1.0), (ch + 10.0) / 2.0));
            modal.texts.push(uit((w - hw) / 2.0 + 12.0, h - ch - 11.0, help, theme.bg, 0.85));
        }

        // ---- mapa dos agentes
        // Com algo aberto por cima (detalhe, paleta), o mapa sai: na mesma
        // camada o texto dele ficaria por cima do painel.
        let no_overlay = self.overlay.is_none();
        if let Some(g) = self.graph.as_mut().filter(|_| no_overlay) {
            super::graph::draw_graph(g, r, &mut modal, &mut self.hits, &theme, canvas, w, h, ch, rad);
        }

        // ---- overlay (paleta, seletores, prompts, dialogos, previa, slider)
        if let Some(o) = &self.overlay {
            draw_overlay(r, &mut modal, &mut self.hits, o, &theme, mouse, w, h, chrome.top);
        }

        // ---- apresentacao: o pato se monta pixel a pixel, balanca e some
        if let Some(t0) = self.splash {
            let t = t0.elapsed().as_secs_f32();
            if t > 2.2 { self.splash = None; } else {
                animating = true;
                let fade = if t > 1.8 { 1.0 - (t - 1.8) / 0.4 } else { 1.0 };
                modal.shapes.push(Shape::rect(0.0, 0.0, w, h, lin(canvas, fade), 0.0));
                let grid: Vec<&str> = DUCK_ART.lines().filter(|l| !l.is_empty()).collect();
                let (rows, cols) = (grid.len() as f32, grid.iter().map(|l| l.len()).max().unwrap_or(1) as f32);
                let px = (h * 0.30 / rows).floor().clamp(4.0, 16.0);
                let (gw, gh) = (cols * px, rows * px);
                let bob = (t * 3.2).sin() * px * 0.5;
                let (x0, y0) = ((w - gw) / 2.0, h * 0.46 - gh * 0.75 + bob);
                for (ry, row) in grid.iter().enumerate() {
                    for (cx, c) in row.chars().enumerate() {
                        let col = match c { 'y' => Rgb(0xFF, 0xC8, 0x43), 'Y' => Rgb(0xF3, 0xB7, 0x3F), 'b' => Rgb(0x6C, 0xCF, 0xE2), 'B' => Rgb(0x58, 0xA4, 0xB3), 'e' => Rgb(0x4A, 0x50, 0x52), _ => continue };
                        let k = ease((t - (ry + cx) as f32 * 0.014) / 0.28);
                        if k <= 0.0 { continue; }
                        let s = px * (0.55 + 0.45 * k) - 1.0;
                        modal.shapes.push(Shape::rect(x0 + cx as f32 * px + (px - s) / 2.0, y0 + ry as f32 * px + (px - s) / 2.0, s, s, lin(col, k * fade), px * 0.22));
                    }
                }
                let ta = fade * ease((t - 0.45) / 0.4);
                let name = "Duck";
                let nw = r.measure(name, true, 2.6);
                modal.texts.push(TextItem { bold: true, alpha: ta, ..uit((w - nw) / 2.0, y0 + gh + px * 2.0, name, theme.fg, 2.6) });
                let sub = format!("terminal para agentes · {}", env!("CARGO_PKG_VERSION"));
                let sw = r.measure(&sub, true, 0.9);
                modal.texts.push(TextItem { alpha: ta, ..uit((w - sw) / 2.0, y0 + gh + px * 2.0 + ch * 2.9, &sub, theme.muted, 0.9) });
            }
        }

        let clear = if gradient.len() >= 2 { lin(gradient[0], alpha) } else { lin(canvas, alpha) };
        if std::env::var_os("DUCK_DEBUG_SHAPES").is_some() {
            let mut out = format!("janela {w}x{h}
");
            for (li, l) in [&base, &flt, &over, &modal].iter().enumerate() {
                for s in l.shapes.iter().filter(|s| s.w > 300.0 && s.h > 200.0) { out += &format!("L{li} shape {:.0},{:.0} {:.0}x{:.0} r{} soft{} b{} c{:?}
", s.x, s.y, s.w, s.h, s.radius, s.soft, s.border, s.color); }
                for q in l.quads.iter().filter(|q| q.w > 300.0 && q.h > 200.0) { out += &format!("L{li} quad {:.0},{:.0} {:.0}x{:.0} c{:?}
", q.x, q.y, q.w, q.h, q.color); }
            }
            let _ = std::fs::write(crate::proto::data_dir().join("shapes.txt"), out);
        }
        r.draw(clear, &[base, flt, over, modal]);
        if r.animated() { r.window.request_redraw(); }
        // Proximo quadro: animacao (60 fps) ou pulso/piscar (15 fps).
        let next = if animating { Some(now + Duration::from_millis(16)) }
            else if pulse || blinking { Some(now + Duration::from_millis(66)) }
            else { None };
        if let Some(t) = next { self.redraw_at = Some(self.redraw_at.map_or(t, |x| x.min(t))); }
    }

    // -------------------------------------------------------------- mouse

    pub(super) fn mouse_button(&mut self, state: ElementState, button: MouseButton) {
        let (x, y) = self.mouse;
        let pressed = state == ElementState::Pressed;
        if pressed && self.splash.take().is_some() { return self.redraw(); }
        if pressed && button == MouseButton::Left {
            if let (Some(dir), Some(r)) = (self.resize_edge, &self.r) { let _ = r.window.drag_resize_window(dir); return; }
            // Editor de comando: clique posiciona o cursor.
            if let (Some(c), Some(r)) = (self.composer.as_mut(), &self.r) {
                if inside(c.geom, (x, y)) {
                    let line = ((y - c.geom[1]) / r.cell_h).floor().max(0.0) as usize;
                    let col = ((x - c.geom[0]) / r.cell_w).round().max(0.0) as usize;
                    let text: String = c.text.iter().collect();
                    let mut idx = 0;
                    for (k, l) in text.split('\n').enumerate() {
                        let n = l.chars().count();
                        if k == line { idx += col.min(n); break; }
                        idx += n + 1;
                    }
                    c.cur = idx.min(c.text.len());
                    c.anchor = None;
                    self.redraw();
                    return;
                }
            }
            // Badge de extensao: clique abre; segurar e arrastar reordena.
            if self.overlay.is_none() && button == MouseButton::Left && y >= self.badge_row.0 && y < self.badge_row.0 + self.badge_row.1 {
                if let Some(b) = self.badge_rects.iter().find(|b| x >= b.0 && x < b.1) {
                    self.badge_drag = Some((b.2.clone(), x, false));
                    return;
                }
            }
            // Botoes e alvos do ultimo quadro (o de cima primeiro).
            if let Some(a) = self.hits.iter().rev().find(|h| inside(h.r, (x, y))).map(|h| h.action.clone()) {
                self.run(a);
                self.redraw();
                return;
            }
            if self.overlay.is_some() {
                // Clique fora fecha (pedido de aprovacao fechado = negado).
                let o = self.overlay.take().unwrap();
                if let Some(t) = o.gallery { self.cfg.theme = t; }
                if let OverlayKind::Slider { key, orig, .. } = &o.kind { self.apply_slider(key, *orig); }
                let deny = o.items.iter().find_map(|i| match &i.action { Action::Builtin(n, a) if n == "approve" => a.first().cloned(), _ => None });
                if let Some(id) = deny { self.builtin("approve", &[id, json!(false)]); }
                self.redraw();
                return;
            }
            if self.grid.is_some() { return; }
        }
        // Abas: clique seleciona e comeca a arrastar; meio fecha.
        if pressed {
            if let Some(idx) = self.tab_at(x, y) {
                match button {
                    MouseButton::Left => { self.cmd(json!({"cmd": "tab", "index": idx})); self.tab_drag = Some((idx, x, false)); }
                    MouseButton::Middle => self.close_tab_asking(idx),
                    _ => {}
                }
                self.redraw();
                return;
            }
        }
        if !pressed && button == MouseButton::Left {
            if let Some((name, _, moved)) = self.badge_drag.take() {
                if !moved {
                    if let Some(a) = self.badge_rects.iter().find(|b| b.2 == name).map(|b| b.3.clone()) { self.run(a); }
                } else { self.drop_badge(&name, x); }
                self.redraw();
                return;
            }
            if let Some((idx, _, moved)) = self.tab_drag.take() {
                if moved { self.drop_tab(idx, x, y); }
                self.redraw();
                return;
            }
            if self.div_drag.take().is_some() { self.redraw(); return; }
            if std::mem::take(&mut self.ext_drag) {
                let _ = set_local_setting("ext_panel_width", &format!("{}", self.ext_w.round()));
                self.send_size();
                self.redraw();
                return;
            }
        }
        // Barra de titulo vazia: arrasta a janela; duplo clique maximiza.
        let c = self.chrome();
        if pressed && button == MouseButton::Left && y < c.top && self.custom_titlebar() {
            if let Some(r) = &self.r {
                if self.title_click.elapsed() < Duration::from_millis(400) { r.window.set_maximized(!r.window.is_maximized()); }
                else { let _ = r.window.drag_window(); }
            }
            self.title_click = Instant::now();
            return;
        }
        if pressed && button == MouseButton::Left && self.ext_panel_edge(x, y) {
            self.ext_drag = true;
            return;
        }
        if pressed && button == MouseButton::Left {
            if let Some((pane, vertical)) = self.divider_at(x, y) {
                self.div_drag = Some((pane, vertical, if vertical { x } else { y }));
                return;
            }
        }
        // Clique na faixa vermelha de um bloco que falhou: corrigir com agente.
        if pressed && button == MouseButton::Left {
            if let Some(r) = &self.r {
                let (ox, oy) = self.origin;
                let hit = self.frame.panes.iter().find_map(|p| {
                    let px = ox + p.x as f32 * r.cell_w;
                    if (x - (px - 4.0)).abs() > 5.0 { return None; }
                    let row = ((y - oy) / r.cell_h).floor() as i64 - p.y as i64;
                    p.blocks.iter().find(|b| b.row == row && b.exit.is_some_and(|e| e != 0)).map(|b| (p.id, b.cmd.clone(), b.exit))
                });
                if let Some((pid, cmd, exit)) = hit {
                    if pid != self.frame.active_pane { self.cmd(json!({"cmd": "focus", "pane": pid})); }
                    let items = vec![
                        Item::new("Corrigir com agente", "manda comando + saida", "fix_with_agent", vec![]),
                        Item::new("Copiar a saida", "", "copy_last_output", vec![]),
                        Item::new("Cancelar", "Esc", "", vec![]),
                    ];
                    self.overlay = Some(Overlay::dialog(&format!("Falhou (exit {}): {cmd}", exit.unwrap_or(0)), items));
                    self.redraw();
                    return;
                }
            }
        }
        let Some((pane, cell)) = self.cell_at(x, y) else { return };
        if self.pane(pane).is_some_and(|p| p.mouse) && !self.mods.shift_key() {
            let b = match button { MouseButton::Left => 0, MouseButton::Middle => 1, MouseButton::Right => 2, _ => return };
            let end = if pressed { 'M' } else { 'm' };
            self.dragging = pressed;
            if pane != self.frame.active_pane && pressed { self.cmd(json!({"cmd": "focus", "pane": pane})); }
            self.cmd(json!({"cmd": "send", "pane": pane, "text": format!("\x1b[<{};{};{}{}", b, cell.col + 1, cell.row + 1, end)}));
            return;
        }
        match (button, state) {
            (MouseButton::Left, ElementState::Pressed) => {
                if self.mods.control_key() && self.open_link_at(pane, cell) { return; }
                if pane != self.frame.active_pane { self.cmd(json!({"cmd": "focus", "pane": pane})); }
                if self.last_click.0.elapsed() < Duration::from_millis(400) && self.last_click.1 == cell {
                    if let Some(p) = self.pane(pane) {
                        let chars = p.row_chars(cell.row as usize);
                        let word = |c: char| c.is_alphanumeric() || "_-./\\:~".contains(c);
                        let (mut a, mut b) = (cell.col as usize, cell.col as usize);
                        while a > 0 && chars.get(a - 1).is_some_and(|c| word(*c)) { a -= 1; }
                        while chars.get(b + 1).is_some_and(|c| word(*c)) { b += 1; }
                        self.sel = Some(Selection { pane, a: Cell { col: a as i32, row: cell.row }, b: Cell { col: b as i32, row: cell.row } });
                        self.copy_selection();
                    }
                    return;
                }
                self.last_click = (Instant::now(), cell);
                self.sel = Some(Selection { pane, a: cell, b: cell });
                self.dragging = true;
            }
            (MouseButton::Left, ElementState::Released) => {
                self.dragging = false;
                if self.sel.as_ref().is_some_and(|s| s.a != s.b) { self.copy_selection(); } else { self.sel = None; }
            }
            (MouseButton::Right, ElementState::Pressed) | (MouseButton::Middle, ElementState::Pressed) => self.builtin("paste", &[]),
            _ => {}
        }
        self.redraw();
    }

    /// Soltou a aba: em outra posicao reordena; fora da janela vira janela nova.
    /// Solta o badge: reordena e grava a ordem (ext_order no ~/.duck-local.lua).
    fn drop_badge(&mut self, name: &str, x: f32) {
        let mut rects = self.badge_rects.clone();
        rects.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut order: Vec<String> = rects.iter().map(|b| b.2.clone()).collect();
        let (Some(from), Some(to)) = (order.iter().position(|n| n == name), rects.iter().position(|b| x >= b.0 && x < b.1)) else { return };
        if from == to { return; }
        let n = order.remove(from);
        order.insert(to, n);
        // Extensoes sem badge agora (desativadas) mantem o lugar salvo no fim.
        for o in &self.ext_order { if !order.contains(o) { order.push(o.clone()); } }
        self.ext_order = order.clone();
        if let Err(e) = set_local_setting("ext_order", &super::ext::lua_lit(&json!(order))) { self.flash(&e); }
    }

    fn drop_tab(&mut self, idx: usize, x: f32, y: f32) {
        let Some(r) = &self.r else { return };
        let (w, h) = (r.width(), r.height());
        if y < -20.0 || y > h + 20.0 || x < -20.0 || x > w + 20.0 {
            if self.frame.tabs.len() < 2 { return self.flash("A unica aba do workspace nao sai dele"); }
            let name = format!("{}-{}", self.frame.workspace, SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() % 10000).unwrap_or(0));
            match self.call(json!({"cmd": "move-tab", "from": idx, "workspace": name})) {
                Ok(_) => {
                    if let Some(exe) = crate::gui_exe() {
                        let _ = crate::proto::spawn_detached(&[exe.to_string_lossy().to_string(), "--workspace".into(), name.clone()], &[]);
                    }
                    self.flash(&format!("Aba aberta numa janela nova (workspace {name})"));
                }
                Err(e) => self.flash(&e),
            }
        } else if let Some(to) = self.tab_at(x, y) {
            if to != idx { self.cmd(json!({"cmd": "move-tab", "from": idx, "to": to})); }
        }
    }

    pub(super) fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        if let Some((name, x0, moved)) = self.badge_drag.clone() {
            if !moved && (x - x0).abs() > 8.0 { self.badge_drag = Some((name, x0, true)); }
            self.redraw();
            return;
        }
        if let Some((idx, x0, moved)) = self.tab_drag {
            if !moved && (x - x0).abs() > 8.0 { self.tab_drag = Some((idx, x0, true)); }
            self.redraw();
            return;
        }
        let Some(r) = &self.r else { return };
        let (cw, ch, w, h) = (r.cell_w, r.cell_h, r.width(), r.height());
        if self.ext_drag {
            self.ext_w = (w - x).clamp(260.0, (w * 0.75).max(260.0));
            self.send_size();
            self.redraw();
            return;
        }
        if let Some((pane, vertical, last)) = self.div_drag {
            let cell = if vertical { cw } else { ch };
            let n = (((if vertical { x } else { y }) - last) / cell).trunc() as i64;
            if n != 0 {
                self.cmd(json!({"cmd": "adjust", "pane": pane, "dir": if vertical { "right" } else { "down" }, "amount": n}));
                self.div_drag = Some((pane, vertical, last + n as f32 * cell));
            }
            return;
        }
        // Janela sem moldura: as bordas redimensionam.
        self.resize_edge = None;
        if self.custom_titlebar() && !r.window.is_maximized() {
            let e = 6.0;
            let (l, rt, t, b) = (x < e, x > w - e, y < e, y > h - e);
            self.resize_edge = match (l, rt, t, b) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (_, true, true, _) => Some(ResizeDirection::NorthEast),
                (true, _, _, true) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, ..) => Some(ResizeDirection::West),
                (_, true, ..) => Some(ResizeDirection::East),
                (_, _, true, _) => Some(ResizeDirection::North),
                (_, _, _, true) => Some(ResizeDirection::South),
                _ => None,
            };
        }
        let ctrl = self.mods.control_key();
        let c = self.chrome();
        let over_hit = self.hits.iter().any(|hh| inside(hh.r, (x, y))) || (y >= self.badge_row.0 && y < self.badge_row.0 + self.badge_row.1 && self.badge_rects.iter().any(|b| x >= b.0 && x < b.1));
        let icon = match self.resize_edge {
            Some(ResizeDirection::West | ResizeDirection::East) => CursorIcon::EwResize,
            Some(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            Some(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => CursorIcon::NwseResize,
            Some(_) => CursorIcon::NeswResize,
            None if self.ext_drag || self.ext_panel_edge(x, y) => CursorIcon::ColResize,
            None => match self.divider_at(x, y) {
                Some((_, true)) => CursorIcon::ColResize,
                Some((_, false)) => CursorIcon::RowResize,
                None if over_hit || ctrl => CursorIcon::Pointer,
                None if y < c.top || y > h - c.bottom || x < c.left || x > w - c.right || self.overlay.is_some() => CursorIcon::Default,
                None => CursorIcon::Text,
            },
        };
        r.window.set_cursor(icon);
        // Previa da aba: depois de parar o mouse em cima.
        let th = self.tab_at(x, y);
        if th != self.tab_hover.map(|t| t.0) {
            self.tab_hover = th.map(|i| (i, Instant::now()));
            self.tab_preview = None;
            if th.is_some() { self.tick_in(Duration::from_millis(480)); }
        }
        // Ctrl sobre um link OSC 8: mostra o destino real antes do clique.
        if ctrl {
            if let Some((pane, cell)) = self.cell_at(x, y) {
                if let Some((uri, _)) = self.pane(pane).and_then(|p| p.link_at(cell.row as usize, cell.col as usize)) {
                    if self.message.as_ref().is_none_or(|m| m.0 != uri) { self.flash(&uri); }
                }
            }
        }
        self.redraw();
        if !self.dragging { return; }
        if let Some((pane, cell)) = self.cell_at(x, y) {
            if self.pane(pane).is_some_and(|p| p.mouse && p.mouse_motion) && !self.mods.shift_key() {
                self.cmd(json!({"cmd": "send", "pane": pane, "text": format!("\x1b[<32;{};{}M", cell.col + 1, cell.row + 1)}));
                return;
            }
            if let Some(s) = self.sel.as_mut() { if s.pane == pane { s.b = cell; } }
        }
    }

    pub(super) fn wheel(&mut self, lines: f32) {
        let (x, y) = self.mouse;
        let n = (lines * 3.0).round() as i64;
        if n == 0 { return; }
        // Roda sobre as abas troca de aba; sobre a paleta move a selecao.
        if self.tab_at(x, y).is_some() { return self.cmd(json!({"cmd": "tab", "delta": if n > 0 { -1 } else { 1 }})); }
        if self.ext_panel.is_some() && self.overlay.is_none() {
            let c = self.chrome();
            if let Some(r) = &self.r {
                if x >= r.width() - c.right {
                    let max = (self.ext_content - (r.height() - c.top - c.bottom) + 60.0).max(0.0);
                    self.ext_scroll = (self.ext_scroll - lines * r.cell_h * 3.0).clamp(0.0, max);
                    return self.redraw();
                }
            }
        }
        if let Some(Overlay { kind: OverlayKind::Detail { scroll, lines: body, .. }, .. }) = self.overlay.as_mut() {
            *scroll = (*scroll as i64 - n).clamp(0, body.len().saturating_sub(5) as i64) as usize;
            return self.redraw();
        }
        if let Some(o) = self.overlay.as_mut() {
            if !o.shown.is_empty() { o.sel = (o.sel as i64 - n.signum()).rem_euclid(o.shown.len() as i64) as usize; }
            return self.redraw();
        }
        let Some((pane, cell)) = self.cell_at(x, y) else { return };
        let Some(p) = self.pane(pane) else { return };
        if p.mouse {
            let b = if n > 0 { 64 } else { 65 };
            let seq = format!("\x1b[<{};{};{}M", b, cell.col + 1, cell.row + 1).repeat(n.unsigned_abs().min(5) as usize);
            self.cmd(json!({"cmd": "send", "pane": pane, "text": seq}));
        } else if p.alt_screen {
            let key = if n > 0 { if p.app_cursor { "\x1bOA" } else { "\x1b[A" } } else if p.app_cursor { "\x1bOB" } else { "\x1b[B" };
            self.cmd(json!({"cmd": "send", "pane": pane, "text": key.repeat(n.unsigned_abs() as usize)}));
        } else {
            self.cmd(json!({"cmd": "scroll", "pane": pane, "delta": n}));
        }
    }

    // -------------------------------------------------------------- acoes de UI

    /// Acoes novas; false = nome desconhecido.
    pub(super) fn builtin_ui(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let n = |i: usize| args.get(i).and_then(|v| v.as_i64()).unwrap_or(0);
        let f = |i: usize| args.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let ap = self.frame.active_pane;
        match name {
            "win_min" => if let Some(r) = &self.r { r.window.set_minimized(true) },
            "win_max" => if let Some(r) = &self.r { r.window.set_maximized(!r.window.is_maximized()) },
            "win_close" => std::process::exit(0),
            "close_tab_idx" => self.close_tab_asking(n(0) as usize),
            "close_tab_now" => self.cmd(json!({"cmd": "close-tab", "index": n(0)})),
            "toggle_sidebar" => {
                // Liga a barra lateral nesta janela (para sempre: Configuracoes).
                if self.cfg.tab_bar != "left" { self.cfg.tab_bar = "left".into(); self.sidebar_collapsed = false; }
                else { self.sidebar_collapsed = !self.sidebar_collapsed; }
                self.send_size();
            }
            "focus_mode" => { self.focus_mode = !self.focus_mode; self.send_size(); if self.focus_mode { self.flash("Modo foco (Ctrl+Shift+Z sai)"); } }
            "inbox" => self.open_inbox(),
            "inbox_clear" => { self.cmd(json!({"cmd": "inbox-clear"})); self.overlay = None; self.flash("Avisos apagados"); }
            "inbox_open" => {
                let v = args.first().cloned().unwrap_or(Value::Null);
                let pane = v["pane"].as_u64().unwrap_or(0);
                if v["kind"] == "agent" || (v["kind"] == "blocked" && pane != 0) {
                    self.overlay = None;
                    self.cmd(json!({"cmd": "focus", "pane": pane}));
                } else if v["kind"] == "ext" {
                    let ext = v["ext"].as_str().unwrap_or("").to_string();
                    let item = v["item"].as_str().unwrap_or("").to_string();
                    let actions = v["actions"].as_array().into_iter().flatten()
                        .map(|a| (a["id"].as_str().unwrap_or("").to_string(), a["title"].as_str().unwrap_or("").to_string(), item.clone())).collect();
                    let lines = v["body"].as_str().unwrap_or("").replace("\r\n", "\n").lines().map(String::from).collect();
                    let subtitle = format!("{} · {}", self.exts.label(&ext), ago(v["at"].as_u64().unwrap_or(0)));
                    self.overlay = Some(Overlay { kind: OverlayKind::Detail { ext, subtitle, lines, actions, scroll: 0 }, ..Overlay::list(v["title"].as_str().unwrap_or(""), vec![]) });
                }
            }
            "agent_panel" => {
                if self.agent_panel.take().is_none() { self.sys_panel = false; self.ext_panel = None; self.refresh_agent_panel(true); }
                self.stats.lock().unwrap().want_procs = self.sys_panel;
                self.send_size();
            }
            "sys_panel" => {
                self.sys_panel = !self.sys_panel;
                if self.sys_panel { self.agent_panel = None; self.ext_panel = None; }
                self.stats.lock().unwrap().want_procs = self.sys_panel;
                self.send_size();
            }
            "agent_timeline" => self.open_timeline(),
            "composer" => {
                // Comeca com o que ja estava digitado no prompt (e apaga de la).
                let typed = self.typed_now().unwrap_or_default();
                if !typed.is_empty() { self.send_keys(&"\x7f".repeat(typed.chars().count())); }
                let text: Vec<char> = typed.chars().collect();
                let cur = text.len();
                self.composer = Some(Composer { pane: ap, text, cur, anchor: None, undo: vec![], redo: vec![], geom: [0.0; 4] });
                self.ac = None;
            }
            "home" => self.open_home(),
            "theme_gallery" => self.open_theme_gallery(),
            "wallpaper_theme" => match wallpaper_theme() {
                Ok(name) => {
                    let _ = set_local_setting("theme", &format!("{name:?}"));
                    self.flash(&format!("Tema gerado do papel de parede: {name}"));
                }
                Err(e) => self.flash(&format!("Tema do papel de parede: {e}")),
            },
            "shader_pick" => {
                let name = s(0);
                if name.is_empty() { let _ = set_local_setting("shader", "nil"); self.flash("Shader desligado"); return true; }
                let src = match name.as_str() { "crt" => SHADER_CRT, "glow" => SHADER_GLOW, _ => SHADER_GRAIN };
                let dir = config::home().join(".duck").join("shaders");
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join(format!("{name}.wgsl"));
                match std::fs::write(&path, src).map_err(|e| e.to_string()).and_then(|_| set_local_setting("shader", &format!("{:?}", path.to_string_lossy()))) {
                    Ok(_) => self.flash(&format!("Shader {name} ligado")),
                    Err(e) => self.flash(&e),
                }
            }
            "slider" => {
                let (key, label) = (s(0), s(1));
                let val = self.slider_value(&key);
                self.overlay = Some(Overlay { kind: OverlayKind::Slider { key, label, val, orig: val, min: f(2), max: f(3), step: f(4) }, ..Overlay::list("", vec![]) });
            }
            "slider_set" => {
                let mx = self.mouse.0;
                let set = self.overlay.as_mut().and_then(|o| match &mut o.kind {
                    OverlayKind::Slider { key, val, min, max, step, .. } => {
                        let frac = ((mx - f(0)) / f(1)).clamp(0.0, 1.0);
                        *val = ((*min + frac * (*max - *min)) / *step).round() * *step;
                        Some((key.clone(), *val))
                    }
                    _ => None,
                });
                if let Some((k, v)) = set { self.apply_slider(&k, v); }
            }
            "block_copy" | "block_agent" | "block_pin" => {
                let (pane, abs) = (n(0) as u64, n(1));
                match self.call(json!({"cmd": "block-output", "pane": pane, "abs": abs})) {
                    Ok(v) => {
                        let text = v["text"].as_str().unwrap_or("").to_string();
                        let cmd = v["cmd"].as_str().unwrap_or("").to_string();
                        match name {
                            "block_copy" => {
                                if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(text.replace('\n', "\r\n")); }
                                self.flash(&format!("Saida copiada ({} linhas)", text.lines().count()));
                            }
                            "block_pin" => {
                                let lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect();
                                let tail = lines[lines.len().saturating_sub(8)..].to_vec();
                                self.pinned.insert(pane, (abs, cmd, tail));
                            }
                            _ => {
                                let lines: Vec<&str> = text.lines().collect();
                                let tail = lines[lines.len().saturating_sub(80)..].join("\n");
                                let msg = format!("Veja a saida deste comando (exit {}):\n\n$ {cmd}\n{tail}", v["exit"]);
                                let target = self.agent_in_tab().filter(|id| *id != pane);
                                match target {
                                    Some(t) => { self.cmd(json!({"cmd": "queue", "pane": t, "text": msg})); self.flash(&format!("Saida enviada ao agente do pane {t}")); }
                                    None => self.flash("Nenhum agente nesta aba (Ctrl+Shift+N abre um)"),
                                }
                            }
                        }
                    }
                    Err(e) => self.flash(&e),
                }
            }
            "block_rerun" => {
                let cmd = s(1);
                if !cmd.is_empty() { self.cmd(json!({"cmd": "send", "pane": n(0), "text": format!("{cmd}\r")})); }
            }
            "block_toggle" => {
                let cur = args.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
                self.collapsed.insert((n(0) as u64, n(1)), !cur);
            }
            "unpin" => { self.pinned.remove(&(n(0) as u64)); }
            "minimap" => {
                let (pane, y0, ph, hist, rows) = (n(0), f(1), f(2), n(3), n(4));
                let frac = ((self.mouse.1 - y0) / ph).clamp(0.0, 1.0) as f64;
                let abs = (frac * (hist + rows) as f64) as i64;
                self.cmd(json!({"cmd": "goto-line", "pane": pane, "line": abs - hist}));
            }
            "send_pane" => self.cmd(json!({"cmd": "send", "pane": n(0), "text": s(1)})),
            "search_prev" => self.search_step(1),
            "goto_tab" => {
                self.cmd(json!({"cmd": "workspace", "name": s(0)}));
                self.cmd(json!({"cmd": "tab", "index": n(1)}));
            }
            "ac_accept" => self.ac_accept(n(0) as usize),
            "overlay_pick" => {
                if let Some(mut o) = self.overlay.take() {
                    o.sel = n(0) as usize;
                    self.overlay_enter(o);
                }
            }
            "graph_pick" => self.graph_pick(n(0) as usize),
            "grid_pick" => {
                if let Some(id) = self.grid.as_ref().and_then(|g| g.panes.get(n(0) as usize)).map(|p| p.0) {
                    self.grid = None;
                    self.cmd(json!({"cmd": "focus", "pane": id}));
                }
            }
            "preview" => self.open_preview(&s(0), n(1).max(1), n(2).max(1)),
            "preview_open" => {
                if let Some(o) = self.overlay.take() {
                    if let OverlayKind::Preview { file, line, col, .. } = o.kind { self.open_in_editor(&file, line, col); }
                }
            }
            "detail_action" => {
                if let Some(o) = self.overlay.take() {
                    if let OverlayKind::Detail { ext, actions, .. } = o.kind {
                        if let Some((id, _, item)) = actions.get(n(0) as usize) { self.ext_builtin("ext_action", &[json!(ext), json!(id), json!(item)]); }
                    }
                }
            }
            _ => return self.ext_builtin(name, args),
        }
        self.redraw();
        true
    }

    /// Fechar aba pelo ✕ ou clique do meio: com agente, pergunta antes.
    pub(super) fn close_tab_asking(&mut self, idx: usize) {
        let Some(t) = self.frame.tabs.iter().find(|t| t.index == idx).cloned() else { return };
        if t.profile.is_empty() && t.state.is_empty() { return self.cmd(json!({"cmd": "close-tab", "index": idx})); }
        let title = if t.title.is_empty() { t.pane_title.clone() } else { t.title.clone() };
        let items = vec![
            Item::new("Cancelar", "Esc", "", vec![]),
            Item::new("Fechar e encerrar o agente", "", "close_tab_now", vec![json!(idx)]),
        ];
        self.overlay = Some(Overlay::dialog(&format!("Fechar a aba \"{title}\"? O agente dela sera encerrado (a sessao fica salva e da para retomar com Ctrl+Shift+H)."), items));
    }

    /// Enter numa lista/prompt (teclado ou clique).
    pub(super) fn overlay_enter(&mut self, o: Overlay) {
        match o.kind {
            OverlayKind::List => if let Some(&i) = o.shown.get(o.sel) {
                let it = o.items[i].clone();
                if o.palette && is_shortcut(&it.hint) {
                    if let Action::Builtin(n, _) = &it.action {
                        let c = self.usage.entry(n.clone()).or_insert(0);
                        *c += 1;
                        if *c == 3 { self.message = Some((format!("Dica: {} faz isso direto", it.hint), Instant::now())); }
                    }
                }
                self.run(it.action);
            },
            OverlayKind::Prompt(action, mut args) => { args.push(json!(o.input)); self.builtin(&action, &args); }
            OverlayKind::Preview { file, line, col, .. } => self.open_in_editor(&file, line, col),
            OverlayKind::Detail { .. } => {}
            OverlayKind::Slider { key, val, .. } => {
                let v = if key == "ui.font_weight" { format!("{}", val.round() as i64) } else { format!("{val:.2}") };
                match set_local_setting(&key, &v) { Ok(_) => self.flash(&format!("{key} = {v}")), Err(e) => self.flash(&e) }
            }
        }
    }

    /// Teclas especiais de overlays novos; true = tratada.
    pub(super) fn overlay_key_ui(&mut self, ev: &KeyEvent) -> bool {
        let Some(o) = self.overlay.as_mut() else { return false };
        let key = &ev.logical_key;
        match &mut o.kind {
            OverlayKind::Detail { scroll, lines, .. } => {
                let max = lines.len().saturating_sub(5);
                match key {
                    Key::Named(NamedKey::Escape) => { self.overlay = None; }
                    Key::Named(NamedKey::ArrowDown) => *scroll = (*scroll + 1).min(max),
                    Key::Named(NamedKey::ArrowUp) => *scroll = scroll.saturating_sub(1),
                    Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => *scroll = (*scroll + 15).min(max),
                    Key::Named(NamedKey::PageUp) => *scroll = scroll.saturating_sub(15),
                    _ => {}
                }
                return true;
            }
            OverlayKind::Preview { .. } => {
                match key {
                    Key::Named(NamedKey::Escape) => { self.overlay = None; }
                    Key::Named(NamedKey::Enter) => { let o = self.overlay.take().unwrap(); self.overlay_enter(o); }
                    _ => {}
                }
                return true;
            }
            OverlayKind::Slider { key: k, val, orig, min, max, step, .. } => {
                let (k, orig) = (k.clone(), *orig);
                match key {
                    Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowDown) => { *val = (*val - *step).max(*min); let v = *val; self.apply_slider(&k, v); }
                    Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowUp) => { *val = (*val + *step).min(*max); let v = *val; self.apply_slider(&k, v); }
                    Key::Named(NamedKey::Escape) => { self.overlay = None; self.apply_slider(&k, orig); }
                    Key::Named(NamedKey::Enter) => { let o = self.overlay.take().unwrap(); self.overlay_enter(o); }
                    _ => {}
                }
                return true;
            }
            _ => {}
        }
        // Dialogo: setas laterais trocam o botao.
        if o.dialog {
            match key {
                Key::Named(NamedKey::ArrowRight) => { if !o.shown.is_empty() { o.sel = (o.sel + 1) % o.shown.len(); } return true; }
                Key::Named(NamedKey::ArrowLeft) => { if !o.shown.is_empty() { o.sel = (o.sel + o.shown.len() - 1) % o.shown.len(); } return true; }
                _ => {}
            }
        }
        // Galeria: mover a selecao aplica o tema na hora.
        if o.gallery.is_some() && matches!(key, Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab | NamedKey::PageDown | NamedKey::PageUp)) {
            let len = o.shown.len().max(1);
            o.sel = match key {
                Key::Named(NamedKey::ArrowUp) => (o.sel + len - 1) % len,
                Key::Named(NamedKey::PageDown) => (o.sel + 10).min(len - 1),
                Key::Named(NamedKey::PageUp) => o.sel.saturating_sub(10),
                _ => (o.sel + 1) % len,
            };
            let name = o.shown.get(o.sel).and_then(|&i| match &o.items[i].action { Action::Builtin(_, a) => a.get(1).and_then(|v| v.as_str()).map(|s| s.trim_matches('"').to_string()), _ => None });
            if let Some(n) = name { self.cfg.theme = config::named_theme(&n); }
            return true;
        }
        false
    }

    // -------------------------------------------------------------- paineis

    pub(super) fn refresh_agent_panel(&mut self, open: bool) {
        if !open && self.agent_panel.as_ref().is_some_and(|a| a.fetched.elapsed() < Duration::from_secs(2)) { return; }
        let want = self.agent_panel.as_ref().map(|a| a.pane);
        let pick = |p: &PaneView| !p.agent.is_empty();
        let pane = want.filter(|id| self.frame.panes.iter().any(|p| p.id == *id))
            .or_else(|| self.active().filter(|p| pick(p)).map(|p| p.id))
            .or_else(|| self.frame.panes.iter().find(|p| pick(p)).map(|p| p.id))
            .or(want);
        let Some(pane) = pane else {
            if open { self.flash("Nenhum agente nesta aba (Ctrl+Shift+N abre um)"); }
            return;
        };
        let Ok(v) = self.call(json!({"cmd": "list"})) else { return };
        let mut found = None;
        for w in v["workspaces"].as_array().into_iter().flatten() {
            for t in w["tabs"].as_array().into_iter().flatten() {
                for p in t["panes"].as_array().into_iter().flatten() {
                    if p["id"].as_u64() == Some(pane) { found = Some(p["agent"].clone()); }
                }
            }
        }
        let Some(a) = found.filter(|a| !a.is_null()) else {
            if open { self.flash("Este pane nao tem agente"); }
            return;
        };
        let dir = a["config_dir"].as_str().unwrap_or("").to_string();
        let plan = a["session"].as_str().and_then(|s| find_transcript(&dir, s))
            .map(|p| crate::cli::transcript_plan(&p.to_string_lossy())).unwrap_or_default();
        let profile = if dir.is_empty() { String::new() } else {
            let b = dir.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").trim_start_matches(".claude").trim_start_matches(['-', '_']).to_string();
            if b.is_empty() { "pessoal".into() } else { b }
        };
        self.agent_panel = Some(AgentPanel {
            pane, name: a["name"].as_str().unwrap_or("agente").to_string(), profile,
            state: a["state"].as_str().unwrap_or("").to_string(), ctx: a["context_tokens"].as_u64(),
            session_tokens: a["session_tokens"].as_u64(), plan, fetched: Instant::now(),
            todo: a["todo_done"].as_u64().zip(a["todo_total"].as_u64()),
        });
    }

    /// Linha do tempo da sessao do agente: pedidos, ferramentas, arquivos, erros.
    fn open_timeline(&mut self) {
        let pick = self.active().filter(|p| p.session.is_some()).map(|p| (p.session.clone(), p.config_dir.clone()))
            .or_else(|| self.frame.panes.iter().find(|p| p.session.is_some()).map(|p| (p.session.clone(), p.config_dir.clone())));
        let Some((Some(session), dir)) = pick else { return self.flash("Nenhuma sessao de agente nesta aba (hooks do Duck instalados?)") };
        let Some(path) = find_transcript(dir.as_deref().unwrap_or(""), &session) else { return self.flash("Transcript da sessao nao encontrado") };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let mut items = Vec::new();
        // O transcript grava em UTC; mostra no fuso da maquina.
        let utc_now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| (d.as_secs() / 60 % 1440) as i64).unwrap_or(0);
        let local_now = clock().split(':').map(|x| x.parse::<i64>().unwrap_or(0)).fold(0, |a, x| a * 60 + x);
        let off = (local_now - utc_now).rem_euclid(1440);
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let ts = v["timestamp"].as_str().and_then(|t| t.get(11..16)).map(|hm| {
                let m = hm.split(':').map(|x| x.parse::<i64>().unwrap_or(0)).fold(0, |a, x| a * 60 + x);
                let l = (m + off).rem_euclid(1440);
                format!("{:02}:{:02}", l / 60, l % 60)
            }).unwrap_or_default();
            let content = &v["message"]["content"];
            if v["type"] == "user" {
                if let Some(t) = content.as_str() {
                    if !t.starts_with('<') { items.push(Item::new(format!("💬 {}", t.replace('\n', " ").chars().take(120).collect::<String>()), ts.clone(), "", vec![])); }
                }
                for b in content.as_array().into_iter().flatten() {
                    if b["type"] == "tool_result" && b["is_error"] == true {
                        let msg = b["content"].as_str().map(String::from).or_else(|| b["content"].as_array().and_then(|a| a.iter().find_map(|x| x["text"].as_str().map(String::from)))).unwrap_or_default();
                        items.push(Item::new(format!("✗ erro: {}", msg.lines().next().unwrap_or("").chars().take(120).collect::<String>()), ts.clone(), "", vec![]));
                    } else if b["type"] == "text" {
                        let t = b["text"].as_str().unwrap_or("");
                        if !t.starts_with('<') && !t.is_empty() { items.push(Item::new(format!("💬 {}", t.replace('\n', " ").chars().take(120).collect::<String>()), ts.clone(), "", vec![])); }
                    }
                }
            }
            if v["type"] == "assistant" {
                for b in content.as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                    let name = b["name"].as_str().unwrap_or("");
                    let i = &b["input"];
                    let file = i["file_path"].as_str().or_else(|| i["notebook_path"].as_str()).unwrap_or("");
                    let it = match name {
                        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Item::new(format!("✎ {name} {file}"), ts.clone(), "preview", vec![json!(file), json!(1), json!(1)]),
                        "Read" => Item::new(format!("📄 Read {file}"), ts.clone(), "preview", vec![json!(file), json!(1), json!(1)]),
                        "Bash" | "PowerShell" => Item::new(format!("⚙ {name}: {}", i["command"].as_str().unwrap_or("").replace('\n', " ").chars().take(110).collect::<String>()), ts.clone(), "", vec![]),
                        "TaskCreate" => Item::new(format!("☐ tarefa: {}", i["subject"].as_str().unwrap_or("")), ts.clone(), "", vec![]),
                        "TaskUpdate" => Item::new(format!("☑ tarefa {} → {}", i["taskId"], i["status"].as_str().unwrap_or("")), ts.clone(), "", vec![]),
                        _ => {
                            let d = i["pattern"].as_str().or_else(|| i["description"].as_str()).or_else(|| i["query"].as_str()).unwrap_or("");
                            Item::new(format!("⚙ {name} {}", d.chars().take(100).collect::<String>()), ts.clone(), "", vec![])
                        }
                    };
                    items.push(it);
                }
            }
        }
        if items.is_empty() { return self.flash("Linha do tempo vazia"); }
        let start = items.len().saturating_sub(600);
        let items: Vec<Item> = items.into_iter().skip(start).collect();
        let n = items.len();
        let mut o = Overlay::list(&format!("Linha do tempo da sessao ({n} eventos; Enter num arquivo abre no editor)"), items);
        o.sel = n.saturating_sub(1);
        self.overlay = Some(o);
    }

    pub(super) fn session_items(&mut self, limit: usize) -> Vec<Item> {
        // (mtime, perfil, sessao, caminho, WSL: (dominio, home no Linux))
        let mut found: Vec<(u64, String, String, String, Option<(String, String)>)> = Vec::new();
        for (mtime, prof, session, path) in scan_profiles(&config::home()) {
            found.push((mtime, prof, session, path, None));
        }
        found.extend(wsl_sessions(&self.cfg.domains));
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.truncate(limit);
        let mut items = Vec::new();
        for (mtime, prof, session, path, wsl) in found {
            let head = read_head(&path, 96 * 1024);
            let (mut cwd, mut title) = (String::new(), String::new());
            for line in head.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                if cwd.is_empty() { if let Some(c) = v["cwd"].as_str() { cwd = c.to_string(); } }
                if title.is_empty() && v["type"] == "user" {
                    let c = &v["message"]["content"];
                    let t = c.as_str().map(String::from).or_else(|| c.as_array().and_then(|a| a.iter().find_map(|x| x["text"].as_str().map(String::from)))).unwrap_or_default();
                    if !t.starts_with('<') { title = t.replace('\n', " ").chars().take(80).collect(); }
                }
                if !cwd.is_empty() && !title.is_empty() { break; }
            }
            if cwd.is_empty() { continue; }
            let dir = cwd.rsplit(['/', '\\']).next().unwrap_or("").to_string();
            let perfil = prof.trim_start_matches(".claude").trim_start_matches('-');
            let perfil = if perfil.is_empty() { "pessoal" } else { perfil };
            match wsl {
                None => {
                    let cfg_dir = config::home().join(&prof).display().to_string();
                    let env = match (prof == ".claude", cfg!(windows)) {
                        (true, _) => String::new(),
                        (false, true) => format!("$env:CLAUDE_CONFIG_DIR='{cfg_dir}'; "),
                        (false, false) => format!("CLAUDE_CONFIG_DIR='{cfg_dir}' "),
                    };
                    let cmd = format!("{env}claude --resume {session}");
                    let argv = if cfg!(windows) { json!(["pwsh", "-NoLogo", "-NoExit", "-Command", cmd]) } else { crate::shell_keep(&cmd) };
                    items.push(Item::new(format!("{dir} · {title}"), format!("sessao · {perfil} · {}", ago(mtime)), "spawn",
                        vec![json!({"where": "tab", "cwd": cwd, "title": dir, "cmd": argv})]));
                }
                Some((domain, home)) => {
                    // No shell do WSL (o bash fica depois que o claude sai), como a restauracao do checkpoint.
                    let env = if prof == ".claude" { String::new() } else { format!("CLAUDE_CONFIG_DIR='{home}/{prof}' ") };
                    items.push(Item::new(format!("{dir} · {title}"), format!("sessao · {domain} · {perfil} · {}", ago(mtime)), "spawn",
                        vec![json!({"where": "tab", "domain": domain, "cwd": cwd, "title": dir, "type": format!("{env}claude --resume {session}")})]));
                }
            }
        }
        items
    }

    /// Paleta unificada: abas de todos os workspaces, sessoes, historico e arquivos.
    pub(super) fn palette_more(&mut self, items: &mut Vec<Item>) {
        if let Ok(v) = self.call(json!({"cmd": "list"})) {
            for w in v["workspaces"].as_array().into_iter().flatten() {
                let wn = w["name"].as_str().unwrap_or("");
                for (i, t) in w["tabs"].as_array().into_iter().flatten().enumerate() {
                    let title = t["title"].as_str().filter(|s| !s.is_empty()).map(String::from)
                        .or_else(|| t["panes"][0]["title"].as_str().map(String::from)).unwrap_or_default();
                    items.push(Item::new(format!("Aba: {title}"), format!("aba · {wn} {}", i + 1), "goto_tab", vec![json!(wn), json!(i + 1)]));
                }
            }
        }
        items.extend(self.session_items(20));
        if let Ok(v) = self.call(json!({"cmd": "history", "limit": 400})) {
            let mut seen = HashSet::new();
            for h in v["history"].as_array().into_iter().flatten().rev() {
                let cmd = h["cmd"].as_str().unwrap_or("").to_string();
                if cmd.is_empty() || !seen.insert(cmd.clone()) { continue; }
                items.push(Item::new(cmd.clone(), "historico", "type_text", vec![json!(cmd)]));
                if seen.len() >= 120 { break; }
            }
        }
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        if !cwd.is_empty() && !cwd.starts_with('/') {
            let files = git(&cwd, &["ls-files", "--cached", "--others", "--exclude-standard"]).unwrap_or_default();
            let top = git(&cwd, &["rev-parse", "--show-toplevel"]).map(|t| t.trim().to_string());
            for f in files.lines().take(3000) {
                let full = std::path::Path::new(&cwd).join(f);
                items.push(Item::new(f.to_string(), "arquivo", "open_file", vec![json!(full.to_string_lossy()), json!(1), json!(1)]));
            }
            let _ = top;
        }
    }

    /// Tela inicial: acoes, projetos recentes e sessoes.
    fn open_home(&mut self) {
        let mut items = vec![
            Item::new("Novo agente", "Acoes", "agent_new", vec![]),
            Item::new("Nova aba", "Acoes", "new_tab", vec![]),
            Item::new("Paleta de comandos", "Acoes", "palette", vec![]),
            Item::new("Galeria de temas", "Acoes", "theme_gallery", vec![]),
            Item::new("Configuracoes", "Acoes", "settings", vec![]),
        ];
        if let Ok(v) = self.call(json!({"cmd": "history", "limit": 2000})) {
            let mut seen = Vec::new();
            for h in v["history"].as_array().into_iter().flatten().rev() {
                let c = h["cwd"].as_str().unwrap_or("").to_string();
                if c.is_empty() || seen.contains(&c) { continue; }
                seen.push(c);
                if seen.len() >= 8 { break; }
            }
            for c in seen {
                let name = c.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(&c).to_string();
                items.push(Item::new(format!("{name}   {c}"), "Projetos recentes", "workspace", vec![json!(name), json!(c)]));
            }
        }
        for mut it in self.session_items(8) {
            it.hint = "Sessoes do Claude".into();
            items.push(it);
        }
        let mut o = Overlay::list(&format!("Workspace {}", self.frame.workspace), items);
        o.home = true;
        self.overlay = Some(o);
    }

    fn open_theme_gallery(&mut self) {
        let mut names: Vec<String> = config::THEMES.iter().map(|s| s.to_string()).collect();
        for e in std::fs::read_dir(config::home().join(".duck").join("themes")).into_iter().flatten().flatten() {
            if let Some(stem) = e.path().file_stem() { names.push(stem.to_string_lossy().to_string()); }
        }
        let mut items: Vec<Item> = names.iter().map(|t| Item::new(t.clone(), "setas: previa · Enter aplica", "setting", vec![json!("theme"), json!(format!("{t:?}"))])).collect();
        items.push(Item::new("Gerar tema do papel de parede", "Windows", "wallpaper_theme", vec![]));
        let mut o = Overlay::list("Galeria de temas (a previa muda ao mover a selecao)", items);
        o.gallery = Some(self.cfg.theme.clone());
        self.overlay = Some(o);
    }

    pub(super) fn settings_more(&mut self, items: &mut Vec<Item>) {
        let set = |label: &str, k: &str, v: &str| Item::new(label, "", "setting", vec![json!(k), json!(v)]);
        items.insert(0, Item::new("Galeria de temas (previa ao vivo)", "", "theme_gallery", vec![]));
        items.insert(1, Item::new("Tema a partir do papel de parede", "", "wallpaper_theme", vec![]));
        let slider = |label: &str, k: &str, min: f32, max: f32, step: f32| Item::new(label, "slider", "slider", vec![json!(k), json!(label), json!(min), json!(max), json!(step)]);
        items.extend([
            slider("Espacamento de linha", "font.line_height", 0.9, 2.0, 0.05),
            slider("Tamanho da fonte", "font.size", 8.0, 24.0, 0.5),
            slider("Peso da fonte", "ui.font_weight", 100.0, 900.0, 100.0),
            slider("Opacidade da janela", "opacity", 0.5, 1.0, 0.02),
            slider("Raio dos cantos", "ui.radius", 0.0, 16.0, 1.0),
            set("Barra de abas na lateral (abas verticais)", "tab_bar", "\"left\""),
            set("Barra de titulo propria (abas + botoes)", "ui.titlebar", "\"custom\""),
            set("Barra de titulo do Windows", "ui.titlebar", "\"native\""),
            set("Panes como cartoes (cantos e sombra): sim", "ui.cards", "true"),
            set("Panes como cartoes: nao", "ui.cards", "false"),
            set("Brilho no pane ativo: sim", "ui.glow", "true"),
            set("Brilho no pane ativo: nao", "ui.glow", "false"),
            set("Cabecalho do pane: so com varios panes", "ui.pane_header", "\"multi\""),
            set("Cabecalho do pane: sempre", "ui.pane_header", "\"always\""),
            set("Cabecalho do pane: nunca", "ui.pane_header", "\"off\""),
            set("Animacoes: sim", "ui.animations", "true"),
            set("Animacoes: nao", "ui.animations", "false"),
            set("Cursor suave: sim", "ui.cursor_smooth", "true"),
            set("Cursor suave: nao", "ui.cursor_smooth", "false"),
            set("Cursor piscando: sim", "ui.cursor_blink", "true"),
            set("Cursor piscando: nao", "ui.cursor_blink", "false"),
            set("Autocompletar em popup: sim", "ui.autocomplete", "\"auto\""),
            set("Autocompletar em popup: nao", "ui.autocomplete", "\"off\""),
            set("Realce do comando digitado: fora do pwsh", "ui.syntax", "\"auto\""),
            set("Realce do comando digitado: sempre", "ui.syntax", "\"always\""),
            set("Realce do comando digitado: nao", "ui.syntax", "\"off\""),
            set("Previa ao clicar em arquivo: sim", "ui.link_preview", "true"),
            set("Previa ao clicar em arquivo: nao (direto no editor)", "ui.link_preview", "false"),
            set("Ligaduras da fonte: ligadas", "ui.font_features", "{}"),
            set("Ligaduras da fonte: desligadas", "ui.font_features", "{ '-calt', '-liga' }"),
            set("Variante da fonte ss01", "ui.font_features", "{ 'ss01' }"),
            set("Recolher saidas com mais de 300 linhas", "ui.collapse_lines", "300"),
            set("Nunca recolher saidas", "ui.collapse_lines", "0"),
            Item::new("Shader: CRT (curvatura e scanlines)", "", "shader_pick", vec![json!("crt")]),
            Item::new("Shader: brilho (glow)", "", "shader_pick", vec![json!("glow")]),
            Item::new("Shader: grao de filme", "", "shader_pick", vec![json!("grain")]),
            Item::new("Shader: nenhum", "", "shader_pick", vec![json!("")]),
        ]);
    }

    fn slider_value(&self, key: &str) -> f32 {
        match key {
            "font.line_height" => self.cfg.line_height,
            "font.size" => self.cfg.font_size,
            "ui.font_weight" => self.cfg.ui.font_weight as f32,
            "opacity" => self.cfg.opacity,
            "ui.radius" => self.cfg.ui.radius,
            _ => 0.0,
        }
    }

    pub(super) fn apply_slider(&mut self, key: &str, v: f32) {
        match key {
            "font.line_height" => self.cfg.line_height = v,
            "font.size" => self.cfg.font_size = v,
            "ui.font_weight" => self.cfg.ui.font_weight = v.round() as u16,
            "opacity" => self.cfg.opacity = v,
            "ui.radius" => self.cfg.ui.radius = v,
            _ => {}
        }
        if let Some(r) = self.r.as_mut() {
            let scale = r.window.scale_factor() as f32;
            if key.starts_with("font.") { r.set_font(&self.cfg.font_family, (self.cfg.font_size + self.font_delta) * 96.0 / 72.0 * scale, self.cfg.line_height); }
            if key == "ui.font_weight" || key.starts_with("font.") { r.set_style(&self.cfg.ui.font, self.cfg.ui.font_weight, &self.cfg.ui.font_features); }
        }
        if key.starts_with("font.") { self.send_size(); }
        self.redraw();
    }

    /// Previa de arquivo: 40 linhas em volta da linha pedida.
    pub(super) fn open_preview(&mut self, file: &str, line: i64, col: i64) {
        let text = std::fs::read(file).map(|b| String::from_utf8_lossy(&b[..b.len().min(4 * 1024 * 1024)]).to_string()).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        let first = (line - 12).max(1);
        let lines: Vec<String> = all.iter().skip((first - 1) as usize).take(40).map(|l| l.replace('\t', "    ")).collect();
        self.overlay = Some(Overlay { kind: OverlayKind::Preview { file: file.to_string(), line, col, first, lines }, ..Overlay::list(file, vec![]) });
    }

    // -------------------------------------------------------------- busca

    pub(super) fn search_run(&mut self, q: String) {
        let pane = self.frame.active_pane;
        let same = self.search.as_ref().is_some_and(|s| s.query == q && s.pane == pane) && self.search_hits.as_ref().is_some_and(|h| h.0 == pane);
        if same { self.search_step(-1); } else {
            self.search = Some(Search { query: q.clone(), pane });
            match self.call(json!({"cmd": "search-all", "pane": pane, "query": q})) {
                Ok(v) => {
                    let lines: Vec<i64> = v["lines"].as_array().into_iter().flatten().filter_map(|l| l.as_i64()).collect();
                    if lines.is_empty() {
                        self.search_hits = None;
                        if !q.is_empty() { self.flash(&format!("Nada encontrado: {q}")); }
                    } else {
                        let cur = lines.len() - 1;
                        self.search_hits = Some((pane, lines, cur, v["history"].as_i64().unwrap_or(0), v["total"].as_i64().unwrap_or(1)));
                        self.search_step(0);
                    }
                }
                Err(e) => self.flash(&e),
            }
        }
        let count = self.search_hits.as_ref().map(|h| format!(" ({}/{})", h.2 + 1, h.1.len())).unwrap_or_default();
        self.overlay = Some(Overlay::prompt(&format!("Buscar{count}: Enter = anterior (acima) · Shift+Enter = proxima · /regex/ · Esc sai"), &q, "search_run", vec![]));
    }

    /// Anda na lista de ocorrencias: -1 sobe (mais antiga), +1 desce.
    pub(super) fn search_step(&mut self, d: i64) {
        let Some((pane, lines, cur, _, _)) = self.search_hits.as_mut() else { return };
        let n = lines.len() as i64;
        *cur = (*cur as i64 + d).rem_euclid(n) as usize;
        let (pane, line) = (*pane, lines[*cur]);
        self.cmd(json!({"cmd": "goto-line", "pane": pane, "line": line}));
        if d != 0 {
            if let (Some(o), Some(h)) = (self.overlay.as_mut(), self.search_hits.as_ref()) {
                o.title = format!("Buscar ({}/{}): Enter = anterior (acima) · Shift+Enter = proxima · /regex/ · Esc sai", h.2 + 1, h.1.len());
            }
        }
    }

    // -------------------------------------------------------------- digitacao

    /// O que esta digitado agora no prompt do pane ativo (shell integration).
    fn typed_now(&self) -> Option<String> {
        let p = self.active()?;
        let (row, col) = p.input?;
        let (cc, cr, _) = p.cursor.clone()?;
        if p.alt_screen || row < 0 || cr as i64 != row { return None; }
        let line = p.row_chars(row as usize);
        let end = line.iter().rposition(|c| *c != ' ').map(|e| e + 1).unwrap_or(col).max(cc as usize).min(line.len());
        Some(line.get(col.min(end)..end).unwrap_or(&[]).iter().collect::<String>().trim_end().to_string())
    }

    pub(super) fn composer_key(&mut self, ev: &KeyEvent) {
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => { self.composer = None; return self.redraw(); }
            Key::Named(NamedKey::Enter) if !shift => {
                let Some(c) = self.composer.take() else { return };
                let text: String = c.text.iter().collect();
                if !text.trim().is_empty() {
                    self.cmd(json!({"cmd": "paste", "pane": c.pane, "text": text}));
                    self.cmd(json!({"cmd": "send", "pane": c.pane, "text": "\r"}));
                }
                return self.redraw();
            }
            _ => {}
        }
        let clip = arboard::Clipboard::new().ok();
        let Some(c) = self.composer.as_mut() else { return };
        let snap = |c: &mut Composer| { c.undo.push((c.text.clone(), c.cur)); if c.undo.len() > 200 { c.undo.remove(0); } c.redo.clear(); };
        let sel = |c: &Composer| c.anchor.filter(|a| *a != c.cur).map(|a| (a.min(c.cur), a.max(c.cur)));
        let del_sel = |c: &mut Composer| -> bool { if let Some((a, b)) = sel(c) { c.text.drain(a..b); c.cur = a; c.anchor = None; true } else { false } };
        let line_start = |c: &Composer, i: usize| c.text[..i].iter().rposition(|x| *x == '\n').map(|p| p + 1).unwrap_or(0);
        let line_end = |c: &Composer, i: usize| c.text[i..].iter().position(|x| *x == '\n').map(|p| p + i).unwrap_or(c.text.len());
        let mv = |c: &mut Composer, to: usize, shift: bool| {
            if shift { if c.anchor.is_none() { c.anchor = Some(c.cur); } } else { c.anchor = None; }
            c.cur = to.min(c.text.len());
        };
        match &ev.logical_key {
            Key::Named(NamedKey::Enter) => { snap(c); del_sel(c); c.text.insert(c.cur, '\n'); c.cur += 1; }
            Key::Named(NamedKey::Backspace) => {
                snap(c);
                if !del_sel(c) && c.cur > 0 {
                    let mut a = c.cur - 1;
                    if ctrl { while a > 0 && c.text[a - 1].is_alphanumeric() { a -= 1; } }
                    c.text.drain(a..c.cur);
                    c.cur = a;
                }
            }
            Key::Named(NamedKey::Delete) => { snap(c); if !del_sel(c) && c.cur < c.text.len() { c.text.remove(c.cur); } }
            Key::Named(NamedKey::ArrowLeft) => {
                let mut to = c.cur.saturating_sub(1);
                if ctrl { while to > 0 && c.text[to - 1].is_alphanumeric() { to -= 1; } }
                mv(c, to, shift);
            }
            Key::Named(NamedKey::ArrowRight) => {
                let mut to = (c.cur + 1).min(c.text.len());
                if ctrl { while to < c.text.len() && c.text[to].is_alphanumeric() { to += 1; } }
                mv(c, to, shift);
            }
            Key::Named(NamedKey::Home) => { let to = line_start(c, c.cur); mv(c, to, shift); }
            Key::Named(NamedKey::End) => { let to = line_end(c, c.cur); mv(c, to, shift); }
            Key::Named(NamedKey::ArrowUp) => {
                let s = line_start(c, c.cur);
                if s > 0 { let col = c.cur - s; let ps = line_start(c, s - 1); let to = (ps + col).min(s - 1); mv(c, to, shift); }
            }
            Key::Named(NamedKey::ArrowDown) => {
                let s = line_start(c, c.cur);
                let e = line_end(c, c.cur);
                if e < c.text.len() { let col = c.cur - s; let ne = line_end(c, e + 1); let to = (e + 1 + col).min(ne); mv(c, to, shift); }
            }
            Key::Character(k) if ctrl => match k.to_lowercase().as_str() {
                "a" => { c.anchor = Some(0); c.cur = c.text.len(); }
                "z" => if let Some((t, cur)) = c.undo.pop() { c.redo.push((c.text.clone(), c.cur)); c.text = t; c.cur = cur; c.anchor = None; },
                "y" => if let Some((t, cur)) = c.redo.pop() { c.undo.push((c.text.clone(), c.cur)); c.text = t; c.cur = cur; c.anchor = None; },
                "c" | "x" => if let Some((a, b)) = sel(c) {
                    let t: String = c.text[a..b].iter().collect();
                    if let Some(mut cb) = clip { let _ = cb.set_text(t); }
                    if k.eq_ignore_ascii_case("x") { snap(c); del_sel(c); }
                },
                "v" => if let Some(t) = clip.and_then(|mut cb| cb.get_text().ok()) {
                    snap(c);
                    del_sel(c);
                    for ch in t.replace("\r\n", "\n").chars() { c.text.insert(c.cur, ch); c.cur += 1; }
                },
                _ => {}
            },
            _ => if let Some(t) = &ev.text {
                if !t.is_empty() && !t.chars().any(|x| x.is_control()) {
                    snap(c);
                    del_sel(c);
                    for ch in t.chars() { c.text.insert(c.cur, ch); c.cur += 1; }
                }
            },
        }
        self.redraw();
    }

    /// Alt+setas/Alt+Enter no popup de autocompletar.
    pub(super) fn ac_key(&mut self, name: &str) -> bool {
        let Some(ac) = self.ac.as_mut() else { return false };
        let n = ac.items.len();
        match name {
            "arrowdown" => ac.sel = Some(ac.sel.map(|s| (s + 1) % n).unwrap_or(0)),
            "arrowup" => ac.sel = Some(ac.sel.map(|s| (s + n - 1) % n).unwrap_or(n - 1)),
            "enter" => { let i = ac.sel.unwrap_or(0); self.ac_accept(i); }
            _ => return false,
        }
        self.redraw();
        true
    }

    fn ac_accept(&mut self, i: usize) {
        let Some(ac) = self.ac.take() else { return };
        let Some(it) = ac.items.get(i) else { return };
        let text = format!("{}{}", "\x7f".repeat(it.delete), it.insert);
        self.ac_dismissed = format!("{}{}", ac.typed.chars().take(ac.typed.chars().count().saturating_sub(it.delete)).collect::<String>(), it.insert);
        self.send_keys(&text);
    }

    /// Sugestoes do popup: historico, caminhos, git e npm.
    pub(super) fn compute_ac(&mut self) {
        self.ac = None;
        if self.cfg.ui.autocomplete == "off" || self.composer.is_some() || self.overlay.is_some() { return; }
        let Some(p) = self.active() else { return };
        let (Some((row, col)), Some((cc, cr, _))) = (p.input, p.cursor.clone()) else { return };
        if p.alt_screen || row < 0 || cr as i64 != row { return; }
        let line = p.row_chars(row as usize);
        let typed: String = line.get(col..(cc as usize).min(line.len())).unwrap_or(&[]).iter().collect();
        if line.get(cc as usize..).is_some_and(|rest| rest.iter().any(|c| *c != ' ')) { return; }
        if typed.trim().chars().count() < 2 || typed == self.ac_dismissed { return; }
        let (cwd, domain) = (p.cwd.clone(), p.domain.clone());
        let wsl = self.is_wsl(&domain) || cwd.starts_with('/');
        let mut items: Vec<AcItem> = Vec::new();
        // Historico: comandos que comecam igual (desta pasta primeiro).
        if self.hist_cache.0.elapsed() > Duration::from_secs(5) {
            if let Ok(v) = self.call(json!({"cmd": "history", "limit": 3000})) {
                let list = v["history"].as_array().into_iter().flatten()
                    .filter_map(|h| Some((h["cmd"].as_str()?.to_string(), h["cwd"].as_str().unwrap_or("").to_string()))).collect();
                self.hist_cache = (Instant::now(), list);
            }
        }
        let mut seen = HashSet::new();
        let mut hist: Vec<&(String, String)> = self.hist_cache.1.iter().rev().filter(|(c, _)| c.starts_with(&typed) && c.len() > typed.len()).collect();
        hist.sort_by_key(|(_, d)| *d != cwd);
        for (c, _) in hist {
            if !seen.insert(c.clone()) { continue; }
            items.push(AcItem { label: c.clone(), insert: c[typed.len()..].to_string(), delete: 0, kind: "historico" });
            if items.len() >= 3 { break; }
        }
        let words: Vec<&str> = typed.split_whitespace().collect();
        let ends_space = typed.ends_with(' ');
        let cur = if ends_space { "" } else { words.last().copied().unwrap_or("") };
        let mut cands: Vec<(String, &'static str)> = Vec::new();
        match words.first().copied() {
            Some("git") => {
                let sub = words.get(1).copied().unwrap_or("");
                if words.len() == 1 && ends_space || words.len() == 2 && !ends_space {
                    for s in ["status", "add", "commit", "push", "pull", "checkout", "switch", "branch", "log", "diff", "stash", "rebase", "merge", "fetch", "restore", "reset", "tag", "clone", "worktree", "cherry-pick", "show"] {
                        cands.push((s.into(), "git"));
                    }
                } else if cur.starts_with('-') {
                    let flags: &[&str] = match sub {
                        "commit" => &["--amend", "--no-edit", "-m", "--all", "--no-verify", "--fixup"],
                        "push" => &["--force-with-lease", "-u", "--tags", "--set-upstream", "--dry-run"],
                        "pull" => &["--rebase", "--ff-only", "--no-rebase"],
                        "log" => &["--oneline", "--graph", "--all", "--stat", "-n", "--decorate", "--author"],
                        "diff" => &["--staged", "--stat", "--name-only", "--word-diff"],
                        "checkout" | "switch" => &["-b", "-c", "--track", "--detach"],
                        "stash" => &["--include-untracked", "-m", "--keep-index"],
                        "reset" => &["--soft", "--hard", "--mixed"],
                        "rebase" => &["--continue", "--abort", "--onto", "--autosquash"],
                        _ => &["--help"],
                    };
                    for f in flags { cands.push((f.to_string(), "git")); }
                } else if ["checkout", "switch", "merge", "rebase", "branch", "diff", "log"].contains(&sub) && !wsl {
                    let fresh = self.git_cache.get(&cwd).is_some_and(|(t, _)| t.elapsed() < Duration::from_secs(10));
                    if !fresh {
                        let b = git(&cwd, &["branch", "--all", "--format=%(refname:short)"]).unwrap_or_default().lines().map(String::from).collect();
                        self.git_cache.insert(cwd.clone(), (Instant::now(), b));
                    }
                    for b in &self.git_cache[&cwd].1 { cands.push((b.clone(), "git")); }
                }
            }
            Some("npm" | "pnpm" | "yarn" | "bun") if words.get(1) == Some(&"run") || (words.len() == 1 && ends_space) => {
                let base = if wsl { from_wsl(&cwd, &domain) } else { cwd.clone() };
                if let Ok(t) = std::fs::read_to_string(std::path::Path::new(&base).join("package.json")) {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        for k in v["scripts"].as_object().into_iter().flatten().map(|x| x.0) { cands.push((k.clone(), "npm")); }
                    }
                }
                if words.len() == 1 { cands.push(("run".into(), "npm")); cands.push(("install".into(), "npm")); }
            }
            _ => {}
        }
        // Caminhos: o ultimo pedaco digitado (a partir do 2o) vira lista da pasta.
        if words.len() >= 2 || ends_space || cur.contains(['/', '\\']) {
            let sep = if wsl || cur.contains('/') { '/' } else { '\\' };
            let (dir_part, prefix) = match cur.rfind(['/', '\\']) { Some(i) => (&cur[..=i], &cur[i + 1..]), None => ("", cur) };
            let base_cwd = if cwd.starts_with('/') { from_wsl(&cwd, &domain) } else { cwd.clone() };
            let dir = if dir_part.is_empty() { std::path::PathBuf::from(&base_cwd) }
                else if dir_part.starts_with('/') { std::path::PathBuf::from(from_wsl(dir_part, &domain)) }
                else if dir_part.len() > 1 && dir_part.as_bytes()[1] == b':' { std::path::PathBuf::from(dir_part) }
                else { std::path::Path::new(&base_cwd).join(dir_part) };
            let mut entries: Vec<(String, bool)> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
                .map(|e| (e.file_name().to_string_lossy().to_string(), e.path().is_dir()))
                .filter(|(n, _)| n.to_lowercase().starts_with(&prefix.to_lowercase()) && (!n.starts_with('.') || prefix.starts_with('.')))
                .collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (n, is_dir) in entries.into_iter().take(6) {
                let full = format!("{dir_part}{n}{}", if is_dir { sep.to_string() } else { String::new() });
                cands.push((full, if is_dir { "pasta" } else { "arquivo" }));
            }
        }
        for (cand, kind) in cands {
            if cand.to_lowercase().starts_with(&cur.to_lowercase()) && cand != cur && seen.insert(cand.clone()) {
                items.push(AcItem { label: cand.clone(), insert: cand, delete: cur.chars().count(), kind });
            }
            if items.len() >= 8 { break; }
        }
        if !items.is_empty() { self.ac = Some(Ac { typed, items, sel: None }); }
    }

    /// Realce do comando digitado: (pane, linha, [(coluna, texto, cor, sublinhado)]).
    pub(super) fn syntax_spans(&mut self) -> Vec<(u64, i32, Vec<(usize, String, Rgb, bool)>)> {
        let mode = self.cfg.ui.syntax.clone();
        if mode == "off" || self.composer.is_some() { return vec![]; }
        let Some(p) = self.active() else { return vec![] };
        let pwsh_pane = p.domain.is_empty() && self.cfg.shell.first().is_some_and(|s| s.to_lowercase().contains("pwsh") || s.to_lowercase().contains("powershell"));
        if mode == "auto" && pwsh_pane { return vec![]; }
        let (Some((row, col)), Some((cc, cr, _))) = (p.input, p.cursor.clone()) else { return vec![] };
        if p.alt_screen || row < 0 || cr as i64 != row { return vec![]; }
        let line = p.row_chars(row as usize);
        let end = line.iter().rposition(|c| *c != ' ').map(|e| e + 1).unwrap_or(col).max(cc as usize).min(line.len());
        if end <= col { return vec![]; }
        let typed: Vec<char> = line[col..end].to_vec();
        let (id, cwd, domain) = (p.id, p.cwd.clone(), p.domain.clone());
        let wsl = self.is_wsl(&domain) || cwd.starts_with('/');
        let theme = self.theme().clone();
        let mut spans = Vec::new();
        let mut i = 0;
        let mut expect_cmd = true;
        while i < typed.len() {
            let c = typed[i];
            if c == ' ' { i += 1; continue; }
            let start = i;
            let (text, color, underline) = if c == '"' || c == '\'' {
                i += 1;
                while i < typed.len() && typed[i] != c { i += 1; }
                i = (i + 1).min(typed.len());
                (typed[start..i].iter().collect::<String>(), theme.palette[3], false)
            } else if "|;&<>".contains(c) {
                while i < typed.len() && "|;&<>".contains(typed[i]) { i += 1; }
                expect_cmd = true;
                (typed[start..i].iter().collect::<String>(), theme.palette[5], false)
            } else {
                while i < typed.len() && typed[i] != ' ' && !"|;<>".contains(typed[i]) { i += 1; }
                let w: String = typed[start..i].iter().collect();
                if expect_cmd {
                    expect_cmd = false;
                    let ok = self.is_exe(&w, wsl, &cwd);
                    (w, if ok { theme.done } else { theme.blocked }, false)
                } else if w.starts_with('-') {
                    (w, theme.working, false)
                } else if w.starts_with('$') {
                    (w, theme.palette[5], false)
                } else {
                    let path = if wsl { None } else { Some(std::path::Path::new(&cwd).join(&w)) };
                    let exists = !w.is_empty() && path.is_some_and(|p| p.exists());
                    (w, if exists { theme.accent } else { theme.fg }, exists)
                }
            };
            spans.push((col + start, text, color, underline));
        }
        vec![(id, row as i32, spans)]
    }

    fn is_exe(&mut self, name: &str, wsl: bool, cwd: &str) -> bool {
        if wsl || name.is_empty() { return true; }
        const BUILTIN: [&str; 40] = ["cd", "ls", "dir", "echo", "cls", "clear", "exit", "set", "if", "for", "where", "type", "copy", "move", "del", "rm", "mkdir", "rmdir",
            "pushd", "popd", "start", "cat", "pwd", "mv", "cp", "ni", "sl", "gci", "gc", "iwr", "irm", "iex", "foreach", "while", "function", "return", "param", "ren", "md", "rd"];
        let lower = name.to_lowercase();
        if BUILTIN.contains(&lower.as_str()) || name.contains('-') && name.chars().next().is_some_and(|c| c.is_uppercase()) { return true; }
        if name.contains(['/', '\\']) { return std::path::Path::new(cwd).join(name).exists() || std::path::Path::new(name).exists(); }
        if let Some(v) = self.exe_cache.get(&lower) { return *v; }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = std::env::split_paths(&path).any(|d| ["", ".exe", ".cmd", ".bat", ".ps1", ".com"].iter().any(|e| d.join(format!("{name}{e}")).is_file()));
        if self.exe_cache.len() > 500 { self.exe_cache.clear(); }
        self.exe_cache.insert(lower, found);
        found
    }
}

/// Contexto comum para desenhar abas.
struct TabCtx<'a> { theme: &'a Theme, mouse: (f32, f32), t: f32, drag: Option<(usize, f32, bool)> }

fn state_glyph(theme: &Theme, t: &TabInfo, now: f32) -> (&'static str, Rgb, f32) {
    match t.state.as_str() {
        "working" => ("●", theme.accent, 0.35 + 0.65 * (now * 3.2).sin().abs()),
        "blocked" => ("●", theme.working, 1.0),
        "done" => ("✓", theme.done, 1.0),
        _ if t.attention => ("●", theme.blocked, 1.0),
        "idle" => ("●", theme.idle, 0.5),
        _ => ("", theme.muted, 1.0),
    }
}

fn state_label(s: &str) -> &'static str {
    match s { "working" => "trabalhando", "blocked" => "pede aprovacao", "done" => "terminou", "idle" => "parado", _ => "" }
}

/// Progresso da aba: OSC 9;4 do pane ou o plano do agente. (estado, fracao)
fn tab_progress(t: &TabInfo) -> Option<(u8, f32)> {
    t.progress.map(|(s, v)| (s, v as f32 / 100.0)).or_else(|| t.todo.filter(|(_, n)| *n > 0).map(|(d, n)| (1, d as f32 / n as f32)))
}

/// Linha de abas como pilulas; devolve (x final, tem animacao).
#[allow(clippy::too_many_arguments)]
fn tabs_row(r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, rects: &mut Vec<(f32, f32, usize)>, x0: f32, y: f32, x_max: f32, bar_h: f32,
    labels: &[(String, TabInfo, Option<Rgb>)], c: &TabCtx) -> (f32, bool) {
    let theme = c.theme;
    let ch = r.cell_h;
    let cw = r.cell_w;
    let mut x = x0;
    let mut pulse = false;
    let pill_h = bar_h - 10.0;
    let ty = y + 5.0;
    let size = 0.9;
    let n = labels.len().max(1) as f32;
    // Muitas abas: cada uma encolhe ate caber.
    let max_w = ((x_max - x0 - 34.0) / n - 4.0).clamp(84.0, 240.0);
    for (label, t, color) in labels {
        if x > x_max - 40.0 { break; }
        let ind = !state_glyph(theme, t, c.t).0.is_empty();
        let (name, sub) = split_subtitle(label);
        let sub_gap = if sub.is_empty() { 0.0 } else { 6.0 };
        let name_w = r.measure(name, true, size);
        let text_w = name_w + sub_gap + if sub.is_empty() { 0.0 } else { r.measure(sub, true, size * 0.9) };
        // Espaco fixo do ✕: nunca por cima do titulo ou do icone de estado.
        let close_w = 26.0;
        let tw = (14.0 + text_w + if ind { 16.0 } else { 0.0 } + close_w).clamp(72.0, max_w);
        let rect = [x, ty, tw, pill_h];
        let hover = inside(rect, c.mouse) && c.drag.is_none();
        let dragging = c.drag.is_some_and(|d| d.0 == t.index && d.2);
        let bg = match (t.active, hover, color) {
            (true, _, Some(cc)) => mix(theme.tab_active, *cc, 0.30),
            (true, _, None) => theme.tab_active,
            (false, true, _) => mix(theme.tab_bar, theme.fg, 0.07),
            (false, false, Some(cc)) => mix(theme.tab_bar, *cc, 0.14),
            _ => theme.tab_bar,
        };
        if t.active { l.shapes.push(Shape::shadow(x, ty + 1.5, tw, pill_h, [0.0, 0.0, 0.0, 0.30], 8.0, 4.0)); }
        if t.active || hover || color.is_some() || dragging { l.shapes.push(Shape::rect(x, ty, tw, pill_h, lin(if dragging { mix(bg, theme.accent, 0.25) } else { bg }, 1.0), 8.0)); }
        if t.attention && !t.active { l.shapes.push(Shape::ring(x, ty, tw, pill_h, lin(theme.blocked, 0.8), 8.0, 1.2)); }
        let cy = ty + pill_h / 2.0;
        let fg = if t.active { theme.fg } else if hover { mix(theme.fg, theme.muted, 0.3) } else { theme.muted };
        let text_right = x + tw - close_w - if ind { 16.0 } else { 0.0 };
        l.texts.push(TextItem { clip_right: Some(text_right), ..uit(x + 12.0, cy - ch * size / 2.0, name, fg, size) });
        if !sub.is_empty() && x + 12.0 + name_w + sub_gap < text_right {
            let ss = size * 0.9;
            l.texts.push(TextItem { italic: true, clip_right: Some(text_right), ..uit(x + 12.0 + name_w + sub_gap, cy - ch * ss / 2.0, sub, mix(fg, theme.muted, 0.6), ss) });
        }
        if ind {
            let (g, gc, a) = state_glyph(theme, t, c.t);
            if t.state == "working" { pulse = true; }
            l.texts.push(TextItem { alpha: a, ..uit(text_right + 2.0, cy - ch * 0.8 / 2.0, g, gc, 0.8) });
        }
        if let Some((s, f)) = tab_progress(t) {
            let (px, pw) = (x + 10.0, tw - 20.0);
            let py = ty + pill_h - 4.0;
            l.shapes.push(Shape::rect(px, py, pw, 2.5, lin(theme.muted, 0.25), 1.25));
            if s == 3 {
                pulse = true;
                let seg = pw * 0.3;
                let off = (c.t * 0.8).fract() * (pw + seg) - seg;
                let (a0, a1) = (off.max(0.0), (off + seg).min(pw));
                if a1 > a0 { l.shapes.push(Shape::rect(px + a0, py, a1 - a0, 2.5, lin(theme.accent, 1.0), 1.25)); }
            } else {
                let col = match s { 2 => theme.blocked, 4 => theme.working, _ => theme.done };
                l.shapes.push(Shape::rect(px, py, pw * f.clamp(0.0, 1.0), 2.5, lin(col, 1.0), 1.25));
            }
        } else if let Some(cc) = color {
            l.shapes.push(Shape::rect(x + 12.0, ty + pill_h - 3.0, tw - 24.0, 2.0, lin(*cc, 0.9), 1.0));
        }
        if hover || t.active {
            let cr = [x + tw - 24.0, cy - 9.0, 18.0, 18.0];
            let hv = inside(cr, c.mouse);
            if hv { l.shapes.push(Shape::rect(cr[0], cr[1], cr[2], cr[3], lin(theme.fg, 0.12), 4.0)); }
            l.texts.push(uit(cr[0] + 4.0, cy - ch * 0.72 / 2.0, "✕", if hv { theme.fg } else { theme.muted }, 0.72));
            hits.push(Hit { r: cr, action: Action::Builtin("close_tab_idx".into(), vec![json!(t.index)]) });
        }
        rects.push((x, x + tw, t.index));
        x += tw + 4.0;
    }
    // Nova aba
    let pr = [x + 2.0, ty + (pill_h - 24.0) / 2.0, 24.0, 24.0];
    let hv = inside(pr, c.mouse);
    if hv { l.shapes.push(Shape::rect(pr[0], pr[1], pr[2], pr[3], lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), 6.0)); }
    l.texts.push(uit(pr[0] + 7.0, ty + (pill_h - ch) / 2.0, "+", if hv { theme.fg } else { theme.muted }, 1.0));
    hits.push(Hit { r: pr, action: Action::Builtin("new_tab".into(), vec![]) });
    let _ = cw;
    (x + 30.0, pulse)
}

/// Paleta, listas, prompts, dialogos com botoes, previa de arquivo e slider.
#[allow(clippy::too_many_arguments)]
fn draw_overlay(r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, o: &Overlay, theme: &Theme, mouse: (f32, f32), w: f32, h: f32, top: f32) {
    let (cw, ch) = (r.cell_w, r.cell_h);
    l.shapes.push(Shape::rect(0.0, 0.0, w, h, [0.0, 0.0, 0.0, 0.40], 0.0));
    let card = |l: &mut Layer, x: f32, y: f32, ww: f32, hh: f32| {
        l.shapes.push(Shape::shadow(x, y + 6.0, ww, hh, [0.0, 0.0, 0.0, 0.55], 14.0, 18.0));
        l.shapes.push(Shape::rect(x, y, ww, hh, lin(theme.tab_bar, 1.0), 14.0));
        l.shapes.push(Shape::ring(x, y, ww, hh, lin(mix(theme.tab_bar, theme.fg, 0.12), 1.0), 14.0, 1.0));
    };
    let wrap = |r: &mut Renderer, text: &str, max: f32, size: f32, lines: usize| -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        for word in text.split(' ') {
            let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if r.measure(&cand, true, size) > max && !cur.is_empty() { out.push(std::mem::replace(&mut cur, word.to_string())); } else { cur = cand; }
            if out.len() >= lines { break; }
        }
        if out.len() < lines && !cur.is_empty() { out.push(cur); }
        out
    };
    match &o.kind {
        OverlayKind::Preview { file, line, first, lines, .. } => {
            // So as linhas que cabem, com a pedida perto do meio.
            let fit = (((h - 40.0) / ch) as usize).saturating_sub(4).max(3);
            let skip = if lines.len() > fit { ((*line - *first) as usize).saturating_sub(fit / 2).min(lines.len() - fit) } else { 0 };
            let lines: Vec<&String> = lines.iter().skip(skip).take(fit).collect();
            let first = &(*first + skip as i64);
            let ow = (w - 60.0).min(cw * 110.0);
            let oh = (lines.len() as f32 + 3.0) * ch + 24.0;
            let (x, y) = ((w - ow) / 2.0, (h - oh) / 2.0);
            card(l, x, y, ow, oh);
            l.texts.push(TextItem { bold: true, clip_right: Some(x + ow - 170.0), ..uit(x + 16.0, y + 12.0, &format!("{file}:{line}"), theme.fg, 0.9) });
            let br = [x + ow - 158.0, y + 8.0, 144.0, ch + 8.0];
            let hv = inside(br, mouse);
            l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(theme.accent, if hv { 1.0 } else { 0.85 }), 7.0));
            l.texts.push(uit(br[0] + 12.0, br[1] + 4.0, "Abrir no editor ⏎", theme.bg, 0.85));
            hits.push(Hit { r: br, action: Action::Builtin("preview_open".into(), vec![]) });
            let gutter = cw * 6.0;
            for (k, text) in lines.iter().enumerate() {
                let n = first + k as i64;
                let ly = y + 20.0 + ch * (k as f32 + 1.0);
                if n == *line { l.shapes.push(Shape::rect(x + 8.0, ly, ow - 16.0, ch, lin(theme.accent, 0.20), 4.0)); }
                l.texts.push(mt(x + 14.0, ly, &format!("{n:>5}"), if n == *line { theme.accent } else { theme.muted }));
                l.texts.push(TextItem { clip_right: Some(x + ow - 12.0), ..mt(x + 14.0 + gutter, ly, text, theme.fg) });
            }
            l.texts.push(uit(x + 16.0, y + oh - ch - 6.0, "Enter abre no editor · Esc fecha · Shift+Ctrl+clique abre direto", theme.muted, 0.78));
        }
        OverlayKind::Slider { label, val, min, max, .. } => {
            let ow = 460.0_f32.min(w - 40.0);
            let oh = 130.0;
            let (x, y) = ((w - ow) / 2.0, (h * 0.3).max(top + 20.0));
            card(l, x, y, ow, oh);
            l.texts.push(TextItem { bold: true, ..uit(x + 20.0, y + 16.0, label, theme.fg, 1.0) });
            let vs = format!("{val:.2}");
            let vw = r.measure(&vs, true, 1.0);
            l.texts.push(uit(x + ow - 20.0 - vw, y + 16.0, &vs, theme.accent, 1.0));
            let (tx, tw, ty) = (x + 20.0, ow - 40.0, y + 66.0);
            let f = ((val - min) / (max - min)).clamp(0.0, 1.0);
            l.shapes.push(Shape::rect(tx, ty, tw, 6.0, lin(theme.muted, 0.3), 3.0));
            l.shapes.push(Shape::rect(tx, ty, tw * f, 6.0, lin(theme.accent, 1.0), 3.0));
            l.shapes.push(Shape::shadow(tx + tw * f - 9.0, ty - 6.0 + 2.0, 18.0, 18.0, [0.0, 0.0, 0.0, 0.4], 9.0, 4.0));
            l.shapes.push(Shape::rect(tx + tw * f - 9.0, ty - 6.0, 18.0, 18.0, lin(theme.fg, 1.0), 9.0));
            hits.push(Hit { r: [tx - 6.0, ty - 12.0, tw + 12.0, 30.0], action: Action::Builtin("slider_set".into(), vec![json!(tx), json!(tw)]) });
            l.texts.push(uit(x + 20.0, y + oh - ch - 10.0, "←/→ ajusta ao vivo · Enter grava · Esc desfaz", theme.muted, 0.8));
        }
        OverlayKind::Detail { subtitle, lines, actions, scroll, .. } => {
            let ow = (w - 60.0).min(cw * 110.0);
            let oh = (h * 0.82).min(h - top - 30.0);
            let (x, y) = ((w - ow) / 2.0, top + ((h - top) - oh) / 2.0);
            card(l, x, y, ow, oh);
            l.texts.push(TextItem { bold: true, clip_right: Some(x + ow - 20.0), ..uit(x + 20.0, y + 16.0, &o.title, theme.fg, 1.05) });
            l.texts.push(TextItem { clip_right: Some(x + ow - 20.0), ..uit(x + 20.0, y + 16.0 + ch * 1.35, subtitle, theme.muted, 0.82) });
            let body_top = y + 20.0 + ch * 2.7;
            let body_bottom = y + oh - 60.0;
            l.quads.push(Quad { x: x + 16.0, y: body_top - 6.0, w: ow - 32.0, h: 1.0, color: lin(theme.border, 0.8) });
            // Texto monoespacado quebrado pela largura (cabe como num terminal).
            let per = (((ow - 48.0) / (cw * 0.9)) as usize).max(20);
            let mut wrapped: Vec<String> = Vec::new();
            for line in lines.iter() {
                let chars: Vec<char> = line.chars().collect();
                if chars.is_empty() { wrapped.push(String::new()); continue; }
                for c in chars.chunks(per) { wrapped.push(c.iter().collect()); }
            }
            let rows = ((body_bottom - body_top) / (ch * 0.9 * 1.05)).max(1.0) as usize;
            for (k, t) in wrapped.iter().skip(*scroll).take(rows).enumerate() {
                let c = if t.starts_with('#') { theme.accent } else if t.starts_with('>') { theme.muted } else if t.starts_with("──") || t.starts_with("--") { theme.border } else { theme.fg };
                l.texts.push(TextItem { size: 0.9, ..mt(x + 24.0, body_top + k as f32 * ch * 0.9 * 1.05, t, c) });
            }
            if wrapped.len() > rows {
                let th = (body_bottom - body_top) * rows as f32 / wrapped.len() as f32;
                let ty = body_top + (body_bottom - body_top - th) * (*scroll as f32 / (wrapped.len() - rows).max(1) as f32).min(1.0);
                l.shapes.push(Shape::rect(x + ow - 10.0, ty, 3.0, th, lin(theme.muted, 0.5), 1.5));
            }
            let mut bx = x + ow - 16.0;
            for (k, (_, title, _)) in actions.iter().enumerate().rev() {
                let bw = r.measure(title, true, 0.88) + 26.0;
                bx -= bw;
                let br = [bx, y + oh - 46.0, bw, 32.0];
                let hv = inside(br, mouse);
                let primary = k == 0;
                let bg = if primary { theme.accent } else { mix(theme.tab_bar, theme.fg, if hv { 0.16 } else { 0.10 }) };
                l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(if hv && primary { mix(bg, theme.fg, 0.15) } else { bg }, 1.0), 8.0));
                l.texts.push(uit(bx + 13.0, br[1] + (32.0 - ch * 0.88) / 2.0, title, if primary { theme.bg } else { theme.fg }, 0.88));
                hits.push(Hit { r: br, action: Action::Builtin("detail_action".into(), vec![json!(k)]) });
                bx -= 8.0;
            }
            l.texts.push(uit(x + 20.0, y + oh - 40.0, "↑↓/roda rolam · Esc fecha", theme.muted, 0.75));
        }
        _ if o.dialog => {
            let ow = (w * 0.5).clamp(380.0_f32.min(w - 20.0), 580.0);
            let lines = wrap(r, &o.title, ow - 40.0, 0.95, 6);
            let oh = lines.len() as f32 * ch * 1.15 + 92.0;
            let (x, y) = ((w - ow) / 2.0, (h * 0.28).max(top + 20.0));
            card(l, x, y, ow, oh);
            for (k, t) in lines.iter().enumerate() { l.texts.push(uit(x + 20.0, y + 18.0 + k as f32 * ch * 1.15, t, theme.fg, 0.95)); }
            let mut bx = x + ow - 16.0;
            let by = y + oh - 50.0;
            for (k, &i) in o.shown.iter().enumerate().rev() {
                let it = &o.items[i];
                let bw = r.measure(&it.label, true, 0.9) + 28.0;
                bx -= bw;
                let br = [bx, by, bw, 34.0];
                let sel = k == o.sel;
                let hv = inside(br, mouse);
                let bg = if sel { theme.accent } else { mix(theme.tab_bar, theme.fg, if hv { 0.16 } else { 0.10 }) };
                l.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(bg, 1.0), 8.0));
                l.texts.push(uit(bx + 14.0, by + (34.0 - ch * 0.9) / 2.0, &it.label, if sel { theme.bg } else { theme.fg }, 0.9));
                hits.push(Hit { r: br, action: Action::Builtin("overlay_pick".into(), vec![json!(k)]) });
                bx -= 8.0;
            }
            l.texts.push(uit(x + 20.0, by + 10.0, "←/→ escolhe · Enter confirma · Esc", theme.muted, 0.75));
        }
        _ => {
            let home = o.home;
            let ow = if home { (w - 80.0).min(900.0) } else { (w - 40.0).min(cw * 100.0).max(cw * 30.0).min(w - 20.0) };
            let row_h = (ch * 1.35).round();
            let x = (w - ow) / 2.0;
            let y = if home { (h * 0.08).max(top + 16.0) } else { (h * 0.12).max(top + 12.0) };
            let visible = if home { (((h - y - 150.0) / row_h) as usize).clamp(5, 24) } else { (((h * 0.7) / row_h) as usize).clamp(5, 14) };
            let is_list = matches!(o.kind, OverlayKind::List);
            let rows = if is_list { o.shown.len().min(visible) } else { 0 };
            let head = if home { 64.0 } else { 0.0 };
            let title_lines = wrap(r, &o.title, ow - 40.0, 0.85, 3);
            let th = title_lines.len().max(1) as f32 * ch * 0.95;
            let oh = head + th + 26.0 + row_h + 10.0 + rows as f32 * row_h + if is_list && o.shown.is_empty() { row_h } else { 0.0 } + 14.0;
            card(l, x, y, ow, oh);
            let mut cy = y + 14.0;
            if home {
                l.texts.push(TextItem { bold: true, ..uit(x + 22.0, cy, "Duck", theme.fg, 1.9) });
                cy += head;
            }
            for t in &title_lines { l.texts.push(uit(x + 20.0, cy, t, theme.muted, 0.85)); cy += ch * 0.95; }
            cy += 8.0;
            // Campo de busca/entrada
            let fr = [x + 14.0, cy, ow - 28.0, row_h + 4.0];
            l.shapes.push(Shape::rect(fr[0], fr[1], fr[2], fr[3], lin(mix(theme.tab_bar, theme.bg, 0.6), 1.0), 8.0));
            l.shapes.push(Shape::ring(fr[0], fr[1], fr[2], fr[3], lin(theme.accent, 0.6), 8.0, 1.0));
            let iy = cy + (row_h + 4.0 - ch) / 2.0;
            l.texts.push(TextItem { size: 0.9, ..mt(fr[0] + 10.0, iy, if is_list { "\u{f002}" } else { "\u{f054}" }, theme.muted) });
            if o.input.is_empty() && is_list {
                l.texts.push(uit(fr[0] + 32.0, iy, "Digite para filtrar…", theme.muted, 0.9));
            } else {
                let shown = if o.secret { "•".repeat(o.input.chars().count()) } else { o.input.clone() };
                l.texts.push(TextItem { clip_right: Some(fr[0] + fr[2] - 8.0), ..mt(fr[0] + 32.0, iy, &format!("{shown}▏"), theme.fg) });
            }
            cy += row_h + 14.0;
            if is_list {
                let start = o.sel.saturating_sub(visible - 1);
                let mut last_cat = String::new();
                for (k, &i) in o.shown.iter().skip(start).take(visible).enumerate() {
                    let it = &o.items[i];
                    let ry = cy + k as f32 * row_h;
                    let rr = [x + 10.0, ry, ow - 20.0, row_h];
                    let sel = start + k == o.sel;
                    let hv = inside(rr, mouse);
                    if sel || hv {
                        l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(theme.accent, if sel { 0.22 } else { 0.10 }), 7.0));
                        if sel { l.shapes.push(Shape::rect(rr[0] + 2.0, rr[1] + 6.0, 3.0, row_h - 12.0, lin(theme.accent, 1.0), 1.5)); }
                    }
                    let ty = ry + (row_h - ch * 0.95) / 2.0;
                    let mut lx = x + 22.0;
                    if home {
                        let cat = it.hint.split(" · ").next().unwrap_or("").to_string();
                        if cat != last_cat { l.texts.push(TextItem { bold: true, clip_right: Some(x + 180.0), ..uit(lx, ty, &cat, theme.accent, 0.8) }); last_cat = cat; }
                        lx = x + 190.0;
                    }
                    let mut right = x + ow - 22.0;
                    if !home && !it.hint.is_empty() {
                        if is_shortcut(&it.hint) { right -= keycaps(r, l, right, ry, row_h, &it.hint, theme) + 8.0; }
                        else {
                            let hw = r.measure(&it.hint, true, 0.78).min(ow * 0.35);
                            right -= hw;
                            l.texts.push(TextItem { clip_right: Some(x + ow - 18.0), ..uit(right, ry + (row_h - ch * 0.78) / 2.0, &it.hint, theme.muted, 0.78) });
                            right -= 10.0;
                        }
                    }
                    l.texts.push(TextItem { clip_right: Some(right), ..uit(lx, ty, &it.label, theme.fg, 0.95) });
                    hits.push(Hit { r: rr, action: Action::Builtin("overlay_pick".into(), vec![json!(start + k)]) });
                }
                if o.shown.is_empty() { l.texts.push(uit(x + 22.0, cy + (row_h - ch * 0.9) / 2.0, "Nada encontrado", theme.muted, 0.9)); }
                if o.shown.len() > visible {
                    let th = (rows as f32 * row_h) * visible as f32 / o.shown.len() as f32;
                    let ty = cy + (rows as f32 * row_h - th) * (start as f32 / (o.shown.len() - visible) as f32);
                    l.shapes.push(Shape::rect(x + ow - 8.0, ty, 3.0, th, lin(theme.muted, 0.5), 1.5));
                }
            }
            let _ = bit(true);
        }
    }
}

/// Transcript da sessao: <perfil>/projects/*/<sessao>.jsonl.
fn find_transcript(config_dir: &str, session: &str) -> Option<std::path::PathBuf> {
    let base = if config_dir.is_empty() { config::home().join(".claude") } else { std::path::PathBuf::from(config_dir) };
    std::fs::read_dir(base.join("projects")).ok()?.flatten().map(|d| d.path().join(format!("{session}.jsonl"))).find(|p| p.is_file())
}

// -------------------------------------------------------------- tema do papel de parede

fn hsl(c: Rgb) -> (f32, f32, f32) {
    let [r, g, b] = c.f();
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if mx == mn { return (0.0, 0.0, l); }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r { (g - b) / d + if g < b { 6.0 } else { 0.0 } } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    (h * 60.0, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 { 0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x), 3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x) };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(f(r), f(g), f(b))
}

/// Gera ~/.duck/themes/Papel de parede.conf com as cores da imagem de fundo do Windows.
fn wallpaper_theme() -> Result<String, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("sem APPDATA")?;
    let path = std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Themes\TranscodedWallpaper");
    let img = image::ImageReader::open(&path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?
        .decode().map_err(|e| e.to_string())?.thumbnail(64, 64).to_rgb8();
    // Cores dominantes por baldes de 4 bits.
    let mut counts: HashMap<(u8, u8, u8), u32> = HashMap::new();
    for p in img.pixels() { *counts.entry((p[0] >> 4, p[1] >> 4, p[2] >> 4)).or_insert(0) += 1; }
    let mut top: Vec<((u8, u8, u8), u32)> = counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1));
    let cols: Vec<Rgb> = top.iter().take(12).map(|((r, g, b), _)| Rgb(r * 16 + 8, g * 16 + 8, b * 16 + 8)).collect();
    let dominant = *cols.first().ok_or("imagem vazia")?;
    let accent = cols.iter().copied().max_by(|a, b| { let (sa, sb) = (hsl(*a).1, hsl(*b).1); sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal) }).unwrap_or(dominant);
    let (dh, ds, _) = hsl(dominant);
    let (ah, as_, _) = hsl(accent);
    let bg = from_hsl(dh, (ds * 0.5).min(0.35), 0.09);
    let fg = from_hsl(dh, 0.12, 0.88);
    let sat = as_.clamp(0.45, 0.75);
    // Paleta ANSI nas matizes padrao, puxadas para o tom do papel de parede.
    let hue = |base: f32| base + ((ah - base + 540.0).rem_euclid(360.0) - 180.0) * 0.15;
    let norm = [hue(0.0), hue(120.0), hue(50.0), hue(215.0), hue(300.0), hue(185.0)];
    let mut pal = vec![from_hsl(dh, 0.15, 0.22)];
    for h in norm { pal.push(from_hsl(h, sat, 0.62)); }
    pal.push(from_hsl(dh, 0.10, 0.75));
    pal.push(from_hsl(dh, 0.15, 0.38));
    for h in norm { pal.push(from_hsl(h, sat, 0.72)); }
    pal.push(fg);
    let hex = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2);
    let mut out = format!("background = {}\nforeground = {}\ncursor-color = {}\nselection-background = {}\n", hex(bg), hex(fg), hex(from_hsl(ah, sat, 0.65)), hex(from_hsl(ah, sat * 0.6, 0.30)));
    for (i, c) in pal.iter().enumerate() { out.push_str(&format!("palette = {i}={}\n", hex(*c))); }
    let name = "Papel de parede";
    let dir = config::home().join(".duck").join("themes");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{name}.conf")), out).map_err(|e| e.to_string())?;
    Ok(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_roundtrip() {
        for c in [Rgb(200, 30, 60), Rgb(10, 200, 90), Rgb(40, 40, 40), Rgb(250, 250, 10)] {
            let (h, s, l) = hsl(c);
            let b = from_hsl(h, s, l);
            assert!((c.0 as i32 - b.0 as i32).abs() <= 2 && (c.1 as i32 - b.1 as i32).abs() <= 2 && (c.2 as i32 - b.2 as i32).abs() <= 2, "{c:?} {b:?}");
        }
    }

    #[test]
    fn shortcut_detection() {
        assert!(is_shortcut("Ctrl+Shift+P"));
        assert!(!is_shortcut("historico"));
    }
}

/// Nome da extensao dona do badge ("duck:sys_panel" -> "system", "jira/issues" -> "jira").
fn badge_name(key: &str) -> &str {
    match key { "duck:sys_panel" => "system", "duck:agent_panel" => "agents", "duck:inbox" => "inbox", k => k.split('/').next().unwrap_or(k) }
}


/// Transcripts do Claude Code nos perfis (~/.claude*/projects) de uma home:
/// (mtime, perfil, sessao, caminho). Copias de seguranca ficam de fora.
fn scan_profiles(home: &std::path::Path) -> Vec<(u64, String, String, String)> {
    let mut found = Vec::new();
    for prof in std::fs::read_dir(home).into_iter().flatten().flatten() {
        let pname = prof.file_name().to_string_lossy().to_string();
        if !pname.starts_with(".claude") || pname.contains(".bak") || pname.contains("backup") || !prof.path().join("projects").is_dir() { continue; }
        for proj in std::fs::read_dir(prof.path().join("projects")).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(proj.path()).into_iter().flatten().flatten() {
                let p = f.path();
                if p.extension().is_none_or(|e| e != "jsonl") { continue; }
                let mtime = f.metadata().and_then(|m| m.modified()).ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
                found.push((mtime, pname.clone(), p.file_stem().unwrap().to_string_lossy().to_string(), p.to_string_lossy().to_string()));
            }
        }
    }
    found
}

/// Sessoes do Claude Code dentro das distros do WSL que sao dominios do Duck.
/// So as distros ja rodando (ler \\wsl.localhost acorda uma parada), e o
/// resultado vale 60 s (o acesso pelo 9P e lento).
#[allow(clippy::type_complexity)]
fn wsl_sessions(domains: &[config::Domain]) -> Vec<(u64, String, String, String, Option<(String, String)>)> {
    #[cfg(not(windows))]
    { let _ = domains; return Vec::new(); }
    #[cfg(windows)]
    {
        type Found = Vec<(u64, String, String, String, Option<(String, String)>)>;
        static CACHE: std::sync::Mutex<Option<(Instant, Found)>> = std::sync::Mutex::new(None);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, v)) = cache.as_ref() { if t.elapsed() < Duration::from_secs(60) { return v.clone(); } }
        let distros: Vec<(String, String)> = domains.iter().filter_map(|d| {
            if !d.cmd.first().is_some_and(|c| c.to_lowercase().contains("wsl")) { return None; }
            let i = d.cmd.iter().position(|a| a == "-d" || a == "--distribution")?;
            Some((d.name.clone(), d.cmd.get(i + 1)?.clone()))
        }).collect();
        let mut out: Found = Vec::new();
        if !distros.is_empty() {
            let mut c = std::process::Command::new("wsl.exe");
            c.args(["-l", "--running", "-q"]);
            { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
            let running: Vec<String> = c.output().ok().map(|o| {
                // wsl.exe escreve UTF-16LE.
                let u: Vec<u16> = o.stdout.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                String::from_utf16_lossy(&u).lines().map(|l| l.trim().trim_matches('\0').to_lowercase()).filter(|l| !l.is_empty()).collect()
            }).unwrap_or_default();
            for (domain, distro) in distros {
                if !running.contains(&distro.to_lowercase()) { continue; }
                let base = std::path::PathBuf::from(format!(r"\\wsl.localhost\{distro}\home"));
                for user in std::fs::read_dir(&base).into_iter().flatten().flatten() {
                    let uname = user.file_name().to_string_lossy().to_string();
                    for (mtime, prof, session, path) in scan_profiles(&user.path()) {
                        out.push((mtime, prof, session, path, Some((domain.clone(), format!("/home/{uname}")))));
                    }
                }
            }
        }
        *cache = Some((Instant::now(), out.clone()));
        out
    }
}
