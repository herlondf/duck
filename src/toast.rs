//! Aviso com a cara do Duck fora da janela principal: um cartao no canto da
//! tela, com o tema e a fonte da config, no lugar do toast do Windows.
//!
//! Um processo `duckw toast` desenha os cartoes e fica ouvindo o pipe
//! `duck-<usuario>-toast.sock`; quem quer avisar manda uma linha JSON
//! ({title, body, color?, seconds?, actions: [[rotulo, url duck://]]}). Sem o
//! processo no ar, `show` o abre com o primeiro aviso. Ocioso, ele sai sozinho.
//! Em tela cheia/apresentacao, `show` devolve false e quem chamou usa o toast
//! nativo (o Windows guarda na central e mostra depois).

use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde_json::Value;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use crate::config::{self, Config, Rgb, Theme};
use crate::render::{lin, Layer, Renderer, Shape, TextItem};

const IDLE_EXIT: Duration = Duration::from_secs(20);

fn pipe() -> String { crate::proto::pipe_name().replace(".sock", "-toast.sock") }

/// Mostra um aviso do Duck. false: nao deu (sem duckw, tela cheia...), use o nativo.
pub fn show(note: &Value) -> bool {
    if busy() { return false; }
    if send(note) { return true; }
    let Some(exe) = crate::gui_exe() else { return false };
    let hex: String = note.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    crate::proto::spawn_detached(&[exe.to_string_lossy().to_string(), "toast".into(), hex], &[]).is_ok()
}

fn send(note: &Value) -> bool {
    let Ok(name) = pipe().to_ns_name::<GenericNamespaced>() else { return false };
    let Ok(mut s) = Stream::connect(name) else { return false };
    s.write_all(format!("{note}\n").as_bytes()).is_ok()
}

/// Tira os cartoes sobre o pane (o usuario ja foi ate ele). So se o processo
/// dos cartoes estiver no ar: nunca abre um so para isso.
pub fn dismiss_pane(pane: u64) {
    if pane != 0 { let _ = send(&serde_json::json!({"dismiss": pane})); }
}

/// Tela cheia, jogo ou apresentacao: nao cobrir com um cartao.
fn busy() -> bool {
    #[cfg(windows)]
    unsafe {
        let mut st = 0;
        // 2 ocupado (tela cheia), 3 Direct3D tela cheia, 4 apresentacao.
        if windows_sys::Win32::UI::Shell::SHQueryUserNotificationState(&mut st) == 0 { return matches!(st, 2..=4); }
    }
    false
}

/// `duckw toast <json em hex>`: o processo que desenha os avisos.
pub fn run(arg: &str) -> i32 {
    let bytes: Vec<u8> = (0..arg.len() / 2).filter_map(|i| u8::from_str_radix(arg.get(2 * i..2 * i + 2)?, 16).ok()).collect();
    let first: Option<Value> = serde_json::from_slice(&bytes).ok();
    let listener = match pipe().to_ns_name::<GenericNamespaced>().and_then(|n| ListenerOptions::new().name(n).create_sync()) {
        Ok(l) => l,
        // Ja tem um no ar (dois avisos ao mesmo tempo): entrega para ele.
        Err(_) => return if first.as_ref().is_some_and(send) { 0 } else { 1 },
    };
    let Ok(el) = EventLoop::<Value>::with_user_event().build() else { return 1 };
    let proxy = el.create_proxy();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            for line in BufReader::new(conn).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) { if proxy.send_event(v).is_err() { return; } }
            }
        }
    });
    let mut app = App::new();
    if let Some(v) = first { app.add(v); }
    let _ = el.run_app(&mut app);
    0
}

struct Note { id: u64, title: String, body: String, color: Option<String>, actions: Vec<(String, String)>, until: Instant, secs: u64 }

enum Hit { Close(u64), Button(u64, String), Body(u64) }

struct App {
    cfg: Config,
    r: Option<Renderer>,
    notes: Vec<Note>,
    seq: u64,
    mouse: Option<(f32, f32)>,
    hits: Vec<([f32; 4], Hit)>,
    idle: Option<Instant>,
    shown: bool,
    size: (u32, u32),
    tick: Instant,
}

impl App {
    fn new() -> App {
        let (cfg, _) = config::load_config();
        App { cfg, r: None, notes: Vec::new(), seq: 0, mouse: None, hits: Vec::new(), idle: None, shown: false, size: (0, 0), tick: Instant::now() }
    }

    fn add(&mut self, v: Value) {
        if let Some(pane) = v["dismiss"].as_u64() {
            let mine: Vec<String> = ["approve", "deny", "focus"].iter().map(|k| format!("duck://{k}/{pane}")).collect();
            let before = self.notes.len();
            self.notes.retain(|n| !n.actions.iter().any(|(_, u)| mine.contains(u)));
            if self.notes.len() != before { self.layout(); }
            return;
        }
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        let actions = v["actions"].as_array().into_iter().flatten()
            .filter_map(|a| Some((a[0].as_str()?.to_string(), a[1].as_str()?.to_string()))).collect();
        let secs = v["seconds"].as_u64().unwrap_or(15).clamp(3, 600);
        self.seq += 1;
        self.notes.push(Note { id: self.seq, title: s("title"), body: s("body"), color: v["color"].as_str().map(String::from),
            actions, until: Instant::now() + Duration::from_secs(secs), secs });
        // No maximo 4 na tela: o mais antigo sai.
        while self.notes.len() > 4 { self.notes.remove(0); }
        self.idle = None;
        self.layout();
    }

    fn theme(&self) -> Theme {
        let light = self.r.as_ref().and_then(|r| r.window.theme()) == Some(winit::window::Theme::Light);
        if light { self.cfg.theme_light.clone().unwrap_or_else(|| self.cfg.theme.clone()) } else { self.cfg.theme.clone() }
    }

    fn scale(&self) -> f32 { self.r.as_ref().map(|r| r.window.scale_factor() as f32).unwrap_or(1.0) }

    /// Quebra o corpo em ate 4 linhas que cabem em `max`.
    fn wrap(r: &mut Renderer, text: &str, max: f32, size: f32) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for para in text.lines() {
            let mut cur = String::new();
            for w in para.split_whitespace() {
                let cand = if cur.is_empty() { w.to_string() } else { format!("{cur} {w}") };
                if !cur.is_empty() && r.measure(&cand, true, size) > max { lines.push(std::mem::take(&mut cur)); cur = w.to_string(); } else { cur = cand; }
            }
            lines.push(cur);
        }
        if lines.len() > 4 { lines.truncate(4); lines[3].push('…'); }
        lines.retain(|l| !l.is_empty());
        lines
    }

    /// Altura de cada cartao (o corpo quebrado) e o tamanho da janela.
    fn heights(&mut self) -> (Vec<(f32, Vec<String>)>, f32, f32) {
        let s = self.scale();
        let Some(r) = self.r.as_mut() else { return (Vec::new(), 0.0, 0.0) };
        let ch = r.cell_h;
        let w = 380.0 * s;
        let out: Vec<(f32, Vec<String>)> = self.notes.iter().map(|n| {
            let body = App::wrap(r, &n.body, w - 32.0 * s, 0.85);
            let mut h = 12.0 * s + ch + 4.0 * s + body.len() as f32 * ch * 0.92 + 14.0 * s;
            if !n.actions.is_empty() { h += 40.0 * s; }
            (h, body)
        }).collect();
        let m = 16.0 * s;
        let total = out.iter().map(|x| x.0).sum::<f32>() + 10.0 * s * out.len().saturating_sub(1) as f32 + 2.0 * m;
        (out, w + 2.0 * m, total)
    }

    /// Tamanho e posicao (canto inferior direito da area de trabalho).
    fn layout(&mut self) {
        if self.r.is_none() { return; }
        if self.notes.is_empty() {
            if let Some(r) = &self.r { hide(&r.window); }
            self.shown = false;
            self.idle.get_or_insert_with(Instant::now);
            return;
        }
        let (_, w, h) = self.heights();
        let (w, h) = (w.ceil() as u32, h.ceil() as u32);
        let (right, bottom) = work_area(self.r.as_ref().unwrap());
        let r = self.r.as_mut().unwrap();
        if (w, h) != self.size {
            self.size = (w, h);
            let _ = r.window.request_inner_size(PhysicalSize::new(w, h));
            r.resize(w, h);
        }
        r.window.set_outer_position(PhysicalPosition::new(right - w as i32, bottom - h as i32));
        if !self.shown { show_no_activate(&r.window); self.shown = true; }
        r.window.request_redraw();
    }

    fn draw(&mut self) {
        let theme = self.theme();
        let s = self.scale();
        let (cards, ww, _) = self.heights();
        let mouse = self.mouse;
        let inside = |b: [f32; 4]| mouse.is_some_and(|p| p.0 >= b[0] && p.0 < b[0] + b[2] && p.1 >= b[1] && p.1 < b[1] + b[3]);
        let Some(r) = self.r.as_mut() else { return };
        let ch = r.cell_h;
        let m = 16.0 * s;
        let cw = ww - 2.0 * m;
        let mut layer = Layer::default();
        self.hits.clear();
        let uit = |x: f32, y: f32, t: &str, c: Rgb, size: f32| TextItem { x, y, text: t.into(), color: c, alpha: 1.0, bold: false, italic: false, clip_right: None, ui: true, size };
        let card_bg = config::mix_rgb(theme.tab_bar, theme.fg, 0.05);
        let mut y = m;
        for (n, (h, body)) in self.notes.iter().zip(&cards) {
            let (x, h) = (m, *h);
            let color = n.color.as_deref().map(|c| theme.named(c)).unwrap_or(theme.accent);
            layer.shapes.push(Shape::shadow(x, y + 4.0 * s, cw, h, [0.0, 0.0, 0.0, 0.45], 12.0 * s, 12.0 * s));
            layer.shapes.push(Shape::rect(x, y, cw, h, lin(card_bg, 1.0), 12.0 * s));
            layer.shapes.push(Shape::ring(x, y, cw, h, lin(config::mix_rgb(card_bg, theme.fg, 0.12), 1.0), 12.0 * s, 1.0));
            layer.shapes.push(Shape::rect(x, y + 12.0 * s, 4.0 * s, h - 24.0 * s, lin(color, 1.0), 2.0 * s));
            self.hits.push(([x, y, cw, h], Hit::Body(n.id)));
            layer.texts.push(TextItem { bold: true, clip_right: Some(x + cw - 34.0 * s), ..uit(x + 16.0 * s, y + 12.0 * s, &n.title, theme.fg, 0.95) });
            let mut ty = y + 12.0 * s + ch + 4.0 * s;
            for l in body {
                layer.texts.push(TextItem { clip_right: Some(x + cw - 12.0 * s), ..uit(x + 16.0 * s, ty, l, theme.muted, 0.85) });
                ty += ch * 0.92;
            }
            // Barrinha do tempo restante.
            let left = n.until.saturating_duration_since(Instant::now()).as_secs_f32() / n.secs as f32;
            layer.shapes.push(Shape::rect(x + 12.0 * s, y + h - 3.0 * s, (cw - 24.0 * s) * left.clamp(0.0, 1.0), 2.0 * s, lin(color, 0.5), 1.0 * s));
            let xr = [x + cw - 30.0 * s, y + 8.0 * s, 22.0 * s, 22.0 * s];
            layer.texts.push(uit(xr[0] + 5.0 * s, xr[1], "✕", if inside(xr) { theme.fg } else { theme.muted }, 0.8));
            self.hits.push((xr, Hit::Close(n.id)));
            let mut bx = x + cw - 12.0 * s;
            for (k, (label, url)) in n.actions.iter().enumerate().rev() {
                let bw = r.measure(label, true, 0.85) + 22.0 * s;
                bx -= bw;
                let br = [bx, y + h - 42.0 * s, bw, 28.0 * s];
                let primary = k == 0;
                let bg = if primary { color } else { config::mix_rgb(theme.tab_bar, theme.fg, 0.12) };
                let bg = if inside(br) { config::mix_rgb(bg, theme.fg, 0.15) } else { bg };
                layer.shapes.push(Shape::rect(br[0], br[1], br[2], br[3], lin(bg, 1.0), 7.0 * s));
                layer.texts.push(uit(bx + 11.0 * s, br[1] + (28.0 * s - ch * 0.85) / 2.0, label, if primary { theme.bg } else { theme.fg }, 0.85));
                self.hits.push((br, Hit::Button(n.id, url.clone())));
                bx -= 6.0 * s;
            }
            y += h + 10.0 * s;
        }
        r.draw([0.0, 0.0, 0.0, 0.0], &[layer]);
    }

    fn click(&mut self) {
        let Some(p) = self.mouse else { return };
        // O ultimo empurrado e o mais especifico (botao/fechar sobre o corpo).
        let Some((_, hit)) = self.hits.iter().rev().find(|(b, _)| p.0 >= b[0] && p.0 < b[0] + b[2] && p.1 >= b[1] && p.1 < b[1] + b[3]) else { return };
        let id = match hit {
            Hit::Close(id) | Hit::Body(id) => *id,
            Hit::Button(id, url) => {
                // So o protocolo do Duck: o cartao nao abre outras URLs.
                if url.starts_with("duck:") {
                    let u = url.clone();
                    std::thread::spawn(move || crate::cli::url(&[u]));
                }
                *id
            }
        };
        self.notes.retain(|n| n.id != id);
        self.layout();
    }

    /// Tira os vencidos; com o mouse em cima, o tempo para (a barrinha congela).
    fn expire(&mut self) {
        let now = Instant::now();
        let dt = now - std::mem::replace(&mut self.tick, now);
        if self.mouse.is_some() {
            for n in &mut self.notes { n.until += dt; }
        }
        let before = self.notes.len();
        self.notes.retain(|n| n.until > now);
        if self.notes.len() != before { self.layout(); }
    }
}

/// Canto inferior direito da area de trabalho (sem a barra de tarefas).
fn work_area(r: &Renderer) -> (i32, i32) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};
        let mut rc: RECT = std::mem::zeroed();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rc as *mut _ as _, 0) != 0 { return (rc.right, rc.bottom); }
    }
    let m = r.window.primary_monitor().or_else(|| r.window.current_monitor());
    m.map(|m| (m.position().x + m.size().width as i32, m.position().y + m.size().height as i32 - 48)).unwrap_or((1600, 900))
}

#[cfg(windows)]
fn hwnd(w: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as _),
        _ => None,
    }
}

/// Mostra sem tirar o foco de quem esta digitando, e fora da barra de tarefas.
/// Direto no Win32 (mostrar e esconder): o winit ativaria a janela.
fn show_no_activate(w: &Window) {
    #[cfg(windows)]
    if let Some(h) = hwnd(w) {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize);
            ShowWindow(h, SW_SHOWNOACTIVATE);
        }
        return;
    }
    w.set_visible(true);
}

fn hide(w: &Window) {
    #[cfg(windows)]
    if let Some(h) = hwnd(w) {
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(h, windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE); }
        return;
    }
    w.set_visible(false);
}

impl ApplicationHandler<Value> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.r.is_some() { return; }
        let mut attrs = Window::default_attributes()
            .with_title("Duck: aviso")
            .with_decorations(false)
            .with_resizable(false)
            .with_transparent(true)
            .with_visible(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(PhysicalSize::new(1, 1));
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_no_redirection_bitmap(true).with_skip_taskbar(true);
        }
        let Ok(window) = el.create_window(attrs) else { el.exit(); return };
        let window = Arc::new(window);
        let c = &self.cfg;
        let mut r = Renderer::new(window.clone(), &c.font_family, &c.font_dirs, c.font_size, c.line_height, true, &c.gpu_backend);
        let scale = window.scale_factor() as f32;
        r.set_font(&c.font_family, c.font_size * 96.0 / 72.0 * scale, c.line_height);
        r.set_style(&c.ui.font, c.ui.font_weight, &c.ui.font_features);
        self.r = Some(r);
        self.layout();
    }

    fn user_event(&mut self, _: &ActiveEventLoop, v: Value) { self.add(v); }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => {
                if let Some(r) = self.r.as_mut() { r.resize(s.width, s.height); r.window.request_redraw(); }
            }
            WindowEvent::ScaleFactorChanged { .. } => self.layout(),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = Some((position.x as f32, position.y as f32));
                if let Some(r) = &self.r { r.window.request_redraw(); }
            }
            WindowEvent::CursorLeft { .. } => {
                self.mouse = None;
                if let Some(r) = &self.r { r.window.request_redraw(); }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => self.click(),
            WindowEvent::RedrawRequested => if self.shown { self.draw() },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.expire();
        if self.notes.is_empty() {
            match self.idle {
                Some(t) if t.elapsed() >= IDLE_EXIT => el.exit(),
                Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t + IDLE_EXIT)),
                None => { self.idle = Some(Instant::now()); el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + IDLE_EXIT)); }
            }
            return;
        }
        // Barrinha do tempo: ~60 quadros por segundo enquanto houver cartao.
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16)));
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: winit::event::StartCause) {
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            if let Some(r) = &self.r { r.window.request_redraw(); }
        }
    }
}
