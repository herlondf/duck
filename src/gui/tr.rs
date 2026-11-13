//! Tradutor na janela (config `translate`): editor de comando em modo
//! traducao (Enter traduz, Enter de novo manda o ingles), balao com a
//! traducao da selecao e o glossario. A CLI roda numa thread; o resultado
//! volta como `UserEv::Translated`.

use super::*;
use super::ui::{inside, uit, Hit};
use crate::render::Shape;
use crate::translate::{self as tl, ComposeState, Enter};

#[derive(Default)]
pub(super) struct Tr {
    /// Respostas cruas do modelo por chave (motor, modelo, direcao, texto).
    cache: HashMap<String, String>,
    next: u64,
    pub balloon: Option<Balloon>,
    mouse: Mouse,
}

/// Clique do esquerdo num app com mouse, com o tradutor ligado.
#[derive(Default, Clone, Copy)]
enum Mouse {
    #[default] Off,
    /// Alt+arrastar: selecao do Duck que traduz ao soltar.
    Duck,
}

pub(super) struct Balloon {
    id: u64, pane: u64, at: Cell, term: String, context: String, state: BState,
    /// Onde foi desenhado (clique dentro nao fecha).
    pub rect: [f32; 4],
}

enum BState { Pending, Ready(tl::Lookup), Failed(String) }

impl Tr {
    /// Guarda a resposta (cache so em memoria, ate 500).
    fn put(&mut self, key: String, out: String) {
        if self.cache.len() >= 500 { self.cache.clear(); }
        self.cache.insert(key, out);
    }
}

/// Linhas do editor em modo tradutor: (texto, tipo) com tipo 0 = traducao,
/// 1 = nota, 2 = andamento, 3 = erro.
pub(super) fn composer_lines(st: &ComposeState, cols: usize) -> Vec<(String, u8)> {
    let mut v = Vec::new();
    match st {
        ComposeState::Idle => {}
        ComposeState::Pending { .. } => v.push(("traduzindo…".to_string(), 2)),
        ComposeState::Failed { msg, .. } => v.extend(wrap(&format!("Erro: {msg}"), cols).into_iter().map(|l| (l, 3))),
        ComposeState::Ready { out, .. } => {
            for l in out.translation.split('\n') { v.extend(wrap(l, cols).into_iter().map(|l| (l, 0))); }
            for n in &out.notes {
                let pair = if n.pt.is_empty() { n.en.clone() } else { format!("{} = {}", n.en, n.pt) };
                let s = if n.why.is_empty() { format!("• {pair}") } else { format!("• {pair}: {}", n.why) };
                v.extend(wrap(&s, cols).into_iter().map(|l| (l, 1)));
            }
        }
    }
    v.truncate(14);
    v
}

/// Quebra por palavras em ate `cols` caracteres.
fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let mut out = vec![];
    let mut cur = String::new();
    for w in s.split(' ') {
        let (cl, wl) = (cur.chars().count(), w.chars().count());
        if cl > 0 && cl + 1 + wl > cols { out.push(std::mem::take(&mut cur)); }
        if !cur.is_empty() { cur.push(' '); }
        cur.push_str(w);
        while cur.chars().count() > cols {
            let head: String = cur.chars().take(cols).collect();
            cur = cur.chars().skip(cols).collect();
            out.push(head);
        }
    }
    if !cur.is_empty() || out.is_empty() { out.push(cur); }
    out
}

/// Balao da selecao: perto do fim dela e sempre dentro da janela.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_balloon(tr: &mut Tr, panes: &[PaneView], r: &mut Renderer, l: &mut Layer, hits: &mut Vec<Hit>, theme: &Theme, origin: (f32, f32), win: (f32, f32), mouse: (f32, f32)) {
    let Some(b) = tr.balloon.as_mut() else { return };
    let Some(p) = panes.iter().find(|p| p.id == b.pane) else { tr.balloon = None; return };
    let (cw, ch, (w, h)) = (r.cell_w, r.cell_h, win);
    let size = 0.85;
    let bw = (cw * 46.0).min(w - 16.0).max(120.0);
    let avg = (r.measure("abcdefghijklmnopqrstuvwxyz", true, size) / 26.0).max(1.0);
    let cols = ((bw - 24.0) / avg) as usize;
    let term: String = b.term.split_whitespace().collect::<Vec<_>>().join(" ");
    let term: String = if term.chars().count() > 60 { format!("{}…", term.chars().take(60).collect::<String>()) } else { term };
    // (texto, cor, negrito)
    let mut lines: Vec<(String, Rgb, bool)> = vec![];
    for t in wrap(&term, cols).into_iter().take(2) { lines.push((t, theme.muted, false)); }
    let mut button = false;
    match &b.state {
        BState::Pending => lines.push(("traduzindo…".into(), theme.accent, false)),
        BState::Failed(e) => for t in wrap(e, cols).into_iter().take(4) { lines.push((t, theme.blocked, false)); },
        BState::Ready(x) => {
            for t in wrap(&x.translation, cols).into_iter().take(8) { lines.push((t, theme.fg, true)); }
            if !x.pos.is_empty() { lines.push((x.pos.clone(), theme.accent, false)); }
            for t in wrap(&x.meaning, cols).into_iter().take(5) { lines.push((t, theme.fg, false)); }
            button = true;
        }
    }
    let lh = ch * size;
    let btn_h = if button { ch + 10.0 } else { 0.0 };
    let bh = lines.len() as f32 * lh + 18.0 + btn_h;
    let (ox, oy) = origin;
    let ax = ox + (p.x as i32 + b.at.col + 1) as f32 * cw;
    let below = oy + (p.y as i32 + b.at.row + 1) as f32 * ch + 4.0;
    let above = oy + (p.y as i32 + b.at.row) as f32 * ch - bh - 4.0;
    let by = if below + bh <= h - 8.0 { below } else { above };
    let bx = (ax - 12.0).min(w - bw - 8.0).max(8.0);
    let by = by.min(h - bh - 8.0).max(8.0);
    b.rect = [bx, by, bw, bh];
    l.shapes.push(Shape::shadow(bx, by + 4.0, bw, bh, [0.0, 0.0, 0.0, 0.5], 10.0, 12.0));
    l.shapes.push(Shape::rect(bx, by, bw, bh, lin(theme.tab_bar, 1.0), 10.0));
    l.shapes.push(Shape::ring(bx, by, bw, bh, lin(theme.accent, 0.8), 10.0, 1.2));
    for (k, (t, c, bold)) in lines.iter().enumerate() {
        l.texts.push(TextItem { bold: *bold, clip_right: Some(bx + bw - 10.0), ..uit(bx + 12.0, by + 9.0 + k as f32 * lh, t, *c, size) });
    }
    if button {
        let label = "+ glossário";
        let tw = r.measure(label, true, 0.8) + 20.0;
        let rr = [bx + 10.0, by + bh - btn_h - 2.0, tw, ch + 6.0];
        let hv = inside(rr, mouse);
        l.shapes.push(Shape::rect(rr[0], rr[1], rr[2], rr[3], lin(mix(theme.tab_bar, theme.accent, if hv { 0.35 } else { 0.18 }), 1.0), 6.0));
        l.texts.push(uit(rr[0] + 10.0, rr[1] + (rr[3] - ch * 0.8) / 2.0, label, theme.fg, 0.8));
        hits.push(Hit { r: rr, action: Action::Builtin("glossary_add".into(), vec![]) });
        l.texts.push(uit(rr[0] + tw + 10.0, rr[1] + (rr[3] - ch * 0.72) / 2.0, "Esc fecha", theme.muted, 0.72));
    }
}

impl Gui {
    fn tr_on(&self) -> bool { self.cfg.translate.enabled }

    /// Rotulo do editor em modo tradutor ("Tradutor pt-BR → en · claude haiku").
    pub(super) fn tr_header(&self, pane: u64) -> String {
        let agent = self.pane(pane).map(|p| p.agent.clone()).unwrap_or_default();
        match self.cfg.translate.resolve(&agent) {
            Ok(p) => format!("{} · {} {}", self.cfg.translate.label(), p.engine, p.model),
            Err(e) => e,
        }
    }

    /// O editor abre em modo tradutor? (ligado, pane com agente, nao privado)
    pub(super) fn tr_composer(&self, pane: u64) -> Option<ComposeState> {
        let p = self.pane(pane)?;
        (self.tr_on() && !p.agent.is_empty() && !p.private).then_some(ComposeState::Idle)
    }

    /// Pede a traducao numa thread; o resultado volta pelo proxy (ate do cache).
    fn tr_request(&mut self, pane: u64, dir: &str, text_key: &str, prompt: String) -> Result<u64, String> {
        let p = self.pane(pane).ok_or("Tradutor: o pane fechou")?;
        if p.private { return Err("Tradutor: pane privado, nada sai dele".into()); }
        let plan = self.cfg.translate.resolve(&p.agent)?;
        let mut env = vec![];
        // A conta certa: o perfil do agente do pane (o caminho do WSL nao vale no Windows).
        if plan.engine == "claude" {
            if let Some(d) = p.config_dir.clone().filter(|d| !d.is_empty() && (cfg!(not(windows)) || !d.starts_with('/'))) {
                env.push(("CLAUDE_CONFIG_DIR".to_string(), d));
            }
        }
        self.tr.next += 1;
        let id = self.tr.next;
        let key = tl::cache_key(&plan.engine, &plan.model, dir, text_key);
        let proxy = self.proxy.clone();
        if let Some(out) = self.tr.cache.get(&key).cloned() {
            let _ = proxy.send_event(UserEv::Translated(tl::Done { id, key, res: Ok(out) }));
            return Ok(id);
        }
        std::thread::spawn(move || {
            let res = tl::run(&plan.argv, &env, &prompt, tl::TIMEOUT);
            let _ = proxy.send_event(UserEv::Translated(tl::Done { id, key, res }));
        });
        Ok(id)
    }

    /// Chegou uma traducao: editor ou balao (o pedido ja descartado e ignorado).
    pub(super) fn tr_done(&mut self, d: tl::Done) {
        let mut err = None;
        if let Some(st) = self.composer.as_mut().and_then(|c| c.tr.as_mut()) {
            if matches!(st, ComposeState::Pending { id, .. } if *id == d.id) {
                let res = d.res.clone().and_then(|o| tl::parse_compose(&o).map(|c| (o, c)));
                match res {
                    Ok((o, c)) => { self.tr.put(d.key.clone(), o); st.finish(d.id, Ok(c)); }
                    Err(e) => { err = Some(e.clone()); st.finish(d.id, Err(e)); }
                }
            }
        }
        if self.tr.balloon.as_ref().is_some_and(|b| b.id == d.id) {
            let state = match d.res.and_then(|o| tl::parse_lookup(&o).map(|l| (o, l))) {
                Ok((o, l)) => { self.tr.put(d.key, o); BState::Ready(l) }
                Err(e) => { err = Some(e.clone()); BState::Failed(e) }
            };
            if let Some(b) = self.tr.balloon.as_mut() { b.state = state; }
        }
        if let Some(e) = err { self.flash(&format!("Tradutor: {e}")); }
        self.redraw();
    }

    /// Enter no editor em modo tradutor.
    pub(super) fn composer_tr_enter(&mut self) {
        let Some(c) = self.composer.as_ref() else { return };
        let Some(st) = c.tr.as_ref() else { return };
        let (text, pane) = (c.text.iter().collect::<String>(), c.pane);
        match st.on_enter(&text) {
            Enter::Nothing => self.composer = None,
            Enter::Wait => {}
            Enter::Send(s) => { self.composer = None; self.composer_send(pane, &s); }
            Enter::Translate => {
                let (from, to) = (self.cfg.translate.from.clone(), self.cfg.translate.to.clone());
                let prompt = tl::compose_prompt(&from, &to, &tl::mask(&text));
                let res = self.tr_request(pane, &format!("{from}>{to}"), &text, prompt);
                if let Err(e) = &res { self.flash(e); }
                if let Some(st) = self.composer.as_mut().and_then(|c| c.tr.as_mut()) {
                    *st = match res { Ok(id) => ComposeState::Pending { src: text, id }, Err(msg) => ComposeState::Failed { src: text, msg } };
                }
            }
        }
        self.redraw();
    }

    /// Manda ao pane como os recados do agente: colar (bracketed) e Enter.
    pub(super) fn composer_send(&mut self, pane: u64, text: &str) {
        if text.trim().is_empty() { return; }
        self.cmd(json!({"cmd": "paste", "pane": pane, "text": text}));
        self.cmd(json!({"cmd": "send", "pane": pane, "text": "\r"}));
    }

    /// Selecao copiada: traduz para a lingua do usuario e mostra o balao.
    pub(super) fn tr_lookup_selection(&mut self) {
        self.tr.balloon = None;
        if !self.tr_on() { return; }
        let span = if let Some(c) = &self.copy { c.anchor.map(|a| (c.pane, a, c.cur)) } else { self.sel.as_ref().map(|s| (s.pane, s.a, s.b)) };
        let Some((pane, a, b)) = span else { return };
        let Some(p) = self.pane(pane) else { return };
        if p.private { return; }
        let (s, e) = if (a.row, a.col) <= (b.row, b.col) { (a, b) } else { (b, a) };
        let Some(text) = self.selection_text() else { return };
        let term: String = text.replace("\r\n", "\n").trim().chars().take(2000).collect();
        if term.is_empty() { return; }
        // Contexto: as linhas inteiras da selecao.
        let context: String = (s.row.max(0)..=e.row.min(p.h as i32 - 1)).map(|r| p.row_string(r as usize).trim().to_string())
            .filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ").chars().take(1200).collect();
        let (from, to) = (self.cfg.translate.from.clone(), self.cfg.translate.to.clone());
        let prompt = tl::lookup_prompt(&from, &to, &tl::mask(&term), &tl::mask(&context));
        let key_text = if tl::is_short(&term) { format!("{term}\u{1e}{context}") } else { term.clone() };
        match self.tr_request(pane, &format!("{to}>{from}"), &key_text, prompt) {
            Ok(id) => self.tr.balloon = Some(Balloon { id, pane, at: e, term, context, state: BState::Pending, rect: [0.0; 4] }),
            Err(e) => self.flash(&e),
        }
    }

    /// Esc fecha o balao; true = tratada.
    pub(super) fn tr_key(&mut self, ev: &KeyEvent) -> bool {
        if self.tr.balloon.is_none() || !matches!(ev.logical_key, Key::Named(NamedKey::Escape)) { return false; }
        self.tr.balloon = None;
        self.redraw();
        true
    }

    /// Clique com o balao aberto: dentro dele so os botoes; fora fecha. true = consumido.
    pub(super) fn tr_click(&mut self, x: f32, y: f32) -> bool {
        let Some(b) = &self.tr.balloon else { return false };
        if !inside(b.rect, (x, y)) { self.tr.balloon = None; self.redraw(); return false; }
        if let Some(a) = self.hits.iter().rev().find(|h| inside(h.r, (x, y))).map(|h| h.action.clone()) { self.run(a); }
        self.redraw();
        true
    }

    /// Apertou num pane; true = Alt+arrastar do tradutor (vira selecao do Duck).
    pub(super) fn tr_press(&mut self, pane: u64, cell: Cell, button: MouseButton) -> bool {
        self.tr.mouse = Mouse::Off;
        if !tl::translate_drag(self.tr_on(), self.mods.alt_key(), button == MouseButton::Left) { return false; }
        if pane != self.frame.active_pane { self.cmd(json!({"cmd": "focus", "pane": pane})); }
        self.sel = Some(Selection { pane, a: cell, b: cell });
        self.tr.balloon = None;
        self.tr.mouse = Mouse::Duck;
        self.dragging = true;
        true
    }

    /// Mexeu com o botao apertado; true = tratado.
    pub(super) fn tr_motion(&mut self, x: f32, y: f32) -> bool {
        if matches!(self.tr.mouse, Mouse::Off) { return false; }
        if let Some((pane, cell)) = self.cell_at(x, y) {
            if let Some(s) = self.sel.as_mut() { if s.pane == pane { s.b = cell; } }
        }
        true
    }

    /// Soltou o esquerdo; true = tratado. Copia e abre o balao da traducao.
    pub(super) fn tr_release(&mut self) -> bool {
        if matches!(std::mem::take(&mut self.tr.mouse), Mouse::Off) { return false; }
        self.dragging = false;
        if self.sel.as_ref().is_some_and(|s| s.a != s.b) { self.copy_selection(); self.tr_lookup_selection(); } else { self.sel = None; }
        true
    }

    /// Acoes do tradutor e do glossario; false = nome desconhecido.
    pub(super) fn tr_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        match name {
            "translate_toggle" => {
                let on = !self.cfg.translate.enabled;
                self.cfg.translate.enabled = on;
                if !on { self.tr.balloon = None; self.tr.mouse = Mouse::Off; }
                match set_local_setting("translate.enabled", if on { "true" } else { "false" }) {
                    Ok(_) => self.flash(if on { "Tradutor ligado: Ctrl+Shift+Enter no pane do agente; selecione um termo para traduzir" } else { "Tradutor desligado" }),
                    Err(e) => self.flash(&e),
                }
            }
            "glossary_add" => {
                let Some(b) = self.tr.balloon.take() else { return true };
                let BState::Ready(x) = &b.state else { return true };
                let meaning = [x.pos.as_str(), x.meaning.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(": ");
                match tl::glossary_add(&tl::glossary_path(), &b.term, &x.translation, &meaning, &b.context, &self.cfg.translate.to) {
                    Ok(true) => self.flash(&format!("Glossário: + {}", b.term)),
                    Ok(false) => self.flash(&format!("{} já está no glossário", b.term)),
                    Err(e) => self.flash(&e),
                }
            }
            "glossary" => {
                let mut all = tl::glossary_load(&tl::glossary_path());
                if all.is_empty() { self.flash("Glossário vazio: com o tradutor ligado, selecione um termo e use + glossário"); return true; }
                all.reverse();
                let items = all.iter().map(|v| {
                    let (t, tr, m) = (v["term"].as_str().unwrap_or(""), v["translation"].as_str().unwrap_or(""), v["meaning"].as_str().unwrap_or(""));
                    Item::new(format!("{t} → {tr}"), m, "glossary_copy", vec![json!(t)])
                }).collect();
                self.overlay = Some(Overlay::list("Glossário (Enter copia o termo)", items));
            }
            "glossary_copy" => {
                let t = args.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
                let _ = clip_set(&t);
                self.flash(&format!("Copiado: {t}"));
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_lines() {
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        assert_eq!(wrap("", 8), vec![""]);
        assert_eq!(wrap("abcdefghijkl", 8), vec!["abcdefgh", "ijkl"]);
        let st = ComposeState::Ready { src: "a".into(), out: tl::Composed { translation: "fix it".into(), notes: vec![tl::Note { en: "fix".into(), pt: "corrigir".into(), why: "verbo".into() }] } };
        let l = composer_lines(&st, 40);
        assert_eq!(l, vec![("fix it".to_string(), 0), ("• fix = corrigir: verbo".to_string(), 1)]);
        assert_eq!(composer_lines(&ComposeState::Idle, 40), vec![]);
        assert_eq!(composer_lines(&ComposeState::Pending { src: "a".into(), id: 1 }, 40)[0].1, 2);
    }
}
