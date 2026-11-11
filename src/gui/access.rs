//! Leitor de tela (AccessKit): a janela, as abas, o pane ativo como terminal,
//! a mensagem curta e os pedidos de aprovacao como regioes vivas. A arvore sai
//! do que a janela ja tem depois de cada quadro; so vai ao sistema o que mudou.

use std::collections::HashMap;

use accesskit::{Action as AkAction, ActionRequest, Live, Node, NodeId, Rect, Role, TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate};
use serde_json::json;

use super::{Gui, OverlayKind, PaneView};

const ROOT: NodeId = NodeId(1);
const TABS: NodeId = NodeId(2);
const TERM: NodeId = NodeId(3);
const STATUS: NodeId = NodeId(4);
const OVERLAY: NodeId = NodeId(5);
const OV_INPUT: NodeId = NodeId(6);
const OV_LIST: NodeId = NodeId(7);
const COMPOSER: NodeId = NodeId(8);
const TAB0: u64 = 1_000;
const LINE0: u64 = 100_000;
const ITEM0: u64 = 200_000;
const ALERT0: u64 = 300_000;
/// Itens da paleta expostos (o resto aparece ao filtrar).
const MAX_ITEMS: usize = 200;

/// Estado do agente na aba, do jeito que o leitor fala.
pub(super) fn tab_state(state: &str, attention: bool) -> Vec<&'static str> {
    let mut v = vec![];
    match state {
        "blocked" => v.push("agente pedindo aprovação"),
        "working" => v.push("agente trabalhando"),
        "done" => v.push("agente terminou"),
        _ => {}
    }
    if attention && state != "blocked" { v.push("pede atenção"); }
    v
}

/// Linha visivel como texto: sem as celulas de continuacao dos caracteres
/// largos e sem espacos no fim (mas ate o cursor, se ele estiver nela).
/// Devolve o texto e o indice (em chars) da coluna `col`.
pub(super) fn line_text(p: &PaneView, row: usize, col: Option<usize>) -> (String, usize) {
    let chars = p.row_chars(row);
    let mut spacer = vec![false; chars.len()];
    if let Some(runs) = p.lines().get(row) {
        for r in runs.iter().filter(|r| r.4 & 32 != 0) {
            for i in 0..r.1.chars().count() { if let Some(s) = spacer.get_mut(r.0 as usize + i * 2 + 1) { *s = true; } }
        }
    }
    let mut end = chars.iter().rposition(|c| *c != ' ').map(|e| e + 1).unwrap_or(0);
    if let Some(c) = col { end = end.max(c.min(chars.len())); }
    let mut s = String::new();
    let mut idx = 0;
    for (i, ch) in chars[..end].iter().enumerate() {
        if col == Some(i) { idx = s.chars().count(); }
        if !spacer[i] { s.push(*ch); }
    }
    if col.is_some_and(|c| c >= end) { idx = s.chars().count(); }
    (s, idx)
}

fn text_run(text: &str) -> Node {
    let mut n = Node::new(Role::TextRun);
    n.set_value(text);
    n.set_character_lengths(text.chars().map(|c| c.len_utf8() as u8).collect::<Vec<u8>>());
    n
}

impl Gui {
    pub(super) fn access_event(&mut self, ev: accesskit_winit::WindowEvent) {
        match ev {
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                self.access_on = true;
                self.access_prev.clear();
                self.access_tick();
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => { self.access_on = false; self.access_prev.clear(); }
            accesskit_winit::WindowEvent::ActionRequested(req) => self.access_action(req),
        }
    }

    /// Pedido do leitor: focar/clicar numa aba troca para ela; num item da lista, escolhe.
    fn access_action(&mut self, req: ActionRequest) {
        if req.target_tree != TreeId::ROOT || !matches!(req.action, AkAction::Focus | AkAction::Click) { return; }
        let id = req.target_node.0;
        if (TAB0..LINE0).contains(&id) {
            let idx = (id - TAB0) as usize;
            if self.frame.tabs.iter().any(|t| t.index == idx && !t.active) { self.cmd(json!({"cmd": "tab", "index": idx})); }
        } else if (ITEM0..ALERT0).contains(&id) {
            let k = (id - ITEM0) as usize;
            let Some(o) = self.overlay.as_mut() else { return };
            if k >= o.shown.len() { return; }
            o.sel = k;
            if req.action == AkAction::Click { let o = self.overlay.take().unwrap(); self.overlay_enter(o); }
            self.redraw();
        }
    }

    /// Depois de cada quadro: monta a arvore e manda so os nos que mudaram.
    pub(super) fn access_tick(&mut self) {
        if !self.access_on || self.access.is_none() { return; }
        let (nodes, focus) = self.access_tree();
        let full = self.access_prev.is_empty();
        let changed: Vec<(NodeId, Node)> = if full { nodes.clone() }
            else { nodes.iter().filter(|(id, n)| self.access_prev.get(id) != Some(n)).cloned().collect() };
        if !full && changed.is_empty() && self.access_focus == focus { return; }
        let mut sent = false;
        if let Some(a) = self.access.as_mut() {
            a.update_if_active(|| {
                sent = true;
                TreeUpdate {
                    nodes: changed,
                    tree: full.then(|| TreeInfo { toolkit_name: Some("Duck".into()), toolkit_version: Some(env!("CARGO_PKG_VERSION").into()), ..TreeInfo::new(ROOT) }),
                    tree_id: TreeId::ROOT,
                    focus,
                }
            });
        }
        if sent {
            self.access_prev = nodes.into_iter().collect::<HashMap<_, _>>();
            self.access_focus = focus;
        }
    }

    fn access_tree(&self) -> (Vec<(NodeId, Node)>, NodeId) {
        let mut out: Vec<(NodeId, Node)> = Vec::new();
        let mut kids = vec![TABS];
        let (cw, ch) = self.r.as_ref().map(|r| (r.cell_w as f64, r.cell_h as f64)).unwrap_or((8.0, 16.0));
        let (ox, oy) = (self.origin.0 as f64, self.origin.1 as f64);

        // ---- abas
        let mut tabs = Node::new(Role::TabList);
        tabs.set_label(format!("Abas do workspace {}", self.frame.workspace));
        let n = self.frame.tabs.len();
        let mut tab_ids = vec![];
        for (k, t) in self.frame.tabs.iter().enumerate() {
            let id = NodeId(TAB0 + t.index as u64);
            let mut parts = vec![self.tab_label(t, "")];
            parts.extend(tab_state(&t.state, t.attention).into_iter().map(String::from));
            if t.pinned { parts.push("fixada".into()); }
            if let Some((a, b)) = t.progress { parts.push(format!("progresso {a} de {b}")); }
            let mut node = Node::new(Role::Tab);
            node.set_label(parts.join(", "));
            node.set_selected(t.active);
            node.set_position_in_set(k + 1);
            node.set_size_of_set(n);
            node.add_action(AkAction::Focus);
            node.add_action(AkAction::Click);
            if let Some(&(x0, x1, _)) = self.tab_rects.iter().find(|r| r.2 == t.index) {
                node.set_bounds(Rect { x0: x0 as f64, y0: self.tab_row.0 as f64, x1: x1 as f64, y1: (self.tab_row.0 + self.tab_row.1) as f64 });
            }
            tab_ids.push(id);
            out.push((id, node));
        }
        tabs.set_children(tab_ids);
        out.push((TABS, tabs));

        // ---- pane ativo: terminal com uma linha por TextRun e o cursor como caret
        if let Some(p) = self.active() {
            let mut term = Node::new(Role::Terminal);
            let title = if p.title.is_empty() { "Terminal".to_string() } else { format!("Terminal: {}", p.title) };
            term.set_label(title);
            let cur = p.cursor.as_ref().map(|c| (c.0 as usize, c.1));
            let mut value = String::new();
            let mut lines = vec![];
            let mut caret = None;
            let rows = p.h as usize;
            for row in 0..rows {
                let col = cur.filter(|c| c.1 == row as i32).map(|c| c.0);
                let (mut s, idx) = line_text(p, row, col);
                let lid = NodeId(LINE0 + row as u64);
                if col.is_some() { caret = Some(TextPosition { node: lid, character_index: idx }); }
                if row + 1 < rows { s.push('\n'); }
                value.push_str(&s);
                let mut run = text_run(&s);
                let (x0, y0) = (ox + p.x as f64 * cw, oy + (p.y as f64 + row as f64) * ch);
                run.set_bounds(Rect { x0, y0, x1: x0 + p.w as f64 * cw, y1: y0 + ch });
                lines.push(lid);
                out.push((lid, run));
            }
            term.set_value(value.trim_end_matches('\n'));
            if let Some((_, row)) = cur {
                let (line, _) = line_text(p, row.max(0) as usize, None);
                term.set_description(format!("cursor na linha {}: {}", row + 1, line.trim()));
            }
            if let Some(c) = caret { term.set_text_selection(TextSelection { anchor: c, focus: c }); }
            term.set_children(lines);
            let (x0, y0) = (ox + p.x as f64 * cw, oy + p.y as f64 * ch);
            term.set_bounds(Rect { x0, y0, x1: x0 + p.w as f64 * cw, y1: y0 + p.h as f64 * ch });
            kids.push(TERM);
            out.push((TERM, term));
        }

        // ---- mensagem curta (o "toast" da janela): regiao viva educada
        let mut status = Node::new(Role::Status);
        status.set_live(Live::Polite);
        if let Some((msg, _)) = &self.message { status.set_label(msg.as_str()); }
        kids.push(STATUS);
        out.push((STATUS, status));

        // ---- pedidos de aprovacao: alerta (anuncia na hora)
        for (k, a) in self.frame.approvals.iter().enumerate() {
            let id = NodeId(ALERT0 + k as u64);
            let mut n = Node::new(Role::Alert);
            n.set_live(Live::Assertive);
            let what = match a.kind.as_str() { "clipboard" => "Clipboard", "peer" => "Mensagem de outro computador", "pair" => "Pedido de conexão", "file" => "Arquivo de outro computador", _ => "Agente pede confirmação" };
            n.set_label(format!("{what}: {}", a.text));
            kids.push(id);
            out.push((id, n));
        }

        let mut focus = if self.active().is_some() { TERM } else { ROOT };

        // ---- editor de comando
        if let (Some(c), true) = (&self.composer, self.overlay.is_none()) {
            let mut n = Node::new(Role::MultilineTextInput);
            n.set_label("Editor de comando");
            let text: String = c.text.iter().collect();
            n.set_value(text);
            n.set_description("Enter envia, Shift+Enter quebra linha, Esc fecha");
            kids.push(COMPOSER);
            out.push((COMPOSER, n));
            focus = COMPOSER;
        }

        // ---- paleta, listas, prompts e dialogos
        if let Some(o) = &self.overlay {
            let mut n = Node::new(if o.dialog { Role::AlertDialog } else { Role::Dialog });
            n.set_label(o.title.as_str());
            n.set_modal();
            if o.dialog { n.set_live(Live::Assertive); }
            let mut ok = vec![];
            focus = OVERLAY;
            if matches!(o.kind, OverlayKind::List | OverlayKind::Prompt(..)) && !o.dialog {
                let mut i = Node::new(Role::TextInput);
                i.set_label(if o.palette { "Buscar comando" } else { "Texto" });
                if !o.secret { i.set_value(o.input.as_str()); }
                ok.push(OV_INPUT);
                out.push((OV_INPUT, i));
                focus = OV_INPUT;
            }
            match &o.kind {
                OverlayKind::Detail { subtitle, lines, .. } => n.set_description(format!("{subtitle}\n{}", lines.join("\n"))),
                OverlayKind::Preview { file, lines, .. } => n.set_description(format!("{file}\n{}", lines.join("\n"))),
                OverlayKind::Slider { label, val, .. } => n.set_description(format!("{label}: {val}")),
                _ => {}
            }
            if !o.shown.is_empty() {
                let mut list = Node::new(Role::ListBox);
                let mut items = vec![];
                let total = o.shown.len().min(MAX_ITEMS);
                for (k, &i) in o.shown.iter().take(MAX_ITEMS).enumerate() {
                    let it = &o.items[i];
                    let id = NodeId(ITEM0 + k as u64);
                    let mut opt = Node::new(Role::ListBoxOption);
                    opt.set_label(it.label.replace('|', ", "));
                    if !it.hint.is_empty() { opt.set_description(it.hint.as_str()); }
                    opt.set_selected(k == o.sel);
                    opt.set_position_in_set(k + 1);
                    opt.set_size_of_set(total);
                    opt.add_action(AkAction::Focus);
                    opt.add_action(AkAction::Click);
                    if k == o.sel { focus = id; }
                    items.push(id);
                    out.push((id, opt));
                }
                list.set_children(items);
                ok.push(OV_LIST);
                out.push((OV_LIST, list));
            }
            n.set_children(ok);
            kids.push(OVERLAY);
            out.push((OVERLAY, n));
        }

        let mut root = Node::new(Role::Window);
        root.set_label(format!("Duck, workspace {}", self.frame.workspace));
        root.set_children(kids);
        out.push((ROOT, root));
        (out, focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::Run;

    fn pane(rows: Vec<Vec<Run>>) -> PaneView { PaneView { w: 10, h: rows.len() as u16, lines: Some(rows), ..Default::default() } }

    #[test]
    fn linha_sem_continuacao_de_largo_e_cursor() {
        // "日本" largo (flag 32) ocupa 4 colunas; depois "ab".
        let p = pane(vec![vec![Run(0, "日本".into(), 0, 0, 32, None), Run(4, "ab".into(), 0, 0, 0, None)]]);
        assert_eq!(line_text(&p, 0, None), ("日本ab".to_string(), 0));
        // Cursor na coluna 5 ("b") = char 3; depois do fim estica com espacos.
        assert_eq!(line_text(&p, 0, Some(5)).1, 3);
        assert_eq!(line_text(&p, 0, Some(8)), ("日本ab  ".to_string(), 6));
    }

    #[test]
    fn estado_da_aba_falado() {
        assert_eq!(tab_state("blocked", true), vec!["agente pedindo aprovação"]);
        assert_eq!(tab_state("", true), vec!["pede atenção"]);
        assert!(tab_state("idle", false).is_empty());
        assert!(crate::gui::ui::wide('日') && !crate::gui::ui::wide('a'));
    }
}
