//! Painel Celulares (Ctrl+Shift+4 > Celulares): parear pelo QR, ver os
//! celulares, ligar e desligar permissoes, revogar. O estado mora no server
//! (`mobile-*`); aqui so lista e manda.

use super::*;

/// O que cada permissao libera (docs/MOBILE.md).
fn scope_hint(s: &str) -> &'static str {
    match s {
        "view" => "ver agentes, telas, avisos, uso e baixar arquivos",
        "approve" => "aprovar pedidos do Duck e responder ao agente",
        "message" => "mandar recado para a fila do agente e traduzir",
        "control" => "abrir e parar agentes, loops, rotinas, mandar arquivos, fechar panes",
        "keyboard" => "digitar em qualquer pane não privado (shell incluído)",
        _ => "",
    }
}

fn ago(t: u64) -> String {
    let s = crate::mobile::now().saturating_sub(t);
    if t == 0 { "nunca".into() } else if s < 120 { "agora".into() } else if s < 7200 { format!("há {} min", s / 60) } else if s < 172_800 { format!("há {} h", s / 3600) } else { format!("há {} dias", s / 86400) }
}

impl Gui {
    fn open_mobile(&mut self) {
        let devs = match self.call(json!({"cmd": "mobile-list"})) { Ok(v) => v["devices"].as_array().cloned().unwrap_or_default(), Err(e) => return self.flash(&e) };
        let mut items = vec![Item::new("   Parear celular", "mostra o QR · o app lê e conecta · vale 10 min, uma vez", "mobile_pair", vec![])];
        if devs.is_empty() { items.push(Item::new("   Nenhum celular pareado", "app Duck (Android/iOS) > Parear > ler o QR", "", vec![])); }
        for d in &devs {
            let name = d["name"].as_str().unwrap_or("?");
            let on = d["online"] == true;
            let scopes: Vec<&str> = d["scopes"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
            items.push(Item::new(format!("{}  {name}{}", if on { "●" } else { "○" }, if on { " · conectado" } else { "" }),
                format!("{} · última conexão {}{} · Enter: permissões e revogar", scopes.join(", "), ago(d["last_seen"].as_u64().unwrap_or(0)), if d["push"] == true { " · push" } else { "" }),
                "mobile_menu", vec![json!(name)]));
        }
        self.overlay = Some(Overlay::list("Celulares: o app do Duck controla os agentes daqui", items));
    }

    fn open_mobile_menu(&mut self, name: &str) {
        let devs = match self.call(json!({"cmd": "mobile-list"})) { Ok(v) => v["devices"].as_array().cloned().unwrap_or_default(), Err(e) => return self.flash(&e) };
        let Some(d) = devs.iter().find(|d| d["name"] == name) else { return self.open_mobile() };
        let has = |s: &str| d["scopes"].as_array().is_some_and(|a| a.iter().any(|x| x == s));
        let mut items: Vec<Item> = crate::mobile::SCOPES.iter().map(|s| {
            let on = has(s);
            Item::new(format!("{}  {s}", if on { "☑" } else { "☐" }), format!("{} · Enter {}", scope_hint(s), if on { "desliga" } else { "liga" }),
                "mobile_scope", vec![json!(name), json!(format!("{}{s}", if on { "-" } else { "+" }))])
        }).collect();
        items.push(Item::new("   Revogar", "apaga a chave do celular e derruba as conexões dele agora", "mobile_revoke", vec![json!(name)]));
        items.push(Item::new("   Voltar", "", "mobile", vec![]));
        let sel = self.overlay.as_ref().filter(|o| o.title.contains(name)).map(|o| o.sel).unwrap_or(0);
        let mut o = Overlay::list(&format!("Celular {name}: permissões"), items);
        o.sel = sel.min(o.shown.len().saturating_sub(1));
        self.overlay = Some(o);
    }

    fn open_mobile_qr(&mut self) {
        let v = match self.call(json!({"cmd": "mobile-pair"})) { Ok(v) => v, Err(e) => return self.flash(&e) };
        let url = v["url"].as_str().unwrap_or("");
        match crate::mobile::qr_modules(url) {
            Ok((n, mods)) => {
                let via = if v["payload"]["relay"].is_string() { " · direto ou pelo relay" } else { " · só na mesma rede (sem relay)" };
                let mut o = Overlay::list("Parear celular: no app Duck, Parear > ler o QR", vec![]);
                o.qr = Some((n, mods, format!("Vale {} min, uma vez{via}. O Duck mostra um aviso com o número de confirmação. Esc fecha.", v["minutes"])));
                self.overlay = Some(o);
            }
            Err(e) => self.flash(&format!("QR: {e}")),
        }
    }

    pub(super) fn mobile_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "mobile" => self.open_mobile(),
            "mobile_pair" => self.open_mobile_qr(),
            "mobile_menu" => self.open_mobile_menu(&s(0)),
            "mobile_scope" => match self.call(json!({"cmd": "mobile-scopes", "name": s(0), "changes": [s(1)]})) {
                Ok(_) => self.open_mobile_menu(&s(0)),
                Err(e) => self.flash(&e),
            },
            "mobile_revoke" => {
                let n = s(0);
                self.overlay = Some(Overlay::dialog(&format!("Revogar o celular {n}? Ele perde o acesso na hora e precisa ler um QR novo."),
                    vec![Item::new("Revogar", "", "mobile_revoke_yes", vec![json!(n)]), Item::new("Cancelar", "", "mobile_menu", vec![json!(n)])]));
            }
            "mobile_revoke_yes" => match self.call(json!({"cmd": "mobile-revoke", "name": s(0)})) {
                Ok(_) => { self.flash(&format!("{} revogado", s(0))); self.open_mobile(); }
                Err(e) => self.flash(&e),
            },
            _ => return false,
        }
        self.redraw();
        true
    }
}
