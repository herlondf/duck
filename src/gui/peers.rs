//! Painel Computadores (Ctrl+Shift+P > Computadores): liga a escuta, procura
//! Ducks na rede (ou conecta por IP), pede conexao e lista os pareados com o
//! estado de cada um. O pedido mostra um numero de confirmacao nas duas telas;
//! o outro lado aceita num popup e as chaves sao trocadas sozinhas. A rede roda em threads; o resultado volta
//! pela fila de acoes (`peer_*_done`) e um Tick.

use super::*;
use crate::peer;

impl Gui {
    /// Abre (ou redesenha) o painel. `ping`: testa os pareados em segundo plano.
    pub(super) fn open_peers(&mut self, ping: bool) {
        let name = self.cfg.peer_name.clone();
        let adv = peer::advertised(&self.cfg.peer_listen);
        let mut items = vec![];
        match &adv {
            None => {
                let ip = peer::local_ip().unwrap_or_else(|| "0.0.0.0".into());
                items.push(Item::new(format!("○  Desligado. Enter liga na rede local ({ip}:{})", peer::DEFAULT_PORT),
                    "reinicia o server; os shells continuam", "peer_enable", vec![]));
            }
            Some(a) => {
                items.push(Item::new(format!("●  Este computador: {name} · escutando em {a}"), "Enter: trocar o nome", "peer_rename", vec![]));
            }
        }
        // Pedido feito daqui esperando o outro aceitar (sai quando ele aparece na lista).
        if let Some(w) = self.peer_status.get("\u{0}espera").cloned() {
            if self.cfg.peers.iter().any(|(n, _)| *n == w) { self.peer_status.remove("\u{0}espera"); } else {
                items.push(Item::new(format!("⏳  Esperando {w} aceitar · número de confirmação: {}", peer::confirm_code(&peer::self_token())),
                    "aceite lá só se aparecer o mesmo número · Enter cancela", "peer_wait_cancel", vec![]));
            }
        }
        let searching = self.peer_status.contains_key("\u{0}busca");
        let hint = match (&self.peer_found, searching) {
            (_, true) => "procurando…".to_string(),
            (None, _) => "acha os Ducks escutando nesta rede".to_string(),
            (Some(f), _) => format!("{} encontrado(s) · Enter procura de novo", f.len()),
        };
        items.push(Item::new("Procurar computadores na rede", hint, "peer_discover", vec![]));
        let known: Vec<&str> = self.cfg.peers.iter().map(|(_, a)| a.as_str()).collect();
        for f in self.peer_found.iter().flatten() {
            let (fname, faddr) = (f["name"].as_str().unwrap_or("duck"), f["addr"].as_str().unwrap_or(""));
            if Some(faddr) == adv.as_deref() || known.contains(&faddr) { continue; }
            items.push(Item::new(format!("+  Conectar com {fname} ({faddr})"), format!("{} · o outro aceita la", f["os"].as_str().unwrap_or("")),
                "peer_connect", vec![json!(fname), json!(faddr)]));
        }
        items.push(Item::new("Conectar por IP", "quando a busca não acha (outra rede, VPN) · o outro aceita lá", "peer_connect_ask", vec![]));
        if !self.cfg.peers.is_empty() {
            items.push(Item::new("── Computadores conectados ──", "", "", vec![]));
        }
        for (pname, paddr) in self.cfg.peers.clone() {
            let st = self.peer_status.get(&pname).cloned().unwrap_or_else(|| "testando…".into());
            let mark = if st.starts_with("online") { "●" } else { "○" };
            let tok = if crate::secret::get(&format!("peer/{pname}")).is_some() { "" } else { " · sem token" };
            items.push(Item::new(format!("{mark}  {pname} · {paddr}"), format!("{st}{tok} · Enter: opções"), "peer_menu", vec![json!(pname)]));
        }
        if adv.is_some() {
            items.push(Item::new(format!("Aprovar mensagens sem perguntar: {}", if self.cfg.peer_trust { "sim" } else { "não" }),
                "Enter muda · sim só entre computadores seus", "peer_trust", vec![]));
            if cfg!(windows) {
                items.push(Item::new("Firewall do Windows: copiar o comando que libera a porta", "cole num terminal de administrador", "peer_firewall", vec![]));
            }
            items.push(Item::new("Desligar (para de escutar na rede)", "reinicia o server", "peer_disable", vec![]));
        }
        let mut o = Overlay::list("Computadores · mensagens entre agentes (duck msg)", items);
        o.peers = true;
        // Redesenho (resultado de ping/busca): fica no item onde estava.
        if let Some(old) = self.overlay.as_ref().filter(|o| o.peers && o.input.is_empty()) { o.sel = old.sel.min(o.shown.len().saturating_sub(1)); }
        self.overlay = Some(o);
        if ping { self.peers_ping(); }
    }

    /// Testa cada pareado numa thread; o resultado chega como `peer_status_done`.
    fn peers_ping(&mut self) {
        for (name, addr) in self.cfg.peers.clone() {
            self.peer_status.insert(name.clone(), "testando…".into());
            let (q, proxy) = (self.queue.clone(), self.proxy.clone());
            std::thread::spawn(move || {
                let st = match peer::ping(&addr) { Ok(ms) => format!("online ({ms} ms)"), Err(e) => format!("sem resposta: {e}") };
                q.lock().unwrap().push(("peer_status_done".into(), vec![json!(name), json!(st)]));
                let _ = proxy.send_event(UserEv::Tick);
            });
        }
    }

    /// Roda `f` numa thread e devolve `{ok, ...}`/`{error}` como acao `done`.
    fn peers_bg(&self, done: &str, extra: Vec<Value>, f: impl FnOnce() -> Result<Value, String> + Send + 'static) {
        let (q, proxy, done) = (self.queue.clone(), self.proxy.clone(), done.to_string());
        std::thread::spawn(move || {
            let r = f().unwrap_or_else(|e| json!({"error": e}));
            let mut args = extra;
            args.push(r);
            q.lock().unwrap().push((done, args));
            let _ = proxy.send_event(UserEv::Tick);
        });
    }

    /// Endereco daqui para o pedido de conexao; desligado: liga antes (o
    /// outro precisa achar este computador para responder).
    fn peers_ensure_on(&mut self) -> Option<String> {
        if let Some(a) = peer::advertised(&self.cfg.peer_listen) { return Some(a); }
        let ip = peer::local_ip()?;
        let addr = format!("{ip}:{}", peer::DEFAULT_PORT);
        if let Err(e) = peer::enable(&self.cfg.peer_name, &addr) { self.flash(&e); return None; }
        self.cfg.peer_listen = addr.clone();
        let _ = self.call(json!({"cmd": "restart-server"}));
        Some(addr)
    }

    fn peers_hint(err: &str) -> String {
        let fw = if err.contains("timed out") || err.contains("tempo") || err.contains("refused") || err.contains("recusad") {
            " · confira se o Duck de la esta escutando (painel Computadores) e, no Windows, a porta 7878 liberada no firewall"
        } else { "" };
        format!("{err}{fw}")
    }

    pub(super) fn peers_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "peers" => self.open_peers(true),
            "peer_enable" => {
                let Some(ip) = peer::local_ip() else { self.flash("Sem rede: nao achei o IP desta maquina"); return true };
                let addr = format!("{ip}:{}", peer::DEFAULT_PORT);
                match peer::enable(&self.cfg.peer_name, &addr) {
                    Ok(_) => {
                        self.cfg.peer_listen = addr.clone();
                        let _ = self.call(json!({"cmd": "restart-server"}));
                        let fw = if cfg!(windows) { " · no Windows, libere a porta: item Firewall no painel" } else { "" };
                        self.flash(&format!("Escutando em {addr}{fw}"));
                        self.open_peers(false);
                    }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_disable" => match config::set_local_setting("peer.listen", "''") {
                Ok(_) => {
                    self.cfg.peer_listen.clear();
                    let _ = self.call(json!({"cmd": "restart-server"}));
                    self.flash("Desligado: este computador nao recebe mais mensagens");
                    self.open_peers(false);
                }
                Err(e) => self.flash(&e),
            },
            "peer_rename" => {
                let cur = self.cfg.peer_name.clone();
                self.overlay = Some(Overlay::prompt("Nome deste computador (o outro usa em duck msg <nome>)", &cur, "peer_rename_set", vec![]));
            }
            "peer_rename_set" => {
                let n = peer::clean_name(&s(0));
                match config::set_local_setting("peer.name", &format!("'{n}'")) {
                    Ok(_) => { self.cfg.peer_name = n.clone(); self.flash(&format!("Nome: {n}. Os ja pareados continuam te chamando pelo nome antigo ate parear de novo.")); self.open_peers(false); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_discover" => {
                self.peer_status.insert("\u{0}busca".into(), String::new());
                let mut ports = vec![peer::DEFAULT_PORT];
                let p = peer::port_of(&self.cfg.peer_listen);
                if !self.cfg.peer_listen.is_empty() && p != ports[0] { ports.push(p); }
                self.peers_bg("peer_discover_done", vec![], move || Ok(json!(peer::discover(&ports, Duration::from_millis(1500)))));
                self.open_peers(false);
            }
            "peer_discover_done" => {
                self.peer_status.remove("\u{0}busca");
                self.peer_found = args.first().and_then(|v| v.as_array().cloned());
                if self.peer_found.as_ref().is_some_and(|f| f.is_empty()) {
                    self.flash("Nenhum Duck respondeu. O outro precisa estar escutando (painel Computadores) e, no Windows, com a porta UDP 7878 liberada.");
                }
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_connect_ask" => {
                self.overlay = Some(Overlay::prompt("IP do outro computador (ex.: 192.168.0.20 ou 192.168.0.20:7878)", "", "peer_connect_ip", vec![]));
            }
            "peer_connect_ip" => {
                let ip = s(0).trim().to_string();
                if ip.is_empty() { return true; }
                let addr = if ip.contains(':') { ip.clone() } else { format!("{ip}:{}", peer::DEFAULT_PORT) };
                self.builtin("peer_connect", &[json!(""), json!(addr)]);
            }
            "peer_connect" => {
                let (pname, addr) = (s(0), s(1));
                let Some(my_addr) = self.peers_ensure_on() else { return true };
                let me = self.cfg.peer_name.clone();
                let label = if pname.is_empty() { addr.clone() } else { pname.clone() };
                self.flash(&format!("Pedindo conexão a {label}…"));
                let a = addr.clone();
                self.peers_bg("peer_connect_done", vec![json!(label)], move || peer::pair_request(&me, &my_addr, &a, None));
                self.open_peers(false);
            }
            "peer_connect_done" => {
                let r = args.get(1).cloned().unwrap_or(Value::Null);
                match r["error"].as_str() {
                    None => {
                        // O nome de verdade vem na resposta (por IP ainda nao sabiamos).
                        let n = r["name"].as_str().map(String::from).unwrap_or_else(|| s(0));
                        let code = peer::confirm_code(&peer::self_token());
                        self.peer_status.insert("\u{0}espera".into(), n.clone());
                        self.flash(&format!("Pedido enviado a {n}. Lá deve aparecer o número {code}: aceite só se for igual."));
                    }
                    Some(e) => self.flash(&format!("{}: {}", s(0), Self::peers_hint(e))),
                }
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_wait_cancel" => {
                // So esquece a espera daqui; o pedido la expira quando alguem negar.
                self.peer_status.remove("\u{0}espera");
                self.open_peers(false);
            }
            "peer_accept" => {
                let id = args.first().cloned().unwrap_or(json!(0));
                self.builtin("approve", &[id, json!(true)]);
                self.flash("Conectado ✓ (o outro computador foi avisado)");
            }
            "peer_status_done" => {
                self.peer_status.insert(s(0), s(1));
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_menu" => {
                let n = s(0);
                let addr = self.cfg.peers.iter().find(|(p, _)| *p == n).map(|(_, a)| a.clone()).unwrap_or_default();
                let items = vec![
                    Item::new("Testar conexão", addr.clone(), "peer_test", vec![json!(n)]),
                    Item::new("Mandar mensagem de teste para o agente de lá", format!("duck msg {n} \"...\""), "peer_msg", vec![json!(n)]),
                    Item::new("Remover", "esquece o endereço e o token deste lado", "peer_remove", vec![json!(n)]),
                    Item::new("Voltar", "", "peers", vec![]),
                ];
                self.overlay = Some(Overlay::list(&format!("{n} · {addr}"), items));
            }
            "peer_test" => { self.open_peers(true); self.flash(&format!("Testando {}…", s(0))); }
            "peer_msg" => {
                self.overlay = Some(Overlay::prompt(&format!("Mensagem para o agente de {}", s(0)), "Teste do Duck: responda só \"ok, recebi\".", "peer_msg_send", vec![json!(s(0))]));
            }
            "peer_msg_send" => {
                let (n, text) = (s(0), s(1));
                if text.trim().is_empty() { return true; }
                let out = match self.call(json!({"cmd": "peer-out", "peer": n, "text": text})) { Ok(v) => v, Err(e) => { self.flash(&e); return true } };
                let Some(token) = crate::secret::get(&format!("peer/{n}")) else { self.flash(&format!("Sem o token de {n}: pareie de novo")); return true };
                let req = json!({"token": token.trim(), "from": out["from"], "conv": out["conv"], "hops": out["hops"], "to": out["to"], "reply_pane": out["reply_pane"], "text": out["text"]});
                let addr = out["addr"].as_str().unwrap_or("").to_string();
                self.peers_bg("peer_msg_done", vec![json!(n)], move || {
                    let v = peer::send(&addr, &req)?;
                    if v["ok"].as_bool() == Some(true) { Ok(v) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
                });
                self.flash(&format!("Enviando para {n}…"));
            }
            "peer_msg_done" => {
                let r = args.get(1).cloned().unwrap_or(Value::Null);
                match r["error"].as_str() {
                    None if r["approval"].as_bool() == Some(true) => self.flash(&format!("Chegou em {}: espera aprovar lá", s(0))),
                    None => self.flash(&format!("Entregue ao agente de {}", s(0))),
                    Some(e) => self.flash(&format!("{}: {}", s(0), Self::peers_hint(e))),
                }
            }
            "peer_remove" => match peer::remove(&s(0)) {
                Ok(_) => { self.peer_status.remove(&s(0)); self.flash(&format!("{} removido", s(0))); }
                Err(e) => self.flash(&e),
            },
            "peer_trust" => {
                let on = !self.cfg.peer_trust;
                match config::set_local_setting("peer.trust", if on { "true" } else { "false" }) {
                    Ok(_) => { self.cfg.peer_trust = on; self.open_peers(false); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_firewall" => {
                let cmd = peer::windows_firewall_cmd(peer::port_of(&self.cfg.peer_listen));
                match clip_set(&cmd) {
                    Ok(_) => self.flash("Comando copiado: cole num terminal de administrador"),
                    Err(e) => self.flash(&format!("clipboard: {e}")),
                }
            }
            _ => return false,
        }
        true
    }
}
