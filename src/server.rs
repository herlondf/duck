//! `duck server`: o mux. Os shells nao moram aqui: cada pane tem um
//! `duck ptyhost` (src/ptyhost.rs) dono do ConPTY. Se este processo cair,
//! os shells continuam; o proximo server reconecta e refaz abas e splits a
//! partir de live.json.
//!
//! Estado unico em `Mux` atras de um Mutex. Threads:
//!   - accept: uma por conexao (request/response em JSON por linha);
//!   - por pane: leitora (host -> parser VT) e escritora (fila -> host);
//!   - eventos do terminal (titulo, bell, clipboard, respostas a queries);
//!   - agentes (a cada 2s), autosave/checkpoint, live.json, flush do
//!     synchronized output.
//! Clientes com `subscribe` recebem frames: a 1a vez a tela inteira, depois
//! so as linhas que mudaram.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alacritty_terminal::event::{Event as TE, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color as AC, CursorShape, NamedColor, Rgb as ARgb};
use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{self, Config};
use crate::proto;
use crate::ptyhost;

pub type Id = u64;

fn now_secs() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

// ------------------------------------------------------------------ layout

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Node {
    Leaf(Id),
    /// vertical = lado a lado (split a direita); senao um em cima do outro.
    Split { vertical: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub fn layout(&self, r: Rect, out: &mut Vec<(Id, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, r)),
            Node::Split { vertical, ratio, a, b } => {
                let (ra, rb) = split_rect(r, *vertical, *ratio);
                a.layout(ra, out);
                b.layout(rb, out);
            }
        }
    }
    pub fn leaves(&self) -> Vec<Id> {
        fn walk(n: &Node, v: &mut Vec<Id>) {
            match n { Node::Leaf(id) => v.push(*id), Node::Split { a, b, .. } => { walk(a, v); walk(b, v); } }
        }
        let mut v = Vec::new();
        walk(self, &mut v);
        v
    }
    pub fn contains(&self, id: Id) -> bool { self.leaves().contains(&id) }
    pub fn replace(&mut self, old: Id, new: Id) -> bool {
        match self {
            Node::Leaf(id) if *id == old => { *id = new; true }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.replace(old, new) || b.replace(old, new),
        }
    }
    pub fn split_leaf(&mut self, target: Id, new: Id, vertical: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split { vertical, ratio: 0.5, a: Box::new(Node::Leaf(target)), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split_leaf(target, new, vertical) || b.split_leaf(target, new, vertical),
        }
    }
    /// Remove a folha; o irmao sobe no lugar do split. None = arvore vazia.
    pub fn remove(self, target: Id) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == target => None,
            Node::Leaf(_) => Some(self),
            Node::Split { vertical, ratio, a, b } => match (a.remove(target), b.remove(target)) {
                (Some(a), Some(b)) => Some(Node::Split { vertical, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            },
        }
    }
    /// Move o divisor mais proximo (da orientacao pedida) que contem o pane.
    pub fn adjust(&mut self, r: Rect, target: Id, vertical: bool, delta: i32) -> bool {
        let Node::Split { vertical: v, ratio, a, b } = self else { return false };
        if !a.contains(target) && !b.contains(target) { return false; }
        let (ra, rb) = split_rect(r, *v, *ratio);
        if a.adjust(ra, target, vertical, delta) || b.adjust(rb, target, vertical, delta) { return true; }
        if *v != vertical { return false; }
        let size = if *v { r.w } else { r.h } as f32;
        if size < 3.0 { return false; }
        *ratio = ((*ratio * size + delta as f32) / size).clamp(0.05, 0.95);
        true
    }
}

pub fn split_rect(r: Rect, vertical: bool, ratio: f32) -> (Rect, Rect) {
    if vertical {
        let wa = (((r.w.saturating_sub(1)) as f32 * ratio).round() as u16).clamp(1, r.w.saturating_sub(2).max(1));
        (Rect { w: wa, ..r }, Rect { x: r.x + wa + 1, w: r.w.saturating_sub(wa + 1).max(1), ..r })
    } else {
        let ha = (((r.h.saturating_sub(1)) as f32 * ratio).round() as u16).clamp(1, r.h.saturating_sub(2).max(1));
        (Rect { h: ha, ..r }, Rect { y: r.y + ha + 1, h: r.h.saturating_sub(ha + 1).max(1), ..r })
    }
}

// ------------------------------------------------------------------ estado

#[derive(Clone)]
struct Listener { pane: Id, tx: Sender<(Id, TE)> }
impl EventListener for Listener {
    fn send_event(&self, e: TE) { let _ = self.tx.send((self.pane, e)); }
}

struct Dims { cols: usize, lines: usize }
impl Dimensions for Dims {
    fn total_lines(&self) -> usize { self.lines }
    fn screen_lines(&self) -> usize { self.lines }
    fn columns(&self) -> usize { self.cols }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub state: String,
    pub since: u64,
    /// "hook" (o agente avisou) ou "auto" (heuristica). Hook manda.
    pub source: String,
    pub session: Option<String>,
    pub config_dir: Option<String>,
    /// Tokens de contexto da ultima resposta (o hook le do transcript).
    #[serde(default)]
    pub context_tokens: Option<u64>,
    /// Tokens gastos na sessao inteira (orcamento).
    #[serde(default)]
    pub session_tokens: Option<u64>,
    /// Progresso do plano do agente (TaskCreate/TaskUpdate ou TodoWrite).
    #[serde(default)]
    pub todo_done: Option<u32>,
    #[serde(default)]
    pub todo_total: Option<u32>,
    #[serde(skip)]
    pub hook_at: Option<Instant>,
}

/// Bloco de comando (shell integration, OSC 133). Linhas em coordenada
/// absoluta: historico + linha da tela no momento da marca.
/// ponytail: com o historico cheio as linhas antigas saem e as marcas velhas
/// deslizam; vale enquanto o scrollback nao enche.
#[derive(Clone, Debug, Default)]
struct Mark {
    prompt: i64,
    /// Onde comeca o que o usuario digita (133;B): linha absoluta e coluna.
    input: Option<(i64, usize)>,
    command: String,
    out_start: Option<i64>,
    out_end: Option<i64>,
    exit: Option<i32>,
    started: Option<Instant>,
    dur_ms: Option<u64>,
}

enum Io { Data(Vec<u8>), Resize(u16, u16), Kill }

struct Pane {
    term: Term<Listener>,
    parser: ansi::Processor,
    io: Sender<Io>,
    /// pid do ptyhost: raiz da arvore de processos do pane.
    pid: Option<u32>,
    title: String,
    cwd: String,
    domain: String,
    cmd: Vec<String>,
    agent: Option<Agent>,
    attention: bool,
    last_output: Instant,
    cols: u16,
    rows: u16,
    /// Pane que criou este pela API (escopo "children").
    parent: Option<Id>,
    /// Token do pane (DUCK_TOKEN): identifica quem chama a API.
    token: String,
    /// Modo privado: texto fora do autosave e da leitura por outros panes.
    private: bool,
    marks: Vec<Mark>,
    /// Portas locais vistas na saida (localhost:3000): viram link e vao para a barra.
    ports: std::collections::BTreeSet<u16>,
    /// Recados para o agente: entregues quando ele parar (done/idle).
    queue: Vec<String>,
    /// Gravacao asciinema (.cast v2) em andamento.
    rec: Option<(std::fs::File, Instant, String)>,
    /// O ConPTY repassa os OSC na hora mas repinta o texto depois: a posicao
    /// das marcas de prompt (A/B) e do fim do comando (D) so vale quando a
    /// saida sossega. Ate la ficam pendentes.
    marks_pending: bool,
    /// Resize: o ConPTY refaz a quebra de linhas e repinta; as marcas antigas
    /// precisam ser reancoradas pelo texto do comando.
    reanchor: bool,
    /// Ao reconectar, o replay da saida antiga re-dispara queries do app;
    /// as respostas nao podem ir para o shell como se fossem digitadas.
    mute_replies_until: Instant,
    bell_at: Option<Instant>,
    /// OSC 9;4: (estado 1=normal 2=erro 3=indeterminado 4=pausa, %).
    progress: Option<(u8, u8)>,
}

impl Pane {
    /// Depois de um resize: acha cada comando no texto (de baixo para cima) e
    /// recalcula prompt, inicio e fim da saida. Bloco cujo comando nao aparece
    /// mais (saiu do historico) perde a posicao.
    fn reanchor_marks(&mut self) {
        let h = self.term.history_size() as i64;
        let top = self.term.topmost_line().0;
        let cols = self.cols.max(1) as usize;
        let mut from = self.term.bottommost_line().0;
        let n = self.marks.len();
        for i in (0..n).rev() {
            let cmd = self.marks[i].command.clone();
            if cmd.is_empty() { continue; } // prompt atual: resolvido pelo cursor
            let needle: String = cmd.chars().take(16).collect();
            let mut found = None;
            let mut l = from;
            while l >= top {
                if line_text(&self.term, l).contains(&needle) { found = Some(l); break; }
                l -= 1;
            }
            let k = &mut self.marks[i];
            match found {
                Some(l) => {
                    let line = line_text(&self.term, l);
                    let col = line.find(&needle).map(|b| line[..b].chars().count()).unwrap_or(0);
                    let abs = h + l as i64;
                    let rows = (col + cmd.chars().count()).div_ceil(cols).max(1) as i64;
                    k.prompt = abs;
                    k.input = Some((abs, col));
                    if k.out_start.is_some() { k.out_start = Some(abs + rows); }
                    from = l - 1;
                }
                None => { k.prompt = i64::MIN / 2; k.input = None; k.out_start = None; k.out_end = None; }
            }
        }
        // Cada bloco termina onde o proximo comeca.
        for i in 0..n.saturating_sub(1) {
            let next = self.marks[i + 1].prompt;
            if self.marks[i].out_end.is_some() && next > i64::MIN / 2 { self.marks[i].out_end = Some(next); }
        }
    }

    fn write(&self, b: &[u8]) { let _ = self.io.send(Io::Data(b.to_vec())); }
    /// Fixa as marcas pendentes na posicao atual do cursor (fim do prompt):
    /// prompt e inicio do texto digitado na linha do cursor; o comando
    /// anterior termina antes dela.
    fn resolve_marks(&mut self) {
        if !self.marks_pending || self.last_output.elapsed() < Duration::from_millis(40) { return; }
        self.marks_pending = false;
        if std::mem::take(&mut self.reanchor) { self.reanchor_marks(); }
        let abs = self.abs_cursor();
        let col = self.term.grid().cursor.point.column.0;
        let n = self.marks.len();
        if let Some(k) = self.marks.last_mut() {
            if k.out_start.is_none() {
                k.prompt = abs;
                if k.input.is_some() { k.input = Some((abs, col)); }
            }
        }
        if n >= 2 {
            let prev = &mut self.marks[n - 2];
            if prev.out_end.is_some() { prev.out_end = Some(abs.max(prev.out_start.unwrap_or(abs))); }
        }
    }
    fn abs_cursor(&self) -> i64 { self.term.history_size() as i64 + self.term.grid().cursor.point.line.0 as i64 }
    fn history_lines(&self, scrollback: usize, mb: f64) -> usize {
        // Teto em bytes alem de linhas: ~32 bytes por celula.
        let by_mem = (mb * 1_048_576.0 / (self.cols.max(1) as f64 * 32.0)) as usize;
        scrollback.min(by_mem.max(1000))
    }
}

struct Tab {
    id: Id,
    title: String,
    root: Node,
    active: Id,
    zoomed: bool,
    floating: Option<Id>,
    float_visible: bool,
    float_prev: Id,
    color: String,
}

impl Tab {
    fn new(id: Id, title: &str) -> Tab {
        Tab { id, title: title.into(), root: Node::Leaf(id), active: id, zoomed: false, floating: None, float_visible: false, float_prev: id, color: String::new() }
    }
    fn has(&self, id: Id) -> bool { self.root.contains(id) || self.floating == Some(id) }
    fn all(&self) -> Vec<Id> { let mut v = self.root.leaves(); v.extend(self.floating); v }
}

struct Workspace { name: String, tabs: Vec<Tab>, active: usize }

struct ClientView { workspace: String, focused: bool }

#[derive(Clone, Serialize)]
struct Approval {
    id: u64,
    kind: String,
    text: String,
    pane: Id,
    #[serde(skip)]
    payload: String,
    #[serde(skip)]
    decided: Option<bool>,
}

pub struct Mux {
    panes: HashMap<Id, Pane>,
    workspaces: Vec<Workspace>,
    next: Id,
    size: (u16, u16),
    clients: HashMap<String, ClientView>,
    cfg: Config,
    clipboard: (u64, String),
    events: Vec<(u64, Value)>,
    event_seq: u64,
    approvals: Vec<Approval>,
    approval_seq: u64,
    secrets: Vec<regex::Regex>,
    /// Shells recuperados de um server anterior.
    recovered: usize,
    /// Nada vivo no start, mas ha checkpoint: a GUI oferece restaurar.
    resurrect: bool,
    light: bool,
    /// Historico de comandos de todos os panes (estilo atuin), em memoria e
    /// em history.jsonl.
    history: Vec<Value>,
    budget_warned: bool,
    /// Extensoes: rodam aqui, com ou sem janela aberta.
    ext: crate::ext::Host,
    /// Janelas assinando a tela agora.
    subs: usize,
}

pub struct Server {
    mux: Mutex<Mux>,
    gen: Mutex<u64>,
    cv: Condvar,
    tx: Sender<(Id, TE)>,
    sync: (Mutex<Option<Instant>>, Condvar),
}

impl Server {
    fn lock(&self) -> MutexGuard<'_, Mux> { self.mux.lock().unwrap_or_else(|e| e.into_inner()) }
    fn notify(&self) {
        *self.gen.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_all();
    }
}

impl Mux {
    fn theme(&self) -> &config::Theme {
        if self.light { self.cfg.theme_light.as_ref().unwrap_or(&self.cfg.theme) } else { &self.cfg.theme }
    }
}

// ------------------------------------------------------------------ cores

const FG: i64 = -1;
const BG: i64 = -2;
fn pal(i: u8) -> i64 { -10 - i as i64 }

fn idx256(i: u8) -> i64 {
    if i < 16 { return pal(i); }
    let (r, g, b) = if i < 232 {
        let i = i - 16;
        let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        (c(i / 36), c((i / 6) % 6), c(i % 6))
    } else {
        let v = 8 + (i - 232) * 10;
        (v, v, v)
    };
    ((r as i64) << 16) | ((g as i64) << 8) | b as i64
}

fn enc(c: AC) -> i64 {
    match c {
        AC::Spec(ARgb { r, g, b }) => ((r as i64) << 16) | ((g as i64) << 8) | b as i64,
        AC::Indexed(i) => idx256(i),
        AC::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => FG,
            NamedColor::Background => BG,
            NamedColor::Cursor => FG,
            n if (n as usize) < 16 => pal(n as u8),
            n => pal((n as usize - NamedColor::DimBlack as usize) as u8), // Dim* -> base
        },
    }
}

/// Texto vindo do app (titulo, notificacao) sem caracteres de controle: um
/// titulo com ESC viraria sequencia de escape no toast, no painel, no log.
pub fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() && !('\u{80}'..='\u{9f}').contains(c)).take(max).collect()
}

// ------------------------------------------------------------------ panes

pub struct SpawnOpts {
    pub cwd: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub domain: Option<String>,
    pub text: Option<String>,
    pub parent: Option<Id>,
}

impl SpawnOpts {
    fn shell() -> SpawnOpts { SpawnOpts { cwd: None, cmd: None, domain: None, text: None, parent: None } }
}

/// Scripts de shell integration ficam no perfil do usuario (o pwsh e o WSL
/// leem de la).
fn install_shell_scripts() {
    let d = proto::data_dir().join("shell");
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join("duck.ps1"), include_str!("shell/duck.ps1"));
    let _ = std::fs::write(d.join("duck.sh"), include_str!("shell/duck.sh").replace("\r\n", "\n"));
}

fn parse_argv(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(x) => Some(x.iter().filter_map(|s| s.as_str().map(String::from)).collect()),
        Value::String(s) if !s.is_empty() => Some(split_cmd(s)),
        _ => None,
    }
}

fn spawn_pane(srv: &Arc<Server>, m: &mut Mux, o: SpawnOpts) -> Result<Id, String> {
    let id = m.next;
    m.next += 1;
    let (cols, rows) = (m.size.0.max(2), m.size.1.max(2));

    let domain = o.domain.clone().unwrap_or_default();
    let mut argv: Vec<String> = match m.cfg.domains.iter().find(|d| d.name == domain) {
        Some(d) => d.cmd.clone(),
        None if !domain.is_empty() => return Err(format!("dominio desconhecido: {domain}")),
        None => Vec::new(),
    };
    // WSL: o cwd salvo e um caminho Linux; troca o `--cd ~` do dominio por ele.
    if let (Some(cwd), Some(i)) = (&o.cwd, argv.iter().position(|a| a == "--cd")) {
        if cwd.starts_with('/') && i + 1 < argv.len() { argv[i + 1] = cwd.clone(); }
    }
    let shell_only = o.cmd.as_ref().is_none_or(|c| c.is_empty());
    match &o.cmd {
        Some(c) if !c.is_empty() => argv.extend(c.iter().cloned()),
        _ if argv.is_empty() => argv = m.cfg.shell.clone(),
        _ => {}
    }
    // Shell integration automatica no pwsh (o -Command roda depois do profile,
    // entao embrulha o prompt do oh-my-posh).
    let exe0 = argv[0].to_lowercase();
    if m.cfg.shell_integration && shell_only && (exe0.contains("pwsh") || exe0.contains("powershell"))
        && !argv.iter().any(|a| ["-command", "-file", "-c", "-f"].contains(&a.to_lowercase().as_str())) {
        let script = proto::data_dir().join("shell").join("duck.ps1");
        argv.extend(["-NoExit".into(), "-Command".into(), format!(". '{}'", script.display())]);
    }
    let cwd = o.cwd.clone().filter(|c| !c.starts_with('/') && std::path::Path::new(c).exists())
        .or_else(|| m.cfg.cwd.clone())
        .unwrap_or_else(|| config::home().to_string_lossy().to_string());
    let token = proto::random_hex(16);
    // DUCK_BIN = o de console (CLI para agentes); o host roda no sem console.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.to_string_lossy().to_string();
    let cli_exe = std::env::current_exe().ok().map(|p| p.with_file_name(if cfg!(windows) { "duck.exe" } else { "duck" }))
        .filter(|p| p.is_file()).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| exe.clone());
    let wslenv = std::env::var("WSLENV").unwrap_or_default();
    let env = vec![
        ("DUCK_PANE".to_string(), id.to_string()),
        ("DUCK_TOKEN".into(), token.clone()),
        ("DUCK_BIN".into(), cli_exe),
        ("TERM".into(), "xterm-256color".into()),
        ("TERM_PROGRAM".into(), "Duck".into()),
        ("COLORTERM".into(), "truecolor".into()),
        // WSL so recebe as variaveis listadas no WSLENV.
        ("WSLENV".into(), format!("{}{}DUCK_PANE:DUCK_TOKEN:TERM_PROGRAM", wslenv, if wslenv.is_empty() { "" } else { ":" })),
    ];
    let user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
    let pipe = format!("duck-pty-{user}-{}", proto::random_hex(10));
    let spec = ptyhost::Spec { id, argv: argv.clone(), cwd: cwd.clone(), cols, rows, pipe: pipe.clone() };
    let spec_path = ptyhost::dir().join(format!("{id}-{}.spec", proto::random_hex(6)));
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).map_err(|e| e.to_string())?;
    let pid = proto::spawn_detached(&[exe, "ptyhost".into(), spec_path.to_string_lossy().to_string()], &env)
        .map_err(|e| format!("ptyhost: {e}"))?;
    let mut client = None;
    for _ in 0..150 {
        if let Ok(c) = ptyhost::connect(&pipe) { client = Some(c); break; }
        std::thread::sleep(Duration::from_millis(20));
    }
    let client = client.ok_or_else(|| format!("ptyhost nao respondeu ({})", argv[0]))?;
    attach(srv, m, id, client, Some(pid), PaneMeta {
        title: String::new(), cwd: o.cwd.unwrap_or(cwd), domain, cmd: o.cmd.unwrap_or_default(),
        agent: None, token, parent: o.parent, private: false,
    }, o.text.as_deref(), cols, rows);
    emit(m, "pane-created", id);
    Ok(id)
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct PaneMeta {
    title: String,
    cwd: String,
    domain: String,
    cmd: Vec<String>,
    agent: Option<Agent>,
    token: String,
    parent: Option<Id>,
    private: bool,
}

/// Liga um host (novo ou recuperado) ao mux: Term, fila de escrita e leitora.
#[allow(clippy::too_many_arguments)]
fn attach(srv: &Arc<Server>, m: &mut Mux, id: Id, client: ptyhost::Client, pid: Option<u32>, meta: PaneMeta, text: Option<&str>, cols: u16, rows: u16) {
    let listener = Listener { pane: id, tx: srv.tx.clone() };
    let tcfg = TermConfig { scrolling_history: m.cfg.scrollback, osc52: Osc52::OnlyCopy, ..TermConfig::default() };
    let mut term = Term::new(tcfg, &Dims { cols: cols as usize, lines: rows as usize }, listener);
    let mut parser = ansi::Processor::new();
    // Restauracao de estado salvo: o texto aparece antes do prompt novo.
    if let Some(t) = text {
        parser.advance(&mut term, t.replace('\n', "\r\n").as_bytes());
        parser.advance(&mut term, b"\r\n\x1b[2m--- restaurado ---\x1b[0m\r\n");
    }
    let (tx, rx) = channel::<Io>();
    let mut input = client.input;
    // Escritora propria: um shell que nao le a entrada (paste gigante) nao
    // trava o mux esperando o pipe esvaziar.
    std::thread::spawn(move || {
        for msg in rx {
            let r = match msg {
                Io::Data(d) => ptyhost::write_frame(&mut input, ptyhost::T_DATA, &d),
                Io::Resize(c, r) => {
                    let mut b = c.to_le_bytes().to_vec();
                    b.extend(r.to_le_bytes());
                    ptyhost::write_frame(&mut input, ptyhost::T_RESIZE, &b)
                }
                Io::Kill => { let _ = ptyhost::write_frame(&mut input, ptyhost::T_KILL, &[]); return; }
            };
            if r.is_err() { return; }
        }
    });
    let mut p = Pane {
        term, parser, io: tx, pid, title: meta.title, cwd: meta.cwd, domain: meta.domain, cmd: meta.cmd,
        agent: meta.agent, attention: false, last_output: Instant::now(), cols, rows, parent: meta.parent,
        token: meta.token, private: meta.private, marks: Vec::new(),
        ports: Default::default(), queue: Vec::new(), rec: None, marks_pending: false, reanchor: false, progress: None,
        mute_replies_until: Instant::now(), bell_at: None,
    };
    let lines = p.history_lines(m.cfg.scrollback, m.cfg.scrollback_mb);
    p.term.set_options(TermConfig { scrolling_history: lines, osc52: Osc52::OnlyCopy, ..TermConfig::default() });
    m.panes.insert(id, p);

    let s = srv.clone();
    let mut out = client.output;
    std::thread::spawn(move || {
        let mut code: i32 = 0;
        while let Ok((t, data)) = ptyhost::read_frame(&mut out) {
            if t == ptyhost::T_EXIT {
                if data.len() >= 4 { code = i32::from_le_bytes([data[0], data[1], data[2], data[3]]); }
                break;
            }
            if t != ptyhost::T_DATA && t != ptyhost::T_REPLAY { continue; }
            match std::panic::catch_unwind(AssertUnwindSafe(|| feed(&s, id, &data, t == ptyhost::T_REPLAY))) {
                Ok(false) => break,
                Ok(true) => {}
                Err(_) => proto::log(&format!("panic ao processar a saida do pane {id}")),
            }
            s.notify();
        }
        let mut m = s.lock();
        // Dominio com reconnect (ssh + tmux remoto): a conexao caiu, abre de
        // novo no mesmo lugar do layout; a sessao remota continua de onde estava.
        let reconnect = code != 0 && m.panes.get(&id).is_some_and(|p| m.cfg.domains.iter().any(|d| d.name == p.domain && d.reconnect));
        if reconnect {
            if let Err(e) = respawn_in_place(&s, &mut m, id) { proto::log(&format!("reconexao do pane {id}: {e}")); close_pane(&mut m, id); }
        } else {
            close_pane(&mut m, id);
        }
        drop(m);
        s.notify();
    });
}

fn respawn_in_place(srv: &Arc<Server>, m: &mut Mux, old: Id) -> Result<Id, String> {
    let p = m.panes.get(&old).ok_or("pane sumiu")?;
    let o = SpawnOpts {
        cwd: Some(p.cwd.clone()).filter(|c| !c.is_empty()), cmd: Some(p.cmd.clone()).filter(|c| !c.is_empty()),
        domain: Some(p.domain.clone()), parent: p.parent,
        text: Some(format!("{}\n\x1b[33m--- conexao caiu; reconectando ---\x1b[0m", pane_text(&p.term, Some(30)))),
    };
    std::thread::sleep(Duration::from_secs(2));
    let new = spawn_pane(srv, m, o)?;
    for w in &mut m.workspaces {
        for t in &mut w.tabs {
            if t.root.replace(old, new) { if t.active == old { t.active = new; } if t.float_prev == old { t.float_prev = new; } }
            if t.floating == Some(old) { t.floating = Some(new); if t.active == old { t.active = new; } }
        }
    }
    m.panes.remove(&old);
    Ok(new)
}

/// Saida do shell -> parser, intercalando os OSC que o alacritty ignora
/// (7 = pasta, 9/777 = notificacao, 133 = blocos de comando) na posicao certa.
fn feed(srv: &Arc<Server>, id: Id, data: &[u8], replay: bool) -> bool {
    let mut m = srv.lock();
    let notify_secs = m.cfg.notify_command_secs;
    let seen = is_seen(&m, id);
    let Some(p) = m.panes.get_mut(&id) else { return false };
    if replay { p.mute_replies_until = Instant::now() + Duration::from_millis(800); }
    // Chegou saida nova (ex.: o eco do que o usuario digita) depois do prompt
    // ter sossegado: fixa as marcas pendentes antes de o cursor andar.
    if !replay { p.resolve_marks(); }
    if let Some((f, t0, _)) = p.rec.as_mut() {
        let line = json!([t0.elapsed().as_secs_f64(), "o", String::from_utf8_lossy(data)]);
        let _ = writeln!(f, "{line}");
    }
    if !replay {
        static PORT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = PORT.get_or_init(|| regex::Regex::new(r"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):(\d{2,5})\b").unwrap());
        for c in re.captures_iter(&String::from_utf8_lossy(data)) {
            if let Ok(port) = c[1].parse::<u16>() { if p.ports.len() < 12 { p.ports.insert(port); } }
        }
    }
    let mut pos = 0;
    let mut finished: Option<u64> = None;
    let mut done_cmd: Option<Value> = None;
    let mut had_prompt_marks = false;
    for (end, ev) in osc_events(data) {
        p.parser.advance(&mut p.term, &data[pos..end]);
        pos = end;
        match ev {
            Osc::Cwd(c) => p.cwd = c,
            Osc::Notify => if !seen && !replay { p.attention = true },
            Osc::Progress(st, v) => p.progress = (st != 0).then_some((st, v)),
            Osc::Prompt => {
                had_prompt_marks = true;
                // Prompt novo = nenhum app no controle. Um TUI que saiu sem
                // desligar o mouse (ex.: claude) faria o clique virar texto no shell.
                p.parser.advance(&mut p.term, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
                let abs = p.abs_cursor();
                if p.marks.last().is_none_or(|k| k.prompt != abs) {
                    p.marks.push(Mark { prompt: abs, ..Default::default() });
                    if p.marks.len() > 1000 { p.marks.remove(0); }
                }
            }
            Osc::InputStart => {
                had_prompt_marks = true;
                let abs = p.abs_cursor();
                let col = p.term.grid().cursor.point.column.0;
                if let Some(k) = p.marks.last_mut() { if k.out_start.is_none() { k.input = Some((abs, col)); } }
            }
            Osc::CmdStart => {
                if p.marks_pending { p.last_output = Instant::now() - Duration::from_millis(100); p.resolve_marks(); }
                // pwsh marca ainda na linha digitada; bash/zsh ja na linha
                // seguinte (coluna 0): a saida comeca ali mesmo.
                let abs = p.abs_cursor();
                let at_line_start = p.term.grid().cursor.point.column.0 == 0;
                let out_start = if at_line_start { abs } else { abs + 1 };
                let command = p.marks.last().and_then(|k| k.input).map(|(ia, col)| typed_text(p, ia, col, out_start)).unwrap_or_default();
                if let Some(k) = p.marks.last_mut() { k.out_start = Some(out_start); k.started = Some(Instant::now()); k.command = command; }
            }
            Osc::CmdEnd(code) => {
                had_prompt_marks = true;
                let abs = p.abs_cursor();
                if let Some(k) = p.marks.last_mut() {
                    if k.out_start.is_some() && k.exit.is_none() {
                        k.exit = Some(code);
                        k.out_end = Some(abs);
                        k.dur_ms = k.started.map(|s| s.elapsed().as_millis() as u64);
                        if !replay && notify_secs > 0 && k.dur_ms.unwrap_or(0) >= notify_secs * 1000 { finished = k.dur_ms; }
                        if !replay && !k.command.is_empty() && !p.private {
                            done_cmd = Some(json!({"ts": now_secs(), "cmd": k.command, "cwd": p.cwd, "exit": code,
                                "ms": k.dur_ms, "pane": id, "domain": p.domain}));
                        }
                    }
                }
            }
        }
    }
    p.parser.advance(&mut p.term, &data[pos..]);
    if !replay { p.last_output = Instant::now(); }
    // Marcas novas (ou resize esperando o repaint): um frame depois do silencio
    // fixa as posicoes.
    if (had_prompt_marks || p.marks_pending) && !replay {
        p.marks_pending = true;
        let s2 = srv.clone();
        std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(60)); s2.notify(); });
    }
    // Synchronized output (DEC 2026): o parser segura a tela ate o fim do
    // quadro; se o fim nao vier, o flusher solta no timeout.
    if let Some(deadline) = p.parser.sync_timeout().sync_timeout() {
        let (lock, cv) = &srv.sync;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none_or(|d| deadline < d) { *g = Some(deadline); cv.notify_one(); }
    }
    if let Some(h) = done_cmd {
        let line = scrub(&m, h.to_string());
        let path = proto::data_dir().join("history.jsonl");
        if std::fs::metadata(&path).map(|x| x.len() > 8 << 20).unwrap_or(false) {
            let _ = std::fs::rename(&path, proto::data_dir().join("history.jsonl.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) { let _ = writeln!(f, "{line}"); }
        m.history.push(serde_json::from_str(&line).unwrap_or(h));
        if m.history.len() > 50_000 { m.history.drain(..10_000); }
    }
    let Some(p) = m.panes.get_mut(&id) else { return true };
    if let Some(ms) = finished {
        if !seen { p.attention = true; }
        emit(&mut m, "command-finished", id);
        if let Some(e) = m.events.last_mut() { e.1["duration_ms"] = json!(ms); }
    }
    true
}

enum Osc { Cwd(String), Notify, Progress(u8, u8), Prompt, InputStart, CmdStart, CmdEnd(i32) }

/// Acha os OSC de interesse e devolve (fim da sequencia, evento).
/// ponytail: sequencia partida entre dois reads se perde; o shell reemite a
/// cada prompt, entao no pior caso um bloco fica sem marca.
fn osc_events(data: &[u8]) -> Vec<(usize, Osc)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let start = i + 2;
            let mut j = start;
            let mut end = None;
            while j < data.len() {
                if data[j] == 0x07 { end = Some((j, j + 1)); break; }
                if data[j] == 0x1b && j + 1 < data.len() && data[j + 1] == b'\\' { end = Some((j, j + 2)); break; }
                j += 1;
            }
            let Some((body_end, seq_end)) = end else { break };
            let body = String::from_utf8_lossy(&data[start..body_end]);
            let ev = if let Some(url) = body.strip_prefix("7;") {
                let path = url.strip_prefix("file://").map(|u| u.find('/').map(|k| &u[k..]).unwrap_or(u)).unwrap_or(url);
                let mut path = percent_decode(path);
                // file://host/C:/x -> C:/x
                if path.len() > 2 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' { path.remove(0); }
                (!path.is_empty()).then(|| Osc::Cwd(clean(&path, 1024)))
            } else if let Some(r) = body.strip_prefix("9;4;") {
                // Barra de progresso (ConEmu/Windows Terminal): 9;4;estado;pct. 0 = some.
                let mut it = r.split(';');
                let st = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0u8);
                let pct = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0u8);
                Some(Osc::Progress(st, pct.min(100)))
            } else if body.starts_with("9;") || body.starts_with("777;notify") {
                Some(Osc::Notify)
            } else if let Some(k) = body.strip_prefix("133;") {
                match k.as_bytes().first() {
                    Some(b'A') => Some(Osc::Prompt),
                    Some(b'B') => Some(Osc::InputStart),
                    Some(b'C') => Some(Osc::CmdStart),
                    Some(b'D') => Some(Osc::CmdEnd(k.split(';').nth(1).and_then(|c| c.trim().parse().ok()).unwrap_or(0))),
                    _ => None,
                }
            } else { None };
            if let Some(ev) = ev { out.push((seq_end, ev)); }
            i = seq_end;
        } else {
            i += 1;
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn close_pane(m: &mut Mux, id: Id) {
    if let Some(p) = m.panes.remove(&id) {
        let _ = p.io.send(Io::Kill);
        emit(m, "pane-closed", id);
    }
    let mut orphans = Vec::new();
    for ws in &mut m.workspaces {
        for t in &mut ws.tabs {
            if t.floating == Some(id) {
                t.floating = None;
                t.float_visible = false;
                if t.active == id { t.active = if t.root.contains(t.float_prev) { t.float_prev } else { t.root.leaves()[0] }; }
                continue;
            }
            if t.root.contains(id) {
                t.zoomed = false;
                if let Some(r) = t.root.clone().remove(id) {
                    t.root = r;
                    if t.active == id { t.active = t.root.leaves()[0]; }
                } else {
                    orphans.extend(t.floating); // aba fechou: o flutuante dela vai junto
                    t.active = 0; // marca a aba para remocao
                }
            }
        }
        ws.tabs.retain(|t| t.active != 0);
        if ws.active >= ws.tabs.len() { ws.active = ws.tabs.len().saturating_sub(1); }
    }
    // Workspace sem abas some (o "default" volta sozinho no proximo frame).
    m.workspaces.retain(|w| !w.tabs.is_empty());
    for o in orphans { if let Some(p) = m.panes.remove(&o) { let _ = p.io.send(Io::Kill); } }
}

fn line_text(term: &Term<Listener>, line: i32) -> String {
    let row = &term.grid()[Line(line)];
    let mut s = String::new();
    for c in 0..term.columns() {
        let cell = &row[Column(c)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { continue; }
        s.push(cell.c);
    }
    s.trim_end().to_string()
}

fn pane_text(term: &Term<Listener>, last: Option<usize>) -> String {
    let top = term.topmost_line().0;
    let bottom = term.bottommost_line().0;
    let mut lines: Vec<String> = (top..=bottom).map(|l| line_text(term, l)).collect();
    while lines.last().is_some_and(|l| l.is_empty()) { lines.pop(); }
    if let Some(n) = last { let k = lines.len().saturating_sub(n); lines.drain(..k); }
    lines.join("\n")
}

/// Linhas absolutas [a, b) como texto.
fn abs_text(p: &Pane, a: i64, b: i64) -> String {
    let h = p.term.history_size() as i64;
    let top = p.term.topmost_line().0 as i64;
    let bottom = p.term.bottommost_line().0 as i64;
    let mut v: Vec<String> = (a..b).map(|abs| abs - h).filter(|l| *l >= top && *l <= bottom)
        .map(|l| line_text(&p.term, l as i32)).collect();
    while v.last().is_some_and(|l| l.is_empty()) { v.pop(); }
    v.join("\n")
}

/// Texto digitado: da coluna do 133;B ate antes da saida (linhas absolutas).
fn typed_text(p: &Pane, input_abs: i64, col: usize, out_start: i64) -> String {
    let h = p.term.history_size() as i64;
    let mut parts = Vec::new();
    for abs in input_abs..out_start.max(input_abs + 1) {
        let l = abs - h;
        if l < p.term.topmost_line().0 as i64 || l > p.term.bottommost_line().0 as i64 { continue; }
        let t = line_text(&p.term, l as i32);
        let t = if abs == input_abs { t.chars().skip(col).collect::<String>() } else { t };
        parts.push(t.trim().to_string());
    }
    parts.join(" ").trim().chars().take(2000).collect()
}

fn load_history() -> Vec<Value> {
    let mut v: Vec<Value> = std::fs::read_to_string(proto::data_dir().join("history.jsonl")).unwrap_or_default()
        .lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let n = v.len().saturating_sub(50_000);
    v.drain(..n);
    v
}

/// Entrega um texto ao agente como se fosse colado e aperta Enter: varias
/// linhas chegam inteiras (bracketed paste) e o Enter vai depois do paste.
fn deliver(p: &Pane, text: &str) {
    let bracketed = p.term.mode().contains(TermMode::BRACKETED_PASTE);
    let clean_text: String = text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect();
    if bracketed { p.write(format!("\x1b[200~{}\x1b[201~", clean_text.replace('\n', "\r")).as_bytes()); }
    else { p.write(clean_text.replace('\n', " ").as_bytes()); }
    let io = p.io.clone();
    std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(350)); let _ = io.send(Io::Data(b"\r".to_vec())); });
}

fn scrub(m: &Mux, text: String) -> String {
    m.secrets.iter().fold(text, |t, re| re.replace_all(&t, "***").to_string())
}

// ------------------------------------------------------------------ eventos

fn emit(m: &mut Mux, event: &str, pane: Id) {
    let (title, cwd, state) = m.panes.get(&pane)
        .map(|p| (p.title.clone(), p.cwd.clone(), p.agent.as_ref().map(|a| a.state.clone()).unwrap_or_default()))
        .unwrap_or_default();
    m.event_seq += 1;
    let ev = json!({"event": event, "pane": pane, "title": title, "cwd": cwd, "state": state});
    m.events.push((m.event_seq, ev));
    if m.events.len() > 64 { m.events.remove(0); }
    if let Some(cmd) = m.cfg.hooks.get(event).cloned() {
        run_hook(&cmd, event, pane, &title, &cwd, &state);
    }
}

fn run_hook(cmd: &str, event: &str, pane: Id, title: &str, cwd: &str, state: &str) {
    #[cfg(windows)]
    let mut c = {
        // raw_arg: o Command escaparia as aspas com \" e o cmd nao entende.
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd.exe");
        c.arg("/C").raw_arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", cmd]);
        c
    };
    c.env("DUCK_EVENT", event).env("DUCK_PANE", pane.to_string()).env("DUCK_TITLE", title)
        .env("DUCK_CWD", cwd).env("DUCK_STATE", state)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c.spawn();
}

/// Pane visivel numa janela focada: atencao nele nao faz sentido.
fn is_seen(m: &Mux, id: Id) -> bool {
    m.clients.values().filter(|c| c.focused).any(|c| {
        m.workspaces.iter().find(|w| w.name == c.workspace)
            .and_then(|w| w.tabs.get(w.active))
            .is_some_and(|t| t.active == id)
    })
}

fn clear_seen(m: &mut Mux) {
    let ids: Vec<Id> = m.panes.keys().copied().collect();
    for id in ids {
        if is_seen(m, id) { if let Some(p) = m.panes.get_mut(&id) { p.attention = false; } }
    }
}

fn set_state(m: &mut Mux, id: Id, state: &str, name: Option<&str>, source: &str) {
    let seen = is_seen(m, id);
    let Some(p) = m.panes.get_mut(&id) else { return };
    if state == "none" {
        p.agent = None;
        return;
    }
    let a = p.agent.get_or_insert_with(|| Agent { name: name.unwrap_or("agent").into(), ..Default::default() });
    if let Some(n) = name { a.name = n.into(); }
    if source == "hook" { a.hook_at = Some(Instant::now()); }
    a.source = source.into();
    if a.state == state { return; }
    a.state = state.into();
    a.since = now_secs();
    // Atencao so na passagem para blocked/done (nao a cada troca enquanto ela
    // continua marcada, senao cada working->done de novo avisava outra vez).
    let call = (state == "blocked" || state == "done") && !seen;
    if call { p.attention = true; }
    // Recado na fila: entregue quando o agente para (sem interromper o trabalho).
    if (state == "done" || state == "idle") && !p.queue.is_empty() {
        let text = p.queue.remove(0);
        deliver(p, &text);
        p.attention = false;
    }
    emit(m, "agent-state-changed", id);
    if call { emit(m, "attention", id); }
}

fn ask(m: &mut Mux, kind: &str, pane: Id, text: String, payload: String) -> u64 {
    m.approval_seq += 1;
    let id = m.approval_seq;
    m.approvals.push(Approval { id, kind: kind.into(), text, pane, payload, decided: None });
    emit(m, "approval", pane);
    id
}

fn event_loop(srv: Arc<Server>, rx: std::sync::mpsc::Receiver<(Id, TE)>) {
    while let Ok((id, ev)) = rx.recv() {
        let mut m = srv.lock();
        let theme = m.theme().clone();
        let clip_mode = m.cfg.clipboard_write.clone();
        let seen = is_seen(&m, id);
        let Some(p) = m.panes.get_mut(&id) else { continue };
        let muted = Instant::now() < p.mute_replies_until;
        match ev {
            TE::PtyWrite(s) => if !muted { p.write(s.as_bytes()) },
            TE::Title(t) => p.title = clean(&t, 200),
            TE::ResetTitle => p.title.clear(),
            TE::Bell => {
                if muted { continue; }
                p.bell_at = Some(Instant::now());
                if !seen { p.attention = true; }
                emit(&mut m, "bell", id);
            }
            TE::ClipboardStore(_, text) => {
                if muted { continue; }
                // OSC 52: um app (ou um arquivo exibido com cat) escrevendo no
                // seu clipboard e o ataque "o proximo Ctrl+V roda o meu comando".
                match clip_mode.as_str() {
                    "allow" => { m.clipboard.0 += 1; m.clipboard.1 = text; }
                    "deny" => proto::log(&format!("OSC 52 negado no pane {id}")),
                    _ => {
                        let preview: String = clean(&text.replace('\n', "⏎"), 160);
                        let title = p.title.clone();
                        ask(&mut m, "clipboard", id, format!("O pane {id} ({title}) quer copiar para o clipboard: {preview}"), text);
                    }
                }
            }
            TE::ColorRequest(i, f) => {
                let c = match i {
                    256 => theme.fg,
                    257 => theme.bg,
                    258 => theme.cursor,
                    i if i < 16 => theme.palette[i],
                    i if i < 256 => theme.resolve(idx256(i as u8)),
                    _ => theme.fg,
                };
                if !muted { p.write(f(ARgb { r: c.0, g: c.1, b: c.2 }).as_bytes()); }
            }
            TE::TextAreaSizeRequest(f) => {
                if !muted { p.write(f(WindowSize { num_lines: p.rows, num_cols: p.cols, cell_width: 8, cell_height: 16 }).as_bytes()); }
            }
            _ => continue,
        }
        drop(m);
        srv.notify();
    }
}

/// Solta o synchronized output de quem nao mandou o fim do quadro a tempo.
fn sync_loop(srv: Arc<Server>) {
    loop {
        {
            let (lock, cv) = &srv.sync;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                match *g {
                    None => g = cv.wait(g).unwrap_or_else(|e| e.into_inner()),
                    Some(d) if d > Instant::now() => g = cv.wait_timeout(g, d - Instant::now()).unwrap_or_else(|e| e.into_inner()).0,
                    Some(_) => { *g = None; break; }
                }
            }
        }
        let mut m = srv.lock();
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        for p in m.panes.values_mut() {
            match p.parser.sync_timeout().sync_timeout() {
                Some(d) if d <= now => p.parser.stop_sync(&mut p.term),
                Some(d) => next = Some(next.map_or(d, |n| n.min(d))),
                None => {}
            }
        }
        drop(m);
        if let Some(n) = next { *srv.sync.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(n); }
        srv.notify();
    }
}

// ------------------------------------------------------------------ agentes

const BLOCKED_HINTS: [&str; 6] = ["Do you want to", "Would you like to", "❯ 1. Yes", "(y/n)", "[Y/n]", "Allow "];

fn agent_loop(srv: Arc<Server>) {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut sys) = System::new() else { return };
    let mut children: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
    let mut tick = 0u64;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        // Arvore de processos a cada 6s (a varredura custa ~20ms); titulo e
        // estado a cada 2s. ponytail: trocar por eventos de processo (ETW) se
        // ainda aparecer no profiling.
        if tick % 3 == 0 && !srv.lock().panes.is_empty() {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true,
                ProcessRefreshKind::nothing().with_exe(UpdateKind::Never));
            children.clear();
            for (pid, pr) in sys.processes() {
                if let Some(pp) = pr.parent() {
                    let name = pr.name().to_string_lossy().to_lowercase();
                    children.entry(pp.as_u32()).or_default().push((pid.as_u32(), name));
                }
            }
        }
        tick += 1;
        let mut m = srv.lock();
        let agents = m.cfg.agents.clone();
        let ids: Vec<Id> = m.panes.keys().copied().collect();
        let mut changed = false;
        for id in ids {
            let p = &m.panes[&id];
            let mut found: Option<String> = None;
            if let Some(root) = p.pid {
                let mut stack = vec![root];
                'outer: while let Some(cur) = stack.pop() {
                    for (cpid, name) in children.get(&cur).into_iter().flatten() {
                        let base = name.trim_end_matches(".exe");
                        if let Some(a) = agents.iter().find(|a| base == a.as_str() || base.starts_with(&format!("{a}-"))) {
                            found = Some(a.clone());
                            break 'outer;
                        }
                        stack.push(*cpid);
                    }
                }
            }
            // Titulo (o WSL nao expoe o processo): o Claude Code poe "✳" ou
            // um spinner braille no titulo.
            let spinner = p.title.chars().next().is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
            if found.is_none() && (p.title.starts_with('✳') || spinner) { found = Some("claude".into()); }
            if found.is_none() {
                let tl = p.title.to_lowercase();
                found = agents.iter().find(|a| tl.contains(a.as_str())).cloned();
            }
            let hook_fresh = p.agent.as_ref().and_then(|a| a.hook_at).is_some_and(|t| t.elapsed() < Duration::from_secs(600));
            if found.is_none() {
                if p.agent.as_ref().is_some_and(|a| a.source == "auto") {
                    m.panes.get_mut(&id).unwrap().agent = None;
                    changed = true;
                }
                continue;
            }
            if hook_fresh { continue; }
            let screen = {
                let rows = p.term.screen_lines() as i32;
                (rows.saturating_sub(12)..rows).map(|l| line_text(&p.term, l)).collect::<Vec<_>>().join("\n")
            };
            // Claude Code: o titulo diz o estado (spinner = trabalhando, ✳ = parado).
            // A saida nao serve: parado, ele ainda redesenha a linha de status.
            let idle_title = p.title.starts_with('✳');
            let was_working = p.agent.as_ref().filter(|a| a.state == "working");
            // So "terminou" (com aviso) se trabalhou de verdade, nao por um repaint.
            let long_work = was_working.is_some_and(|a| now_secs().saturating_sub(a.since) >= 8);
            let state = if BLOCKED_HINTS.iter().any(|h| screen.contains(h)) {
                "blocked"
            } else if spinner || (!idle_title && p.last_output.elapsed() < Duration::from_millis(1500)) {
                "working"
            } else if was_working.is_some() {
                if long_work { "done" } else { "idle" }
            } else {
                p.agent.as_ref().map(|a| a.state.as_str()).filter(|s| !s.is_empty()).unwrap_or("idle")
            }.to_string();
            let before = p.agent.as_ref().map(|a| a.state.clone());
            set_state(&mut m, id, &state, found.as_deref(), "auto");
            if before.as_deref() != Some(state.as_str()) { changed = true; }
        }
        drop(m);
        if changed { srv.notify(); }
    }
}

// ------------------------------------------------------------------ frames

fn ensure_workspace(m: &mut Mux, srv: &Arc<Server>, name: &str) -> Result<(), String> {
    if m.workspaces.iter().any(|w| w.name == name) { return Ok(()); }
    let id = spawn_pane(srv, m, SpawnOpts::shell())?;
    m.workspaces.push(Workspace { name: name.into(), tabs: vec![Tab::new(id, "")], active: 0 });
    Ok(())
}

fn client_ws(m: &mut Mux, srv: &Arc<Server>, client: &str) -> Result<usize, String> {
    let name = m.clients.get(client).map(|c| c.workspace.clone())
        .filter(|n| m.workspaces.iter().any(|w| &w.name == n) || m.workspaces.is_empty())
        .or_else(|| m.workspaces.first().map(|w| w.name.clone()))
        .unwrap_or_else(|| "default".into());
    ensure_workspace(m, srv, &name)?;
    view(m, client, &name);
    Ok(m.workspaces.iter().position(|w| w.name == name).unwrap())
}

/// Aponta a janela do cliente para o workspace.
fn view(m: &mut Mux, client: &str, ws: &str) {
    m.clients.entry(client.into()).or_insert(ClientView { workspace: ws.into(), focused: true }).workspace = ws.into();
}

fn tab_state(m: &Mux, t: &Tab) -> (String, bool) {
    let rank = |s: &str| match s { "blocked" => 4, "done" => 3, "working" => 2, "idle" => 1, _ => 0 };
    let mut best = String::new();
    let mut att = false;
    for id in t.all() {
        if let Some(p) = m.panes.get(&id) {
            att |= p.attention;
            if let Some(a) = &p.agent { if rank(&a.state) > rank(&best) { best = a.state.clone(); } }
        }
    }
    (best, att)
}

type RunT = (usize, usize, String, i64, i64, u16, Option<String>);

fn pane_json(p: &mut Pane, id: Id, r: Rect, floating: bool, cap: (usize, f64)) -> Value {
    p.resolve_marks();
    if p.cols != r.w || p.rows != r.h {
        p.cols = r.w;
        p.rows = r.h;
        p.term.resize(Dims { cols: r.w as usize, lines: r.h as usize });
        let _ = p.io.send(Io::Resize(r.w, r.h));
        if !p.marks.is_empty() { p.reanchor = true; p.marks_pending = true; p.last_output = Instant::now(); }
        let lines = p.history_lines(cap.0, cap.1);
        p.term.set_options(TermConfig { scrolling_history: lines, osc52: Osc52::OnlyCopy, ..TermConfig::default() });
    }
    let history = p.term.history_size() as i64;
    let content = p.term.renderable_content();
    let offset = content.display_offset as i32;
    let mode = content.mode;
    let cursor = content.cursor;
    let mut lines: Vec<Vec<Value>> = vec![Vec::new(); r.h as usize];
    // run = [coluna, texto, fg, bg, flags, link?]
    let mut run: Option<RunT> = None;
    let flush = |run: &mut Option<RunT>, lines: &mut Vec<Vec<Value>>| {
        if let Some((row, col, text, fg, bg, fl, link)) = run.take() {
            if row < lines.len() && !(text.trim().is_empty() && bg == BG && fl & 4 == 0) {
                lines[row].push(match link { Some(u) => json!([col, text, fg, bg, fl, u]), None => json!([col, text, fg, bg, fl]) });
            }
        }
    };
    for cell in content.display_iter {
        let row = (cell.point.line.0 + offset) as usize;
        let col = cell.point.column.0;
        let f = cell.flags;
        if f.contains(Flags::WIDE_CHAR_SPACER) || f.contains(Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
        let (mut fg, mut bg) = (enc(cell.fg), enc(cell.bg));
        if f.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg); }
        if f.contains(Flags::HIDDEN) { fg = bg; }
        let mut fl = 0u16;
        if f.contains(Flags::BOLD) { fl |= 1; }
        if f.contains(Flags::ITALIC) { fl |= 2; }
        if f.intersects(Flags::ALL_UNDERLINES) { fl |= 4; }
        if f.contains(Flags::STRIKEOUT) { fl |= 8; }
        if f.contains(Flags::DIM) { fl |= 16; }
        if f.contains(Flags::WIDE_CHAR) { fl |= 32; }
        // OSC 8: link que o app pos no texto (o destino pode diferir do texto).
        let link = cell.hyperlink().map(|h| clean(h.uri(), 2048));
        let same = run.as_ref().is_some_and(|r| r.0 == row && r.3 == fg && r.4 == bg && r.5 == fl && fl & 32 == 0
            && r.6 == link && r.1 + r.2.chars().count() == col);
        if !same { flush(&mut run, &mut lines); run = Some((row, col, String::new(), fg, bg, fl, link)); }
        let r = run.as_mut().unwrap();
        r.2.push(cell.c);
        if let Some(z) = cell.zerowidth() { r.2.extend(z.iter()); }
    }
    flush(&mut run, &mut lines);
    let show_cursor = mode.contains(TermMode::SHOW_CURSOR) && offset == 0 && cursor.shape != CursorShape::Hidden;
    let cur = if show_cursor {
        let shape = match cursor.shape { CursorShape::Beam => "beam", CursorShape::Underline => "underline", _ => "block" };
        json!([cursor.point.column.0, cursor.point.line.0 + offset, shape])
    } else { Value::Null };
    // Sticky scroll: o comando cuja saida esta no topo da tela.
    let top = history - offset as i64;
    let sticky = if offset > 0 {
        p.marks.iter().rev().find(|k| k.prompt < top && k.out_end.is_none_or(|e| e > top)).map(|k| k.command.clone()).filter(|c| !c.is_empty())
    } else { None };
    // Onde comeca o que o usuario esta digitando agora (sugestao inline).
    let input = p.marks.last().filter(|k| k.out_start.is_none() && offset == 0)
        .and_then(|k| k.input).map(|(abs, col)| json!([abs - history, col]));
    // Blocos de comando visiveis (linha da tela) e todos (fracao, p/ a barra).
    let total = (history + r.h as i64).max(1);
    let mut blocks = Vec::new();
    let mut ticks = Vec::new();
    let rel = |a: i64| a - history + offset as i64;
    // O ultimo bloco que comecou acima da tela tambem vai (o cartao dele continua aqui).
    let above = p.marks.iter().rposition(|k| rel(k.prompt) < 0 && k.prompt > i64::MIN / 2);
    for (i, k) in p.marks.iter().enumerate() {
        let row = rel(k.prompt);
        if (row >= 0 && row < r.h as i64) || Some(i) == above {
            blocks.push(json!({"row": row, "exit": k.exit, "ms": k.dur_ms, "cmd": k.command, "abs": k.prompt,
                "out": k.out_start.map(rel), "end": k.out_end.map(rel),
                "n": match (k.out_start, k.out_end) { (Some(a), Some(b)) => b - a, _ => 0 }}));
        }
        if k.prompt > i64::MIN / 2 {
            ticks.push(json!([k.prompt as f64 / total as f64, match k.exit { None => 0, Some(0) => 1, _ => 2 }]));
        }
    }
    let profile = p.agent.as_ref().and_then(|a| a.config_dir.as_deref()).map(profile_name).unwrap_or_default();
    let running = p.marks.last().filter(|k| k.out_start.is_some() && k.exit.is_none() && !k.command.is_empty())
        .and_then(|k| Some(json!([k.command, k.started?.elapsed().as_secs()]))).unwrap_or(Value::Null);
    json!({
        "id": id, "x": r.x, "y": r.y, "w": r.w, "h": r.h, "floating": floating,
        "title": p.title, "cwd": p.cwd, "domain": p.domain, "private": p.private,
        "state": p.agent.as_ref().map(|a| a.state.clone()).unwrap_or_default(),
        "agent": p.agent.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
        "attention": p.attention,
        "bell": p.bell_at.is_some_and(|b| b.elapsed() < Duration::from_millis(300)),
        "scroll": offset, "history": history,
        "app_cursor": mode.contains(TermMode::APP_CURSOR),
        "alt_screen": mode.contains(TermMode::ALT_SCREEN),
        // App pediu o mouse (vim, htop, TUIs): a GUI repassa cliques em SGR.
        "mouse": mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE),
        "mouse_motion": mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
        "cursor": cur, "lines": lines, "blocks": blocks, "ticks": ticks,
        "sticky": sticky, "input": input, "ports": p.ports, "recording": p.rec.is_some(),
        "queued": p.queue.len(),
        "todo": p.agent.as_ref().and_then(|a| Some([a.todo_done?, a.todo_total?])),
        "progress": p.progress, "profile": profile, "running": running,
        "tokens": p.agent.as_ref().and_then(|a| a.context_tokens),
        "session": p.agent.as_ref().and_then(|a| a.session.clone()),
        "config_dir": p.agent.as_ref().and_then(|a| a.config_dir.clone()),
    })
}

/// ".claude-trabalho" -> "trabalho"; ".claude" -> "pessoal".
fn profile_name(dir: &str) -> String {
    let base = dir.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    let p = base.trim_start_matches(".claude").trim_start_matches(['-', '_']);
    if p.is_empty() { "pessoal".into() } else { p.to_string() }
}

fn frame(srv: &Arc<Server>, m: &mut Mux, client: &str) -> Result<Value, String> {
    let wi = client_ws(m, srv, client)?;
    clear_seen(m);
    let (cols, rows) = m.size;
    let area = Rect { x: 0, y: 0, w: cols.max(2), h: rows.max(2) };
    let ws = &m.workspaces[wi];
    let tab = &ws.tabs[ws.active];
    let mut rects = Vec::new();
    if tab.zoomed && tab.root.contains(tab.active) { rects.push((tab.active, area)); } else { tab.root.layout(area, &mut rects); }
    let float = tab.floating.filter(|_| tab.float_visible).map(|f| {
        let (w, h) = ((cols as f32 * 0.8) as u16, (rows as f32 * 0.8) as u16);
        (f, Rect { x: cols.saturating_sub(w) / 2, y: rows.saturating_sub(h) / 2, w: w.max(10), h: h.max(4) })
    });
    // Cabecalho por pane: uma linha acima de cada um (a GUI desenha).
    let header = match m.cfg.pane_header.as_str() { "always" => true, "off" => false, _ => rects.len() > 1 };
    if header {
        for (_, r) in rects.iter_mut() { if r.h > 3 { r.y += 1; r.h -= 1; } }
    }
    let active = tab.active;
    let ws_name = ws.name.clone();
    let tabs: Vec<Value> = ws.tabs.iter().enumerate().map(|(i, t)| {
        let (state, attention) = tab_state(m, t);
        let p = m.panes.get(&t.active);
        let agent = t.all().iter().filter_map(|id| m.panes.get(id)?.agent.as_ref()).next().cloned();
        let progress = t.all().iter().filter_map(|id| m.panes.get(id)?.progress).max_by_key(|x| x.1);
        json!({
            "index": i + 1, "title": t.title, "active": i == ws.active, "color": t.color,
            "pane_title": p.map(|p| p.title.clone()).unwrap_or_default(),
            "cwd": p.map(|p| p.cwd.clone()).unwrap_or_default(),
            "domain": p.map(|p| p.domain.clone()).unwrap_or_default(),
            "state": state, "attention": attention,
            // Progresso do plano de qualquer agente da aba (nao so do pane ativo).
            "todo": t.all().iter().filter_map(|id| m.panes.get(id)?.agent.as_ref()).find_map(|a| Some([a.todo_done?, a.todo_total?])),
            "pane": t.active, "progress": progress,
            "agent": agent.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            "profile": agent.as_ref().and_then(|a| a.config_dir.as_deref()).map(profile_name).unwrap_or_default(),
        })
    }).collect();
    let workspaces: Vec<String> = m.workspaces.iter().map(|w| w.name.clone()).collect();
    let mut counts = HashMap::from([("working", 0), ("blocked", 0), ("done", 0), ("idle", 0)]);
    for p in m.panes.values() {
        if let Some(a) = &p.agent { if let Some(c) = counts.get_mut(a.state.as_str()) { *c += 1; } }
    }
    let cap = (m.cfg.scrollback, m.cfg.scrollback_mb);
    let mut panes = Vec::new();
    let list: Vec<(Id, Rect, bool)> = rects.into_iter().map(|(i, r)| (i, r, false)).chain(float.map(|(i, r)| (i, r, true))).collect();
    for (id, r, fl) in list {
        let Some(p) = m.panes.get_mut(&id) else { continue };
        let mut v = pane_json(p, id, r, fl, cap);
        v["header"] = json!(header && !fl && r.y > 0);
        panes.push(v);
    }
    let approvals: Vec<&Approval> = m.approvals.iter().filter(|a| a.decided.is_none()).collect();
    Ok(json!({
        "t": "frame", "workspace": ws_name, "workspaces": workspaces, "tabs": tabs,
        "active_pane": active, "cols": cols, "rows": rows, "panes": panes,
        "agents": counts, "clipboard": [m.clipboard.0, m.clipboard.1],
        "approvals": approvals, "resurrect": m.resurrect, "recovered": m.recovered,
        "tokens": m.panes.values().filter_map(|p| p.agent.as_ref()?.session_tokens).sum::<u64>(),
        "ports": m.panes.values().flat_map(|p| p.ports.iter().copied()).collect::<std::collections::BTreeSet<u16>>(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ------------------------------------------------------------------ comandos

fn arg_id(a: &Value, k: &str) -> Option<Id> {
    a.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}
fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> { a.get(k).and_then(|v| v.as_str()) }

/// Workspace e aba onde o pane mora.
fn locate(m: &Mux, id: Id) -> Option<(usize, usize)> {
    for (wi, w) in m.workspaces.iter().enumerate() {
        for (ti, t) in w.tabs.iter().enumerate() {
            if t.has(id) { return Some((wi, ti)); }
        }
    }
    None
}

/// Pane alvo: `pane` explicito, senao o ativo da janela do cliente.
fn target(srv: &Arc<Server>, m: &mut Mux, a: &Value) -> Result<Id, String> {
    if let Some(id) = arg_id(a, "pane") {
        return if m.panes.contains_key(&id) { Ok(id) } else { Err(format!("pane {id} nao existe")) };
    }
    let client = arg_str(a, "client").unwrap_or("cli").to_string();
    let wi = client_ws(m, srv, &client)?;
    let w = &m.workspaces[wi];
    Ok(w.tabs[w.active].active)
}

/// Quem chamou a API: None = o dono (GUI ou CLI fora de um pane).
#[derive(Clone, Copy)]
struct Caller(Option<Id>);

const READ_CMDS: [&str; 15] = ["ping", "list", "read", "states", "events", "frame", "doctor", "grep", "last-output", "wait", "last-failure", "history", "blocks", "block-output", "search-all"];
const MUTATING_PANE: [&str; 7] = ["send", "paste", "close", "scroll", "set-state", "clear-attention", "private"];

fn is_descendant(m: &Mux, id: Id, of: Id) -> bool {
    let mut cur = Some(id);
    let mut hops = 0;
    while let Some(c) = cur {
        if c == of { return true; }
        cur = m.panes.get(&c).and_then(|p| p.parent);
        hops += 1;
        if hops > 64 { break; }
    }
    false
}

/// Escopo da API para chamadas vindas de um pane (cfg.api_scope).
fn check_scope(m: &Mux, caller: Caller, cmd: &str, a: &Value) -> Result<(), String> {
    let Some(me) = caller.0 else { return Ok(()) };
    if READ_CMDS.contains(&cmd) {
        // Pane privado nao e lido por outro pane.
        if ["read", "grep", "last-output", "frame", "last-failure", "block-output", "search-all"].contains(&cmd) {
            if let Some(t) = arg_id(a, "pane") {
                if t != me && m.panes.get(&t).is_some_and(|p| p.private) { return Err("pane privado".into()); }
            }
        }
        return Ok(());
    }
    // set-state no proprio pane e o hook do agente: sempre pode.
    if cmd == "set-state" && arg_id(a, "pane").is_none_or(|t| t == me) { return Ok(()); }
    match m.cfg.api_scope.as_str() {
        "read" => Err(format!("api_scope=read: o pane {me} so pode ler")),
        "children" => {
            if MUTATING_PANE.contains(&cmd) {
                let t = arg_id(a, "pane").unwrap_or(me);
                if t != me && !is_descendant(m, t, me) {
                    return Err(format!("api_scope=children: o pane {t} nao foi criado pelo pane {me}"));
                }
            }
            if ["kill-server", "reload", "delete-state", "workspace", "rename-workspace"].contains(&cmd) {
                return Err(format!("api_scope=children: '{cmd}' nao e permitido a um pane"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Acao destrutiva pedida por um agente: o usuario confirma na janela.
fn needs_approval(m: &Mux, caller: Caller, cmd: &str, a: &Value) -> Option<String> {
    let me = caller.0?;
    if !m.cfg.confirm_agent_actions { return None; }
    match cmd {
        "kill-server" => Some(format!("O pane {me} quer ENCERRAR o server (fecha todos os shells).")),
        "close" => {
            let t = arg_id(a, "pane")?;
            (t != me && !is_descendant(m, t, me)).then(|| {
                let title = m.panes.get(&t).map(|p| p.title.clone()).unwrap_or_default();
                format!("O pane {me} quer fechar o pane {t} ({title}).")
            })
        }
        "send" | "paste" => {
            let t = arg_id(a, "pane")?;
            let busy = m.panes.get(&t).and_then(|p| p.agent.as_ref()).is_some_and(|ag| ag.state == "working");
            (t != me && busy && !is_descendant(m, t, me)).then(|| {
                let text: String = clean(arg_str(a, "text").unwrap_or(""), 120);
                format!("O pane {me} quer digitar no agente do pane {t} (trabalhando): {text}")
            })
        }
        _ => None,
    }
}

fn audit(m: &Mux, caller: Caller, a: &Value) {
    if !m.cfg.audit { return; }
    let cmd = arg_str(a, "cmd").unwrap_or("");
    if READ_CMDS.contains(&cmd) || ["subscribe", "resize", "window-focus", "theme-mode", "ext-send"].contains(&cmd) { return; }
    // A GUI digitando nao e auditoria; agentes e scripts sao.
    if arg_str(a, "client").is_some_and(|c| c.starts_with("gui-")) { return; }
    let mut a = a.clone();
    if let Some(t) = a.get_mut("text") { *t = json!(clean(t.as_str().unwrap_or(""), 200)); }
    let line = json!({"ts": now_secs(), "caller_pane": caller.0, "req": a});
    let p = proto::data_dir().join("audit.log");
    if std::fs::metadata(&p).map(|x| x.len() > 4 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&p, proto::data_dir().join("audit.log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{line}");
    }
}

fn wait_approval(srv: &Arc<Server>, id: u64, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        {
            let mut m = srv.lock();
            if let Some(d) = m.approvals.iter().find(|x| x.id == id).and_then(|x| x.decided) {
                m.approvals.retain(|x| x.id != id);
                return d;
            }
            if start.elapsed() > timeout {
                m.approvals.retain(|x| x.id != id);
                return false;
            }
        }
        let g = srv.gen.lock().unwrap_or_else(|e| e.into_inner());
        let _ = srv.cv.wait_timeout(g, Duration::from_millis(500));
    }
}

fn handle(srv: &Arc<Server>, a: &Value, caller: Caller) -> Result<Value, String> {
    let cmd = arg_str(a, "cmd").ok_or("falta cmd")?;
    {
        let mut m = srv.lock();
        check_scope(&m, caller, cmd, a)?;
        audit(&m, caller, a);
        if let Some(text) = needs_approval(&m, caller, cmd, a) {
            let id = ask(&mut m, "action", caller.0.unwrap_or(0), text, String::new());
            drop(m);
            srv.notify();
            if !wait_approval(srv, id, Duration::from_secs(120)) { return Err("negado pelo usuario".into()); }
        }
    }
    // Comandos que bloqueiam nao podem segurar o lock.
    if cmd == "wait" { return wait(srv, a); }
    let mut m = srv.lock();
    let m = &mut *m;
    let r = match cmd {
        "ping" => proto::ok(json!({"version": env!("CARGO_PKG_VERSION"), "pid": std::process::id()})),
        "doctor" => proto::ok(json!({
            "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id(),
            "panes": m.panes.len(), "workspaces": m.workspaces.len(),
            "hosts_vivos": ptyhost::alive().len(), "recuperados_no_start": m.recovered,
            "config": config::config_path(), "config_existe": config::config_path().exists(),
            "log": proto::data_dir().join("server.log"), "audit": proto::data_dir().join("audit.log"),
            "estado": proto::state_dir(), "api_scope": m.cfg.api_scope, "clipboard_write": m.cfg.clipboard_write,
            "shell_integration": m.cfg.shell_integration,
        })),
        "list" => {
            let ws: Vec<Value> = m.workspaces.iter().map(|w| json!({
                "name": w.name, "active_tab": w.active,
                "tabs": w.tabs.iter().map(|t| json!({
                    "id": t.id, "title": t.title, "active_pane": t.active, "color": t.color,
                    "panes": t.all().iter().filter_map(|id| m.panes.get(id).map(|p| json!({
                        "id": id, "title": p.title, "cwd": p.cwd, "domain": p.domain,
                        "agent": p.agent, "attention": p.attention, "parent": p.parent,
                        "floating": t.floating == Some(*id), "private": p.private,
                        "idle_secs": p.last_output.elapsed().as_secs(),
                    }))).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })).collect();
            proto::ok(json!({"workspaces": ws}))
        }
        "frame" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            frame(srv, m, &client)?
        }
        "resize" => {
            let c = a.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            let r = a.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            m.size = (c.max(2), r.max(2));
            proto::ok(json!({}))
        }
        "window-focus" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let f = a.get("focused").and_then(|v| v.as_bool()).unwrap_or(true);
            client_ws(m, srv, &client)?;
            m.clients.get_mut(&client).unwrap().focused = f;
            proto::ok(json!({}))
        }
        "raise" => {
            // Toast "Abrir": a janela vem para a frente.
            let p = arg_id(a, "pane").unwrap_or(0);
            emit(m, "raise", p);
            proto::ok(json!({}))
        }
        "theme-mode" => {
            m.light = arg_str(a, "mode") == Some("light");
            proto::ok(json!({}))
        }
        "approve" => {
            let id = a.get("id").and_then(|v| v.as_u64()).ok_or("falta id")?;
            let ok = a.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let Some(ap) = m.approvals.iter_mut().find(|x| x.id == id) else { return Err("pedido nao existe".into()) };
            ap.decided = Some(ok);
            if ok && ap.kind == "clipboard" {
                let text = ap.payload.clone();
                m.clipboard.0 += 1;
                m.clipboard.1 = text;
            }
            // Clipboard ninguem espera: sai ja. Acao: quem pediu le e remove.
            m.approvals.retain(|x| x.decided.is_none() || x.kind == "action");
            proto::ok(json!({}))
        }
        "spawn" => {
            let wh = arg_str(a, "where").unwrap_or("tab");
            let o = SpawnOpts {
                cwd: arg_str(a, "cwd").map(String::from),
                cmd: a.get("argv").and_then(parse_argv),
                domain: arg_str(a, "domain").filter(|d| !d.is_empty()).map(String::from),
                text: None,
                parent: caller.0,
            };
            let focus = a.get("focus").and_then(|v| v.as_bool()).unwrap_or(true);
            let new = spawn_into(srv, m, a, wh, o, focus)?;
            if let Some(t) = arg_str(a, "type").filter(|t| !t.is_empty()) {
                // Digita no shell novo quando ele terminar de subir (oh-my-posh, profile).
                let (s2, line) = (srv.clone(), format!("{t}\r"));
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(2500));
                    if let Some(p) = s2.lock().panes.get(&new) { p.write(line.as_bytes()); }
                });
            }
            proto::ok(json!({"pane": new}))
        }
        "send" => {
            let id = target(srv, m, a)?;
            let mut text = arg_str(a, "text").unwrap_or("").to_string();
            if a.get("enter").and_then(|v| v.as_bool()).unwrap_or(false) { text.push('\r'); }
            let p = m.panes.get_mut(&id).unwrap();
            if p.term.grid().display_offset() != 0 { p.term.scroll_display(Scroll::Bottom); }
            p.write(text.as_bytes());
            proto::ok(json!({}))
        }
        "paste" => {
            let id = target(srv, m, a)?;
            // Colar com seguranca: sem caracteres de controle (so tab/linha).
            let text: String = arg_str(a, "text").unwrap_or("").replace("\r\n", "\n")
                .chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect::<String>().replace('\n', "\r");
            let p = m.panes.get_mut(&id).unwrap();
            let bracketed = p.term.mode().contains(TermMode::BRACKETED_PASTE);
            let data = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
            p.term.scroll_display(Scroll::Bottom);
            p.write(data.as_bytes());
            proto::ok(json!({}))
        }
        "read" => {
            let id = target(srv, m, a)?;
            let n = a.get("lines").and_then(|v| v.as_u64()).map(|v| v as usize);
            let p = &m.panes[&id];
            let n = if a.get("all").and_then(|v| v.as_bool()).unwrap_or(false) { None } else { Some(n.unwrap_or(p.rows as usize)) };
            proto::ok(json!({"pane": id, "text": pane_text(&p.term, n)}))
        }
        "last-output" => {
            let id = target(srv, m, a)?;
            m.panes.get_mut(&id).unwrap().resolve_marks();
            let p = &m.panes[&id];
            let k = p.marks.iter().rev().find(|k| k.out_start.is_some() && k.out_end.is_some())
                .ok_or("nenhum comando terminado neste pane (a shell integration esta ligada?)")?;
            let text = abs_text(p, k.out_start.unwrap(), k.out_end.unwrap());
            proto::ok(json!({"pane": id, "text": text, "exit": k.exit, "ms": k.dur_ms}))
        }
        "blocks" => {
            // Comandos do pane (shell integration): texto, exit, duracao, linhas.
            let id = target(srv, m, a)?;
            let p = m.panes.get_mut(&id).unwrap();
            p.resolve_marks();
            let list: Vec<Value> = p.marks.iter().map(|k| json!({"cmd": k.command, "exit": k.exit, "ms": k.dur_ms,
                "prompt": k.prompt, "input": k.input, "out_start": k.out_start, "out_end": k.out_end})).collect();
            proto::ok(json!({"pane": id, "history": p.term.history_size(), "offset": p.term.grid().display_offset(), "blocks": list}))
        }
        "last-failure" => {
            let id = target(srv, m, a)?;
            m.panes.get_mut(&id).unwrap().resolve_marks();
            let p = &m.panes[&id];
            let k = p.marks.iter().rev().find(|k| k.exit.is_some_and(|e| e != 0) && k.out_start.is_some())
                .ok_or("nenhum comando falhou neste pane (a shell integration esta ligada?)")?;
            let text = abs_text(p, k.out_start.unwrap(), k.out_end.unwrap_or(k.out_start.unwrap() + 200));
            proto::ok(json!({"pane": id, "command": k.command, "exit": k.exit, "output": scrub(m, text), "cwd": p.cwd, "domain": p.domain}))
        }
        "history" => {
            // Mais recentes primeiro, sem repetir comando; filtros opcionais.
            let cwd = arg_str(a, "cwd");
            let prefix = arg_str(a, "prefix").unwrap_or("");
            let only_ok = a.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(500) as usize;
            let mut seen = std::collections::HashSet::new();
            let out: Vec<&Value> = m.history.iter().rev()
                .filter(|h| cwd.is_none_or(|c| h["cwd"].as_str() == Some(c)))
                .filter(|h| !only_ok || h["exit"] == 0)
                .filter(|h| h["cmd"].as_str().is_some_and(|c| c.starts_with(prefix)))
                .filter(|h| seen.insert(h["cmd"].as_str().unwrap_or("").to_string()))
                .take(limit).collect();
            proto::ok(json!({"history": out}))
        }
        "queue" => {
            let id = target(srv, m, a)?;
            let text = arg_str(a, "text").ok_or("falta text")?.to_string();
            let p = m.panes.get_mut(&id).unwrap();
            let idle = p.agent.as_ref().is_some_and(|ag| ag.state == "done" || ag.state == "idle");
            if idle {
                // Ja parado: entrega agora.
                deliver(p, &text);
                proto::ok(json!({"delivered": true}))
            } else {
                p.queue.push(text);
                proto::ok(json!({"queued": p.queue.len()}))
            }
        }
        "record" => {
            let id = target(srv, m, a)?;
            let p = m.panes.get_mut(&id).unwrap();
            match p.rec.take() {
                Some((_, _, path)) => proto::ok(json!({"recording": false, "path": path})),
                None => {
                    let dir = proto::data_dir().join("recordings");
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join(format!("pane{id}-{}.cast", now_secs())).to_string_lossy().to_string();
                    let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                    let head = json!({"version": 2, "width": p.cols, "height": p.rows, "timestamp": now_secs(), "title": p.title});
                    writeln!(f, "{head}").map_err(|e| e.to_string())?;
                    p.rec = Some((f, Instant::now(), path.clone()));
                    proto::ok(json!({"recording": true, "path": path}))
                }
            }
        }
        "jump" => {
            let id = target(srv, m, a)?;
            let p = m.panes.get_mut(&id).unwrap();
            p.resolve_marks();
            let h = p.term.history_size() as i64;
            let top = h - p.term.grid().display_offset() as i64;
            let up = arg_str(a, "dir") != Some("down");
            let k = if up { p.marks.iter().rev().find(|k| k.prompt < top).cloned() } else { p.marks.iter().find(|k| k.prompt > top).cloned() };
            match k {
                Some(k) => {
                    let want = (h - k.prompt).clamp(0, h);
                    let cur = p.term.grid().display_offset() as i64;
                    p.term.scroll_display(Scroll::Delta((want - cur) as i32));
                    proto::ok(json!({"found": true}))
                }
                None => {
                    if !up { p.term.scroll_display(Scroll::Bottom); }
                    proto::ok(json!({"found": false}))
                }
            }
        }
        "grep" => {
            let id = target(srv, m, a)?;
            let re = regex::RegexBuilder::new(arg_str(a, "regex").unwrap_or("")).case_insensitive(true).build().map_err(|e| e.to_string())?;
            let p = &m.panes[&id];
            let mut hits = Vec::new();
            for l in (p.term.topmost_line().0..=p.term.bottommost_line().0).rev() {
                let t = line_text(&p.term, l);
                if re.is_match(&t) { hits.push(json!({"line": l, "text": t})); if hits.len() >= 300 { break; } }
            }
            hits.reverse();
            proto::ok(json!({"hits": hits}))
        }
        "goto-line" => {
            let id = target(srv, m, a)?;
            let l = a.get("line").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let p = m.panes.get_mut(&id).unwrap();
            let screen = p.term.screen_lines() as i32;
            let want = if l >= 0 { 0 } else { (-l + screen / 2).max(0) };
            let cur = p.term.grid().display_offset() as i32;
            p.term.scroll_display(Scroll::Delta(want - cur));
            proto::ok(json!({}))
        }
        "close" => {
            let id = target(srv, m, a)?;
            close_pane(m, id);
            proto::ok(json!({}))
        }
        "private" => {
            let id = target(srv, m, a)?;
            let p = m.panes.get_mut(&id).unwrap();
            p.private = a.get("on").and_then(|v| v.as_bool()).unwrap_or(!p.private);
            proto::ok(json!({"private": p.private}))
        }
        "focus" => {
            let id = match (arg_id(a, "pane"), arg_str(a, "dir")) {
                (Some(id), _) => id,
                (None, Some(dir)) => neighbor(srv, m, a, dir)?,
                _ => return Err("focus: pane ou dir".into()),
            };
            let (wi, ti) = locate(m, id).ok_or("pane sem aba")?;
            let w = &mut m.workspaces[wi];
            w.active = ti;
            let t = &mut w.tabs[ti];
            if t.floating == Some(id) { t.float_visible = true; }
            t.active = id;
            let name = w.name.clone();
            if let Some(c) = arg_str(a, "client") { view(m, c, &name); }
            if let Some(p) = m.panes.get_mut(&id) { p.attention = false; }
            proto::ok(json!({"pane": id}))
        }
        "tab" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let w = &mut m.workspaces[wi];
            let n = w.tabs.len() as i64;
            let idx = if let Some(d) = a.get("delta").and_then(|v| v.as_i64()) {
                (w.active as i64 + d).rem_euclid(n)
            } else {
                (a.get("index").and_then(|v| v.as_i64()).unwrap_or(1) - 1).clamp(0, n - 1)
            };
            w.active = idx as usize;
            proto::ok(json!({}))
        }
        "move-tab" => {
            // delta (relativo a aba ativa) | from/to (1-based, arrastar) | workspace (aba vira outra janela).
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let n = m.workspaces[wi].tabs.len() as i64;
            let from = a.get("from").and_then(|v| v.as_i64()).map(|f| (f - 1).clamp(0, n - 1) as usize).unwrap_or(m.workspaces[wi].active);
            if let Some(dest) = arg_str(a, "workspace") {
                let dest = clean(dest, 100);
                if n < 2 { return Err("a unica aba do workspace nao sai dele".into()); }
                let tab = m.workspaces[wi].tabs.remove(from);
                let w = &mut m.workspaces[wi];
                w.active = w.active.min(w.tabs.len() - 1);
                match m.workspaces.iter_mut().find(|w| w.name == dest) {
                    Some(w) => { w.tabs.push(tab); w.active = w.tabs.len() - 1; }
                    None => m.workspaces.push(Workspace { name: dest.clone(), tabs: vec![tab], active: 0 }),
                }
                return Ok(proto::ok(json!({"workspace": dest})));
            }
            let w = &mut m.workspaces[wi];
            let to = match a.get("to").and_then(|v| v.as_i64()) {
                Some(t) => (t - 1).clamp(0, n - 1) as usize,
                None => (from as i64 + a.get("delta").and_then(|v| v.as_i64()).unwrap_or(1)).clamp(0, n - 1) as usize,
            };
            let t = w.tabs.remove(from);
            w.tabs.insert(to, t);
            w.active = to;
            proto::ok(json!({}))
        }
        "close-tab" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let w = &m.workspaces[wi];
            let i = a.get("index").and_then(|v| v.as_i64()).map(|i| (i - 1).max(0) as usize).unwrap_or(w.active);
            let ids = w.tabs.get(i).map(|t| t.all()).ok_or("aba inexistente")?;
            for id in ids { close_pane(m, id); }
            proto::ok(json!({"closed": i + 1}))
        }
        "block-output" => {
            let id = target(srv, m, a)?;
            m.panes.get_mut(&id).unwrap().resolve_marks();
            let p = &m.panes[&id];
            let abs = a.get("abs").and_then(|v| v.as_i64()).ok_or("falta abs")?;
            let k = p.marks.iter().find(|k| k.prompt == abs).ok_or("bloco nao encontrado")?;
            let end = k.out_end.unwrap_or(p.term.history_size() as i64 + p.term.bottommost_line().0 as i64 + 1);
            let text = k.out_start.map(|s| abs_text(p, s, end)).unwrap_or_default();
            proto::ok(json!({"pane": id, "cmd": k.command, "text": text, "exit": k.exit, "ms": k.dur_ms}))
        }
        "search-all" => {
            // Todas as linhas que casam (para contador e marcas na barra de rolagem).
            let id = target(srv, m, a)?;
            let q = arg_str(a, "query").unwrap_or("");
            let pat = if q.len() > 2 && q.starts_with('/') && q.ends_with('/') { q[1..q.len() - 1].to_string() } else { regex::escape(q) };
            let re = regex::RegexBuilder::new(&pat).case_insensitive(true).build().map_err(|e| e.to_string())?;
            let p = &m.panes[&id];
            let mut lines = Vec::new();
            if !q.is_empty() {
                for l in p.term.topmost_line().0..=p.term.bottommost_line().0 {
                    if re.is_match(&line_text(&p.term, l)) { lines.push(l); if lines.len() >= 5000 { break; } }
                }
            }
            let history = p.term.history_size() as i64;
            proto::ok(json!({"lines": lines, "history": history, "total": history + p.term.screen_lines() as i64}))
        }
        "rename-tab" | "tab-color" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let w = &mut m.workspaces[wi];
            let t = w.active;
            if cmd == "rename-tab" { w.tabs[t].title = clean(arg_str(a, "title").unwrap_or(""), 100); }
            else { w.tabs[t].color = arg_str(a, "color").unwrap_or("").into(); }
            proto::ok(json!({}))
        }
        "rename-workspace" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let new = clean(arg_str(a, "name").ok_or("falta name")?, 100);
            let old = std::mem::replace(&mut m.workspaces[wi].name, new.clone());
            for c in m.clients.values_mut() { if c.workspace == old { c.workspace = new.clone(); } }
            proto::ok(json!({}))
        }
        "workspace" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let name = clean(arg_str(a, "name").ok_or("falta name")?, 100);
            if let Some(cwd) = arg_str(a, "cwd") {
                if !m.workspaces.iter().any(|w| w.name == name) {
                    let id = spawn_pane(srv, m, SpawnOpts { cwd: Some(cwd.into()), ..SpawnOpts::shell() })?;
                    m.workspaces.push(Workspace { name: name.clone(), tabs: vec![Tab::new(id, "")], active: 0 });
                }
            }
            ensure_workspace(m, srv, &name)?;
            view(m, &client, &name);
            proto::ok(json!({}))
        }
        "open-layout" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let spec = a.get("spec").ok_or("falta spec")?;
            let name = open_layout(srv, m, spec)?;
            view(m, &client, &name);
            proto::ok(json!({"workspace": name}))
        }
        "zoom" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let w = &mut m.workspaces[wi];
            let t = w.active;
            w.tabs[t].zoomed = !w.tabs[t].zoomed;
            proto::ok(json!({}))
        }
        "float" => {
            // Pane flutuante: liga/desliga e continua rodando escondido.
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let ti = m.workspaces[wi].active;
            // Popup: troca o flutuante atual por um novo rodando o comando.
            if a.get("replace").and_then(|v| v.as_bool()).unwrap_or(false) {
                if let Some(f) = m.workspaces[wi].tabs[ti].floating { close_pane(m, f); }
            }
            let wi = client_ws(m, srv, &client)?;
            let ti = m.workspaces[wi].active;
            match m.workspaces[wi].tabs[ti].floating {
                Some(f) => {
                    let t = &mut m.workspaces[wi].tabs[ti];
                    t.float_visible = !t.float_visible;
                    if t.float_visible { t.float_prev = t.active; t.active = f; }
                    else if t.active == f { t.active = if t.root.contains(t.float_prev) { t.float_prev } else { t.root.leaves()[0] }; }
                    proto::ok(json!({"pane": f, "visible": t.float_visible}))
                }
                None => {
                    let src_cwd = m.panes.get(&m.workspaces[wi].tabs[ti].active).map(|p| p.cwd.clone()).filter(|c| !c.is_empty());
                    let o = SpawnOpts {
                        cwd: arg_str(a, "cwd").map(String::from).or(src_cwd),
                        cmd: a.get("argv").and_then(parse_argv),
                        domain: arg_str(a, "domain").filter(|d| !d.is_empty()).map(String::from),
                        text: None, parent: caller.0,
                    };
                    let id = spawn_pane(srv, m, o)?;
                    let t = &mut m.workspaces[wi].tabs[ti];
                    t.floating = Some(id);
                    t.float_visible = true;
                    t.float_prev = t.active;
                    t.active = id;
                    proto::ok(json!({"pane": id, "visible": true}))
                }
            }
        }
        "adjust" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let wi = client_ws(m, srv, &client)?;
            let dir = arg_str(a, "dir").unwrap_or("right");
            let n = a.get("amount").and_then(|v| v.as_i64()).unwrap_or(2) as i32;
            let (vertical, delta) = match dir { "left" => (true, -n), "right" => (true, n), "up" => (false, -n), _ => (false, n) };
            let area = Rect { x: 0, y: 0, w: m.size.0, h: m.size.1 };
            let w = &mut m.workspaces[wi];
            let t = w.active;
            let tab = &mut w.tabs[t];
            // Arrastar o divisor com o mouse manda o pane do lado.
            let active = arg_id(a, "pane").filter(|id| tab.root.contains(*id)).unwrap_or(tab.active);
            proto::ok(json!({"moved": tab.root.adjust(area, active, vertical, delta)}))
        }
        "scroll" => {
            let id = target(srv, m, a)?;
            let p = m.panes.get_mut(&id).unwrap();
            let s = match arg_str(a, "to") {
                Some("bottom") => Scroll::Bottom,
                Some("top") => Scroll::Top,
                Some("page_up") => Scroll::PageUp,
                Some("page_down") => Scroll::PageDown,
                _ => Scroll::Delta(a.get("delta").and_then(|v| v.as_i64()).unwrap_or(0) as i32),
            };
            p.term.scroll_display(s);
            proto::ok(json!({"offset": p.term.grid().display_offset()}))
        }
        "search" => {
            let id = target(srv, m, a)?;
            let q = arg_str(a, "query").unwrap_or("");
            if q.is_empty() { return Ok(proto::ok(json!({"found": false}))); }
            // "/regex/" busca por regex; senao texto simples sem caixa.
            let pat = if q.len() > 2 && q.starts_with('/') && q.ends_with('/') { q[1..q.len() - 1].to_string() } else { regex::escape(q) };
            let re = regex::RegexBuilder::new(&pat).case_insensitive(true).build().map_err(|e| e.to_string())?;
            let p = m.panes.get_mut(&id).unwrap();
            let offset = p.term.grid().display_offset() as i32;
            let start = a.get("from").and_then(|v| v.as_i64()).map(|v| v as i32).unwrap_or(-offset + p.term.screen_lines() as i32 - 1);
            let top = p.term.topmost_line().0;
            let mut found = None;
            let mut l = start;
            while l >= top {
                if re.is_match(&line_text(&p.term, l)) { found = Some(l); break; }
                l -= 1;
            }
            match found {
                Some(l) => {
                    let screen = p.term.screen_lines() as i32;
                    let want = if l >= 0 && l < screen { 0 } else { (-l + screen / 2).max(0) };
                    let cur = p.term.grid().display_offset() as i32;
                    p.term.scroll_display(Scroll::Delta(want - cur));
                    proto::ok(json!({"found": true, "line": l}))
                }
                None => proto::ok(json!({"found": false})),
            }
        }
        "set-state" => {
            let id = arg_id(a, "pane").or(caller.0).ok_or("falta pane")?;
            let state = arg_str(a, "state").ok_or("falta state")?;
            set_state(m, id, state, arg_str(a, "agent"), arg_str(a, "source").unwrap_or("hook"));
            if let Some(p) = m.panes.get_mut(&id) {
                if let Some(ag) = p.agent.as_mut() {
                    if let Some(s) = arg_str(a, "session") { ag.session = Some(clean(s, 100)); }
                    if let Some(d) = arg_str(a, "config_dir") { ag.config_dir = Some(d.into()).filter(|d: &String| !d.is_empty()); }
                    if let Some(t) = a.get("context_tokens").and_then(|v| v.as_u64()) { ag.context_tokens = Some(t); }
                    if let Some(t) = a.get("session_tokens").and_then(|v| v.as_u64()) { ag.session_tokens = Some(t); }
                    if let (Some(d), Some(t)) = (a.get("todo_done").and_then(|v| v.as_u64()), a.get("todo_total").and_then(|v| v.as_u64())) {
                        ag.todo_done = Some(d as u32);
                        ag.todo_total = Some(t as u32);
                    }
                }
                if let Some(c) = arg_str(a, "cwd") { if !c.is_empty() { p.cwd = clean(c, 1024); } }
            }
            // Orcamento de tokens: avisa uma vez quando a soma passa do limite.
            let total: u64 = m.panes.values().filter_map(|p| p.agent.as_ref()?.session_tokens).sum();
            if m.cfg.token_budget > 0 && total > m.cfg.token_budget && !m.budget_warned {
                m.budget_warned = true;
                emit(m, "budget", id);
                if let Some(e) = m.events.last_mut() { e.1["tokens"] = json!(total); }
            }
            proto::ok(json!({}))
        }
        "clear-attention" => {
            let id = target(srv, m, a)?;
            m.panes.get_mut(&id).unwrap().attention = false;
            proto::ok(json!({}))
        }
        "gui-action" => {
            // Dispara uma acao da janela (paleta, home, tema...). So o dono:
            // um pane nao comanda a GUI.
            if caller.0.is_some() { return Err("gui-action: so fora dos panes".into()); }
            m.event_seq += 1;
            let seq = m.event_seq;
            m.events.push((seq, json!({"event": "gui-action", "name": arg_str(a, "name").unwrap_or(""), "args": a.get("args").cloned().unwrap_or(json!([]))})));
            if m.events.len() > 64 { m.events.remove(0); }
            proto::ok(json!({}))
        }
        "events" => {
            let since = a.get("since").and_then(|v| v.as_u64()).unwrap_or(0);
            let ev: Vec<Value> = m.events.iter().filter(|(s, _)| *s > since).map(|(s, e)| {
                let mut e = e.clone(); e["seq"] = json!(s); e
            }).collect();
            proto::ok(json!({"events": ev}))
        }
        "save" => {
            let client = arg_str(a, "client").unwrap_or("cli").to_string();
            let names: Vec<String> = match arg_str(a, "workspace") {
                Some("*") => m.workspaces.iter().map(|w| w.name.clone()).collect(),
                Some(n) => vec![n.to_string()],
                None => { let wi = client_ws(m, srv, &client)?; vec![m.workspaces[wi].name.clone()] }
            };
            for n in &names { save_workspace(m, n)?; }
            proto::ok(json!({"saved": names}))
        }
        "states" => {
            let mut v: Vec<(u64, String)> = std::fs::read_dir(proto::state_dir()).map_err(|e| e.to_string())?
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json") && e.file_name() != "checkpoint.json")
                .map(|e| (e.metadata().and_then(|m| m.modified()).ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
                    e.path().file_stem().unwrap().to_string_lossy().to_string()))
                .collect();
            v.sort_by(|a, b| b.0.cmp(&a.0));
            proto::ok(json!({"states": v.iter().map(|(t, n)| json!({"name": n, "saved_at": t})).collect::<Vec<_>>()}))
        }
        "restore" => {
            let name = arg_str(a, "name").ok_or("falta name")?.to_string();
            let ws = restore_workspace(srv, m, &name)?;
            if let Some(c) = arg_str(a, "client") { view(m, c, &ws); }
            proto::ok(json!({"workspace": ws}))
        }
        "delete-state" => {
            let name = arg_str(a, "name").ok_or("falta name")?;
            std::fs::remove_file(state_file(name)).map_err(|e| e.to_string())?;
            proto::ok(json!({}))
        }
        "checkpoint" => {
            let (names, agents) = checkpoint(m)?;
            proto::ok(json!({"workspaces": names, "agents": agents}))
        }
        "checkpoint-restore" => {
            m.resurrect = false;
            let cp: Value = serde_json::from_slice(&std::fs::read(proto::state_dir().join("checkpoint.json"))
                .map_err(|_| "nenhum checkpoint salvo")?).map_err(|e| e.to_string())?;
            let mut restored = Vec::new();
            for n in cp["workspaces"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
                // Ja aberto com conteudo (server nao reiniciou): nao duplica.
                let open = m.workspaces.iter().any(|w| w.name == n && (w.tabs.len() > 1 || w.tabs.iter().any(|t| t.root.leaves().len() > 1)));
                if open { continue; }
                // Workspace "vazio" criado pela GUI ao abrir: substitui.
                if let Some(i) = m.workspaces.iter().position(|w| w.name == n) {
                    let ids: Vec<Id> = m.workspaces[i].tabs.iter().flat_map(|t| t.all()).collect();
                    for id in ids { close_pane(m, id); }
                }
                restored.push(restore_workspace(srv, m, n)?);
            }
            if let (Some(c), Some(first)) = (arg_str(a, "client"), restored.first()) { view(m, c, first); }
            proto::ok(json!({"restored": restored}))
        }
        "resurrect-dismiss" => { m.resurrect = false; proto::ok(json!({})) }
        "reload" => {
            m.cfg = config::load_config().0;
            m.secrets = m.cfg.secret_patterns.iter().filter_map(|r| regex::Regex::new(r).ok()).collect();
            proto::ok(json!({}))
        }
        "restart-server" => {
            // Atualizar o Duck sem perder nada: grava o layout e sai; os shells
            // ficam nos ptyhosts e o proximo server (a janela sobe um) reconecta.
            if caller.0.is_some() { return Err("restart-server: so fora dos panes".into()); }
            let snap = serde_json::to_string(&live_snapshot(m)).map_err(|e| e.to_string())?;
            std::fs::write(proto::data_dir().join("live.json"), snap).map_err(|e| e.to_string())?;
            std::thread::spawn(|| { std::thread::sleep(Duration::from_millis(300)); std::process::exit(0); });
            proto::ok(json!({"restarting": true}))
        }
        // Extensoes (src/ext.rs): so o dono; um pane nao fala com elas.
        "ext-send" | "ext-restart" => {
            if caller.0.is_some() { return Err(format!("{cmd}: so fora dos panes")); }
            let ext = arg_str(a, "ext").unwrap_or("");
            if cmd == "ext-restart" {
                m.ext.restart(Some(ext).filter(|e| !e.is_empty()), &ext_sink(srv));
            } else {
                let msg = a.get("msg").cloned().ok_or("falta msg")?;
                match arg_str(a, "perm") { Some(p) => m.ext.send_perm(ext, p, msg), None => m.ext.send(ext, msg) }
            }
            proto::ok(json!({}))
        }
        "kill-server" => {
            for p in m.panes.values() { let _ = p.io.send(Io::Kill); }
            let _ = std::fs::remove_file(proto::data_dir().join("live.json"));
            std::thread::sleep(Duration::from_millis(300));
            std::process::exit(0);
        }
        other => return Err(format!("comando desconhecido: {other}")),
    };
    Ok(r)
}

fn split_cmd(s: &str) -> Vec<String> {
    // Aspas simples/duplas agrupam; o resto separa por espaco.
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for c in s.chars() {
        match (q, c) {
            (None, '"' | '\'') => q = Some(c),
            (Some(x), c) if c == x => q = None,
            (None, ' ') => if !cur.is_empty() { out.push(std::mem::take(&mut cur)); },
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

fn spawn_into(srv: &Arc<Server>, m: &mut Mux, a: &Value, wh: &str, o: SpawnOpts, focus: bool) -> Result<Id, String> {
    match wh {
        "right" | "down" | "left" | "up" => {
            let t = target(srv, m, a)?;
            let (wi, ti) = locate(m, t).ok_or("pane sem aba")?;
            // Flutuante nao divide: o split vai para o pane debaixo dele.
            let t = { let tab = &m.workspaces[wi].tabs[ti]; if tab.floating == Some(t) { tab.float_prev } else { t } };
            // Herda o cwd e o dominio do pane de origem, como o split do WezTerm.
            let src = &m.panes[&t];
            let o = SpawnOpts {
                cwd: o.cwd.or_else(|| Some(src.cwd.clone()).filter(|c| !c.is_empty())),
                domain: o.domain.or_else(|| Some(src.domain.clone()).filter(|d| !d.is_empty())),
                ..o
            };
            let new = spawn_pane(srv, m, o)?;
            let tab = &mut m.workspaces[wi].tabs[ti];
            tab.zoomed = false;
            tab.float_visible = false;
            tab.root.split_leaf(t, new, wh == "right" || wh == "left");
            if wh == "left" || wh == "up" {
                fn swap(n: &mut Node, new: Id) {
                    if let Node::Split { a, b, .. } = n {
                        if matches!(**b, Node::Leaf(x) if x == new) { std::mem::swap(a, b); return; }
                        swap(a, new); swap(b, new);
                    }
                }
                swap(&mut tab.root, new);
            }
            if focus { tab.active = new; }
            Ok(new)
        }
        _ => {
            let title = clean(arg_str(a, "title").unwrap_or(""), 100);
            let wi = if let Some(ws) = arg_str(a, "workspace") {
                match m.workspaces.iter().position(|w| w.name == ws) {
                    Some(i) => i,
                    None => {
                        // Workspace novo nasce ja com este pane (sem aba extra).
                        let new = spawn_pane(srv, m, o)?;
                        m.workspaces.push(Workspace { name: clean(ws, 100), tabs: vec![Tab::new(new, &title)], active: 0 });
                        return Ok(new);
                    }
                }
            } else if let Some(p) = arg_id(a, "pane") {
                locate(m, p).ok_or("pane sem aba")?.0
            } else {
                client_ws(m, srv, arg_str(a, "client").unwrap_or("cli"))?
            };
            let new = spawn_pane(srv, m, o)?;
            let w = &mut m.workspaces[wi];
            w.tabs.push(Tab::new(new, &title));
            if focus { w.active = w.tabs.len() - 1; }
            Ok(new)
        }
    }
}

/// Layout declarativo (config `layouts`): {name, cwd, tabs=[{title, color,
/// layout = {cmd, cwd, domain} | {split='right'|'down', ratio, a, b}}]}.
fn open_layout(srv: &Arc<Server>, m: &mut Mux, spec: &Value) -> Result<String, String> {
    let base = spec["name"].as_str().unwrap_or("layout").to_string();
    let mut name = base.clone();
    let mut i = 2;
    while m.workspaces.iter().any(|w| w.name == name) { name = format!("{base} {i}"); i += 1; }
    let cwd = spec["cwd"].as_str().map(String::from);
    fn build(srv: &Arc<Server>, m: &mut Mux, n: &Value, cwd: &Option<String>) -> Result<Node, String> {
        if let Some(dir) = n["split"].as_str() {
            let a = build(srv, m, &n["a"], cwd)?;
            let b = build(srv, m, &n["b"], cwd)?;
            return Ok(Node::Split { vertical: dir != "down", ratio: n["ratio"].as_f64().unwrap_or(0.5) as f32, a: Box::new(a), b: Box::new(b) });
        }
        let id = spawn_pane(srv, m, SpawnOpts {
            cwd: n["cwd"].as_str().map(String::from).or_else(|| cwd.clone()),
            cmd: parse_argv(&n["cmd"]),
            domain: n["domain"].as_str().filter(|d| !d.is_empty()).map(String::from), text: None, parent: None,
        })?;
        Ok(Node::Leaf(id))
    }
    let mut tabs = Vec::new();
    for t in spec["tabs"].as_array().ok_or("layout sem tabs")? {
        let root = build(srv, m, &t["layout"], &cwd)?;
        let first = root.leaves()[0];
        let mut tab = Tab::new(first, t["title"].as_str().unwrap_or(""));
        tab.root = root;
        tab.color = t["color"].as_str().unwrap_or("").into();
        tabs.push(tab);
    }
    if tabs.is_empty() { return Err("layout sem abas".into()); }
    m.workspaces.push(Workspace { name: name.clone(), tabs, active: 0 });
    Ok(name)
}

fn neighbor(srv: &Arc<Server>, m: &mut Mux, a: &Value, dir: &str) -> Result<Id, String> {
    let client = arg_str(a, "client").unwrap_or("cli").to_string();
    let wi = client_ws(m, srv, &client)?;
    let w = &m.workspaces[wi];
    let t = &w.tabs[w.active];
    let mut rects = Vec::new();
    t.root.layout(Rect { x: 0, y: 0, w: m.size.0, h: m.size.1 }, &mut rects);
    let Some(cur) = rects.iter().find(|(id, _)| *id == t.active).map(|x| x.1) else { return Ok(t.active) };
    let (cx, cy) = (cur.x as i32 * 2 + cur.w as i32, cur.y as i32 * 2 + cur.h as i32);
    let best = rects.iter().filter(|(id, r)| *id != t.active && match dir {
        "left" => r.x + r.w <= cur.x,
        "right" => r.x >= cur.x + cur.w,
        "up" => r.y + r.h <= cur.y,
        _ => r.y >= cur.y + cur.h,
    }).min_by_key(|(_, r)| {
        let (x, y) = (r.x as i32 * 2 + r.w as i32, r.y as i32 * 2 + r.h as i32);
        (x - cx).abs() + (y - cy).abs()
    });
    Ok(best.map(|b| b.0).unwrap_or(t.active))
}

fn wait(srv: &Arc<Server>, a: &Value) -> Result<Value, String> {
    let id = arg_id(a, "pane").ok_or("falta pane")?;
    let want: Vec<String> = arg_str(a, "state").unwrap_or("blocked,done,idle").split(',').map(String::from).collect();
    let timeout = Duration::from_secs_f64(a.get("timeout").and_then(|v| v.as_f64()).unwrap_or(600.0));
    let start = Instant::now();
    loop {
        {
            let m = srv.lock();
            let Some(p) = m.panes.get(&id) else { return Ok(proto::ok(json!({"state": "closed"}))) };
            let st = p.agent.as_ref().map(|a| a.state.clone()).unwrap_or_default();
            if want.contains(&st) { return Ok(proto::ok(json!({"state": st}))); }
        }
        if start.elapsed() > timeout { return Err("timeout".into()); }
        let g = srv.gen.lock().unwrap_or_else(|e| e.into_inner());
        let _ = srv.cv.wait_timeout(g, Duration::from_secs(2));
    }
}

// ------------------------------------------------------------------ persistencia

#[derive(Serialize, Deserialize)]
enum Saved {
    Leaf { cwd: String, domain: String, cmd: Vec<String>, text: String, agent: Option<Agent> },
    Split { vertical: bool, ratio: f32, a: Box<Saved>, b: Box<Saved> },
}

#[derive(Serialize, Deserialize)]
struct SavedTab { title: String, zoomed: bool, active: usize, root: Saved, #[serde(default)] color: String }

#[derive(Serialize, Deserialize)]
struct SavedWs { name: String, saved_at: u64, active: usize, tabs: Vec<SavedTab> }

fn state_file(name: &str) -> std::path::PathBuf {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_ .".contains(c) { c } else { '_' }).collect();
    proto::state_dir().join(format!("{safe}.json"))
}

fn save_node(m: &Mux, n: &Node) -> Saved {
    match n {
        Node::Leaf(id) => {
            let p = &m.panes[id];
            Saved::Leaf {
                cwd: p.cwd.clone(), domain: p.domain.clone(), cmd: p.cmd.clone(),
                // Privado nao vai para o disco; o resto passa pelo filtro de segredos.
                text: if p.private { String::new() } else { scrub(m, pane_text(&p.term, Some(200))) },
                agent: p.agent.clone().filter(|a| a.session.is_some()),
            }
        }
        Node::Split { vertical, ratio, a, b } => Saved::Split {
            vertical: *vertical, ratio: *ratio, a: Box::new(save_node(m, a)), b: Box::new(save_node(m, b)),
        },
    }
}

fn save_workspace(m: &Mux, name: &str) -> Result<(), String> {
    let w = m.workspaces.iter().find(|w| w.name == name).ok_or(format!("workspace {name} nao existe"))?;
    let s = SavedWs {
        name: name.into(), saved_at: now_secs(), active: w.active,
        tabs: w.tabs.iter().map(|t| SavedTab {
            title: t.title.clone(), zoomed: t.zoomed, color: t.color.clone(),
            active: t.root.leaves().iter().position(|x| *x == t.active).unwrap_or(0),
            root: save_node(m, &t.root),
        }).collect(),
    };
    std::fs::write(state_file(name), serde_json::to_vec(&s).unwrap()).map_err(|e| e.to_string())
}

fn checkpoint(m: &Mux) -> Result<(Vec<String>, usize), String> {
    let names: Vec<String> = m.workspaces.iter().map(|w| w.name.clone()).collect();
    for n in &names { save_workspace(m, n)?; }
    let agents: Vec<Value> = m.panes.values().filter_map(|p| p.agent.as_ref().filter(|a| a.session.is_some())
        .map(|a| json!({"cwd": p.cwd, "domain": p.domain, "session": a.session, "agent": a.name, "config_dir": a.config_dir}))).collect();
    let n = agents.len();
    let cp = json!({"saved_at": now_secs(), "workspaces": names, "agents": agents});
    std::fs::write(proto::state_dir().join("checkpoint.json"), serde_json::to_vec_pretty(&cp).unwrap()).map_err(|e| e.to_string())?;
    Ok((names, n))
}

/// Comando que retoma a sessao do agente, no shell do pane (o shell fica
/// depois que o agente sai).
fn resume_line(domain_cmd: &[String], a: &Agent, extra: &str) -> Option<String> {
    let session = a.session.as_ref()?;
    let bin = if a.name.is_empty() { "claude" } else { &a.name };
    // Opcoes da config (ex.: --dangerously-skip-permissions) voltam junto.
    let session = &if extra.trim().is_empty() { session.clone() } else { format!("{session} {}", extra.trim()) };
    let wsl = domain_cmd.first().is_some_and(|c| c.to_lowercase().contains("wsl"));
    Some(match (&a.config_dir, wsl) {
        (Some(d), true) => format!("CLAUDE_CONFIG_DIR='{d}' {bin} --resume {session}\r"),
        (Some(d), false) => format!("$env:CLAUDE_CONFIG_DIR='{d}'; {bin} --resume {session}\r"),
        (None, _) => format!("{bin} --resume {session}\r"),
    })
}

fn restore_node(srv: &Arc<Server>, m: &mut Mux, s: &Saved, pending: &mut Vec<(Id, String)>) -> Result<Node, String> {
    match s {
        Saved::Leaf { cwd, domain, cmd, text, agent } => {
            let id = spawn_pane(srv, m, SpawnOpts {
                cwd: Some(cwd.clone()).filter(|c| !c.is_empty()),
                cmd: Some(cmd.clone()).filter(|c| !c.is_empty()),
                domain: Some(domain.clone()).filter(|d| !d.is_empty() && m.cfg.domains.iter().any(|x| &x.name == d)),
                text: Some(text.clone()),
                parent: None,
            })?;
            if let Some(a) = agent {
                let dcmd = m.cfg.domains.iter().find(|d| &d.name == domain).map(|d| d.cmd.clone()).unwrap_or_default();
                if let Some(line) = resume_line(&dcmd, a, &m.cfg.resume_args) { pending.push((id, line)); }
            }
            Ok(Node::Leaf(id))
        }
        Saved::Split { vertical, ratio, a, b } => Ok(Node::Split {
            vertical: *vertical, ratio: *ratio,
            a: Box::new(restore_node(srv, m, a, pending)?),
            b: Box::new(restore_node(srv, m, b, pending)?),
        }),
    }
}

fn restore_workspace(srv: &Arc<Server>, m: &mut Mux, name: &str) -> Result<String, String> {
    let s: SavedWs = serde_json::from_slice(&std::fs::read(state_file(name)).map_err(|_| format!("estado {name} nao existe"))?)
        .map_err(|e| e.to_string())?;
    let mut ws_name = s.name.clone();
    let mut i = 2;
    while m.workspaces.iter().any(|w| w.name == ws_name) { ws_name = format!("{} {i}", s.name); i += 1; }
    let mut tabs = Vec::new();
    let mut pending = Vec::new();
    for t in &s.tabs {
        let root = restore_node(srv, m, &t.root, &mut pending)?;
        let leaves = root.leaves();
        let mut tab = Tab::new(leaves[0], &t.title);
        tab.active = leaves[t.active.min(leaves.len() - 1)];
        tab.root = root;
        tab.zoomed = t.zoomed;
        tab.color = t.color.clone();
        tabs.push(tab);
    }
    let active = s.active.min(tabs.len().saturating_sub(1));
    m.workspaces.push(Workspace { name: ws_name.clone(), tabs, active });
    // O shell precisa subir antes de receber o comando de retomada.
    let s2 = srv.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        let m = s2.lock();
        for (id, line) in pending {
            if let Some(p) = m.panes.get(&id) { p.write(line.as_bytes()); }
        }
    });
    Ok(ws_name)
}

/// Autosave: estado de cada workspace + checkpoint (o que o server oferece
/// para restaurar depois de um reboot).
fn autosave_loop(srv: Arc<Server>) {
    loop {
        let mins = srv.lock().cfg.autosave_minutes;
        std::thread::sleep(Duration::from_secs(60 * mins.max(1)));
        if mins == 0 { continue; }
        let m = srv.lock();
        if m.workspaces.is_empty() { continue; }
        if let Err(e) = checkpoint(&m) { proto::log(&format!("autosave: {e}")); }
    }
}

// ------------------------------------------------------------------ live.json

#[derive(Serialize, Deserialize)]
struct LiveTab { title: String, root: Node, active: Id, zoomed: bool, floating: Option<Id>, color: String }
#[derive(Serialize, Deserialize)]
struct LiveWs { name: String, active: usize, tabs: Vec<LiveTab> }
#[derive(Serialize, Deserialize)]
struct Live { workspaces: Vec<LiveWs>, panes: HashMap<Id, PaneMeta>, next: Id }

fn live_snapshot(m: &Mux) -> Live {
    Live {
        next: m.next,
        workspaces: m.workspaces.iter().map(|w| LiveWs {
            name: w.name.clone(), active: w.active,
            tabs: w.tabs.iter().map(|t| LiveTab {
                title: t.title.clone(), root: t.root.clone(), active: t.active, zoomed: t.zoomed,
                floating: t.floating, color: t.color.clone(),
            }).collect(),
        }).collect(),
        panes: m.panes.iter().map(|(id, p)| (*id, PaneMeta {
            title: p.title.clone(), cwd: p.cwd.clone(), domain: p.domain.clone(), cmd: p.cmd.clone(),
            agent: p.agent.clone(), token: p.token.clone(), parent: p.parent, private: p.private,
        })).collect(),
    }
}

/// Grava o layout vivo quando muda (o server seguinte remonta as abas).
fn live_loop(srv: Arc<Server>) {
    let path = proto::data_dir().join("live.json");
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let snap = serde_json::to_string(&live_snapshot(&srv.lock())).unwrap_or_default();
        if snap != last {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, &snap).is_ok() { let _ = std::fs::rename(&tmp, &path); }
            last = snap;
        }
    }
}

/// Server novo: reconecta aos hosts vivos e remonta o layout de live.json.
fn recover(srv: &Arc<Server>) {
    let hosts = ptyhost::alive();
    let live: Option<Live> = std::fs::read(proto::data_dir().join("live.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let mut m = srv.lock();
    if hosts.is_empty() {
        // Nada vivo (reboot ou primeira vez): oferece o ultimo checkpoint.
        m.resurrect = proto::state_dir().join("checkpoint.json").exists();
        return;
    }
    let (cols, rows) = m.size;
    let mut attached = Vec::new();
    for h in &hosts {
        let Ok(c) = ptyhost::connect(&h.pipe) else { proto::log(&format!("host {} vivo mas sem pipe", h.id)); continue };
        let meta = live.as_ref().and_then(|l| l.panes.get(&h.id).cloned()).unwrap_or_else(|| PaneMeta {
            cwd: h.cwd.clone(), token: proto::random_hex(16), ..Default::default()
        });
        attach(srv, &mut m, h.id, c, Some(h.pid), meta, None, cols, rows);
        attached.push(h.id);
        m.next = m.next.max(h.id + 1);
    }
    if let Some(l) = &live { m.next = m.next.max(l.next); }
    let mut placed: Vec<Id> = Vec::new();
    for lw in live.into_iter().flat_map(|l| l.workspaces) {
        let mut tabs = Vec::new();
        for lt in lw.tabs {
            let mut root = Some(lt.root);
            for id in root.as_ref().map(|r| r.leaves()).unwrap_or_default() {
                if !attached.contains(&id) { root = root.and_then(|r| r.remove(id)); }
            }
            let Some(root) = root else { continue };
            let leaves = root.leaves();
            placed.extend(&leaves);
            let mut tab = Tab::new(leaves[0], &lt.title);
            tab.active = if leaves.contains(&lt.active) { lt.active } else { leaves[0] };
            tab.root = root;
            tab.zoomed = lt.zoomed;
            tab.color = lt.color;
            if let Some(f) = lt.floating.filter(|f| attached.contains(f)) { tab.floating = Some(f); placed.push(f); }
            tabs.push(tab);
        }
        if !tabs.is_empty() {
            let active = lw.active.min(tabs.len() - 1);
            m.workspaces.push(Workspace { name: lw.name, tabs, active });
        }
    }
    let orphans: Vec<Id> = attached.iter().copied().filter(|id| !placed.contains(id)).collect();
    if !orphans.is_empty() {
        m.workspaces.push(Workspace { name: "recuperado".into(), tabs: orphans.iter().map(|id| Tab::new(*id, "")).collect(), active: 0 });
    }
    m.recovered = attached.len();
    proto::log(&format!("recuperados {} shells de um server anterior", attached.len()));
}

// ------------------------------------------------------------------ conexoes

fn serve_conn(srv: Arc<Server>, conn: Stream, token: String) {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() { return; }
    let presented = line.trim().to_string();
    // Token do dono (GUI, CLI fora do Duck) ou de um pane (escopo do pane).
    let caller = if presented == token {
        Caller(None)
    } else {
        match srv.lock().panes.iter().find(|(_, p)| !p.token.is_empty() && p.token == presented).map(|(id, _)| *id) {
            Some(id) => Caller(Some(id)),
            None => return,
        }
    };
    loop {
        line.clear();
        match reader.read_line(&mut line) { Ok(0) | Err(_) => return, _ => {} }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => { let _ = writeln!(reader.get_mut(), "{}", json!({"error": e.to_string()})); continue; }
        };
        if req.get("cmd").and_then(|c| c.as_str()) == Some("subscribe") {
            if caller.0.is_some() { return; } // so o dono assina a tela
            let client = req.get("client").and_then(|c| c.as_str()).unwrap_or("gui").to_string();
            return subscribe(srv, reader.into_inner(), client);
        }
        let resp = match std::panic::catch_unwind(AssertUnwindSafe(|| handle(&srv, &req, caller))) {
            Ok(r) => r.unwrap_or_else(|e| json!({"error": e})),
            Err(_) => {
                proto::log(&format!("panic no comando {}", req.get("cmd").and_then(|c| c.as_str()).unwrap_or("?")));
                json!({"error": "erro interno (detalhes em server.log)"})
            }
        };
        let quiet = matches!(req.get("cmd").and_then(|c| c.as_str()), Some("ping" | "list" | "read" | "states" | "events" | "doctor" | "grep" | "last-output" | "last-failure" | "history" | "ext-send"));
        if !quiet { srv.notify(); }
        let mut out = serde_json::to_vec(&resp).unwrap();
        out.push(b'\n');
        if reader.get_mut().write_all(&out).is_err() { return; }
    }
}

/// Empurra um frame por mudanca. Depois do primeiro, cada pane leva so as
/// linhas que mudaram (`rows`), o que corta o trafego e o parse da GUI quando
/// so o prompt pisca.
fn subscribe(srv: Arc<Server>, mut conn: Stream, client: String) {
    let mut last_gen = u64::MAX;
    // A janela nova recebe primeiro o estado das extensoes (visoes, badges).
    let (mut last_event, mut last_ext, mut pending) = {
        let mut m = srv.lock();
        m.subs += 1;
        (m.event_seq, m.ext.seq, m.ext.snapshot())
    };
    let mut sent: HashMap<u64, (u16, u16, Vec<u64>)> = HashMap::new();
    let mut pause = Duration::from_millis(8);
    loop {
        {
            let mut g = srv.gen.lock().unwrap_or_else(|e| e.into_inner());
            while *g == last_gen { g = srv.cv.wait(g).unwrap_or_else(|e| e.into_inner()); }
            last_gen = *g;
        }
        // Junta rajadas; se montar o frame ficou caro, espera mais (um pane
        // despejando saida nao trava a GUI nem os outros panes).
        std::thread::sleep(pause);
        let t0 = Instant::now();
        let (f, events) = {
            let mut m = srv.lock();
            let f = std::panic::catch_unwind(AssertUnwindSafe(|| frame(&srv, &mut m, &client)))
                .unwrap_or_else(|_| Err("panic montando o frame".into()));
            let ev: Vec<Value> = m.events.iter().filter(|(s, _)| *s > last_event).map(|(_, e)| e.clone()).collect();
            last_event = m.event_seq;
            pending.extend(m.ext.out.iter().filter(|(s, _, _)| *s > last_ext).map(|(_, e, v)| (e.clone(), v.clone())));
            last_ext = m.ext.seq;
            (f, ev)
        };
        pause = (t0.elapsed() * 2).clamp(Duration::from_millis(8), Duration::from_millis(50));
        let mut out = Vec::new();
        let exts = std::mem::take(&mut pending);
        for (e, v) in &exts {
            out.extend(serde_json::to_vec(&json!({"t": "ext", "ext": e, "msg": v})).unwrap());
            out.push(b'\n');
        }
        for e in events {
            let mut e = e;
            e["t"] = json!("event");
            out.extend(serde_json::to_vec(&e).unwrap());
            out.push(b'\n');
        }
        match f {
            Ok(mut f) => {
                diff_frame(&mut f, &mut sent);
                out.extend(serde_json::to_vec(&f).unwrap());
            }
            Err(e) => out.extend(serde_json::to_vec(&json!({"t": "error", "error": e})).unwrap()),
        }
        out.push(b'\n');
        if conn.write_all(&out).is_err() {
            // A janela fechou: o aviso que ia para ela vira toast.
            for (e, v) in exts.iter().filter(|(_, v)| v["type"] == "notify") { ext_toast(e, v); }
            let mut m = srv.lock();
            m.clients.remove(&client);
            m.subs = m.subs.saturating_sub(1);
            if m.subs == 0 { m.ext.views_closed(); }
            return;
        }
    }
}

fn ext_sink(srv: &Arc<Server>) -> crate::ext::Sink {
    let s = srv.clone();
    Arc::new(move |ext, msg, gen| ext_incoming(&s, &ext, msg, gen))
}

/// Mensagem de uma extensao: vai para as janelas. Sem janela aberta, o server
/// resolve o que da (aviso vira toast, pane abre numa aba); o resto (painel,
/// pergunta, colar) espera a janela, que recebe o estado ao conectar.
fn ext_incoming(srv: &Arc<Server>, ext: &str, msg: Value, gen: u64) {
    let mut m = srv.lock();
    let Some(msg) = m.ext.incoming(ext, msg, gen) else { return };
    if m.subs == 0 {
        match msg["type"].as_str().unwrap_or("") {
            "notify" => return ext_toast(ext, &msg),
            "open-pane" => {
                drop(m);
                let mut req = json!({"cmd": "spawn", "where": "tab", "argv": msg["cmd"].clone(), "focus": false});
                for k in ["cwd", "title"] { if let Some(v) = msg.get(k) { req[k] = v.clone(); } }
                if let Err(e) = handle(srv, &req, Caller(None)) { proto::log(&format!("extensao {ext}: open-pane: {e}")); }
                srv.notify();
                return;
            }
            _ => {}
        }
    }
    m.ext.push(ext, msg);
    drop(m);
    srv.notify();
}

/// Aviso de extensao como toast do Windows; os botoes voltam por duck://ext/...
fn ext_toast(ext: &str, m: &Value) {
    let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
    let enc = |x: &str| x.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let item = s("item");
    let urls: Vec<(String, String)> = m["actions"].as_array().into_iter().flatten()
        .map(|a| (a["title"].as_str().unwrap_or("").to_string(), format!("duck://ext/{ext}/{}/{}", enc(a["id"].as_str().unwrap_or("")), enc(&item)))).collect();
    crate::gui::toast_actions(&s("title"), &s("body"), &urls);
}

fn hash_v(v: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.to_string().hash(&mut h);
    h.finish()
}

/// Troca `lines` por `rows` = {indice: runs} com so as linhas alteradas desde
/// o ultimo frame enviado a este cliente (mesmo pane, mesmo tamanho).
fn diff_frame(f: &mut Value, sent: &mut HashMap<u64, (u16, u16, Vec<u64>)>) {
    let Some(panes) = f.get_mut("panes").and_then(|p| p.as_array_mut()) else { return };
    let mut seen = Vec::new();
    for p in panes {
        let id = p["id"].as_u64().unwrap_or(0);
        let (w, h) = (p["w"].as_u64().unwrap_or(0) as u16, p["h"].as_u64().unwrap_or(0) as u16);
        seen.push(id);
        let Some(lines) = p.get("lines").and_then(|l| l.as_array()).cloned() else { continue };
        let hashes: Vec<u64> = lines.iter().map(hash_v).collect();
        if let Some((pw, ph, prev)) = sent.get(&id) {
            if *pw == w && *ph == h {
                let mut rows = serde_json::Map::new();
                for (i, hv) in hashes.iter().enumerate() {
                    if prev.get(i) != Some(hv) { rows.insert(i.to_string(), lines[i].clone()); }
                }
                p.as_object_mut().unwrap().remove("lines");
                p["rows"] = Value::Object(rows);
            }
        }
        sent.insert(id, (w, h, hashes));
    }
    sent.retain(|id, _| seen.contains(id));
}

pub fn run() {
    let name = proto::pipe_name();
    let ns = name.clone().to_ns_name::<GenericNamespaced>().expect("nome do pipe");
    let listener = match ListenerOptions::new().name(ns).create_sync() {
        Ok(l) => l,
        Err(e) => { eprintln!("duck server: {e} (ja tem um rodando?)"); std::process::exit(1); }
    };
    std::panic::set_hook(Box::new(|info| proto::log(&format!("PANIC: {info}"))));
    let token = proto::new_token();
    install_shell_scripts();
    let (cfg, err) = config::load_config();
    if let Some(e) = err { proto::log(&format!("config: {e}")); }
    let (tx, rx) = channel();
    let secrets = cfg.secret_patterns.iter().filter_map(|r| regex::Regex::new(r).ok()).collect();
    let srv = Arc::new(Server {
        mux: Mutex::new(Mux {
            panes: HashMap::new(), workspaces: Vec::new(), next: 1, size: (100, 30),
            clients: HashMap::new(), cfg, clipboard: (0, String::new()), events: Vec::new(), event_seq: 0,
            approvals: Vec::new(), approval_seq: 0, secrets, recovered: 0, resurrect: false, light: false,
            history: load_history(), budget_warned: false, ext: Default::default(), subs: 0,
        }),
        gen: Mutex::new(0),
        cv: Condvar::new(),
        tx,
        sync: (Mutex::new(None), Condvar::new()),
    });
    proto::log(&format!("server {} iniciado (pid {})", env!("CARGO_PKG_VERSION"), std::process::id()));
    recover(&srv);
    { let s = srv.clone(); std::thread::spawn(move || event_loop(s, rx)); }
    { let s = srv.clone(); std::thread::spawn(move || agent_loop(s)); }
    { let s = srv.clone(); std::thread::spawn(move || autosave_loop(s)); }
    { let s = srv.clone(); std::thread::spawn(move || live_loop(s)); }
    { let s = srv.clone(); std::thread::spawn(move || sync_loop(s)); }
    { let s = srv.clone(); std::thread::spawn(move || { let sink = ext_sink(&s); s.lock().ext.start_all(&sink); s.notify(); }); }
    for conn in listener.incoming().flatten() {
        let (s, t) = (srv.clone(), token.clone());
        std::thread::spawn(move || serve_conn(s, conn, t));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_split_remove_adjust() {
        let mut n = Node::Leaf(1);
        assert!(n.split_leaf(1, 2, true));
        assert!(n.split_leaf(2, 3, false));
        let area = Rect { x: 0, y: 0, w: 101, h: 41 };
        let mut out = Vec::new();
        n.layout(area, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].1, Rect { x: 0, y: 0, w: 50, h: 41 });
        assert_eq!(out[1].1.x, 51);
        assert_eq!(out[2].1.y, out[1].1.h + 1);
        assert!(n.adjust(area, 1, true, 10));
        let mut out2 = Vec::new();
        n.layout(area, &mut out2);
        assert_eq!(out2[0].1.w, 60);
        let n = n.remove(2).unwrap();
        assert_eq!(n.leaves(), vec![1, 3]);
        assert!(n.remove(1).unwrap().remove(3).is_none());
    }

    #[test]
    fn colors_and_cmd() {
        assert_eq!(idx256(16), 0);
        assert_eq!(idx256(231), 0xffffff);
        assert_eq!(idx256(1), pal(1));
        assert_eq!(split_cmd("claude --resume 'a b'"), vec!["claude", "--resume", "a b"]);
        let ag = Agent { name: "claude".into(), session: Some("abc".into()), config_dir: Some("C:/p".into()), ..Default::default() };
        assert_eq!(resume_line(&[], &ag, "--dangerously-skip-permissions").unwrap(), "$env:CLAUDE_CONFIG_DIR='C:/p'; claude --resume abc --dangerously-skip-permissions\r");
        assert_eq!(resume_line(&[], &ag, "").unwrap(), "$env:CLAUDE_CONFIG_DIR='C:/p'; claude --resume abc\r");
        assert_eq!(percent_decode("/C:/My%20Dir"), "/C:/My Dir");
    }

    #[test]
    fn osc_parsing() {
        let data = b"a\x1b]133;A\x07b\x1b]7;file://pc/C:/x%20y\x1b\\c\x1b]133;D;2\x07";
        let ev = osc_events(data);
        assert_eq!(ev.len(), 3);
        assert!(matches!(ev[0].1, Osc::Prompt));
        assert!(matches!(&ev[1].1, Osc::Cwd(c) if c == "C:/x y"));
        assert!(matches!(ev[2].1, Osc::CmdEnd(2)));
        assert_eq!(ev[0].0, 9); // logo depois do BEL
    }

    #[test]
    fn osc_input_start() {
        let ev = osc_events(b"\x1b]133;A\x07PS> \x1b]133;B\x07ls\x1b]133;C\x07");
        assert!(matches!(ev[1].1, Osc::InputStart));
        assert!(matches!(ev[2].1, Osc::CmdStart));
    }

    #[test]
    fn node_replace() {
        let mut n = Node::Leaf(1);
        n.split_leaf(1, 2, true);
        assert!(n.replace(2, 9));
        assert_eq!(n.leaves(), vec![1, 9]);
    }

    #[test]
    fn clean_strips_controls() {
        assert_eq!(clean("ti\x1b]52;c;ZWNobw==\x07tulo\u{9b}", 100), "ti]52;c;ZWNobw==tulo");
    }

    #[test]
    fn frame_diff_sends_only_changed_rows() {
        let mut sent = HashMap::new();
        let mut f1 = json!({"panes": [{"id": 1, "w": 10, "h": 2, "lines": [[[0, "a"]], [[0, "b"]]]}]});
        diff_frame(&mut f1, &mut sent);
        assert!(f1["panes"][0].get("lines").is_some()); // primeiro: tela inteira
        let mut f2 = json!({"panes": [{"id": 1, "w": 10, "h": 2, "lines": [[[0, "a"]], [[0, "c"]]]}]});
        diff_frame(&mut f2, &mut sent);
        assert!(f2["panes"][0].get("lines").is_none());
        assert_eq!(f2["panes"][0]["rows"].as_object().unwrap().len(), 1);
        assert_eq!(f2["panes"][0]["rows"]["1"], json!([[0, "c"]]));
    }

    #[test]
    fn scope_children() {
        let lua = mlua::Lua::new();
        let t: mlua::Table = lua.load("return { api_scope = 'children' }").eval().unwrap();
        let m = Mux {
            panes: HashMap::new(), workspaces: vec![], next: 1, size: (10, 10), clients: HashMap::new(),
            cfg: config::parse(&t),
            clipboard: (0, String::new()), events: vec![], event_seq: 0, approvals: vec![], approval_seq: 0,
            secrets: vec![], recovered: 0, resurrect: false, light: false, history: vec![], budget_warned: false, ext: Default::default(), subs: 0,
        };
        assert!(check_scope(&m, Caller(Some(5)), "send", &json!({"pane": 9})).is_err());
        assert!(check_scope(&m, Caller(Some(5)), "read", &json!({"pane": 9})).is_ok());
        assert!(check_scope(&m, Caller(None), "kill-server", &json!({})).is_ok());
        assert!(check_scope(&m, Caller(Some(5)), "set-state", &json!({"state": "working"})).is_ok());
    }
}

