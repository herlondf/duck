//! A janela. Nao e dona de shell nenhum: recebe frames prontos do server e
//! manda teclas/comandos. Fechar a janela so desanexa.
//!
//! `duck --quick` abre a mesma janela como "quick terminal": sem borda, no
//! topo da tela, escondida, alternada por uma tecla global.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde::Deserialize;
use serde_json::{json, Value};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, UserAttentionType, Window, WindowId, WindowLevel};

use crate::config::{self, Config, Rgb, Theme};
use crate::proto::Conn;
use crate::render::{lin, Layer, Quad, Renderer, TextItem};

mod ext;
mod ui;
use ui::{Ac, AgentPanel, Card, Composer, Hit};

// ------------------------------------------------------------------ frame

#[derive(Deserialize, Default, Clone)]
struct TabInfo {
    index: usize, title: String, active: bool, pane_title: String, cwd: String, state: String, attention: bool,
    #[serde(default)] color: String,
    #[serde(default)] domain: String,
    #[serde(default)] todo: Option<(u32, u32)>,
    #[serde(default)] pane: u64,
    #[serde(default)] progress: Option<(u8, u8)>,
    #[serde(default)] profile: String,
}

/// Trecho de texto: coluna, texto, fg, bg, flags e link OSC 8 opcional.
#[derive(Deserialize, Default, Clone, PartialEq)]
struct Run(u16, String, i64, i64, u16, #[serde(default)] Option<String>);

#[derive(Deserialize, Default, Clone)]
struct Block {
    row: i64, exit: Option<i32>, ms: Option<u64>, #[serde(default)] cmd: String,
    /// Linha absoluta do prompt (identifica o bloco), inicio/fim da saida na tela e tamanho.
    #[serde(default)] abs: i64,
    #[serde(default)] out: Option<i64>,
    #[serde(default)] end: Option<i64>,
    #[serde(default)] n: i64,
}

#[derive(Deserialize, Default, Clone)]
struct PaneView {
    id: u64, x: u16, y: u16, w: u16, h: u16,
    title: String, cwd: String,
    #[serde(default)] domain: String,
    #[serde(default)] floating: bool,
    #[serde(default)] private: bool,
    attention: bool,
    #[serde(default)] bell: bool,
    scroll: i64, history: i64,
    app_cursor: bool, alt_screen: bool,
    #[serde(default)] mouse: bool,
    #[serde(default)] mouse_motion: bool,
    cursor: Option<(u16, i32, String)>,
    #[serde(default)] lines: Option<Vec<Vec<Run>>>,
    /// Frame por diferenca: so as linhas que mudaram.
    #[serde(default)] rows: Option<HashMap<String, Vec<Run>>>,
    #[serde(default)] blocks: Vec<Block>,
    /// (fracao, 0 rodando / 1 ok / 2 falhou)
    #[serde(default)] ticks: Vec<(f64, u8)>,
    #[serde(default)] state: String,
    #[serde(default)] header: bool,
    #[serde(default)] progress: Option<(u8, u8)>,
    #[serde(default)] profile: String,
    #[serde(default)] running: Option<(String, u64)>,
    #[serde(default)] session: Option<String>,
    #[serde(default)] config_dir: Option<String>,
    #[serde(default)] agent: String,
    /// Comando cuja saida esta no topo (sticky scroll).
    #[serde(default)] sticky: Option<String>,
    /// Inicio do texto digitado agora (linha da tela, coluna): sugestao inline.
    #[serde(default)] input: Option<(i64, usize)>,
    #[serde(default)] recording: bool,
    #[serde(default)] queued: usize,
}

#[derive(Deserialize, Default, Clone)]
struct Approval { id: u64, kind: String, text: String }

#[derive(Deserialize, Default, Clone)]
struct Frame {
    workspace: String,
    workspaces: Vec<String>,
    tabs: Vec<TabInfo>,
    active_pane: u64,
    cols: u16,
    rows: u16,
    panes: Vec<PaneView>,
    agents: HashMap<String, u32>,
    clipboard: (u64, String),
    #[serde(default)] approvals: Vec<Approval>,
    #[serde(default)] resurrect: bool,
    #[serde(default)] recovered: usize,
    #[serde(default)] version: String,
    #[serde(default)] tokens: u64,
    #[serde(default)] ports: Vec<u16>,
    #[serde(default)] inbox_unread: usize,
}

impl PaneView {
    fn lines(&self) -> &[Vec<Run>] { self.lines.as_deref().unwrap_or(&[]) }
    /// Linha como vetor de chars posicionados por coluna (selecao, links, hints).
    fn row_chars(&self, row: usize) -> Vec<char> {
        let mut v = vec![' '; self.w as usize];
        if let Some(runs) = self.lines().get(row) {
            for Run(col, text, _, _, fl, _) in runs {
                let step = if fl & 32 != 0 { 2 } else { 1 };
                for (i, ch) in text.chars().enumerate() {
                    let c = *col as usize + i * step;
                    if c < v.len() { v[c] = ch; }
                }
            }
        }
        v
    }
    fn row_string(&self, row: usize) -> String { self.row_chars(row).into_iter().collect::<String>() }
    fn link_at(&self, row: usize, col: usize) -> Option<(String, String)> {
        let runs = self.lines().get(row)?;
        runs.iter().find(|r| (r.0 as usize..r.0 as usize + r.1.chars().count()).contains(&col))
            .and_then(|r| r.5.clone().map(|u| (u, r.1.clone())))
    }
}

// ------------------------------------------------------------------ acoes

#[derive(Clone)]
enum Action { Builtin(String, Vec<Value>), Lua(Function) }

#[derive(Clone)]
struct Binding { key: String, mods: ModifiersState, action: Action }

fn parse_mods(s: &str) -> ModifiersState {
    let mut m = ModifiersState::empty();
    for p in s.split('|') {
        match p.trim().to_uppercase().as_str() {
            "CTRL" => m |= ModifiersState::CONTROL,
            "SHIFT" => m |= ModifiersState::SHIFT,
            "ALT" => m |= ModifiersState::ALT,
            "SUPER" | "WIN" | "CMD" => m |= ModifiersState::SUPER,
            _ => {}
        }
    }
    m
}

fn lua_action(lua: &Lua, v: LuaValue) -> Option<Action> {
    match v {
        LuaValue::Function(f) => Some(Action::Lua(f)),
        LuaValue::Table(t) => {
            let name: String = t.get("action").ok()?;
            let args: Value = t.get::<LuaValue>("args").ok().and_then(|a| lua.from_value(a).ok()).unwrap_or(json!([]));
            let args = match args { Value::Array(a) => a, Value::Null => vec![], Value::Object(o) if o.is_empty() => vec![], v => vec![v] };
            Some(Action::Builtin(name, args))
        }
        LuaValue::String(s) => Some(Action::Builtin(s.to_str().ok()?.to_string(), vec![])),
        _ => None,
    }
}

fn parse_bindings(lua: &Lua, t: Option<Table>) -> Vec<Binding> {
    let mut out = Vec::new();
    let Some(t) = t else { return out };
    for b in t.sequence_values::<Table>().flatten() {
        let key: String = b.get("key").unwrap_or_default();
        let mods = parse_mods(&b.get::<Option<String>>("mods").ok().flatten().unwrap_or_default());
        if let Some(action) = b.get::<LuaValue>("action").ok().and_then(|a| lua_action(lua, a)) {
            out.push(Binding { key: key.to_lowercase(), mods, action });
        }
    }
    out
}

/// Nome da tecla no formato da config: "p", "\\", "arrowleft", "f5"...
fn key_name(ev: &KeyEvent) -> String {
    match ev.key_without_modifiers() {
        Key::Character(c) => c.to_lowercase(),
        Key::Named(NamedKey::Space) => " ".into(),
        Key::Named(n) => format!("{n:?}").to_lowercase(),
        _ => String::new(),
    }
}

// ------------------------------------------------------------------ overlay

#[derive(Clone)]
struct Item { label: String, hint: String, action: Action, pane: Option<u64> }

impl Item {
    fn new(label: impl Into<String>, hint: impl Into<String>, action: &str, args: Vec<Value>) -> Item {
        Item { label: label.into(), hint: hint.into(), action: Action::Builtin(action.into(), args), pane: None }
    }
}

enum OverlayKind {
    List,
    /// Linha de texto; Enter roda a acao com o texto como ultimo argumento.
    Prompt(String, Vec<Value>),
    /// Previa de arquivo (Ctrl+clique num caminho): Enter abre no editor.
    Preview { file: String, line: i64, col: i64, first: i64, lines: Vec<String> },
    /// Ajuste ao vivo (setas), Enter grava na config.
    Slider { key: String, label: String, val: f32, orig: f32, min: f32, max: f32, step: f32 },
    /// Detalhe vindo de uma extensao (issue, PR...): texto rolavel e botoes.
    Detail { ext: String, subtitle: String, lines: Vec<String>, actions: Vec<(String, String, String)>, scroll: usize },
}

struct Overlay {
    title: String,
    input: String,
    items: Vec<Item>,
    shown: Vec<usize>,
    sel: usize,
    kind: OverlayKind,
    /// Painel de agentes: Ctrl+Y aprova e Ctrl+N nega o pedido do agente.
    agents: bool,
    /// Pergunta com botoes (aprovacao, colar perigoso): desenhada como dialogo.
    dialog: bool,
    /// Tela inicial do workspace (grande, com secoes).
    home: bool,
    /// Paleta de comandos (conta uso para ensinar atalhos).
    palette: bool,
    /// Galeria de temas: tema antes da previa (Esc volta).
    gallery: Option<Theme>,
    /// Campo de token: mostra bolinhas no lugar do texto.
    secret: bool,
}

/// Fuzzy por subsequencia: todos os chars na ordem; consecutivos e inicio de
/// palavra pontuam mais. None = nao casa.
/// Minusculo e sem acento: "Extensões" casa com "extensoes" e vice-versa.
fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a', 'é' | 'è' | 'ê' | 'ë' => 'e', 'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o', 'ú' | 'ù' | 'û' | 'ü' => 'u', 'ç' => 'c', c => c,
    }
}

fn fuzzy(q: &str, s: &str) -> Option<i32> {
    if q.is_empty() { return Some(0); }
    let s: Vec<char> = s.to_lowercase().chars().map(fold).collect();
    let mut score = 0;
    let mut last: Option<usize> = None;
    let mut i = 0;
    for qc in q.to_lowercase().chars().map(fold) {
        if qc == ' ' { continue; }
        while i < s.len() && s[i] != qc { i += 1; }
        if i == s.len() { return None; }
        score += 1;
        if last.is_some_and(|l| l + 1 == i) { score += 5; }
        if i == 0 || !s[i - 1].is_alphanumeric() { score += 3; }
        last = Some(i);
        i += 1;
    }
    Some(score * 100 - s.len() as i32)
}

impl Overlay {
    fn list(title: &str, items: Vec<Item>) -> Overlay {
        let mut o = Overlay { title: title.into(), input: String::new(), items, shown: vec![], sel: 0, kind: OverlayKind::List, agents: false,
            dialog: false, home: false, palette: false, gallery: None, secret: false };
        o.filter();
        o
    }
    fn prompt(title: &str, initial: &str, action: &str, args: Vec<Value>) -> Overlay {
        Overlay { title: title.into(), input: initial.into(), items: vec![], shown: vec![], sel: 0, kind: OverlayKind::Prompt(action.into(), args), agents: false,
            dialog: false, home: false, palette: false, gallery: None, secret: false }
    }
    fn dialog(title: &str, items: Vec<Item>) -> Overlay {
        let mut o = Overlay::list(title, items);
        o.dialog = true;
        o
    }
    fn filter(&mut self) {
        let mut v: Vec<(i32, usize)> = self.items.iter().enumerate()
            .filter_map(|(i, it)| fuzzy(&self.input, &format!("{} {}", it.label, it.hint)).map(|s| (s, i))).collect();
        if !self.input.is_empty() { v.sort_by(|a, b| b.0.cmp(&a.0)); }
        self.shown = v.into_iter().map(|x| x.1).collect();
        self.sel = 0;
    }
}

/// Paste perigoso pede confirmacao (o resto cola direto).
const DANGEROUS: &str = r"(?i)(\bsudo\b|\brm\s+-[a-z]*[rf]|\b(curl|wget|iwr|irm|invoke-webrequest|invoke-restmethod)\b[^|\n]*\|\s*(ba|z)?sh\b|\|\s*(iex|invoke-expression)\b|\biex\s*\(|\bformat-volume\b|\bmkfs\b|\bdd\s+if=|:\(\)\s*\{|\bremove-item\b[^\n]*-recurse)";

// ------------------------------------------------------------------ app

#[derive(Debug)]
pub enum UserEv { Frame, Reload, Tick, Quick, Connected, Ext }

#[derive(Clone, Copy, PartialEq)]
struct Cell { col: i32, row: i32 }

struct Selection { pane: u64, a: Cell, b: Cell }

struct CopyMode { pane: u64, cur: Cell, anchor: Option<Cell> }

struct Search { query: String, pane: u64 }

struct Hint { label: String, pane: u64, row: usize, col: usize, len: usize, text: String }

/// Medicoes do sistema (thread propria, a cada 2s; 1s com o painel aberto).
#[derive(Default, Clone)]
struct Stats {
    cpu: f32, ram: f64, ram_total: f64,
    cores: Vec<f32>, brand: String, freq_mhz: u64,
    avail: f64, swap: f64, swap_total: f64, uptime: u64,
    /// Ultimas 60 amostras de CPU e RAM (%) para os graficos.
    cpu_hist: std::collections::VecDeque<f32>, ram_hist: std::collections::VecDeque<f32>,
    /// Processos que mais gastam: (nome, CPU % do total, MB, quantos).
    top: Vec<(String, f32, f64, usize)>,
    /// O painel esta aberto: so entao vale a pena ler os processos.
    want_procs: bool,
}

pub struct Gui {
    proxy: EventLoopProxy<UserEv>,
    client: String,
    conn: Arc<Mutex<Option<Conn>>>,
    latest: Arc<Mutex<(Option<Frame>, Vec<Value>)>>,
    frame: Frame,
    r: Option<Renderer>,
    lua: Lua,
    cfg: Config,
    cfg_table: Option<Table>,
    cfg_error: Option<String>,
    keys: Vec<Binding>,
    tables: HashMap<String, Vec<Binding>>,
    table_stack: Vec<String>,
    palette_extra: Vec<Item>,
    hyperlinks: Vec<(regex::Regex, String)>,
    queue: Arc<Mutex<Vec<(String, Vec<Value>)>>>,
    mods: ModifiersState,
    overlay: Option<Overlay>,
    sel: Option<Selection>,
    dragging: bool,
    copy: Option<CopyMode>,
    search: Option<Search>,
    hints: Option<(Vec<Hint>, String)>,
    broadcast: HashSet<(String, usize)>,
    mouse: (f32, f32),
    last_click: (Instant, Cell),
    stats: Arc<Mutex<Stats>>,
    clip_seq: u64,
    font_delta: f32,
    focused: bool,
    message: Option<(String, Instant)>,
    origin: (f32, f32),
    startup: Vec<Value>,
    light: bool,
    bell_until: HashMap<u64, Instant>,
    asked: HashSet<u64>,
    shown_notices: HashSet<String>,
    last_draw: Instant,
    redraw_at: Option<Instant>,
    quick: bool,
    quick_visible: bool,
    plugins: Vec<crate::plugins::Plugin>,
    /// Agent Manager em grade (Ctrl+Shift+A).
    grid: Option<Grid>,
    /// Comentarios da revisao de codigo ainda nao enviados.
    review: Vec<String>,
    /// Historico para a sugestao inline (comandos, mais recente primeiro).
    hist_cache: (Instant, Vec<(String, String)>),
    ghost: Option<String>,
    branches: HashMap<String, (Instant, String)>,
    // ---- UI/UX (src/gui/ui.rs)
    /// Botoes e alvos desenhados no ultimo quadro (clique resolve aqui).
    hits: Vec<Hit>,
    tab_rects: Vec<(f32, f32, usize)>,
    /// (y, altura) da linha de abas.
    tab_row: (f32, f32),
    sidebar_collapsed: bool,
    focus_mode: bool,
    agent_panel: Option<AgentPanel>,
    cards: Vec<Card>,
    card_seq: u64,
    /// Animacao do cartao do pane: (de, para, inicio, nascendo).
    rect_anim: HashMap<u64, ([f32; 4], [f32; 4], Instant, bool)>,
    prev_rects: HashMap<u64, [f32; 4]>,
    ghosts: Vec<([f32; 4], Instant)>,
    anim_tab_key: String,
    ws_anim: Option<(Instant, f32)>,
    cursor_anim: Option<([f32; 2], [f32; 2], Instant)>,
    cursor_shown: [f32; 2],
    last_key: Instant,
    tab_hover: Option<(usize, Instant)>,
    tab_preview: Option<(usize, Vec<String>)>,
    tab_drag: Option<(usize, f32, bool)>,
    /// Arrastando um badge de extensao: (nome, x inicial, ja moveu).
    badge_drag: Option<(String, f32, bool)>,
    /// Badges do ultimo quadro: (x0, x1, nome, acao do clique).
    badge_rects: Vec<(f32, f32, String, Action)>,
    /// Ordem dos badges (config ext_order).
    ext_order: Vec<String>,
    div_drag: Option<(u64, bool, f32)>,
    title_click: Instant,
    resize_edge: Option<winit::window::ResizeDirection>,
    /// Blocos recolhidos/abertos pelo usuario: (pane, linha do prompt) -> recolhido.
    collapsed: HashMap<(u64, i64), bool>,
    /// Bloco fixado no topo do pane: (linha do prompt, comando, ultimas linhas).
    pinned: HashMap<u64, (i64, String, Vec<String>)>,
    /// Busca: (pane, linhas, atual, historico, total).
    search_hits: Option<(u64, Vec<i64>, usize, i64, i64)>,
    composer: Option<Composer>,
    ac: Option<Ac>,
    ac_dismissed: String,
    usage: HashMap<String, u32>,
    exe_cache: HashMap<String, bool>,
    git_cache: HashMap<String, (Instant, Vec<String>)>,
    started: Instant,
    /// Ultimo toast por pane (no maximo um por minuto).
    toasted: HashMap<u64, Instant>,
    /// Apresentacao ao abrir a janela (o pato).
    splash: Option<Instant>,
    /// Painel lateral do sistema (clique em CPU/RAM na barra).
    sys_panel: bool,
    /// Extensoes (src/gui/ext.rs): processos, visoes, status.
    exts: ext::Exts,
    /// Visao de extensao aberta na lateral ("extensao/visao"), rolagem e altura do conteudo.
    ext_panel: Option<String>,
    /// Largura do painel lateral das extensoes (arrastavel; config ext_panel_width).
    ext_w: f32,
    ext_drag: bool,
    ext_scroll: f32,
    ext_content: f32,
    /// Ultimo contexto (pane ativo) mandado as extensoes.
    ext_context: Option<Value>,
}

struct Grid { panes: Vec<(u64, String, String)>, sel: usize, texts: HashMap<u64, String>, fetched: Instant }

impl Gui {
    pub fn new(proxy: EventLoopProxy<UserEv>, startup: Vec<Value>, quick: bool) -> Gui {
        let mut g = Gui {
            proxy, client: format!("gui-{}", std::process::id()),
            conn: Arc::new(Mutex::new(None)), latest: Arc::new(Mutex::new((None, vec![]))), frame: Frame::default(),
            r: None, lua: Lua::new(), cfg: config::load_config().0, cfg_table: None, cfg_error: None,
            keys: vec![], tables: HashMap::new(), table_stack: vec![], palette_extra: vec![], hyperlinks: vec![],
            queue: Arc::new(Mutex::new(vec![])), mods: ModifiersState::empty(), overlay: None, sel: None,
            dragging: false, copy: None, search: None, hints: None, broadcast: HashSet::new(), mouse: (0.0, 0.0),
            last_click: (Instant::now() - Duration::from_secs(5), Cell { col: -1, row: -1 }),
            stats: Arc::new(Mutex::new(Stats::default())),
            clip_seq: 0, font_delta: 0.0, focused: true, message: None, origin: (0.0, 0.0),
            startup, light: false, bell_until: HashMap::new(), asked: HashSet::new(), shown_notices: HashSet::new(),
            last_draw: Instant::now(), redraw_at: None, quick, quick_visible: !quick, plugins: Vec::new(),
            grid: None, review: Vec::new(), hist_cache: (Instant::now() - Duration::from_secs(60), Vec::new()), ghost: None,
            branches: HashMap::new(),
            hits: vec![], tab_rects: vec![], tab_row: (0.0, 0.0), sidebar_collapsed: false, focus_mode: false, agent_panel: None,
            cards: vec![], card_seq: 0, rect_anim: HashMap::new(), prev_rects: HashMap::new(), ghosts: vec![], anim_tab_key: String::new(),
            ws_anim: None, cursor_anim: None, cursor_shown: [0.0, 0.0], last_key: Instant::now(), tab_hover: None, tab_preview: None,
            tab_drag: None, badge_drag: None, badge_rects: vec![], ext_order: vec![], div_drag: None, title_click: Instant::now() - Duration::from_secs(5), resize_edge: None,
            collapsed: HashMap::new(), pinned: HashMap::new(), search_hits: None, composer: None, ac: None, ac_dismissed: String::new(),
            usage: HashMap::new(), exe_cache: HashMap::new(), git_cache: HashMap::new(), started: Instant::now(), toasted: HashMap::new(), splash: None, sys_panel: false, exts: Default::default(), ext_panel: None, ext_w: 400.0, ext_drag: false, ext_scroll: 0.0, ext_content: 0.0, ext_context: None,
        };
        g.load_config();
        g
    }

    fn theme(&self) -> &Theme {
        if self.light { self.cfg.theme_light.as_ref().unwrap_or(&self.cfg.theme) } else { &self.cfg.theme }
    }

    // -------------------------------------------------------------- config

    fn load_config(&mut self) {
        let (lua, t, err) = config::load();
        self.cfg = config::parse(&t);
        self.cfg_error = err.clone();
        if let Some(e) = err { self.flash(&e); }
        self.keys = parse_bindings(&lua, t.get("keys").ok());
        // Atalhos novos com padrao (a config do usuario pode sobrescrever).
        let defaults: [(&str, &str, &str, Vec<Value>); 25] = [
            ("w", "CTRL|SHIFT", "close_tab", vec![]),
            ("i", "CTRL|SHIFT", "agent_panel", vec![]),
            ("j", "CTRL|SHIFT", "inbox", vec![]),
            ("y", "CTRL|SHIFT", "agent_timeline", vec![]),
            ("z", "CTRL|SHIFT", "focus_mode", vec![]),
            ("b", "CTRL|ALT", "toggle_sidebar", vec![]),
            ("enter", "CTRL|SHIFT", "composer", vec![]),
            ("home", "CTRL|SHIFT", "home", vec![]),
            ("e", "CTRL|SHIFT", "fix_with_agent", vec![]),
            ("a", "CTRL|SHIFT", "agent_grid", vec![]),
            ("r", "CTRL|SHIFT", "history", vec![]),
            ("n", "CTRL|SHIFT", "agent_new", vec![]),
            ("m", "CTRL|SHIFT", "queue_message", vec![]),
            ("d", "CTRL|SHIFT", "review", vec![]),
            ("k", "CTRL|SHIFT", "fork_agent", vec![]),
            ("arrowup", "CTRL|SHIFT", "jump_prompt", vec![json!("up")]),
            ("arrowdown", "CTRL|SHIFT", "jump_prompt", vec![json!("down")]),
            ("o", "CTRL|SHIFT", "copy_last_output", vec![]),
            (" ", "CTRL|SHIFT", "hints", vec![]),
            ("b", "CTRL|SHIFT", "broadcast", vec![]),
            ("f", "ALT", "float", vec![]),
            ("g", "CTRL|SHIFT", "filter", vec![]),
            ("h", "CTRL|SHIFT", "sessions", vec![]),
            (",", "CTRL", "settings", vec![]),
            ("l", "CTRL|SHIFT", "lock", vec![]),
        ];
        for (k, m, a, args) in defaults {
            let mods = parse_mods(m);
            if !self.keys.iter().any(|b| b.key == k && b.mods == mods) {
                self.keys.push(Binding { key: k.into(), mods, action: Action::Builtin(a.into(), args) });
            }
        }
        self.tables.clear();
        if let Ok(kt) = t.get::<Table>("key_tables") {
            for (name, list) in kt.pairs::<String, Table>().flatten() {
                self.tables.insert(name, parse_bindings(&lua, Some(list)));
            }
        }
        self.palette_extra.clear();
        if let Ok(p) = t.get::<Table>("palette") {
            for e in p.sequence_values::<Table>().flatten() {
                let label: String = e.get("label").unwrap_or_default();
                if let Some(a) = e.get::<LuaValue>("action").ok().and_then(|a| lua_action(&lua, a)) {
                    self.palette_extra.push(Item { label, hint: String::new(), action: a, pane: None });
                }
            }
        }
        // Workflows: comandos com {{parametros}} na paleta.
        if let Ok(w) = t.get::<Table>("workflows") {
            for (i, e) in w.sequence_values::<Table>().flatten().enumerate() {
                let name: String = e.get("name").unwrap_or_default();
                let cmd: String = e.get("command").unwrap_or_default();
                self.palette_extra.push(Item::new(format!("Workflow: {name}"), cmd, "workflow", vec![json!(i)]));
            }
        }
        if let Ok(l) = t.get::<Table>("layouts") {
            for (name, _) in l.pairs::<String, Table>().flatten() {
                self.palette_extra.push(Item::new(format!("Layout: {name}"), "abre num workspace novo", "layout", vec![json!(name)]));
            }
        }
        self.hyperlinks = self.cfg.hyperlinks.iter().filter_map(|(r, f)| regex::Regex::new(r).ok().map(|r| (r, f.clone()))).collect();
        self.install_runtime(&lua);
        self.lua = lua;
        if let Ok(w) = t.get::<f32>("ext_panel_width") { self.ext_w = w.clamp(260.0, 1400.0); }
        self.ext_order = t.get::<Vec<String>>("ext_order").unwrap_or_default();
        self.cfg_table = Some(t);
        let bg = self.cfg.background.clone();
        let shader = self.cfg.shader.clone();
        let mut errs = Vec::new();
        if let Some(r) = self.r.as_mut() {
            let scale = r.window.scale_factor() as f32;
            r.set_font(&self.cfg.font_family, (self.cfg.font_size + self.font_delta) * 96.0 / 72.0 * scale, self.cfg.line_height);
            r.set_style(&self.cfg.ui.font, self.cfg.ui.font_weight, &self.cfg.ui.font_features);
            apply_backdrop(&r.window, &self.cfg.backdrop);
            if let Err(e) = r.set_background(bg.image.as_deref(), bg.opacity) { errs.push(format!("imagem de fundo: {e}")); }
            if let Err(e) = r.set_shader(shader.as_deref()) { errs.push(format!("shader: {e}")); }
        }
        for e in errs { self.flash(&e); }
        let (plugins, perrs) = crate::plugins::load_all();
        self.plugins = plugins;
        for e in perrs { self.flash(&e); }
        if !self.plugins.is_empty() { self.run_plugins(&json!({"event": "load"})); }
    }

    /// Entrega o evento aos plugins WASM e executa o que eles pedirem.
    fn run_plugins(&mut self, ev: &Value) {
        let text = ev.to_string();
        let mut reqs = Vec::new();
        let mut errs = Vec::new();
        for p in &mut self.plugins {
            match p.on_event(&text) {
                Ok(r) => reqs.extend(r),
                Err(e) => errs.push(format!("plugin {}: {e}", p.name)),
            }
        }
        for e in errs { self.flash(&e); }
        for r in reqs {
            let Ok(v) = serde_json::from_str::<Value>(&r) else { continue };
            if let Some(a) = v["action"].as_str() {
                let args = v["args"].as_array().cloned().unwrap_or_default();
                self.builtin(a, &args);
            } else if v.get("cmd").is_some() {
                self.cmd(v);
            }
        }
    }

    /// Funcoes que a config chama em tempo de execucao: duck.cli{...},
    /// duck.toast(...), duck.run(...).
    fn install_runtime(&self, lua: &Lua) {
        let m: Table = lua.globals().get("duck").unwrap();
        let conn = self.conn.clone();
        let _ = m.set("cli", lua.create_function(move |lua, req: LuaValue| {
            let req: Value = lua.from_value(req)?;
            let r = with_conn(&conn, |c| c.call(req.clone()));
            to_lua(lua, &r.unwrap_or_else(|e| json!({"error": e})))
        }).unwrap());
        let _ = m.set("toast", lua.create_function(|_, (title, msg): (String, Option<String>)| {
            toast(&title, &msg.unwrap_or_default());
            Ok(())
        }).unwrap());
        let q = self.queue.clone();
        let _ = m.set("run", lua.create_function(move |lua, (name, args): (String, mlua::Variadic<LuaValue>)| {
            let a: Vec<Value> = args.into_iter().filter_map(|v| lua.from_value(v).ok()).collect();
            q.lock().unwrap().push((name, a));
            Ok(())
        }).unwrap());
    }

    fn win_table(&self) -> Table {
        let lua = &self.lua;
        let t = lua.create_table().unwrap();
        let q = self.queue.clone();
        let _ = t.set("run", lua.create_function(move |lua, (_s, name, args): (LuaValue, String, mlua::Variadic<LuaValue>)| {
            let a: Vec<Value> = args.into_iter().filter_map(|v| lua.from_value(v).ok()).collect();
            q.lock().unwrap().push((name, a));
            Ok(())
        }).unwrap());
        let conn = self.conn.clone();
        let client = self.client.clone();
        let _ = t.set("cli", lua.create_function(move |lua, (_s, req): (LuaValue, LuaValue)| {
            let mut req: Value = lua.from_value(req)?;
            if req.get("client").is_none() { req["client"] = json!(client); }
            let r = with_conn(&conn, |c| c.call(req.clone()));
            to_lua(lua, &r.unwrap_or_else(|e| json!({"error": e})))
        }).unwrap());
        let _ = t.set("toast", lua.create_function(|_, (_s, title, msg): (LuaValue, String, Option<String>)| {
            toast(&title, &msg.unwrap_or_default());
            Ok(())
        }).unwrap());
        let _ = t.set("pane", self.frame.active_pane);
        let _ = t.set("workspace", self.frame.workspace.clone());
        t
    }

    // -------------------------------------------------------------- server

    fn connect(&mut self) {
        let conn = match Conn::connect_or_spawn() {
            Ok(c) => c,
            Err(e) => { eprintln!("duck: sem server: {e}"); std::process::exit(1); }
        };
        *self.conn.lock().unwrap() = Some(conn);
        for req in std::mem::take(&mut self.startup) {
            let mut req = req;
            req["client"] = json!(self.client);
            if let Err(e) = self.call(req) { self.flash(&e); }
        }
        let latest = self.latest.clone();
        let proxy = self.proxy.clone();
        let client = self.client.clone();
        let inbox = self.exts.inbox.clone();
        std::thread::spawn(move || loop {
            if let Ok(mut c) = Conn::connect_or_spawn() {
                if c.send(&json!({"cmd": "subscribe", "client": client})).is_ok() {
                    // Server novo (reinicio/queda) nao sabe o tamanho da janela.
                    let _ = proxy.send_event(UserEv::Connected);
                    while let Ok(v) = c.recv() {
                        if v.get("t").and_then(|t| t.as_str()) == Some("ext") {
                            let ext = v["ext"].as_str().unwrap_or("").to_string();
                            inbox.lock().unwrap().push((ext, v["msg"].clone()));
                            if proxy.send_event(UserEv::Ext).is_err() { return; }
                            continue;
                        }
                        let mut l = latest.lock().unwrap();
                        match v.get("t").and_then(|t| t.as_str()) {
                            // Frames por diferenca se acumulam: aplica sobre o anterior ainda nao lido.
                            Some("frame") => if let Ok(f) = serde_json::from_value::<Frame>(v) {
                                let prev = l.0.take();
                                l.0 = Some(merge_frame(prev.as_ref(), f));
                            },
                            Some("event") => l.1.push(v),
                            _ => {}
                        }
                        drop(l);
                        if proxy.send_event(UserEv::Frame).is_err() { return; }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1)); // server caiu: tenta de novo
        });
    }

    fn call(&self, mut req: Value) -> Result<Value, String> {
        if req.get("client").is_none() { req["client"] = json!(self.client); }
        with_conn(&self.conn, |c| c.call(req.clone()))
    }

    fn cmd(&mut self, req: Value) {
        if let Err(e) = self.call(req) { self.flash(&e); }
    }

    fn flash(&mut self, msg: &str) {
        self.message = Some((msg.to_string(), Instant::now()));
        self.redraw();
    }

    fn redraw(&self) { if let Some(r) = &self.r { r.window.request_redraw(); } }

    // -------------------------------------------------------------- geometria

    fn cell_at(&self, x: f32, y: f32) -> Option<(u64, Cell)> {
        let r = self.r.as_ref()?;
        let (ox, oy) = self.origin;
        let col = ((x - ox) / r.cell_w).floor() as i32;
        let row = ((y - oy) / r.cell_h).floor() as i32;
        // Flutuante por cima: testa primeiro.
        for p in self.frame.panes.iter().rev() {
            if col >= p.x as i32 && col < (p.x + p.w) as i32 && row >= p.y as i32 && row < (p.y + p.h) as i32 {
                return Some((p.id, Cell { col: col - p.x as i32, row: row - p.y as i32 }));
            }
        }
        None
    }

    fn pane(&self, id: u64) -> Option<&PaneView> { self.frame.panes.iter().find(|p| p.id == id) }
    fn active(&self) -> Option<&PaneView> { self.pane(self.frame.active_pane) }
    fn active_tab_key(&self) -> (String, usize) {
        (self.frame.workspace.clone(), self.frame.tabs.iter().position(|t| t.active).unwrap_or(0))
    }

    // -------------------------------------------------------------- acoes

    fn run(&mut self, a: Action) {
        match a {
            Action::Builtin(n, args) => self.builtin(&n, &args),
            Action::Lua(f) => {
                let win = self.win_table();
                if let Err(e) = f.call::<()>(win) { self.flash(&format!("lua: {e}")); }
            }
        }
        let pending: Vec<_> = std::mem::take(&mut *self.queue.lock().unwrap());
        for (n, args) in pending { self.builtin(&n, &args); }
    }

    fn builtin(&mut self, name: &str, args: &[Value]) {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let n = |i: usize, d: i64| args.get(i).and_then(|v| v.as_i64()).unwrap_or(d);
        let ap = self.frame.active_pane;
        match name {
            "palette" => self.open_palette(),
            "split" => self.cmd(json!({"cmd": "spawn", "where": if s(0).is_empty() { "right".into() } else { s(0) },
                "pane": ap, "domain": args.get(1).cloned().unwrap_or(Value::Null)})),
            "new_tab" => self.cmd(json!({"cmd": "spawn", "where": "tab", "domain": args.first().cloned().unwrap_or(Value::Null)})),
            "spawn" => {
                // {where, cmd={...}|"linha", domain, cwd, title}; no protocolo o argv e "argv".
                let mut req = json!({"cmd": "spawn", "pane": ap});
                if let Some(o) = args.first().and_then(|v| v.as_object()) {
                    for (k, v) in o { req[if k == "cmd" { "argv" } else { k.as_str() }] = v.clone(); }
                }
                self.cmd(req);
            }
            "float" => {
                let mut req = json!({"cmd": "float"});
                if let Some(c) = args.first().filter(|v| !v.is_null()) { req["argv"] = c.clone(); }
                self.cmd(req);
            }
            "close_pane" => self.cmd(json!({"cmd": "close", "pane": ap})),
            "close_tab" => for id in self.frame.panes.iter().map(|p| p.id).collect::<Vec<_>>() { self.cmd(json!({"cmd": "close", "pane": id})) },
            "zoom" => self.cmd(json!({"cmd": "zoom"})),
            "rename_tab" => {
                let cur = self.frame.tabs.iter().find(|t| t.active).map(|t| t.title.clone()).unwrap_or_default();
                self.overlay = Some(Overlay::prompt("Novo nome da aba (nome|subtitulo; vazio volta ao automatico)", &cur, "rename_tab_to", vec![]));
            }
            "rename_tab_to" => self.cmd(json!({"cmd": "rename-tab", "title": s(0)})),
            "tab_color" => {
                let colors = [("Nenhuma", ""), ("Vermelho", "blocked"), ("Verde", "done"), ("Amarelo", "working"), ("Azul", "accent"), ("Cinza", "muted")];
                let items = colors.iter().map(|(l, c)| Item::new(*l, "", "tab_color_to", vec![json!(c)])).collect();
                self.overlay = Some(Overlay::list("Cor da aba", items));
            }
            "tab_color_to" => self.cmd(json!({"cmd": "tab-color", "color": s(0)})),
            "rename_workspace" => self.overlay = Some(Overlay::prompt("Novo nome do workspace", &self.frame.workspace.clone(), "rename_workspace_to", vec![])),
            "rename_workspace_to" => if !s(0).is_empty() { self.cmd(json!({"cmd": "rename-workspace", "name": s(0)})) },
            "tab" => self.cmd(json!({"cmd": "tab", "index": n(0, 1)})),
            "tab_relative" => self.cmd(json!({"cmd": "tab", "delta": n(0, 1)})),
            "move_tab" => self.cmd(json!({"cmd": "move-tab", "delta": n(0, 1)})),
            "move_tab_ws" => {
                let cur = self.frame.workspace.clone();
                let mut items: Vec<Item> = self.frame.workspaces.iter().filter(|w| **w != cur)
                    .map(|w| Item::new(w.clone(), "", "move_tab_to", vec![json!(w)])).collect();
                items.push(Item::new("Novo workspace...", "", "move_tab_new", vec![]));
                self.overlay = Some(Overlay::list("Mover a aba atual para o workspace", items));
            }
            "move_tab_new" => self.overlay = Some(Overlay::prompt("Nome do workspace novo", "", "move_tab_to", vec![])),
            "move_tab_to" => if !s(0).trim().is_empty() {
                match self.call(json!({"cmd": "move-tab", "workspace": s(0).trim()})) {
                    Ok(_) => self.flash(&format!("Aba movida para o workspace {}", s(0).trim())),
                    Err(e) => self.flash(&e),
                }
            },
            "focus" => self.cmd(json!({"cmd": "focus", "dir": s(0)})),
            "focus_pane" => self.cmd(json!({"cmd": "focus", "pane": n(0, 0)})),
            "adjust" => self.cmd(json!({"cmd": "adjust", "dir": s(0), "amount": n(1, 2)})),
            "key_table" => self.table_stack.push(s(0)),
            "pop_table" => { self.table_stack.pop(); }
            "scroll" => {
                let to = s(0);
                let req = if to.is_empty() { json!({"cmd": "scroll", "delta": n(0, 0), "pane": ap}) } else { json!({"cmd": "scroll", "to": to, "pane": ap}) };
                self.cmd(req);
            }
            "jump_prompt" => match self.call(json!({"cmd": "jump", "pane": ap, "dir": s(0)})) {
                Ok(v) if v["found"] == false && s(0) != "down" => self.flash("Nenhum prompt acima (shell integration ligada?)"),
                Err(e) => self.flash(&e),
                _ => {}
            },
            "copy_last_output" => match self.call(json!({"cmd": "last-output", "pane": ap})) {
                Ok(v) => {
                    let t = v["text"].as_str().unwrap_or("").to_string();
                    if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(t.replace('\n', "\r\n")); }
                    self.flash(&format!("Saida do ultimo comando copiada ({} linhas, exit {})", t.lines().count(), v["exit"]));
                }
                Err(e) => self.flash(&e),
            },
            "workspace_switch" => self.open_workspaces(),
            "workspace_new" => self.overlay = Some(Overlay::prompt("Nome do workspace", "", "workspace", vec![])),
            "workspace" => if !s(0).is_empty() {
                let mut req = json!({"cmd": "workspace", "name": s(0)});
                if args.len() > 1 { req["cwd"] = json!(s(1)); }
                self.cmd(req);
            },
            "layout" => {
                let spec = self.cfg_table.as_ref().and_then(|t| t.get::<Table>("layouts").ok())
                    .and_then(|l| l.get::<LuaValue>(s(0)).ok()).and_then(|v| self.lua.from_value::<Value>(v).ok());
                match spec {
                    Some(mut spec) => {
                        if spec.get("name").is_none() { spec["name"] = json!(s(0)); }
                        self.cmd(json!({"cmd": "open-layout", "spec": spec}));
                    }
                    None => self.flash(&format!("layout nao encontrado: {}", s(0))),
                }
            }
            "agents" => self.open_agents(),
            "sessions" => self.open_sessions(),
            "agent_diff" => {
                // Diff do repo do pane ativo num flutuante (o que o agente mudou).
                let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
                self.cmd(json!({"cmd": "float", "cwd": cwd, "argv": ["pwsh", "-NoLogo", "-NoProfile", "-NoExit", "-Command",
                    "git -c color.ui=always status -sb; git -c color.ui=always --no-pager diff --stat; git -c color.ui=always --no-pager diff"]}));
            }
            "save_state" => match self.call(json!({"cmd": "save"})) {
                Ok(_) => self.flash(&format!("Workspace salvo: {}", self.frame.workspace)),
                Err(e) => self.flash(&e),
            },
            "restore_state" | "delete_state" => self.open_states(name == "delete_state"),
            "restore_state_named" => self.cmd(json!({"cmd": "restore", "name": s(0)})),
            "delete_state_named" => { self.cmd(json!({"cmd": "delete-state", "name": s(0)})); self.flash(&format!("Apagado: {}", s(0))); }
            "checkpoint" => match self.call(json!({"cmd": "checkpoint"})) {
                Ok(v) => self.flash(&format!("Checkpoint: {} workspaces, {} sessoes de agente",
                    v["workspaces"].as_array().map(|a| a.len()).unwrap_or(0), v["agents"])),
                Err(e) => self.flash(&e),
            },
            "checkpoint_restore" => match self.call(json!({"cmd": "checkpoint-restore"})) {
                Ok(v) => self.flash(&format!("Restaurado: {}", v["restored"])),
                Err(e) => self.flash(&e),
            },
            "resurrect_dismiss" => self.cmd(json!({"cmd": "resurrect-dismiss"})),
            "approve" => {
                self.cmd(json!({"cmd": "approve", "id": n(0, 0), "ok": args.get(1).and_then(|v| v.as_bool()).unwrap_or(false)}));
                self.asked.remove(&(n(0, 0) as u64));
            }
            "copy" => self.copy_selection(),
            "paste" => {
                let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
                if text.is_empty() {
                    // Print no clipboard: vira PNG e cola o caminho (o Claude Code le imagens por caminho).
                    match self.clipboard_image() {
                        Some(path) => { self.flash(&format!("Imagem colada como arquivo: {path}")); self.paste(path, true); }
                        None => self.flash("Clipboard vazio"),
                    }
                } else {
                    self.paste(text, false);
                }
            }
            "paste_confirmed" => self.paste(s(0), true),
            "send" => self.send_keys(&s(0)),
            "search" => {
                self.search = Some(Search { query: String::new(), pane: ap });
                self.overlay = Some(Overlay::prompt("Buscar no scrollback (/regex/ tambem; Enter = proxima, Esc sai)", "", "search_run", vec![]));
            }
            "search_run" => self.search_run(s(0)),
            "filter" => self.overlay = Some(Overlay::prompt("Filtrar o scrollback (regex): mostra so as linhas que casam", "", "filter_run", vec![])),
            "filter_run" => match self.call(json!({"cmd": "grep", "pane": ap, "regex": s(0)})) {
                Ok(v) => {
                    let items: Vec<Item> = v["hits"].as_array().into_iter().flatten().map(|h| {
                        Item::new(h["text"].as_str().unwrap_or("").trim(), format!("linha {}", h["line"]), "goto_line", vec![h["line"].clone()])
                    }).collect();
                    if items.is_empty() { self.flash("Nada casou"); } else {
                        let total = items.len();
                        self.overlay = Some(Overlay::list(&format!("{total} linhas com /{}/ (Enter vai ate a linha)", s(0)), items));
                    }
                }
                Err(e) => self.flash(&e),
            },
            "goto_line" => self.cmd(json!({"cmd": "goto-line", "pane": ap, "line": n(0, 0)})),
            "copy_mode" => if let Some(p) = self.active() {
                let cur = p.cursor.as_ref().map(|c| Cell { col: c.0 as i32, row: c.1 }).unwrap_or(Cell { col: 0, row: p.h as i32 - 1 });
                self.copy = Some(CopyMode { pane: p.id, cur, anchor: None });
            },
            "hints" => self.open_hints(),
            "broadcast" => {
                let k = self.active_tab_key();
                if !self.broadcast.remove(&k) { self.broadcast.insert(k); self.flash("Broadcast LIGADO: o que voce digita vai para todos os panes da aba"); }
                else { self.flash("Broadcast desligado"); }
            }
            "private" => match self.call(json!({"cmd": "private", "pane": ap})) {
                Ok(v) => self.flash(if v["private"] == true { "Pane privado: fora do autosave e da leitura por agentes" } else { "Pane voltou ao normal" }),
                Err(e) => self.flash(&e),
            },
            "lock" => self.builtin("private", &[]),
            "workflow" => self.workflow_step(n(0, 0) as usize, &args[1..]),
            "font_size" => {
                let d = n(0, 1) as f32;
                self.font_delta = if d == 0.0 { 0.0 } else { self.font_delta + d };
                if let Some(r) = self.r.as_mut() {
                    let scale = r.window.scale_factor() as f32;
                    r.set_font(&self.cfg.font_family, (self.cfg.font_size + self.font_delta) * 96.0 / 72.0 * scale, self.cfg.line_height);
                }
                self.send_size();
            }
            "settings" => self.open_settings(),
            "setting" => {
                // [chave lua, valor lua literal] -> ~/.duck-local.lua (o hot reload aplica)
                match set_local_setting(&s(0), &s(1)) {
                    Ok(_) => self.flash(&format!("{} = {} (em ~/.duck-local.lua)", s(0), s(1))),
                    Err(e) => self.flash(&e),
                }
            }
            "setting_prompt" => self.overlay = Some(Overlay::prompt(&format!("Valor para {}", s(0)), &s(1), "setting_text", vec![json!(s(0)), json!(s(2))])),
            "setting_text" => {
                let raw = s(2);
                let v = if s(1) == "number" { raw.clone() } else { format!("{raw:?}") };
                self.builtin("setting", &[json!(s(0)), json!(v)]);
            }
            "choose" => {
                let spec = args.first().cloned().unwrap_or(json!({}));
                let title = spec["title"].as_str().unwrap_or("Escolha").to_string();
                let items = spec["choices"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                    let a = &c["action"];
                    Item::new(c["label"].as_str().unwrap_or(""), "", a["action"].as_str().unwrap_or(""), a["args"].as_array().cloned().unwrap_or_default())
                }).collect();
                self.overlay = Some(Overlay::list(&title, items));
            }
            "reload_config" => { self.load_config(); self.cmd(json!({"cmd": "reload"})); self.flash("Config recarregada"); }
            "toast" => toast(&s(0), &s(1)),
            "open_url" => open_url(&s(0)),
            "fix_with_agent" => self.fix_with_agent(),
            "agent_new" => {
                let mut items: Vec<Item> = self.cfg.agent_profiles.iter().enumerate()
                    .map(|(i, pr)| Item::new(format!("{}  (a direita)", pr.name), pr.cmd.clone(), "agent_spawn", vec![json!(i)])).collect();
                items.extend(self.cfg.agent_profiles.iter().enumerate()
                    .map(|(i, pr)| Item::new(format!("{}  em worktree nova (branch propria)", pr.name), pr.cmd.clone(), "agent_worktree", vec![json!(i)])));
                self.overlay = Some(Overlay::list("Novo agente", items));
            }
            "agent_spawn" => {
                let cmd = self.cfg.agent_profiles.get(n(0, 0) as usize).map(|p| p.cmd.clone()).unwrap_or_else(|| "claude".into());
                self.cmd(json!({"cmd": "spawn", "where": "right", "pane": ap, "type": cmd}));
            }
            "agent_worktree" => self.overlay = Some(Overlay::prompt("Nome da branch da worktree", "", "agent_worktree_go", vec![json!(n(0, 0))])),
            "agent_worktree_go" => self.agent_worktree(n(0, 0) as usize, s(1)),
            "worktree_finish" => self.worktree_finish(),
            "agent_grid" => self.open_grid(),
            "queue_message" => {
                let target = args.first().and_then(|v| v.as_u64()).unwrap_or(ap);
                self.overlay = Some(Overlay::prompt("Recado para o agente (entregue quando ele parar)", "", "queue_send", vec![json!(target)]));
            }
            "queue_send" => match self.call(json!({"cmd": "queue", "pane": n(0, ap as i64), "text": s(1)})) {
                Ok(v) if v["delivered"] == true => self.flash("Recado entregue (o agente estava parado)"),
                Ok(v) => self.flash(&format!("Recado na fila ({}): entrego quando o agente parar", v["queued"])),
                Err(e) => self.flash(&e),
            },
            "fork_agent" => self.fork_agent(),
            "review" => self.open_review(),
            "review_comment" => self.overlay = Some(Overlay::prompt(&format!("Comentario para {}", s(0)), "", "review_add", vec![json!(s(0))])),
            "review_add" => { if !s(1).trim().is_empty() { self.review.push(format!("- {}: {}", s(0), s(1).trim())); } self.open_review(); }
            "review_send" => {
                let text = format!("Revisao do diff atual. Ajuste cada ponto:\n{}", self.review.join("\n"));
                let target = self.agent_in_tab().unwrap_or(ap);
                match self.call(json!({"cmd": "queue", "pane": target, "text": text})) {
                    Ok(_) => { self.flash(&format!("{} comentarios enviados ao agente (pane {target})", self.review.len())); self.review.clear(); }
                    Err(e) => self.flash(&e),
                }
            }
            "review_clear" => { self.review.clear(); self.flash("Comentarios descartados"); }
            "history" => self.open_history(),
            "type_text" => self.send_keys(&s(0)),
            "record" => match self.call(json!({"cmd": "record", "pane": ap})) {
                Ok(v) if v["recording"] == true => self.flash("Gravando o pane (asciinema). Repita para parar."),
                Ok(v) => self.flash(&format!("Gravacao salva: {}  (duck play <arquivo>)", v["path"].as_str().unwrap_or(""))),
                Err(e) => self.flash(&e),
            },
            "popup" => self.cmd(json!({"cmd": "float", "replace": true, "argv": s(0)})),
            "open_file" => self.open_in_editor(&s(0), n(1, 1), n(2, 1)),
            "detach" => std::process::exit(0),
            "kill_server" => { let _ = self.call(json!({"cmd": "kill-server"})); std::process::exit(0); }
            "restart_server" => { let _ = self.call(json!({"cmd": "restart-server"})); self.flash("Server reiniciando; os shells continuam"); }
            "" => {}
            other => if !self.builtin_ui(other, args) { self.flash(&format!("acao desconhecida: {other}")) },
        }
        self.redraw();
    }

    /// Colar: sem caracteres de controle (o server tambem filtra) e com
    /// confirmacao se for varias linhas ou parecer perigoso.
    fn paste(&mut self, text: String, confirmed: bool) {
        if text.is_empty() { return; }
        let multi = text.trim_end().contains('\n');
        let danger = regex::Regex::new(DANGEROUS).map(|r| r.is_match(&text)).unwrap_or(false);
        let bracketed_agent = self.active().is_some_and(|p| p.alt_screen); // TUI (Claude, vim): multilinha e normal
        if self.cfg.paste_confirm && !confirmed && (danger || (multi && !bracketed_agent)) {
            let preview: String = text.lines().take(6).map(|l| l.chars().take(90).collect::<String>()).collect::<Vec<_>>().join(" ⏎ ");
            let why = if danger { "Comando potencialmente perigoso" } else { "Texto com varias linhas" };
            // Cancelar primeiro: Enter distraido nao cola.
            let items = vec![
                Item::new("Cancelar", "Esc", "", vec![]),
                Item::new("Colar mesmo assim", format!("{} linhas", text.lines().count()), "paste_confirmed", vec![json!(text)]),
            ];
            self.overlay = Some(Overlay::list(&format!("{why}: {preview}"), items));
            return;
        }
        for id in self.targets() { self.cmd(json!({"cmd": "paste", "pane": id, "text": text})); }
    }

    /// Panes que recebem o que e digitado: o ativo, ou todos da aba no broadcast.
    fn targets(&self) -> Vec<u64> {
        if self.broadcast.contains(&self.active_tab_key()) {
            self.frame.panes.iter().filter(|p| !p.floating).map(|p| p.id).collect()
        } else {
            vec![self.frame.active_pane]
        }
    }

    fn send_keys(&mut self, text: &str) {
        for id in self.targets() { self.cmd(json!({"cmd": "send", "pane": id, "text": text})); }
    }

    fn workflow_step(&mut self, idx: usize, values: &[Value]) {
        let Some(w) = self.cfg_table.as_ref().and_then(|t| t.get::<Table>("workflows").ok()).and_then(|w| w.get::<Table>(idx + 1).ok()) else { return };
        let cmd: String = w.get("command").unwrap_or_default();
        let enter: bool = w.get::<Option<bool>>("enter").ok().flatten().unwrap_or(true);
        // {{nome}} ou {{nome:padrao}}, na ordem em que aparecem (sem repetir).
        let re = regex::Regex::new(r"\{\{\s*([\w-]+)(?::([^}]*))?\s*\}\}").unwrap();
        let mut params: Vec<(String, String)> = Vec::new();
        for c in re.captures_iter(&cmd) {
            let name = c[1].to_string();
            if !params.iter().any(|p| p.0 == name) { params.push((name, c.get(2).map(|m| m.as_str().to_string()).unwrap_or_default())); }
        }
        if values.len() < params.len() {
            let (name, default) = &params[values.len()];
            let mut args = vec![json!(idx)];
            args.extend(values.iter().cloned());
            self.overlay = Some(Overlay::prompt(&format!("{} ({}/{})", name, values.len() + 1, params.len()), default, "workflow", args));
            return;
        }
        let line = re.replace_all(&cmd, |c: &regex::Captures| {
            let i = params.iter().position(|p| p.0 == c[1]).unwrap_or(0);
            values.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string()
        }).to_string();
        self.send_keys(&format!("{line}{}", if enter { "\r" } else { "" }));
    }

    fn hint_for(&self, action: &str, arg0: Option<&str>) -> String {
        for b in &self.keys {
            if let Action::Builtin(n, a) = &b.action {
                if n == action && (arg0.is_none() || a.first().and_then(|v| v.as_str()) == arg0) {
                    let mut s = String::new();
                    if b.mods.control_key() { s.push_str("Ctrl+"); }
                    if b.mods.alt_key() { s.push_str("Alt+"); }
                    if b.mods.shift_key() { s.push_str("Shift+"); }
                    if b.mods.super_key() { s.push_str("Win+"); }
                    let k = &b.key;
                    s.push_str(&match k.as_str() { " " => "Espaco".into(), k if k.len() == 1 => k.to_uppercase(), k => k.to_string() });
                    return s;
                }
            }
        }
        String::new()
    }

    fn open_palette(&mut self) {
        let b = |label: &str, action: &str, args: Vec<Value>| (label.to_string(), action.to_string(), args);
        let mut defs = vec![
            b("Dividir pane a direita", "split", vec![json!("right")]),
            b("Dividir pane abaixo", "split", vec![json!("down")]),
            b("Fechar pane", "close_pane", vec![]),
            b("Fechar aba", "close_tab", vec![]),
            b("Zoom do pane (alternar)", "zoom", vec![]),
            b("Pane flutuante (abre/esconde; continua rodando)", "float", vec![]),
            b("Nova aba", "new_tab", vec![]),
            b("Renomear aba", "rename_tab", vec![]),
            b("Cor da aba", "tab_color", vec![]),
            b("Mover aba para a direita", "move_tab", vec![json!(1)]),
            b("Mover aba para a esquerda", "move_tab", vec![json!(-1)]),
            b("Mover aba para outro workspace", "move_tab_ws", vec![]),
            b("Redimensionar pane (modo: h/j/k/l, Esc sai)", "key_table", vec![json!("resize")]),
            b("Broadcast: digitar em todos os panes da aba", "broadcast", vec![]),
            b("Trocar de workspace", "workspace_switch", vec![]),
            b("Criar/ir para workspace", "workspace_new", vec![]),
            b("Renomear workspace", "rename_workspace", vec![]),
            b("Agentes: painel (Ctrl+Y aprova, Ctrl+N nega)", "agents", vec![]),
            b("Agentes: sessoes do Claude para retomar", "sessions", vec![]),
            b("Agentes: diff do que mudou no repo (flutuante)", "agent_diff", vec![]),
            b("Comandos: prompt anterior", "jump_prompt", vec![json!("up")]),
            b("Comandos: proximo prompt", "jump_prompt", vec![json!("down")]),
            b("Comandos: copiar a saida do ultimo comando", "copy_last_output", vec![]),
            b("Hints: selecionar URL/caminho/hash pelo teclado", "hints", vec![]),
            b("Pane privado (fora do autosave e dos agentes)", "private", vec![]),
            b("Salvar workspace", "save_state", vec![]),
            b("Restaurar workspace salvo", "restore_state", vec![]),
            b("Apagar estado salvo", "delete_state", vec![]),
            b("Checkpoint: salvar tudo (workspaces + sessoes de agente)", "checkpoint", vec![]),
            b("Checkpoint: restaurar", "checkpoint_restore", vec![]),
            b("Buscar no scrollback", "search", vec![]),
            b("Filtrar o scrollback (regex)", "filter", vec![]),
            b("Modo copia (teclado)", "copy_mode", vec![]),
            b("Copiar selecao", "copy", vec![]),
            b("Colar", "paste", vec![]),
            b("Configuracoes (tema, fonte, opacidade...)", "settings", vec![]),
            b("Fonte maior", "font_size", vec![json!(1)]),
            b("Fonte menor", "font_size", vec![json!(-1)]),
            b("Fonte padrao", "font_size", vec![json!(0)]),
            b("Recarregar config", "reload_config", vec![]),
            b("Desanexar (fecha a janela, shells continuam)", "detach", vec![]),
            b("Reiniciar server (atualiza o Duck; os shells continuam)", "restart_server", vec![]),
            b("Encerrar server (fecha todos os shells)", "kill_server", vec![]),
            b("Inicio: projetos, sessoes e acoes", "home", vec![]),
            b("Modo foco (esconde abas e escurece o resto)", "focus_mode", vec![]),
            b("Barra lateral de abas: recolher/abrir", "toggle_sidebar", vec![]),
            b("Agentes: painel lateral do agente (plano, tokens)", "agent_panel", vec![]),
            b("Caixa de entrada: o que espera você e os avisos que passaram", "inbox", vec![]),
            b("Agentes: linha do tempo da sessao", "agent_timeline", vec![]),
            b("Editor de comando (varias linhas, Ctrl+Z)", "composer", vec![]),
            b("Galeria de temas (previa ao vivo)", "theme_gallery", vec![]),
            b("Tema a partir do papel de parede", "wallpaper_theme", vec![]),
            b("Extensões: ativar, configurar, contas e tokens", "extensions", vec![]),
        ];
        defs.extend([
            b("Agentes: corrigir com agente o ultimo comando que falhou", "fix_with_agent", vec![]),
            b("Agentes: novo agente (perfis; opcao em worktree)", "agent_new", vec![]),
            b("Agentes: Agent Manager em grade", "agent_grid", vec![]),
            b("Agentes: recado na fila (entregue quando o agente parar)", "queue_message", vec![]),
            b("Agentes: bifurcar a sessao do agente num pane novo", "fork_agent", vec![]),
            b("Agentes: revisar o diff e comentar para o agente", "review", vec![]),
            b("Git: finalizar worktree (diff, mesclar, remover)", "worktree_finish", vec![]),
            b("Historico de comandos (todos os panes, WSL incluso)", "history", vec![]),
            b("Gravar sessao do pane (asciinema) / parar", "record", vec![]),
        ]);
        for pp in self.cfg.popups.clone() {
            defs.push((format!("Popup: {}", pp.name), "popup".into(), vec![json!(pp.cmd)]));
        }
        for port in self.frame.ports.clone() {
            defs.push((format!("Abrir http://localhost:{port}"), "open_url".into(), vec![json!(format!("http://localhost:{port}"))]));
        }
        for d in self.cfg.domains.clone() {
            defs.push((format!("Novo pane: {}  (a direita)", d.name), "split".into(), vec![json!("right"), json!(d.name)]));
            defs.push((format!("Nova aba: {}", d.name), "new_tab".into(), vec![json!(d.name)]));
        }
        let mut items: Vec<Item> = defs.into_iter().map(|(label, action, args)| {
            let hint = self.hint_for(&action, args.first().and_then(|v| v.as_str()));
            Item::new(label, hint, &action, args)
        }).collect();
        items.extend(self.palette_extra.iter().cloned());
        self.palette_more(&mut items);
        self.ext_palette(&mut items);
        let mut o = Overlay::list("Paleta: comandos, abas, sessoes, historico e arquivos", items);
        o.palette = true;
        self.overlay = Some(o);
    }

    fn open_settings(&mut self) {
        let mut items = Vec::new();
        let mut themes: Vec<String> = config::THEMES.iter().map(|s| s.to_string()).collect();
        for e in std::fs::read_dir(config::home().join(".duck").join("themes")).into_iter().flatten().flatten() {
            if let Some(stem) = e.path().file_stem() { themes.push(stem.to_string_lossy().to_string()); }
        }
        for t in themes { items.push(Item::new(format!("Tema: {t}"), "", "setting", vec![json!("theme"), json!(format!("{t:?}"))])); }
        for (label, v) in [("Barra de abas embaixo", "bottom"), ("Barra de abas em cima", "top"), ("Barra de abas escondida", "hidden")] {
            items.push(Item::new(label, "", "setting", vec![json!("tab_bar"), json!(format!("{v:?}"))]));
        }
        for (label, v) in [("Fundo: Acrylic", "acrylic"), ("Fundo: Mica", "mica"), ("Fundo: solido", "none")] {
            items.push(Item::new(label, "Windows 11", "setting", vec![json!("backdrop"), json!(format!("{v:?}"))]));
        }
        for o in ["1.0", "0.95", "0.9", "0.85", "0.8"] { items.push(Item::new(format!("Opacidade: {o}"), "", "setting", vec![json!("opacity"), json!(o)])); }
        for sz in ["10", "11", "12", "13", "14", "16"] { items.push(Item::new(format!("Tamanho da fonte: {sz}"), "", "setting", vec![json!("font.size"), json!(sz)])); }
        if let Some(r) = &self.r {
            for f in r.mono_families() { items.push(Item::new(format!("Fonte: {f}"), "", "setting", vec![json!("font.family"), json!(format!("{f:?}"))])); }
        }
        for (label, k, v) in [
            ("OSC 52 (app escreve no clipboard): perguntar", "clipboard_write", "\"ask\""),
            ("OSC 52: permitir sempre", "clipboard_write", "\"allow\""),
            ("OSC 52: negar sempre", "clipboard_write", "\"deny\""),
            ("Confirmar colar perigoso/varias linhas: sim", "paste_confirm", "true"),
            ("Confirmar colar perigoso/varias linhas: nao", "paste_confirm", "false"),
            ("API dos panes: tudo", "api_scope", "\"all\""),
            ("API dos panes: so os panes que o agente criou", "api_scope", "\"children\""),
            ("API dos panes: so leitura", "api_scope", "\"read\""),
            ("FPS maximo: sem limite (vsync)", "max_fps", "0"),
            ("FPS maximo: 60", "max_fps", "60"),
            ("FPS maximo: 30 (economia de bateria)", "max_fps", "30"),
        ] {
            items.push(Item::new(label, "", "setting", vec![json!(k), json!(v)]));
        }
        items.push(Item::new("Imagem de fundo...", "caminho png/jpg", "setting_prompt", vec![json!("background.image"), json!(""), json!("text")]));
        self.settings_more(&mut items);
        items.push(Item::new("Abrir ~/.duck.lua no editor", "", "spawn", vec![json!({"where": "tab", "cmd": ["pwsh", "-NoProfile", "-Command", format!("notepad '{}'", config::config_path().display())]})]));
        self.overlay = Some(Overlay::list("Configuracoes (gravam em ~/.duck-local.lua)", items));
    }

    fn open_workspaces(&mut self) {
        let mut items: Vec<Item> = self.frame.workspaces.iter().map(|w| {
            Item::new(w.clone(), if *w == self.frame.workspace { "atual" } else { "" }, "workspace", vec![json!(w)])
        }).collect();
        // Pastas do zoxide: escolher cria o workspace ja nela (como o
        // smart_workspace_switcher do WezTerm).
        let mut zo = std::process::Command::new("zoxide");
        zo.args(["query", "-l"]);
        // Sem a flag, o Windows cria um console para o zoxide (segundos com o Terminal como padrao).
        #[cfg(windows)]
        { use std::os::windows::process::CommandExt; zo.creation_flags(0x0800_0000); }
        if let Ok(out) = zo.output() {
            for line in String::from_utf8_lossy(&out.stdout).lines().take(200) {
                let name = line.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(line).to_string();
                items.push(Item::new(format!("  {line}"), "pasta", "workspace", vec![json!(name), json!(line)]));
            }
        }
        self.overlay = Some(Overlay::list("Workspaces (Enter troca; pasta cria)", items));
    }

    fn open_agents(&mut self) {
        let Ok(v) = self.call(json!({"cmd": "list"})) else { return };
        let mut items = Vec::new();
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        for w in v["workspaces"].as_array().into_iter().flatten() {
            for (ti, t) in w["tabs"].as_array().into_iter().flatten().enumerate() {
                for p in t["panes"].as_array().into_iter().flatten() {
                    let a = &p["agent"];
                    if a.is_null() { continue; }
                    let id = p["id"].as_u64().unwrap_or(0);
                    let age = now.saturating_sub(a["since"].as_u64().unwrap_or(now));
                    let age = if age < 60 { format!("{age}s") } else if age < 3600 { format!("{}min", age / 60) } else { format!("{}h", age / 3600) };
                    let mark = if p["attention"] == true { "! " } else { "  " };
                    let dir = p["cwd"].as_str().unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("").to_string();
                    let state = a["state"].as_str().unwrap_or("");
                    // Bloqueado: mostra a pergunta (linha da tela com "?").
                    let question = if state == "blocked" {
                        self.call(json!({"cmd": "read", "pane": id, "lines": 20})).ok()
                            .and_then(|r| r["text"].as_str().and_then(|t| t.lines().rev().find(|l| l.contains('?')).map(|l| format!(" — {}", l.trim()))))
                            .unwrap_or_default()
                    } else { String::new() };
                    let ctx = a["context_tokens"].as_u64().map(|t| format!(" · ctx {}k", t / 1000)).unwrap_or_default();
                    let ctx = match (a["todo_done"].as_u64(), a["todo_total"].as_u64()) { (Some(d), Some(t)) => format!(" · plano {d}/{t}{ctx}"), _ => ctx };
                    let mut it = Item::new(
                        format!("{mark}{state:<8} {:<7} {} · aba {} · {dir}{question}", a["name"].as_str().unwrap_or(""), w["name"].as_str().unwrap_or(""), ti + 1),
                        format!("{age}{ctx} · pane {id}"), "focus_pane", vec![json!(id)]);
                    it.pane = Some(id);
                    items.push(it);
                }
            }
        }
        if items.is_empty() { self.flash("Nenhum agente rodando"); return; }
        // Quem precisa de voce primeiro.
        let rank = |s: &str| if s.starts_with('!') { 0 } else if s.contains("blocked") { 1 } else if s.contains("done") { 2 } else if s.contains("working") { 3 } else { 4 };
        items.sort_by_key(|i| rank(&i.label));
        let mut o = Overlay::list("Agentes (Enter foca · Ctrl+Y aprova · Ctrl+N nega)", items);
        o.agents = true;
        self.overlay = Some(o);
    }

    /// Sessoes do Claude Code em todos os perfis (~/.claude*/projects), por
    /// data, para retomar com um Enter (como o session dashboard do Kiro).
    fn open_sessions(&mut self) {
        let items = self.session_items(150);
        if items.is_empty() { self.flash("Nenhuma sessao do Claude Code encontrada"); return; }
        self.overlay = Some(Overlay::list("Sessoes do Claude (Enter retoma numa aba nova)", items));
    }

    fn open_states(&mut self, delete: bool) {
        let Ok(v) = self.call(json!({"cmd": "states"})) else { return };
        let items: Vec<Item> = v["states"].as_array().into_iter().flatten().map(|s| {
            let name = s["name"].as_str().unwrap_or("").to_string();
            Item::new(name.clone(), ago(s["saved_at"].as_u64().unwrap_or(0)), if delete { "delete_state_named" } else { "restore_state_named" }, vec![json!(name)])
        }).collect();
        self.overlay = Some(Overlay::list(if delete { "Apagar estado salvo" } else { "Restaurar workspace salvo" }, items));
    }

    /// Hints: rotulos de 1-2 letras sobre URLs, caminhos, hashes, IPs e
    /// numeros visiveis. Letra minuscula copia; maiuscula abre.
    fn open_hints(&mut self) {
        let res: Vec<regex::Regex> = self.cfg.hints.iter().filter_map(|r| regex::Regex::new(r).ok()).collect();
        let mut hits: Vec<Hint> = Vec::new();
        for p in &self.frame.panes {
            for row in 0..p.h as usize {
                let line = p.row_string(row);
                for re in &res {
                    for m in re.find_iter(&line) {
                        let col = line[..m.start()].chars().count();
                        let len = m.as_str().chars().count();
                        if hits.iter().any(|h| h.pane == p.id && h.row == row && col < h.col + h.len && h.col < col + len) { continue; }
                        hits.push(Hint { label: String::new(), pane: p.id, row, col, len, text: m.as_str().to_string() });
                    }
                }
            }
        }
        if hits.is_empty() { self.flash("Nada para selecionar na tela"); return; }
        let alpha: Vec<char> = "asdfghjklqwertyuiopzxcvbnm".chars().collect();
        let two = hits.len() > alpha.len();
        for (i, h) in hits.iter_mut().enumerate() {
            h.label = if two { format!("{}{}", alpha[i / alpha.len() % alpha.len()], alpha[i % alpha.len()]) } else { alpha[i].to_string() };
        }
        self.hints = Some((hits, String::new()));
    }

    // -------------------------------------------------------------- agentes (rodada 2)

    fn is_wsl(&self, domain: &str) -> bool {
        self.cfg.domains.iter().any(|d| d.name == domain && d.cmd.first().is_some_and(|c| c.to_lowercase().contains("wsl")))
    }

    /// Pane com agente na aba atual (o mais a direita/abaixo que nao seja o ativo).
    fn agent_in_tab(&self) -> Option<u64> {
        self.frame.panes.iter().rev().find(|p| !p.agent.is_empty() && p.id != self.frame.active_pane)
            .or_else(|| self.frame.panes.iter().find(|p| !p.agent.is_empty())).map(|p| p.id)
    }

    /// "Corrigir com agente": manda comando + saida + exit do ultimo que falhou
    /// para um agente da aba (ou abre um ao lado). Entra na fila: se o agente
    /// estiver trabalhando, recebe quando parar.
    fn fix_with_agent(&mut self) {
        let ap = self.frame.active_pane;
        let v = match self.call(json!({"cmd": "last-failure", "pane": ap})) { Ok(v) => v, Err(e) => return self.flash(&e) };
        let out: Vec<&str> = v["output"].as_str().unwrap_or("").lines().collect();
        let tail = out[out.len().saturating_sub(60)..].join("\n");
        let msg = format!("O comando abaixo falhou (exit {}) em {}. Investigue a causa e corrija.\n\n$ {}\n{}",
            v["exit"], v["cwd"].as_str().unwrap_or(""), v["command"].as_str().unwrap_or(""), tail);
        let target = match self.agent_in_tab().filter(|id| *id != ap) {
            Some(id) => id,
            None => {
                let cmd = self.cfg.agent_profiles.first().map(|p| p.cmd.clone()).unwrap_or_else(|| "claude".into());
                match self.call(json!({"cmd": "spawn", "where": "right", "pane": ap, "type": cmd, "focus": false})) {
                    Ok(r) => r["pane"].as_u64().unwrap_or(0),
                    Err(e) => return self.flash(&e),
                }
            }
        };
        match self.call(json!({"cmd": "queue", "pane": target, "text": msg})) {
            Ok(_) => self.flash(&format!("Falha enviada ao agente do pane {target}")),
            Err(e) => self.flash(&e),
        }
    }

    /// Agente numa git worktree propria: branch nova, pasta irma do repo, aba nova.
    fn agent_worktree(&mut self, profile: usize, branch: String) {
        let branch = branch.trim().replace(' ', "-");
        if branch.is_empty() { return; }
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        let Some(top) = git(&cwd, &["rev-parse", "--show-toplevel"]) else { return self.flash("O pane ativo nao esta num repositorio git") };
        let top = std::path::PathBuf::from(top.trim());
        let name = top.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into());
        let dest = top.with_file_name(format!("{name}-{}", branch.replace('/', "-")));
        let dest_s = dest.to_string_lossy().to_string();
        if git(&top.to_string_lossy(), &["worktree", "add", "-b", &branch, &dest_s]).is_none() {
            return self.flash(&format!("git worktree add falhou (a branch {branch} ja existe?)"));
        }
        let cmd = self.cfg.agent_profiles.get(profile).map(|p| p.cmd.clone()).unwrap_or_else(|| "claude".into());
        // Porta propria e preparo (.duck-worktree.json do repo): dois agentes
        // com servidor de desenvolvimento nao brigam pela mesma porta.
        let port = worktree_port(&dest_s);
        let wt = WorktreeCfg::load(&top);
        let copied = wt.copy_into(&top, &dest);
        let line = wt.command(port, &cmd);
        self.cmd(json!({"cmd": "spawn", "where": "tab", "cwd": dest_s, "title": format!("⎇ {branch}"), "type": line}));
        let extra = if copied > 0 { format!(", {copied} arquivo(s) copiado(s)") } else { String::new() };
        self.flash(&format!("Worktree {dest_s} na branch {branch}, porta {port}{extra}; agente subindo"));
    }

    fn worktree_finish(&mut self) {
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        let (Some(dir), Some(common)) = (git(&cwd, &["rev-parse", "--absolute-git-dir"]), git(&cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"])) else {
            return self.flash("O pane ativo nao esta num repositorio git");
        };
        if dir.trim() == common.trim() { return self.flash("Este pane nao esta numa worktree (e o repositorio principal)"); }
        let main = std::path::Path::new(common.trim()).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let branch = git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
        let base = git(&main, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
        let here = git(&cwd, &["rev-parse", "--show-toplevel"]).unwrap_or_default().trim().to_string();
        let ps = |c: String| json!(["pwsh", "-NoLogo", "-NoProfile", "-NoExit", "-Command", c]);
        let wt = WorktreeCfg::load(std::path::Path::new(&main));
        let teardown = wt.teardown.as_ref().map(|t| format!("Push-Location '{here}'; {t}; Pop-Location; ")).unwrap_or_default();
        let port = worktree_ports().get(&here.to_lowercase().replace('\\', "/")).copied();
        let items = vec![
            Item::new(format!("Ver o diff de {branch} contra {base}"), "flutuante", "popup",
                vec![ps(format!("git -C '{main}' -c color.ui=always --no-pager diff --stat {base}...{branch}; git -C '{main}' -c color.ui=always --no-pager diff {base}...{branch}"))]),
            Item::new(format!("Mesclar {branch} em {base} (no repo principal)"), main.clone(), "popup",
                vec![ps(format!("git -C '{here}' status --short; git -C '{main}' merge --no-ff {branch}"))]),
            Item::new(format!("Remover a worktree e a branch {branch}"), if teardown.is_empty() { here.clone() } else { format!("{here} (roda o teardown antes)") }, "popup",
                vec![ps(format!("{teardown}git -C '{main}' worktree remove '{here}'; git -C '{main}' branch -d {branch}"))]),
        ];
        let title = match port { Some(p) => format!("Worktree {branch} (base: {base}, porta {p})"), None => format!("Worktree {branch} (base: {base})") };
        self.overlay = Some(Overlay::list(&title, items));
    }

    /// Caixa de entrada: primeiro o que espera voce agora (agente pedindo
    /// aprovacao), depois os avisos que passaram (extensoes, agentes), mesmo
    /// os que chegaram com a janela fechada. Abrir marca tudo como lido.
    fn open_inbox(&mut self) {
        let Ok(v) = self.call(json!({"cmd": "inbox"})) else { return self.flash("Server sem caixa de entrada (reinicie o server)") };
        let read = v["read"].as_u64().unwrap_or(0);
        let mut items = Vec::new();
        if let Ok(l) = self.call(json!({"cmd": "list"})) {
            for w in l["workspaces"].as_array().into_iter().flatten() {
                for t in w["tabs"].as_array().into_iter().flatten() {
                    for p in t["panes"].as_array().into_iter().flatten() {
                        if p["agent"]["state"] != "blocked" { continue; }
                        let title = p["title"].as_str().unwrap_or("").trim_start_matches(|c: char| !c.is_alphanumeric()).to_string();
                        let tab = t["title"].as_str().filter(|x| !x.is_empty()).unwrap_or("aba");
                        items.push(Item::new(format!("⏸ Esperando você: {}", if title.is_empty() { tab } else { &title }),
                            format!("{} · {tab} · Enter abre o pane", w["name"].as_str().unwrap_or("")), "inbox_open",
                            vec![json!({"kind": "blocked", "pane": p["id"]})]));
                    }
                }
            }
        }
        for it in v["items"].as_array().into_iter().flatten() {
            let unread = it["seq"].as_u64().unwrap_or(0) > read;
            let body = it["body"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            let body: String = body.chars().take(90).collect();
            let from = if it["kind"] == "ext" { self.exts.label(it["ext"].as_str().unwrap_or("")) } else { "Agente".to_string() };
            items.push(Item::new(format!("{}{}", if unread { "● " } else { "" }, it["title"].as_str().unwrap_or("")),
                format!("{from} · {} · {body}", ago(it["at"].as_u64().unwrap_or(0))), "inbox_open", vec![it.clone()]));
        }
        if v["items"].as_array().is_some_and(|a| !a.is_empty()) { items.push(Item::new("Limpar os avisos", "", "inbox_clear", vec![])); }
        if items.is_empty() { return self.flash("Nada esperando você e nenhum aviso"); }
        self.cmd(json!({"cmd": "inbox-read"}));
        self.overlay = Some(Overlay::list("Caixa de entrada", items));
    }

    /// Bifurca a sessao do agente ativo: pane novo com --fork-session.
    fn fork_agent(&mut self) {
        let ap = self.frame.active_pane;
        let Ok(v) = self.call(json!({"cmd": "list"})) else { return };
        let mut found = None;
        for w in v["workspaces"].as_array().into_iter().flatten() {
            for t in w["tabs"].as_array().into_iter().flatten() {
                for p in t["panes"].as_array().into_iter().flatten() {
                    if p["id"].as_u64() == Some(ap) { found = Some(p.clone()); }
                }
            }
        }
        let Some(p) = found else { return };
        let Some(session) = p["agent"]["session"].as_str() else {
            return self.flash("Sem sessao conhecida neste pane (os hooks do Duck estao instalados? duck doctor)");
        };
        let dir = p["agent"]["config_dir"].as_str().unwrap_or("");
        let wsl = self.is_wsl(p["domain"].as_str().unwrap_or(""));
        let line = match (dir.is_empty(), wsl) {
            (true, _) => format!("claude --resume {session} --fork-session"),
            (false, true) => format!("CLAUDE_CONFIG_DIR='{dir}' claude --resume {session} --fork-session"),
            (false, false) => format!("$env:CLAUDE_CONFIG_DIR='{dir}'; claude --resume {session} --fork-session"),
        };
        self.cmd(json!({"cmd": "spawn", "where": "right", "pane": ap, "type": line}));
        self.flash("Sessao bifurcada no pane ao lado");
    }

    /// Revisao: linhas alteradas do diff; Enter comenta; os comentarios vao
    /// juntos para o agente da aba.
    fn open_review(&mut self) {
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        let Some(diff) = git(&cwd, &["--no-pager", "diff", "-U0", "--no-color"]) else { return self.flash("Sem repositorio git neste pane") };
        let mut items = Vec::new();
        if !self.review.is_empty() {
            items.push(Item::new(format!("Enviar {} comentarios ao agente", self.review.len()), "Enter", "review_send", vec![]));
            items.push(Item::new("Descartar comentarios", "", "review_clear", vec![]));
        }
        let (mut file, mut line) = (String::new(), 0i64);
        let hunk = regex::Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)").unwrap();
        for l in diff.lines() {
            if let Some(f) = l.strip_prefix("+++ b/") { file = f.to_string(); continue; }
            if l.starts_with("+++") || l.starts_with("---") { continue; }
            if let Some(c) = hunk.captures(l) { line = c[1].parse().unwrap_or(1); continue; }
            if let Some(added) = l.strip_prefix('+') {
                let at = format!("{file}:{line}");
                items.push(Item::new(format!("+ {}", added.trim()), at.clone(), "review_comment", vec![json!(at)]));
                line += 1;
            } else if let Some(removed) = l.strip_prefix('-') {
                let at = format!("{file}:{line} (removida)");
                items.push(Item::new(format!("- {}", removed.trim()), at.clone(), "review_comment", vec![json!(at)]));
            }
        }
        if items.is_empty() { return self.flash("Nada alterado no repositorio"); }
        self.overlay = Some(Overlay::list(&format!("Revisao: Enter comenta a linha ({} comentarios)", self.review.len()), items));
    }

    /// Historico estilo atuin: desta pasta primeiro, depois o resto.
    fn open_history(&mut self) {
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        let Ok(v) = self.call(json!({"cmd": "history", "limit": 2000})) else { return };
        let mut here = Vec::new();
        let mut other = Vec::new();
        for h in v["history"].as_array().into_iter().flatten() {
            let cmd = h["cmd"].as_str().unwrap_or("").to_string();
            let hc = h["cwd"].as_str().unwrap_or("");
            let ok = if h["exit"] == 0 { "ok" } else { "✗" };
            let dir = hc.rsplit(['/', '\\']).next().unwrap_or("");
            let item = Item::new(cmd.clone(), format!("{ok} · {dir} · {}", ago(h["ts"].as_u64().unwrap_or(0))), "type_text", vec![json!(cmd)]);
            if hc == cwd { here.push(item) } else { other.push(item) }
        }
        here.extend(other);
        if here.is_empty() { return self.flash("Historico vazio (a shell integration grava cada comando)"); }
        self.overlay = Some(Overlay::list("Historico (desta pasta primeiro; Enter digita o comando)", here));
    }

    /// Agent Manager: todos os agentes em grade, com a tela de cada um ao vivo.
    fn open_grid(&mut self) {
        let Ok(v) = self.call(json!({"cmd": "list"})) else { return };
        let mut panes = Vec::new();
        for w in v["workspaces"].as_array().into_iter().flatten() {
            for (ti, t) in w["tabs"].as_array().into_iter().flatten().enumerate() {
                for p in t["panes"].as_array().into_iter().flatten() {
                    let a = &p["agent"];
                    if a.is_null() { continue; }
                    let todo = match (a["todo_done"].as_u64(), a["todo_total"].as_u64()) { (Some(d), Some(t)) => format!(" · {d}/{t}"), _ => String::new() };
                    let head = format!("{} · {} · {} aba {}{}", a["state"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""), w["name"].as_str().unwrap_or(""), ti + 1, todo);
                    panes.push((p["id"].as_u64().unwrap_or(0), head, a["state"].as_str().unwrap_or("").to_string()));
                }
            }
        }
        if panes.is_empty() { return self.flash("Nenhum agente rodando"); }
        self.grid = Some(Grid { panes, sel: 0, texts: HashMap::new(), fetched: Instant::now() - Duration::from_secs(10) });
        self.refresh_grid();
    }

    fn refresh_grid(&mut self) {
        let Some(g) = self.grid.as_ref() else { return };
        if g.fetched.elapsed() < Duration::from_millis(900) { return; }
        let ids: Vec<u64> = g.panes.iter().map(|p| p.0).collect();
        let mut texts = HashMap::new();
        for id in ids {
            if let Ok(v) = self.call(json!({"cmd": "read", "pane": id, "lines": 40})) {
                texts.insert(id, v["text"].as_str().unwrap_or("").to_string());
            }
        }
        if let Some(g) = self.grid.as_mut() { g.texts = texts; g.fetched = Instant::now(); }
    }

    fn grid_key(&mut self, ev: &KeyEvent) {
        let ctrl = self.mods.control_key();
        let k = key_name(ev);
        let Some(g) = self.grid.as_mut() else { return };
        let n = g.panes.len();
        let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
        let id = g.panes[g.sel].0;
        match k.as_str() {
            "escape" => { self.grid = None; }
            "arrowright" | "tab" => g.sel = (g.sel + 1) % n,
            "arrowleft" => g.sel = (g.sel + n - 1) % n,
            "arrowdown" => g.sel = (g.sel + cols).min(n - 1),
            "arrowup" => g.sel = g.sel.saturating_sub(cols),
            "enter" => { self.grid = None; self.cmd(json!({"cmd": "focus", "pane": id})); }
            "y" if ctrl => { self.cmd(json!({"cmd": "send", "pane": id, "text": "1"})); self.flash("Aprovado"); }
            "n" if ctrl => { self.cmd(json!({"cmd": "send", "pane": id, "text": "\x1b"})); self.flash("Negado"); }
            "m" => { self.grid = None; self.builtin("queue_message", &[json!(id)]); }
            _ => {}
        }
        self.redraw();
    }

    /// Print no clipboard -> PNG temporario; devolve o caminho (do jeito que o
    /// pane ativo enxerga: caminho Linux no WSL).
    fn clipboard_image(&self) -> Option<String> {
        let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
        let buf = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())?;
        let dir = std::env::temp_dir().join("duck-imagens");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("print-{}.png", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)));
        buf.save(&path).ok()?;
        let p = path.to_string_lossy().to_string();
        Some(if self.active().is_some_and(|a| self.is_wsl(&a.domain)) { to_wsl(&p) } else if p.contains(' ') { format!("\"{p}\"") } else { p })
    }

    /// Abre arquivo:linha:coluna no editor (padrao: VS Code).
    fn open_in_editor(&mut self, file: &str, line: i64, col: i64) {
        let cmd = self.cfg.editor.replace("{file}", file).replace("{line}", &line.to_string()).replace("{col}", &col.to_string());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("cmd.exe").arg("/C").raw_arg(&cmd).creation_flags(0x0800_0000).spawn();
        }
        #[cfg(not(windows))]
        let _ = std::process::Command::new("sh").args(["-c", &cmd]).spawn();
        self.flash(&format!("Abrindo {file}:{line}"));
    }

    /// arquivo:linha[:coluna] sob o cursor, resolvido contra a pasta do pane.
    fn file_link_at(&self, p: &PaneView, row: usize, col: usize) -> Option<(String, i64, i64)> {
        let line = p.row_string(row);
        let re = regex::Regex::new(r"((?:[A-Za-z]:)?[\w.\-\\/~]+\.[A-Za-z0-9]{1,8})(?:[:(](\d+)(?:[:,](\d+))?\)?)").ok()?;
        let byte = line.char_indices().nth(col).map(|x| x.0)?;
        for c in re.captures_iter(&line) {
            let m = c.get(0)?;
            if byte < m.start() || byte >= m.end() { continue; }
            let raw = c[1].to_string();
            let mut path = std::path::PathBuf::from(&raw);
            if path.is_relative() {
                let base = if p.cwd.starts_with('/') { from_wsl(&p.cwd, &p.domain) } else { p.cwd.clone() };
                path = std::path::Path::new(&base).join(&raw);
            } else if raw.starts_with('/') {
                path = std::path::PathBuf::from(from_wsl(&raw, &p.domain));
            }
            if !path.exists() { continue; }
            return Some((path.to_string_lossy().to_string(), c.get(2).and_then(|x| x.as_str().parse().ok()).unwrap_or(1), c.get(3).and_then(|x| x.as_str().parse().ok()).unwrap_or(1)));
        }
        None
    }

    /// Branch git da pasta (le .git/HEAD; cache de 5s).
    fn branch_of(&mut self, cwd: &str) -> String {
        if cwd.is_empty() { return String::new(); }
        if let Some((t, b)) = self.branches.get(cwd) { if t.elapsed() < Duration::from_secs(5) { return b.clone(); } }
        let b = read_branch(std::path::Path::new(cwd)).unwrap_or_default();
        self.branches.insert(cwd.to_string(), (Instant::now(), b.clone()));
        b
    }

    /// Sugestao inline: o resto do comando mais recente que comeca com o que
    /// ja foi digitado (desta pasta primeiro).
    fn compute_ghost(&mut self) {
        self.ghost = None;
        let Some(p) = self.active() else { return };
        let mode = self.cfg.inline_suggestions.as_str();
        // pwsh ja sugere pelo PSReadLine: no "auto" so bash/zsh/cmd ganham a do Duck.
        let pwsh_pane = p.domain.is_empty() && self.cfg.shell.first().is_some_and(|s| s.to_lowercase().contains("pwsh") || s.to_lowercase().contains("powershell"));
        if mode == "off" || (mode == "auto" && pwsh_pane) { return; }
        let (Some((row, col)), Some((cc, cr, _))) = (p.input, p.cursor.clone()) else { return };
        if p.alt_screen || row < 0 || cr as i64 != row { return; }
        let line = p.row_chars(row as usize);
        let typed: String = line.get(col..(cc as usize).min(line.len())).unwrap_or(&[]).iter().collect();
        // So no fim da linha (nada depois do cursor).
        if line.get(cc as usize..).is_some_and(|rest| rest.iter().any(|c| *c != ' ')) { return; }
        if typed.trim().len() < 2 { return; }
        let cwd = p.cwd.clone();
        if self.hist_cache.0.elapsed() > Duration::from_secs(5) {
            if let Ok(v) = self.call(json!({"cmd": "history", "limit": 3000})) {
                let list = v["history"].as_array().into_iter().flatten()
                    .filter_map(|h| Some((h["cmd"].as_str()?.to_string(), h["cwd"].as_str().unwrap_or("").to_string()))).collect();
                self.hist_cache = (Instant::now(), list);
            }
        }
        let pick = self.hist_cache.1.iter().filter(|(c, _)| c.starts_with(&typed) && c.len() > typed.len())
            .max_by_key(|(_, d)| *d == cwd).map(|(c, _)| c[typed.len()..].to_string());
        self.ghost = pick;
    }

    // -------------------------------------------------------------- selecao

    fn selection_text(&self) -> Option<String> {
        let (pane, a, b) = if let Some(c) = &self.copy { (c.pane, c.anchor?, c.cur) } else {
            let s = self.sel.as_ref()?;
            (s.pane, s.a, s.b)
        };
        let p = self.pane(pane)?;
        let (s, e) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
        let mut out = Vec::new();
        for row in s.row.max(0)..=e.row.min(p.h as i32 - 1) {
            let chars = p.row_chars(row as usize);
            let c0 = if row == s.row { s.col.max(0) as usize } else { 0 };
            let c1 = if row == e.row { (e.col.max(0) as usize + 1).min(chars.len()) } else { chars.len() };
            let line: String = chars.get(c0..c1.max(c0)).unwrap_or(&[]).iter().collect();
            out.push(line.trim_end().to_string());
        }
        Some(out.join("\r\n"))
    }

    fn copy_selection(&mut self) {
        if let Some(t) = self.selection_text().filter(|t| !t.trim().is_empty()) {
            if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(t.clone()); }
            self.ext_clipboard(&t);
            self.flash("Copiado");
        }
    }

    fn open_link_at(&mut self, pane: u64, cell: Cell) -> bool {
        let Some(p) = self.pane(pane) else { return false };
        // OSC 8 primeiro: o texto exibido pode nao ser o destino real.
        if let Some((uri, shown)) = p.link_at(cell.row as usize, cell.col as usize) {
            let plain = shown.trim();
            if uri.contains(plain) || plain.contains(uri.trim_end_matches('/')) {
                open_url(&uri);
            } else {
                let items = vec![
                    Item::new(format!("Abrir {uri}"), "", "open_url", vec![json!(uri)]),
                    Item::new("Cancelar", "Esc", "", vec![]),
                ];
                self.overlay = Some(Overlay::list(&format!("O link diz \"{plain}\" mas leva a:"), items));
            }
            return true;
        }
        if let Some((file, l, c)) = self.file_link_at(p, cell.row as usize, cell.col as usize) {
            if self.cfg.ui.link_preview && !self.mods.shift_key() { self.open_preview(&file, l, c); } else { self.open_in_editor(&file, l, c); }
            return true;
        }
        let Some(p) = self.pane(pane) else { return false };
        let line = p.row_string(cell.row as usize);
        let col_byte = line.char_indices().nth(cell.col as usize).map(|x| x.0).unwrap_or(usize::MAX);
        for (re, fmt) in &self.hyperlinks {
            for caps in re.captures_iter(&line) {
                let m = caps.get(0).unwrap();
                if col_byte >= m.start() && col_byte < m.end() {
                    let mut url = String::new();
                    caps.expand(fmt, &mut url);
                    open_url(&url);
                    return true;
                }
            }
        }
        false
    }

    // -------------------------------------------------------------- teclado

    fn key(&mut self, ev: KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        if self.splash.take().is_some() { return self.redraw(); }
        self.last_key = Instant::now();
        if self.composer.is_some() && self.overlay.is_none() { return self.composer_key(&ev); }
        if self.grid.is_some() && self.overlay.is_none() { return self.grid_key(&ev); }
        if self.hints.is_some() { return self.hints_key(&ev); }
        if self.overlay.is_some() { return self.overlay_key(&ev); }
        if self.copy.is_some() { return self.copy_key(&ev); }

        let name = key_name(&ev);
        let mods = self.mods;
        // Modo (key table): tecla fora da tabela sai do modo.
        if let Some(t) = self.table_stack.last().cloned() {
            let bind = self.tables.get(&t).and_then(|bs| bs.iter().find(|b| b.key == name && b.mods == mods)).cloned();
            match bind {
                Some(b) => self.run(b.action),
                None => { self.table_stack.pop(); }
            }
            self.redraw();
            return;
        }
        // Autocompletar: Alt+setas escolhem, Alt+Enter aceita.
        if mods == ModifiersState::ALT && self.ac.as_ref().is_some_and(|a| !a.items.is_empty()) && self.ac_key(&name) { return; }
        if let Some(b) = self.keys.iter().find(|b| b.key == name && b.mods == mods).cloned() {
            return self.run(b.action);
        }
        if mods.is_empty() && matches!(ev.logical_key, Key::Named(NamedKey::Escape)) && self.ac.is_some() {
            self.ac_dismissed = self.ac.take().map(|a| a.typed).unwrap_or_default();
        }
        // Seta para a direita no fim da linha aceita a sugestao inline.
        if mods.is_empty() && matches!(ev.logical_key, Key::Named(NamedKey::ArrowRight)) {
            if let Some(g) = self.ghost.take() { self.send_keys(&g); return; }
        }
        let app_cursor = self.active().is_some_and(|p| p.app_cursor);
        if let Some(bytes) = encode_key(&ev, mods, app_cursor) {
            self.sel = None;
            self.send_keys(&bytes);
        }
    }

    fn hints_key(&mut self, ev: &KeyEvent) {
        let Some((hits, typed)) = self.hints.as_mut() else { return };
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => { self.hints = None; }
            Key::Named(NamedKey::Backspace) => { typed.pop(); }
            Key::Character(c) => {
                let upper = c.chars().next().is_some_and(|c| c.is_uppercase());
                typed.push_str(&c.to_lowercase());
                let matches: Vec<&Hint> = hits.iter().filter(|h| h.label.starts_with(typed.as_str())).collect();
                if matches.is_empty() { self.hints = None; }
                else if matches.len() == 1 && matches[0].label == *typed {
                    let text = matches[0].text.clone();
                    self.hints = None;
                    if upper {
                        open_url(&text);
                    } else {
                        if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(text.clone()); }
                        self.flash(&format!("Copiado: {text}"));
                    }
                }
            }
            _ => {}
        }
        self.redraw();
    }

    fn overlay_key(&mut self, ev: &KeyEvent) {
        if self.overlay_key_ui(ev) { self.redraw(); return; }
        let ctrl = self.mods.control_key();
        let Some(o) = self.overlay.as_mut() else { return };
        // Painel de agentes: aprova (Ctrl+Y) / nega (Ctrl+N) o pedido do agente
        // selecionado sem trocar de aba. "1" escolhe "Yes" no menu do Claude Code.
        if o.agents && ctrl {
            let pane = o.shown.get(o.sel).and_then(|&i| o.items[i].pane);
            let k = key_name(ev);
            if let (Some(p), "y" | "n") = (pane, k.as_str()) {
                let text = if k == "y" { "1" } else { "\x1b" };
                self.overlay = None;
                self.cmd(json!({"cmd": "send", "pane": p, "text": text}));
                self.flash(if k == "y" { "Aprovado no agente" } else { "Negado no agente" });
                return;
            }
        }
        let Some(o) = self.overlay.as_mut() else { return };
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => {
                if matches!(&o.kind, OverlayKind::Prompt(a, _) if a == "search_run") { self.search = None; self.search_hits = None; }
                if let Some(t) = o.gallery.take() { self.cfg.theme = t; }
                // Pedido de aprovacao fechado com Esc = negado.
                let deny = o.items.iter().find_map(|i| match &i.action { Action::Builtin(n, a) if n == "approve" => a.first().cloned(), _ => None });
                self.overlay = None;
                if let Some(id) = deny { self.builtin("approve", &[id, json!(false)]); }
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => if !o.shown.is_empty() { o.sel = (o.sel + 1) % o.shown.len() },
            Key::Named(NamedKey::ArrowUp) => if !o.shown.is_empty() { o.sel = (o.sel + o.shown.len() - 1) % o.shown.len() },
            Key::Named(NamedKey::PageDown) => if !o.shown.is_empty() { o.sel = (o.sel + 10).min(o.shown.len() - 1) },
            Key::Named(NamedKey::PageUp) => { o.sel = o.sel.saturating_sub(10) }
            Key::Named(NamedKey::Backspace) => { o.input.pop(); o.filter(); }
            Key::Named(NamedKey::Enter) => {
                if self.mods.shift_key() && matches!(&o.kind, OverlayKind::Prompt(a, _) if a == "search_run") {
                    self.builtin("search_prev", &[]);
                    return;
                }
                let o = self.overlay.take().unwrap();
                self.overlay_enter(o);
            }
            _ => {
                if ctrl && key_name(ev) == "v" {
                    let t = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
                    o.input.push_str(t.lines().next().unwrap_or(""));
                    o.filter();
                } else if let Some(t) = &ev.text {
                    // Ctrl/Alt + tecla e atalho, nao texto (AltGr do ABNT2 gera outro caractere e passa).
                    let combo = (ctrl || self.mods.alt_key()) && matches!(&ev.logical_key, Key::Character(c) if c.as_str().eq_ignore_ascii_case(t));
                    if !combo && t.chars().all(|c| !c.is_control()) { o.input.push_str(t); o.filter(); }
                }
            }
        }
        self.redraw();
    }

    /// Modo copia estilo vim: hjkl/setas movem, v marca, y copia, Esc sai,
    /// Ctrl+u/d rolam meia tela.
    fn copy_key(&mut self, ev: &KeyEvent) {
        // Tecla como digitada ($ e Shift+4 no ABNT2): key_name ignora o Shift.
        let name = match &ev.logical_key { Key::Character(c) => c.to_string(), _ => key_name(ev) };
        let ctrl = self.mods.control_key();
        let Some(c) = self.copy.as_mut() else { return };
        let Some(p) = self.frame.panes.iter().find(|p| p.id == c.pane) else { self.copy = None; return };
        let (w, h) = (p.w as i32, p.h as i32);
        let mut scroll = 0i64;
        match name.as_str() {
            "escape" | "q" => { self.copy = None; self.cmd(json!({"cmd": "scroll", "to": "bottom", "pane": self.frame.active_pane})); return; }
            "h" | "arrowleft" => c.cur.col = (c.cur.col - 1).max(0),
            "l" | "arrowright" => c.cur.col = (c.cur.col + 1).min(w - 1),
            "k" | "arrowup" => if c.cur.row > 0 { c.cur.row -= 1 } else { scroll = 1 },
            "j" | "arrowdown" => if c.cur.row < h - 1 { c.cur.row += 1 } else { scroll = -1 },
            "0" | "home" => c.cur.col = 0,
            "$" | "end" => c.cur.col = w - 1,
            "u" if ctrl => scroll = (h / 2) as i64,
            "d" if ctrl => scroll = -(h / 2) as i64,
            "g" => scroll = 1_000_000,
            "v" | " " => c.anchor = if c.anchor.is_some() { None } else { Some(c.cur) },
            "y" | "enter" => {
                self.copy_selection();
                self.copy = None;
                self.cmd(json!({"cmd": "scroll", "to": "bottom", "pane": self.frame.active_pane}));
                return;
            }
            _ => {}
        }
        if scroll != 0 {
            // Rolar desloca a ancora junto, para a selecao seguir o texto.
            if let Some(a) = c.anchor.as_mut() { a.row += scroll as i32; }
            let pane = c.pane;
            self.cmd(json!({"cmd": "scroll", "delta": scroll, "pane": pane}));
        }
        self.redraw();
    }

    // -------------------------------------------------------------- mouse

    // -------------------------------------------------------------- frame

    fn take_frame(&mut self) {
        let (f, events) = {
            let mut l = self.latest.lock().unwrap();
            (l.0.take(), std::mem::take(&mut l.1))
        };
        if let Some(f) = f {
            let f = merge_frame(Some(&self.frame), f);
            if f.clipboard.0 > self.clip_seq {
                if self.clip_seq != 0 && !f.clipboard.1.is_empty() {
                    if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(f.clipboard.1.clone()); }
                }
                self.clip_seq = f.clipboard.0;
            }
            for p in &f.panes { if p.bell && self.cfg.visual_bell { self.bell_until.insert(p.id, Instant::now() + Duration::from_millis(220)); self.tick_in(Duration::from_millis(240)); } }
            let title = f.panes.iter().find(|p| p.id == f.active_pane).map(|p| p.title.clone()).unwrap_or_default();
            if let Some(r) = &self.r {
                r.window.set_title(&if title.is_empty() { format!("Duck · {}", f.workspace) } else { format!("{title} · Duck") });
            }
            // Pedido de aprovacao (OSC 52, acao de agente): pergunta uma vez.
            if self.overlay.is_none() {
                if let Some(a) = f.approvals.iter().find(|a| !self.asked.contains(&a.id)).cloned() {
                    self.asked.insert(a.id);
                    let (yes, title) = if a.kind == "clipboard" { ("Permitir copiar", "Clipboard") } else { ("Permitir", "Agente pede confirmacao") };
                    // Negar primeiro: Enter distraido nao libera nada.
                    let items = vec![
                        Item::new("Negar", "Esc", "approve", vec![json!(a.id), json!(false)]),
                        Item::new(yes, "", "approve", vec![json!(a.id), json!(true)]),
                    ];
                    self.overlay = Some(Overlay::dialog(&format!("{title}: {}", a.text), items));
                    if !self.focused { toast("Duck pede confirmacao", &a.text); }
                }
            }
            if f.resurrect && self.overlay.is_none() && self.shown_notices.insert("resurrect".into()) {
                let items = vec![
                    Item::new("Restaurar a sessao anterior (ultimo checkpoint)", "Enter", "checkpoint_restore", vec![]),
                    Item::new("Agora nao", "Esc", "resurrect_dismiss", vec![]),
                ];
                self.overlay = Some(Overlay::list("Nenhum shell vivo do Duck. Restaurar o que estava aberto?", items));
            }
            if f.recovered > 0 && self.shown_notices.insert("recovered".into()) {
                self.flash(&format!("{} shells recuperados de um server anterior (o server tinha caido)", f.recovered));
            }
            if !f.version.is_empty() && f.version != env!("CARGO_PKG_VERSION") && self.shown_notices.insert("version".into()) {
                self.flash(&format!("Server {} e janela {}: rode 'duck cli kill-server' para atualizar", f.version, env!("CARGO_PKG_VERSION")));
            }
            if !self.frame.workspace.is_empty() && f.workspace != self.frame.workspace {
                let pos = |n: &str| f.workspaces.iter().position(|w| w == n).unwrap_or(0) as f32;
                self.ws_anim = Some((Instant::now(), if pos(&f.workspace) >= pos(&self.frame.workspace) { 1.0 } else { -1.0 }));
            }
            self.frame = f;
            self.compute_ghost();
            self.compute_ac();
            self.ext_context_tick();
        }
        if self.grid.is_some() { self.refresh_grid(); }
        if self.agent_panel.is_some() { self.refresh_agent_panel(false); }
        for ev in events { self.on_event(ev); }
        self.redraw();
    }

    fn tick_in(&self, d: Duration) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || { std::thread::sleep(d); let _ = proxy.send_event(UserEv::Tick); });
    }

    fn on_event(&mut self, ev: Value) {
        let name = ev["event"].as_str().unwrap_or("").to_string();
        let pane = ev["pane"].as_u64().unwrap_or(0);
        let title = ev["title"].as_str().unwrap_or("").trim_start_matches(|c: char| !c.is_alphanumeric()).to_string();
        let what = if title.is_empty() { ev["cwd"].as_str().unwrap_or("").to_string() } else { title };
        if name == "budget" { self.flash(&format!("Orcamento de tokens estourado: {} tokens", ev["tokens"])); }
        if name == "gui-action" {
            // --args '["right"]' (JSON) ou um texto so.
            let args = match &ev["args"] {
                Value::Array(a) => a.clone(),
                Value::String(t) => serde_json::from_str::<Vec<Value>>(t).unwrap_or_else(|_| vec![json!(t)]),
                Value::Null => vec![],
                v => vec![v.clone()],
            };
            if let Some(n) = ev["name"].as_str() { self.builtin(n, &args); }
            return;
        }
        if name == "raise" {
            if pane != 0 { self.cmd(json!({"cmd": "focus", "pane": pane})); }
            if let Some(r) = &self.r { r.window.set_minimized(false); r.window.focus_window(); }
        }
        // Janela em foco: cartao dentro do Duck. Fora de foco: so o toast do Windows.
        if name == "attention" && self.focused && pane != self.frame.active_pane { self.agent_card(pane, ev["state"].as_str().unwrap_or(""), &what); }
        let quiet = self.toasted.get(&pane).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) && name == "attention";
        if self.cfg.notify && !self.focused && !quiet {
            if name == "attention" { self.toasted.insert(pane, Instant::now()); }
            match name.as_str() {
                "attention" => {
                    let state = match ev["state"].as_str().unwrap_or("") { "blocked" => "precisa de voce", "done" => "terminou", _ => "chamou" };
                    let acts: Vec<(String, String)> = if ev["state"] == "blocked" {
                        vec![("Aprovar".into(), format!("duck://approve/{pane}")), ("Negar".into(), format!("duck://deny/{pane}")), ("Abrir".into(), format!("duck://focus/{pane}"))]
                    } else { vec![("Abrir".into(), format!("duck://focus/{pane}"))] };
                    toast_actions(&format!("Agente {state}"), &format!("{what} · {}", self.pane_place(pane)), &acts);
                    if let Some(r) = &self.r { r.window.request_user_attention(Some(UserAttentionType::Informational)); }
                }
                "budget" => toast("Orcamento de tokens", &format!("Os agentes ja gastaram {} tokens nesta rodada", ev["tokens"])),
                "command-finished" => {
                    let secs = ev["duration_ms"].as_u64().unwrap_or(0) / 1000;
                    toast("Comando terminou", &format!("{what} · {secs}s · {}", self.pane_place(pane)));
                }
                _ => {}
            }
        }
        if !self.plugins.is_empty() { self.run_plugins(&ev); }
        // Handlers Lua: on = { ['attention'] = function(ev, win) ... end }
        let handler = self.cfg_table.as_ref().and_then(|t| t.get::<Table>("on").ok()).and_then(|on| on.get::<Function>(name.as_str()).ok());
        if let Some(f) = handler {
            let win = self.win_table();
            let lev = to_lua(&self.lua, &ev).unwrap_or(LuaValue::Nil);
            if let Err(e) = f.call::<()>((lev, win)) { self.flash(&format!("lua on.{name}: {e}")); }
            let pending: Vec<_> = std::mem::take(&mut *self.queue.lock().unwrap());
            for (n, args) in pending { self.builtin(&n, &args); }
        }
    }

    // -------------------------------------------------------------- desenho

    fn status_segments(&self) -> Vec<(String, Rgb)> {
        let st = self.stats.lock().unwrap();
        let a = &self.frame.agents;
        let g = |k: &str| a.get(k).copied().unwrap_or(0);
        let mode = self.table_stack.last().cloned()
            .or_else(|| self.copy.as_ref().map(|_| "copia".into()))
            .or_else(|| self.hints.as_ref().map(|_| "hints".into()))
            .or_else(|| self.broadcast.contains(&self.active_tab_key()).then(|| "broadcast".into()))
            .or_else(|| self.active().filter(|p| p.recording).map(|_| "gravando".into()))
            .or_else(|| self.active().filter(|p| p.private).map(|_| "privado".into()));
        let info = json!({
            "workspace": self.frame.workspace, "cpu": st.cpu.round(), "ram": (st.ram * 10.0).round() / 10.0,
            "ram_total": st.ram_total.round(), "time": clock(),
            "agents": {"working": g("working"), "blocked": g("blocked"), "done": g("done"), "idle": g("idle")},
            "title": self.active().map(|p| p.title.clone()).unwrap_or_default(),
            "cwd": self.active().map(|p| p.cwd.clone()).unwrap_or_default(),
            "mode": mode,
            "ports": self.frame.ports, "tokens": self.frame.tokens,
            "branch": self.active().map(|p| read_branch(std::path::Path::new(&p.cwd)).unwrap_or_default()).unwrap_or_default(),
        });
        drop(st);
        let theme = self.theme();
        let f = self.cfg_table.as_ref().and_then(|t| t.get::<Function>("status").ok());
        let Some(f) = f else { return vec![(format!(" {} ", self.frame.workspace), theme.accent)] };
        let arg = to_lua(&self.lua, &info).unwrap_or(LuaValue::Nil);
        match f.call::<LuaValue>(arg).and_then(|v| self.lua.from_value::<Value>(v)) {
            Ok(Value::Array(segs)) => segs.into_iter().map(|s| match s {
                Value::String(t) => (t, theme.fg),
                s => (s["text"].as_str().unwrap_or("").to_string(), theme.named(s["fg"].as_str().unwrap_or("fg"))),
            }).collect(),
            Ok(Value::String(t)) => vec![(t, theme.fg)],
            Ok(_) => vec![],
            Err(e) => vec![(format!(" status: {e} "), theme.blocked)],
        }
    }

    /// "Aba X" (e o workspace, se for outro) do pane: os avisos dizem onde, nao "pane 10".
    pub(super) fn pane_place(&self, pane: u64) -> String {
        let Ok(r) = self.call(json!({"cmd": "list"})) else { return format!("pane {pane}") };
        for w in r["workspaces"].as_array().into_iter().flatten() {
            for t in w["tabs"].as_array().into_iter().flatten() {
                let ps = t["panes"].as_array().cloned().unwrap_or_default();
                let Some(p) = ps.iter().find(|p| p["id"] == pane) else { continue };
                let s = |v: &Value| v.as_str().unwrap_or("").trim().to_string();
                let mut name = s(&t["title"]);
                if name.is_empty() { name = s(&p["title"]).trim_start_matches(|c: char| !c.is_alphanumeric()).to_string(); }
                if name.is_empty() { name = s(&p["cwd"]).rsplit(['/', '\\']).find(|x| !x.is_empty()).unwrap_or("").to_string(); }
                let name: String = name.chars().take(40).collect();
                let ws = s(&w["name"]);
                return if ws != self.frame.workspace { format!("aba {name} ({ws})") } else { format!("aba {name}") };
            }
        }
        format!("pane {pane}")
    }

    fn tab_label(&self, t: &TabInfo, branch: &str) -> String {
        let f = self.cfg_table.as_ref().and_then(|c| c.get::<Function>("tab_title").ok());
        let title = f.and_then(|f| {
            let arg = to_lua(&self.lua, &json!({
                "index": t.index, "title": t.title, "pane_title": t.pane_title, "cwd": t.cwd,
                "state": t.state, "attention": t.attention, "active": t.active, "domain": t.domain,
                "branch": branch, "todo": t.todo.map(|(d, n)| format!("{d}/{n}")),
            })).ok()?;
            f.call::<String>(arg).ok()
        }).unwrap_or_else(|| if t.title.is_empty() { t.pane_title.clone() } else { t.title.clone() });
        title.chars().take(60).collect::<String>().trim().to_string()
    }

    /// Cor da aba: manual > perfil do dominio > regra da config.
    fn tab_color(&self, t: &TabInfo) -> Option<Rgb> {
        let theme = self.theme();
        if !t.color.is_empty() { return Some(theme.named(&t.color)); }
        if let Some(c) = self.cfg.domains.iter().find(|d| d.name == t.domain).and_then(|d| d.tab_color) { return Some(c); }
        let hay = format!("{} {} {} {}", t.title, t.pane_title, t.cwd, t.domain);
        self.cfg.tab_rules.iter().find(|(re, _)| regex::Regex::new(re).is_ok_and(|r| r.is_match(&hay))).map(|x| x.1)
    }
}

/// Aplica as linhas alteradas (`rows`) sobre o frame anterior do mesmo pane.
fn merge_frame(prev: Option<&Frame>, mut f: Frame) -> Frame {
    // Sem base ainda (a janela ja consumiu o anterior): deixa `rows` pendente
    // para a janela aplicar sobre a tela que ela tem.
    let Some(pf) = prev else { return f };
    for p in &mut f.panes {
        if p.lines.is_some() { continue; }
        let old = pf.panes.iter().find(|q| q.id == p.id && q.w == p.w && q.h == p.h);
        // Anterior tambem so com diferencas: soma as duas (a nova vence).
        if let Some(o) = old.filter(|o| o.lines.is_none()) {
            let mut rows = o.rows.clone().unwrap_or_default();
            rows.extend(p.rows.take().unwrap_or_default());
            p.rows = Some(rows);
            continue;
        }
        let mut lines = old.map(|q| q.lines().to_vec()).unwrap_or_default();
        lines.resize(p.h as usize, Vec::new());
        for (k, runs) in p.rows.take().unwrap_or_default() {
            if let Ok(i) = k.parse::<usize>() { if i < lines.len() { lines[i] = runs; } }
        }
        p.lines = Some(lines);
    }
    f
}

/// `.duck-worktree.json` na raiz do repositorio:
/// `{"copy": [".env"], "setup": "npm ci", "env": {"PORT": "{port}"}, "teardown": "docker compose down"}`.
#[derive(Default, serde::Deserialize)]
struct WorktreeCfg {
    #[serde(default)] copy: Vec<String>,
    setup: Option<String>,
    #[serde(default)] env: std::collections::BTreeMap<String, String>,
    teardown: Option<String>,
}

impl WorktreeCfg {
    fn load(repo: &std::path::Path) -> WorktreeCfg {
        let Ok(t) = std::fs::read_to_string(repo.join(".duck-worktree.json")) else { return WorktreeCfg::default() };
        serde_json::from_str(&t).unwrap_or_else(|e| { crate::proto::log(&format!(".duck-worktree.json invalido: {e}")); WorktreeCfg::default() })
    }

    /// Copia os arquivos fora do git (.env etc.) do repo principal; devolve quantos.
    fn copy_into(&self, from: &std::path::Path, to: &std::path::Path) -> usize {
        let mut n = 0;
        for rel in &self.copy {
            if rel.contains("..") || std::path::Path::new(rel).is_absolute() { continue; }
            let (src, dst) = (from.join(rel), to.join(rel));
            if !src.is_file() || dst.exists() { continue; }
            if let Some(d) = dst.parent() { let _ = std::fs::create_dir_all(d); }
            if std::fs::copy(&src, &dst).is_ok() { n += 1; }
        }
        n
    }

    /// Linha digitada no pwsh do pane novo: ambiente, preparo e o agente.
    fn command(&self, port: u16, agent: &str) -> String {
        let q = |v: &str| v.replace("{port}", &port.to_string()).replace('\'', "''");
        let mut s = format!("$env:DUCK_PORT='{port}'; ");
        for (k, v) in &self.env {
            if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { s += &format!("$env:{k}='{}'; ", q(v)); }
        }
        if let Some(setup) = self.setup.as_deref().filter(|x| !x.trim().is_empty()) { s += &format!("{}; ", setup.replace("{port}", &port.to_string())); }
        s + agent
    }
}

fn worktree_ports_file() -> std::path::PathBuf { crate::proto::data_dir().join("worktree-ports.json") }

fn worktree_ports() -> std::collections::BTreeMap<String, u16> {
    std::fs::read_to_string(worktree_ports_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Porta da worktree: a que ja tem, ou a primeira livre a partir de 4100 (de 10 em 10).
fn worktree_port(dir: &str) -> u16 {
    let key = dir.to_lowercase().replace('\\', "/");
    let mut ports = worktree_ports();
    // Worktree que nao existe mais devolve a porta.
    ports.retain(|d, _| std::path::Path::new(d).exists() || *d == key);
    if let Some(p) = ports.get(&key) { return *p; }
    let used: Vec<u16> = ports.values().copied().collect();
    let port = (0..400u16).map(|i| 4100 + i * 10)
        .find(|p| !used.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or(4100);
    ports.insert(key, port);
    let _ = std::fs::write(worktree_ports_file(), serde_json::to_string_pretty(&ports).unwrap_or_default());
    port
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new("git");
    c.arg("-C").arg(cwd).args(args);
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let out = c.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn read_branch(dir: &std::path::Path) -> Option<String> {
    let mut d = Some(dir);
    while let Some(cur) = d {
        let g = cur.join(".git");
        let head = if g.is_dir() { g.join("HEAD") } else if g.is_file() {
            // worktree: ".git" e um arquivo "gitdir: <caminho>"
            let t = std::fs::read_to_string(&g).ok()?;
            std::path::PathBuf::from(t.trim().strip_prefix("gitdir:")?.trim()).join("HEAD")
        } else { d = cur.parent(); continue };
        let h = std::fs::read_to_string(head).ok()?;
        return Some(h.trim().strip_prefix("ref: refs/heads/").map(String::from).unwrap_or_else(|| h.trim().chars().take(7).collect()));
    }
    None
}

/// C:\x\y -> /mnt/c/x/y (caminho visto de dentro do WSL).
fn to_wsl(p: &str) -> String {
    let p = p.replace('\\', "/");
    match p.split_once(":/") { Some((d, rest)) if d.len() == 1 => format!("/mnt/{}/{rest}", d.to_lowercase()), _ => p }
}

/// /mnt/c/x -> C:/x; outro caminho Linux -> \\wsl.localhost\<distro>\...
fn from_wsl(p: &str, distro: &str) -> String {
    if let Some(rest) = p.strip_prefix("/mnt/") {
        if let Some((d, tail)) = rest.split_once('/') { if d.len() == 1 { return format!("{}:/{tail}", d.to_uppercase()); } }
    }
    if p.starts_with('/') && !distro.is_empty() { return format!(r"\\wsl.localhost\{distro}{}", p.replace('/', "\\")); }
    p.to_string()
}

fn fmt_ms(ms: u64) -> String {
    if ms < 60_000 { format!("{:.1}s", ms as f64 / 1000.0) } else { format!("{}min{}s", ms / 60_000, (ms / 1000) % 60) }
}

fn read_head(path: &str, max: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; max];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).to_string()
}

/// Grava `chave = valor` (Lua) num bloco gerenciado do ~/.duck-local.lua; o
/// hot reload aplica. Chave com ponto vira campo aninhado (font.size).
fn set_local_setting(key: &str, value: &str) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')) {
        return Err(format!("chave invalida: {key}"));
    }
    // Nome com hifen (claude-usage) nao e identificador Lua: vira ["claude-usage"].
    let lua_path = |ps: &[&str]| -> String {
        ps.iter().fold("config".to_string(), |acc, p| {
            let ident = p.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && p.chars().all(|c| c.is_alphanumeric() || c == '_');
            if ident { format!("{acc}.{p}") } else { format!("{acc}[\"{p}\"]") }
        })
    };
    let path = config::local_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let (begin, end) = ("-- duck:configuracoes (gerado pela tela de configuracoes)", "-- duck:fim");
    let (head, mut block, tail) = match (text.find(begin), text.find(end)) {
        (Some(a), Some(b)) if b > a => (text[..a].to_string(), text[a + begin.len()..b].to_string(), text[b + end.len()..].to_string()),
        _ => (text.clone(), String::new(), String::new()),
    };
    let full = lua_path(&parts);
    let line_key = format!("{full} =");
    block = block.lines().filter(|l| !l.trim_start().starts_with(&line_key) && !l.trim().is_empty()).map(|l| format!("{l}\n")).collect();
    // Garante as tabelas pai, em todos os niveis (config.extensions.jira...).
    // As guardas ficam no topo, a mais curta primeiro: o pai vem antes do filho.
    let is_guard = |l: &str| l.split_once(" = ").is_some_and(|(a, b)| b == format!("{a} or {{}}"));
    let mut guards: Vec<String> = block.lines().filter(|l| is_guard(l)).map(str::to_string).collect();
    let rest: String = block.lines().filter(|l| !is_guard(l)).map(|l| format!("{l}\n")).collect();
    for k in 1..parts.len() {
        let parent = lua_path(&parts[..k]);
        let guard = format!("{parent} = {parent} or {{}}");
        if !guards.contains(&guard) { guards.push(guard); }
    }
    guards.sort_by_key(|g| g.len());
    block = guards.iter().map(|g| format!("{g}\n")).collect::<String>() + &rest;
    block.push_str(&format!("{full} = {value}\n"));
    let out = format!("{}{begin}\n{block}{end}{}", head.trim_end().to_string() + if head.trim().is_empty() { "" } else { "\n\n" }, tail);
    std::fs::write(&path, out).map_err(|e| e.to_string())
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb { config::mix_rgb(a, b, t) }

/// JSON -> Lua com null virando nil (o padrao do mlua vira userdata e
/// quebra `if info.mode then`).
fn to_lua(lua: &Lua, v: &impl serde::Serialize) -> mlua::Result<LuaValue> {
    lua.to_value_with(v, mlua::serde::SerializeOptions::new().serialize_none_to_null(false).serialize_unit_to_null(false))
}

fn clock() -> String {
    #[cfg(windows)]
    {
        #[repr(C)]
        #[derive(Default)]
        struct St { y: u16, mo: u16, dw: u16, d: u16, h: u16, mi: u16, s: u16, ms: u16 }
        extern "system" { fn GetLocalTime(st: *mut St); }
        let mut st = St::default();
        unsafe { GetLocalTime(&mut st) };
        format!("{:02}:{:02}", st.h, st.mi)
    }
    #[cfg(not(windows))]
    {
        let s = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!("{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60)
    }
}

fn ago(t: u64) -> String {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(t);
    if d < 60 { format!("{d}s atras") } else if d < 3600 { format!("{}min atras", d / 60) }
    else if d < 86400 { format!("{}h atras", d / 3600) } else { format!("{}d atras", d / 86400) }
}

fn with_conn<T>(conn: &Arc<Mutex<Option<Conn>>>, f: impl Fn(&mut Conn) -> Result<T, String>) -> Result<T, String> {
    let mut g = conn.lock().unwrap();
    if let Some(c) = g.as_mut() {
        match f(c) {
            Err(e) if e.contains("server fechou") || e.contains("pipe") || e.contains("os error") => {}
            r => return r,
        }
    }
    // Server reiniciou: reconecta uma vez.
    let mut c = Conn::connect_or_spawn().map_err(|e| e.to_string())?;
    let r = f(&mut c);
    *g = Some(c);
    r
}

fn open_url(url: &str) {
    // ShellExecute direto: o explorer.exe com URL longa (aspas, |, =) desiste e abre Documentos.
    #[cfg(windows)]
    unsafe {
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (op, u) = (w("open"), w(url));
        windows_sys::Win32::UI::Shell::ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), u.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
    }
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Toast nativo do Windows via WinRT no powershell 5.1 (o pwsh 7 nao projeta
/// WinRT). AppId do proprio PowerShell: dispensa registrar um AUMID.
pub fn toast(title: &str, msg: &str) { toast_actions(title, msg, &[]) }

/// Toast com botoes: cada botao abre uma URL duck:// (duck url ...).
pub fn toast_actions(title: &str, msg: &str, actions: &[(String, String)]) { toast_card(title, msg, actions, None) }

/// Aviso no canto da tela com o tema do Duck (cor da barra lateral opcional).
/// Em tela cheia, ou sem o duckw, cai no toast nativo do Windows.
pub fn toast_card(title: &str, msg: &str, actions: &[(String, String)], color: Option<&str>) {
    let note = json!({"title": title, "body": msg, "color": color, "actions": actions.iter().map(|(l, u)| json!([l, u])).collect::<Vec<_>>()});
    if crate::toast::show(&note) { return; }
    native_toast(title, msg, actions)
}

fn native_toast(title: &str, msg: &str, actions: &[(String, String)]) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let esc = |s: &str| crate::server::clean(s, 300).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "''");
        let acts: String = actions.iter().map(|(l, u)| format!("<action content=\"{}\" activationType=\"protocol\" arguments=\"{}\"/>", esc(l), esc(u))).collect();
        let acts = if acts.is_empty() { String::new() } else { format!("<actions>{acts}</actions>") };
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null;\
             $x = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]::new();\
             $x.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>');\
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($x))",
            esc(title), esc(msg), acts);
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    { let _ = actions; let _ = std::process::Command::new("notify-send").args([title, msg]).spawn(); }
}

/// Acrylic/Mica do Windows 11 e barra de titulo escura.
fn apply_backdrop(window: &Window, kind: &str) {
    #[cfg(not(windows))]
    let _ = (window, kind);
    #[cfg(windows)]
    {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
        let Ok(h) = window.window_handle() else { return };
        let RawWindowHandle::Win32(h) = h.as_raw() else { return };
        let hwnd = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
        let dark: i32 = 1;
        let v: i32 = match kind.to_lowercase().as_str() { "mica" => 2, "acrylic" => 3, "tabbed" => 4, _ => 1 };
        unsafe {
            DwmSetWindowAttribute(hwnd, 20, &dark as *const _ as _, 4);
            DwmSetWindowAttribute(hwnd, 38, &v as *const _ as _, 4);
            let round: i32 = 2; // DWMWCP_ROUND
            DwmSetWindowAttribute(hwnd, 33, &round as *const _ as _, 4);
        }
    }
}

/// Tecla global do quick terminal ("CTRL+'", "ALT+F12"...): thread com fila de
/// mensagens propria; cada WM_HOTKEY alterna a janela.
fn register_hotkey(spec: &str, proxy: EventLoopProxy<UserEv>) {
    // ponytail: tecla global so no Windows; Linux/macOS usam o atalho do sistema para `duck --quick`.
    #[cfg(not(windows))]
    let _ = (spec, proxy);
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, VkKeyScanW, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN};
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};
        let spec = spec.to_uppercase();
        let mut mods = MOD_NOREPEAT;
        let mut key = String::new();
        for p in spec.split('+') {
            match p.trim() { "CTRL" => mods |= MOD_CONTROL, "ALT" => mods |= MOD_ALT, "SHIFT" => mods |= MOD_SHIFT, "WIN" | "SUPER" => mods |= MOD_WIN, k => key = k.to_string() }
        }
        let vk: u32 = if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) { 0x6F + n }
            else if key == "SPACE" { 0x20 }
            else if key == "`" || key == "'" || key.chars().count() == 1 { (unsafe { VkKeyScanW(key.chars().next().unwrap_or('\'') as u16) } & 0xff) as u32 }
            else { 0 };
        std::thread::spawn(move || unsafe {
            if RegisterHotKey(std::ptr::null_mut(), 1, mods, vk) == 0 {
                crate::proto::log(&format!("quick terminal: tecla {spec} ja esta em uso"));
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_HOTKEY && proxy.send_event(UserEv::Quick).is_err() { return; }
            }
        });
    }
}

/// Tecla -> bytes para o shell.
fn encode_key(ev: &KeyEvent, mods: ModifiersState, app_cursor: bool) -> Option<String> {
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();
    // Modificador xterm para setas/F-keys: 1 + shift + 2*alt + 4*ctrl.
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |c: &str| if m > 1 { format!("\x1b[1;{m}{c}") } else if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") };
    let tilde = |n: u8| if m > 1 { format!("\x1b[{n};{m}~") } else { format!("\x1b[{n}~") };
    let ss3 = |c: &str| if m > 1 { format!("\x1b[1;{m}{c}") } else { format!("\x1bO{c}") };
    let s = match &ev.logical_key {
        Key::Named(n) => match n {
            NamedKey::Enter => if shift || alt { "\x1b\r".into() } else { "\r".into() },
            NamedKey::Backspace => if ctrl { "\x17".into() } else if alt { "\x1b\x7f".into() } else { "\x7f".into() },
            NamedKey::Tab => if shift { "\x1b[Z".into() } else { "\t".into() },
            NamedKey::Escape => "\x1b".into(),
            NamedKey::Space => if ctrl { "\0".into() } else if alt { "\x1b ".into() } else { " ".into() },
            NamedKey::ArrowUp => csi("A"),
            NamedKey::ArrowDown => csi("B"),
            NamedKey::ArrowRight => csi("C"),
            NamedKey::ArrowLeft => csi("D"),
            NamedKey::Home => csi("H"),
            NamedKey::End => csi("F"),
            NamedKey::Insert => tilde(2),
            NamedKey::Delete => tilde(3),
            NamedKey::PageUp => tilde(5),
            NamedKey::PageDown => tilde(6),
            NamedKey::F1 => ss3("P"),
            NamedKey::F2 => ss3("Q"),
            NamedKey::F3 => ss3("R"),
            NamedKey::F4 => ss3("S"),
            NamedKey::F5 => tilde(15),
            NamedKey::F6 => tilde(17),
            NamedKey::F7 => tilde(18),
            NamedKey::F8 => tilde(19),
            NamedKey::F9 => tilde(20),
            NamedKey::F10 => tilde(21),
            NamedKey::F11 => tilde(23),
            NamedKey::F12 => tilde(24),
            _ => return None,
        },
        Key::Character(c) => {
            let t = ev.text.as_ref().map(|t| t.to_string()).unwrap_or_else(|| c.to_string());
            // AltGr (teclado ABNT2) chega como Ctrl+Alt com texto ja composto.
            if ctrl && alt && t.chars().all(|c| !c.is_control()) && !t.is_empty() && t != c.as_str().to_lowercase() { return Some(t); }
            if ctrl {
                let ch = c.chars().next()?.to_ascii_lowercase();
                let b = match ch {
                    'a'..='z' => ch as u8 - b'a' + 1,
                    '[' | '3' => 0x1b, '\\' | '4' => 0x1c, ']' | '5' => 0x1d, '6' => 0x1e, '/' | '-' | '7' => 0x1f,
                    '2' | '@' => 0,
                    _ => return Some(t),
                };
                let s = (b as char).to_string();
                return Some(if alt { format!("\x1b{s}") } else { s });
            }
            if alt { format!("\x1b{t}") } else { t }
        }
        _ => ev.text.as_ref()?.to_string(),
    };
    Some(s)
}

impl Gui {
    /// Lembra posicao/tamanho/maximizada para a proxima abertura.
    fn save_geometry(&self) {
        let Some(r) = &self.r else { return };
        if self.quick || r.window.is_minimized() == Some(true) { return; }
        let max = r.window.is_maximized();
        let mut g: Value = std::fs::read(crate::proto::data_dir().join("window.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
        g["max"] = json!(max);
        if !max {
            let s = r.window.inner_size();
            g["w"] = json!(s.width);
            g["h"] = json!(s.height);
            if let Ok(p) = r.window.outer_position() { g["x"] = json!(p.x); g["y"] = json!(p.y); }
        }
        let _ = std::fs::write(crate::proto::data_dir().join("window.json"), g.to_string());
    }

    fn toggle_quick(&mut self) {
        let Some(r) = &self.r else { return };
        self.quick_visible = !self.quick_visible;
        r.window.set_visible(self.quick_visible);
        if self.quick_visible { r.window.focus_window(); }
    }

    fn set_light(&mut self, light: bool) {
        if self.cfg.theme_light.is_none() || self.light == light { return; }
        self.light = light;
        self.cmd(json!({"cmd": "theme-mode", "mode": if light { "light" } else { "dark" }}));
        self.redraw();
    }

    fn rebuild_renderer(&mut self) {
        let Some(old) = self.r.take() else { return };
        let window = old.window.clone();
        drop(old);
        let transparent = self.cfg.opacity < 1.0 || self.cfg.backdrop != "none";
        self.r = Some(Renderer::new(window, &self.cfg.font_family, &self.cfg.font_dirs, self.cfg.font_size + self.font_delta,
            self.cfg.line_height, transparent, &self.cfg.gpu_backend));
        self.load_config();
        self.flash("GPU reiniciada: janela recriada");
    }
}

impl ApplicationHandler<UserEv> for Gui {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.r.is_some() { return; }
        let transparent = self.cfg.opacity < 1.0 || self.cfg.backdrop != "none";
        let mut attrs = Window::default_attributes()
            .with_title("Duck")
            .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0))
            .with_transparent(transparent)
            .with_decorations(self.cfg.ui.titlebar != "custom");
        // Janela translucida (Mica/opacidade): sem a superficie de redesenho do
        // GDI, que ficava atras do conteudo no tamanho antigo apos maximizar.
        #[cfg(windows)]
        if transparent {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_no_redirection_bitmap(true);
        }
        if self.quick {
            // Quick terminal: faixa no topo do monitor principal, sem borda, por cima de tudo.
            let mon = el.primary_monitor().or_else(|| el.available_monitors().next());
            let (pos, size) = mon.map(|m| (m.position(), m.size())).unwrap_or((PhysicalPosition::new(0, 0), PhysicalSize::new(1600, 900)));
            attrs = attrs.with_decorations(false).with_window_level(WindowLevel::AlwaysOnTop).with_visible(false)
                .with_position(pos)
                .with_inner_size(PhysicalSize::new(size.width, (size.height as f32 * self.cfg.quick_height) as u32));
            register_hotkey(&self.cfg.quick_key, self.proxy.clone());
        }
        // Posicao e tamanho da ultima vez (fora o quick terminal).
        let geo: Option<Value> = (!self.quick).then(|| std::fs::read(crate::proto::data_dir().join("window.json")).ok()
            .and_then(|b| serde_json::from_slice(&b).ok())).flatten();
        if let Some(g) = &geo {
            if let (Some(w), Some(h)) = (g["w"].as_u64(), g["h"].as_u64()) { attrs = attrs.with_inner_size(PhysicalSize::new(w.max(400) as u32, h.max(300) as u32)); }
            if let (Some(x), Some(y)) = (g["x"].as_i64(), g["y"].as_i64()) { attrs = attrs.with_position(PhysicalPosition::new(x as i32, y as i32)); }
            if g["max"] == true { attrs = attrs.with_maximized(true); }
        }
        // Icone do Duck na janela e na barra de tarefas.
        if let Ok(img) = image::load_from_memory(include_bytes!("../assets/duck.png")) {
            let rgba = img.to_rgba8();
            let (iw, ih) = rgba.dimensions();
            if let Ok(icon) = winit::window::Icon::from_rgba(rgba.into_raw(), iw, ih) { attrs = attrs.with_window_icon(Some(icon)); }
        }
        let window = Arc::new(el.create_window(attrs).expect("janela"));
        if !self.quick { self.splash = Some(Instant::now()); }
        apply_backdrop(&window, &self.cfg.backdrop);
        self.light = self.cfg.theme_light.is_some() && window.theme() == Some(winit::window::Theme::Light);
        let r = Renderer::new(window.clone(), &self.cfg.font_family, &self.cfg.font_dirs, self.cfg.font_size, self.cfg.line_height, transparent, &self.cfg.gpu_backend);
        self.r = Some(r);
        self.load_config();
        ui::register_protocol();
        // As extensoes rodam no server; a janela recebe o estado delas ao assinar.
        self.exts.conn = self.conn.clone();
        self.connect();
        self.send_size();
        self.cmd(json!({"cmd": "window-focus", "focused": true}));
        if self.light { self.cmd(json!({"cmd": "theme-mode", "mode": "light"})); }

        // Estatisticas para a barra (CPU/RAM) e relogio: um tick a cada 2s.
        let stats = self.stats.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let Ok(mut sys) = sysinfo::System::new() else { return };
            const GB: f64 = 1073741824.0;
            loop {
                // Uso e frequencia (refresh_cpu_usage deixa a frequencia em 0).
                sys.refresh_cpu_all();
                sys.refresh_memory();
                let want = stats.lock().unwrap().want_procs;
                // Processos: so com o painel aberto (a varredura custa ~20 ms).
                let top = if want {
                    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true,
                        sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory());
                    let n = sys.cpus().len().max(1) as f32;
                    let mut by: HashMap<String, (f32, f64, usize)> = HashMap::new();
                    for p in sys.processes().values() {
                        let name = p.name().to_string_lossy().trim_end_matches(".exe").to_string();
                        let e = by.entry(name).or_default();
                        e.0 += p.cpu_usage() / n;
                        e.1 += p.memory() as f64 / 1048576.0;
                        e.2 += 1;
                    }
                    let mut v: Vec<(String, f32, f64, usize)> = by.into_iter().map(|(k, (c, m, q))| (k, c, m, q)).collect();
                    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)));
                    v.truncate(10);
                    Some(v)
                } else { None };
                {
                    let mut s = stats.lock().unwrap();
                    s.cpu = sys.global_cpu_usage();
                    s.ram = sys.used_memory() as f64 / GB;
                    s.ram_total = sys.total_memory() as f64 / GB;
                    s.avail = sys.available_memory() as f64 / GB;
                    s.swap = sys.used_swap() as f64 / GB;
                    s.swap_total = sys.total_swap() as f64 / GB;
                    s.cores = sys.cpus().iter().map(|c| c.usage()).collect();
                    if let Some(c) = sys.cpus().first() { s.brand = c.brand().trim().to_string(); s.freq_mhz = c.frequency(); }
                    s.uptime = sysinfo::System::uptime().unwrap_or(0);
                    let (cpu, ram) = (s.cpu, (s.ram / s.ram_total.max(0.001) * 100.0) as f32);
                    s.cpu_hist.push_back(cpu);
                    s.ram_hist.push_back(ram);
                    while s.cpu_hist.len() > 60 { s.cpu_hist.pop_front(); }
                    while s.ram_hist.len() > 60 { s.ram_hist.pop_front(); }
                    if let Some(t) = top { s.top = t; }
                }
                if proxy.send_event(UserEv::Tick).is_err() { return; }
                std::thread::sleep(Duration::from_secs(if want { 1 } else { 2 }));
            }
        });
        // Recarrega a config ao salvar o arquivo.
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let mtime = || [config::config_path(), config::local_path()].iter()
                .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
            let mut last = mtime();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = mtime();
                if now != last { last = now; if proxy.send_event(UserEv::Reload).is_err() { return; } }
            }
        });
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.redraw_at = None;
            self.redraw();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Desenho adiado pelo limite de FPS: acorda na hora certa (sem polling).
        match self.redraw_at {
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, ev: UserEv) {
        let t0 = Instant::now();
        let what = format!("{ev:?}");
        self.user_event_inner(ev);
        let ms = t0.elapsed().as_millis();
        if ms > 250 { gui_log(&format!("{ms} ms em evento {what} (workspace {})", self.frame.workspace)); }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        // Diagnostico: o que travar a janela (>250 ms) vai para gui.log.
        let t0 = Instant::now();
        let what = match &ev { WindowEvent::KeyboardInput { event, .. } => format!("tecla {:?} {:?}", event.logical_key, self.mods), e => format!("{e:?}").chars().take(60).collect() };
        self.window_event_inner(el, id, ev);
        let ms = t0.elapsed().as_millis();
        if ms > 250 {
            let ov = self.overlay.as_ref().map(|o| o.title.clone()).unwrap_or_default();
            gui_log(&format!("{ms} ms em {what} (workspace {}, overlay '{ov}')", self.frame.workspace));
        }
    }
}

impl Gui {
    fn user_event_inner(&mut self, ev: UserEv) {
        match ev {
            UserEv::Frame => self.take_frame(),
            UserEv::Reload => {
                self.load_config();
                self.cmd(json!({"cmd": "reload"}));
                if self.cfg_error.is_none() { self.flash("Config recarregada"); }
                self.send_size();
            }
            UserEv::Tick => {
                if self.grid.is_some() { self.refresh_grid(); }
                if self.agent_panel.is_some() { self.refresh_agent_panel(false); }
                self.redraw()
            }
            UserEv::Quick => self.toggle_quick(),
            UserEv::Connected => { self.send_size(); self.cmd(json!({"cmd": "window-focus", "focused": self.focused})); }
            UserEv::Ext => self.ext_poll(),
        }
    }

}

/// Linha em %LOCALAPPDATA%\duck\gui.log (diagnostico de lentidao).
fn gui_log(msg: &str) {
    use std::io::Write;
    let path = crate::proto::data_dir().join("gui.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024) { let _ = std::fs::remove_file(&path); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{secs} {} {msg}", clock());
    }
}

impl Gui {
    fn window_event_inner(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => if self.quick { self.toggle_quick() } else { el.exit() },
            WindowEvent::Resized(s) => {
                if let Some(r) = self.r.as_mut() {
                    r.resize(s.width, s.height);
                    if s.width == 0 || s.height == 0 { r.hibernate(); }
                }
                self.send_size();
                self.save_geometry();
            }
            WindowEvent::Moved(_) => self.save_geometry(),
            WindowEvent::CursorLeft { .. } => { self.tab_hover = None; self.resize_edge = None; self.redraw(); }
            WindowEvent::ScaleFactorChanged { .. } => self.load_config(),
            WindowEvent::ThemeChanged(t) => self.set_light(t == winit::window::Theme::Light),
            WindowEvent::Focused(f) => {
                self.focused = f;
                self.cmd(json!({"cmd": "window-focus", "focused": f}));
                // Quick terminal some ao perder o foco, como o do Ghostty.
                if self.quick && !f && self.quick_visible { self.toggle_quick(); }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => self.key(event),
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => self.mouse_move(x as f32, y as f32),
            WindowEvent::MouseInput { state, button, .. } => self.mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / self.r.as_ref().map(|r| r.cell_h).unwrap_or(16.0),
                };
                self.wheel(lines);
            }
            WindowEvent::DroppedFile(path) => {
                // Arrastar arquivo cola o caminho (entre aspas se tiver espaco).
                let p = path.to_string_lossy().to_string();
                let p = if p.contains(' ') { format!("\"{p}\"") } else { p };
                self.send_keys(&p);
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                if self.r.as_ref().is_some_and(|r| r.lost.load(Ordering::SeqCst)) { self.rebuild_renderer(); }
            }
            _ => {}
        }
    }
}

pub fn run(startup: Vec<Value>, quick: bool) {
    // Queda da janela fica registrada no gui.log (a janela nao tem console).
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        gui_log(&format!("PANIC: {info} | {}", std::backtrace::Backtrace::force_capture().to_string().lines().filter(|l| l.contains("duck")).take(8).collect::<Vec<_>>().join(" / ")));
        prev(info);
    }));
    let el = EventLoop::<UserEv>::with_user_event().build().expect("event loop");
    let mut app = Gui::new(el.create_proxy(), startup, quick);
    el.run_app(&mut app).expect("event loop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ignora_acento() {
        assert!(fuzzy("extensões", "Extensoes: ativar").is_some());
        assert!(fuzzy("extensoes", "Extensões: ativar").is_some());
    }

    #[test]
    fn fuzzy_ranks() {
        assert!(fuzzy("dpd", "Dividir pane a direita").is_some());
        assert!(fuzzy("xyz", "Dividir pane").is_none());
        assert!(fuzzy("sal", "Salvar workspace").unwrap() > fuzzy("sal", "Restaurar estado salvo").unwrap());
    }

    #[test]
    fn dangerous_paste() {
        let re = regex::Regex::new(DANGEROUS).unwrap();
        for bad in ["curl https://x.sh | sh", "sudo apt install", "rm -rf /", "iwr http://a | iex", "Remove-Item C:\\x -Recurse -Force"] {
            assert!(re.is_match(bad), "{bad}");
        }
        for ok in ["git status", "ls -la", "echo curl", "npm run build"] {
            assert!(!re.is_match(ok), "{ok}");
        }
    }

    #[test]
    fn merge_applies_changed_rows() {
        let prev = Frame { panes: vec![PaneView { id: 1, w: 5, h: 2, lines: Some(vec![vec![Run(0, "a".into(), -1, -2, 0, None)], vec![]]), ..Default::default() }], ..Default::default() };
        let mut rows = HashMap::new();
        rows.insert("1".to_string(), vec![Run(0, "b".into(), -1, -2, 0, None)]);
        let next = Frame { panes: vec![PaneView { id: 1, w: 5, h: 2, lines: None, rows: Some(rows), ..Default::default() }], ..Default::default() };
        let m = merge_frame(Some(&prev), next);
        assert_eq!(m.panes[0].lines()[0][0].1, "a");
        assert_eq!(m.panes[0].lines()[1][0].1, "b");
    }

    #[test]
    fn merge_without_base_keeps_rows_pending() {
        let mut r1 = HashMap::new();
        r1.insert("0".to_string(), vec![Run(0, "x".into(), -1, -2, 0, None)]);
        let mut r2 = HashMap::new();
        r2.insert("1".to_string(), vec![Run(0, "y".into(), -1, -2, 0, None)]);
        let d1 = Frame { panes: vec![PaneView { id: 1, w: 5, h: 2, rows: Some(r1), ..Default::default() }], ..Default::default() };
        let d1 = merge_frame(None, d1);
        assert!(d1.panes[0].lines.is_none());
        let d2 = Frame { panes: vec![PaneView { id: 1, w: 5, h: 2, rows: Some(r2), ..Default::default() }], ..Default::default() };
        let both = merge_frame(Some(&d1), d2);
        assert_eq!(both.panes[0].rows.as_ref().unwrap().len(), 2);
        let base = Frame { panes: vec![PaneView { id: 1, w: 5, h: 2, lines: Some(vec![vec![Run(0, "a".into(), -1, -2, 0, None)], vec![Run(0, "b".into(), -1, -2, 0, None)]]), ..Default::default() }], ..Default::default() };
        let full = merge_frame(Some(&base), both);
        assert_eq!(full.panes[0].lines()[0][0].1, "x");
        assert_eq!(full.panes[0].lines()[1][0].1, "y");
    }

    #[test]
    fn wsl_paths() {
        assert_eq!(to_wsl(r"C:\Users\x\a.png"), "/mnt/c/Users/x/a.png");
        assert_eq!(from_wsl("/mnt/d/Tools/x.rs", "Ubuntu"), "D:/Tools/x.rs");
        assert_eq!(from_wsl("/home/user/a.py", "Ubuntu"), r"\\wsl.localhost\Ubuntu\home\user\a.py");
    }

    #[test]
    fn run_with_and_without_link() {
        let a: Run = serde_json::from_str(r#"[3, "x", -1, -2, 0]"#).unwrap();
        let b: Run = serde_json::from_str(r#"[3, "x", -1, -2, 0, "https://a"]"#).unwrap();
        assert_eq!(a.5, None);
        assert_eq!(b.5.as_deref(), Some("https://a"));
    }
}
