//! Painel Computadores (Ctrl+Shift+P > Computadores). Secoes:
//! - Este computador: nome, rede de escuta, relay, convites, firewall, diagnostico.
//! - Pedidos: conexoes esperando aceite (daqui e de la) e arquivos chegando.
//! - Na rede: busca (rede local e Tailscale) e conectar por IP.
//! - Conectados: cada pareado com sistema, tempo de resposta, versao e se a
//!   conexao e cifrada; Enter abre agentes de la, mensagem, arquivo,
//!   diagnostico, enderecos.
//! - Conversas: as trocas entre agentes, com quantas idas e voltas; encerrar.
//!
//! A rede roda em threads; o resultado volta pela fila de acoes (`peer_*_done`)
//! e um Tick. O estado do server (pedidos, relay, versoes) vem de `peers-state`.

use super::*;
use crate::peer;

/// Icone do sistema (Nerd Font).
fn os_icon(os: &str) -> &'static str {
    match os { "linux" => "\u{f17c}", "windows" => "\u{f17a}", "macos" => "\u{f179}", _ => "\u{f108}" }
}

impl Gui {
    /// Abre (ou redesenha) o painel. `ping`: testa os pareados e le o estado
    /// do server em segundo plano.
    pub(super) fn open_peers(&mut self, ping: bool) {
        // O server pode ter caido numa porta reserva depois que a janela leu a config.
        peer::apply_bound(&mut self.cfg.peer_listen, &mut self.cfg.peer_name, self.cfg.peer_name_explicit);
        let name = self.cfg.peer_name.clone();
        let adv = peer::advertised(&self.cfg.peer_listen);
        let st = self.peer_state.clone();
        let mut items = vec![Item::new("── Este computador ──", "", "", vec![])];
        let key = st["key"].as_str().map(crate::secure::short).unwrap_or_default();
        match &adv {
            None => {
                items.push(Item::new(format!("○  {name} · {} · desligado", os_icon(std::env::consts::OS)), "Enter liga em todas as redes (reinicia o server; os shells continuam)", "peer_enable", vec![]));
            }
            Some(_) => {
                items.push(Item::new(format!("●  {name} · {} {} · chave {key}…", os_icon(std::env::consts::OS), std::env::consts::OS), "Enter: trocar o nome", "peer_rename", vec![]));
                items.push(Item::new(format!("   Rede: {}", Self::listen_label(&self.cfg.peer_listen)),
                    "Enter: escolher em qual rede escutar (casa, VMs, VPN ou todas)", "peer_listen_menu", vec![]));
                if let Some(conf) = peer::reserve_of() {
                    items.push(Item::new(format!("   Porta {}: a {} está com outro Duck deste computador", peer::port_of(&self.cfg.peer_listen), peer::port_of(&conf)),
                        "outro usuário logado com o Duck aberto · os outros acham este pela busca", "", vec![]));
                }
            }
        }
        let relay = if self.cfg.peer_relay.is_empty() { "desligado · para falar com computadores fora da rede".to_string() }
            else { st["relay"].as_str().filter(|r| !r.is_empty()).map(String::from).unwrap_or_else(|| format!("{} (conectando…)", self.cfg.peer_relay)) };
        items.push(Item::new(format!("   Relay: {relay}"), "Enter: configurar", "peer_relay_menu", vec![]));
        items.push(Item::new("   Gerar convite", "código que conecta sem aprovação · vale 10 min, uma vez · copia para a área de transferência", "peer_invite", vec![]));
        items.push(Item::new("   Usar convite de outro computador", "cole o código gerado lá", "peer_join_ask", vec![]));
        if adv.is_some() {
            let port = peer::port_of(&self.cfg.peer_listen);
            if cfg!(windows) {
                items.push(Item::new("   Firewall do Windows: copiar o comando que libera a porta", "cole num terminal de administrador", "peer_firewall", vec![]));
            } else if let Some(b) = self.peer_info.get("\u{0}firewall").and_then(|v| v.as_array()).filter(|b| !b.is_empty()) {
                let zones: Vec<String> = b.iter().filter_map(|z| z[0].as_str().map(String::from)).collect();
                items.push(Item::new(format!("⚠  Firewall bloqueia a porta {port} (zona {})", zones.join(", ")), "Enter copia o comando que libera (precisa de senha)", "peer_copy",
                    vec![json!(zones.iter().map(|z| peer::firewalld_cmd(z, port)).collect::<Vec<_>>().join(" && "))]));
            }
        }
        items.push(Item::new("   Diagnosticar este computador", "escuta, firewall e relay", "peer_doctor", vec![json!("")]));
        items.push(Item::new(format!("   Aprovar mensagens sem perguntar: {}", if self.cfg.peer_trust { "sim" } else { "não" }),
            "Enter muda · sim só entre computadores seus", "peer_trust", vec![]));

        // Pedidos: os daqui esperando o outro, os de la esperando voce.
        let pend = st["pending"].as_array().cloned().unwrap_or_default();
        let asks = st["asks"].as_array().cloned().unwrap_or_default();
        if !pend.is_empty() || !asks.is_empty() { items.push(Item::new("── Pedidos ──", "", "", vec![])); }
        for p in &pend {
            let code = self.peer_info.get(&format!("\u{0}confirm:{}", p["key"].as_str().unwrap_or(""))).and_then(|v| v.as_str()).unwrap_or("?").to_string();
            items.push(Item::new(format!("⏳  Esperando {} aceitar · número {code} · há {}s", p["addr"].as_str().unwrap_or(""), p["secs"]),
                "aceite lá só se aparecer o mesmo número · Enter cancela", "peer_wait_cancel", vec![p["key"].clone()]));
        }
        for a in &asks {
            let first = a["text"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
            let icon = if a["kind"] == "file" { "📄" } else { "❓" };
            items.push(Item::new(format!("{icon}  {first}"), "Enter: responder", "peer_ask", vec![a["id"].clone(), a["kind"].clone(), a["text"].clone()]));
        }

        // Na rede: busca e o que ela achou.
        items.push(Item::new("── Na rede ──", "", "", vec![]));
        let mine = crate::secure::short(&crate::secure::my_key());
        let mut found_new = vec![];
        let mut found_known = vec![];
        for f in self.peer_found.iter().flatten() {
            let faddr = f["addr"].as_str().unwrap_or("").to_string();
            let fkey = f["key"].as_str().unwrap_or("");
            if (!fkey.is_empty() && fkey == mine) || Some(faddr.as_str()) == adv.as_deref() || self.is_own_addr(&faddr) { continue; }
            let known = self.cfg.peers.iter().find(|(n, a)| (!fkey.is_empty() && peer::key_of(n).is_some_and(|k| k.starts_with(fkey))) || peer::split_addrs(a).contains(&faddr));
            match known {
                Some((n, _)) => found_known.push((n.clone(), faddr)),
                None => found_new.push(f.clone()),
            }
        }
        let searching = self.peer_status.contains_key("\u{0}busca");
        let hint = match (&self.peer_found, searching) {
            (_, true) => "procurando…".to_string(),
            (None, _) => "rede local, VMs e Tailscale".to_string(),
            (Some(_), _) if found_new.is_empty() && found_known.is_empty() => "nenhum outro Duck respondeu · Enter procura de novo".to_string(),
            (Some(_), _) => format!("{} novo(s), {} já conectado(s) · Enter procura de novo", found_new.len(), found_known.len()),
        };
        items.push(Item::new("   Procurar computadores", hint, "peer_discover", vec![]));
        for f in found_new {
            let (fname, faddr) = (f["name"].as_str().unwrap_or("duck"), f["addr"].as_str().unwrap_or(""));
            let ts = if f["via"] == "tailscale" { " · Tailscale" } else { "" };
            items.push(Item::new(format!("+  Conectar com {fname} ({faddr})"), format!("{} {} · Duck {}{ts} · o outro aceita lá", os_icon(f["os"].as_str().unwrap_or("")),
                f["os"].as_str().unwrap_or(""), f["version"].as_str().unwrap_or("?")), "peer_connect", vec![json!(fname), json!(faddr)]));
        }
        for (pname, faddr) in found_known {
            items.push(Item::new(format!("✓  {pname} ({faddr}) · já conectado"), "Enter: opções", "peer_menu", vec![json!(pname)]));
        }
        items.push(Item::new("   Conectar por IP", "outra rede, VPN, ou quando a busca não acha · o outro aceita lá", "peer_connect_ask", vec![]));

        // Conectados.
        if !self.cfg.peers.is_empty() { items.push(Item::new("── Conectados ──", "", "", vec![])); }
        for (pname, paddrs) in self.cfg.peers.clone() {
            let info = self.peer_info.get(&pname).cloned().unwrap_or(Value::Null);
            let online = info["online"].as_bool() == Some(true);
            let mark = if online { "●" } else { "○" };
            let os = info["os"].as_str().unwrap_or("");
            let lock = if peer::key_of(&pname).is_some() { "" } else { " · ⚠ sem criptografia: parear de novo" };
            let ver = info["version"].as_str().or(st["seen"][&pname]["version"].as_str()).unwrap_or("").to_string();
            let old = if !ver.is_empty() && crate::update::newer(peer::VERSION, &ver) { " · ⚠ desatualizado" } else { "" };
            let state = match info["state"].as_str() { Some(s) => s.to_string(), None => "testando…".into() };
            let n = peer::split_addrs(&paddrs).len();
            let label = format!("{mark}  {} {pname} · {state}{}", os_icon(os), if ver.is_empty() { String::new() } else { format!(" · Duck {ver}") });
            items.push(Item::new(label, format!("{n} endereço(s){old}{lock} · Enter: opções"), "peer_menu", vec![json!(pname)]));
        }

        // Conversas entre agentes.
        let convs = self.peer_info.get("\u{0}convs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let open: Vec<&Value> = convs.iter().filter(|c| c["closed"] != true).collect();
        if !open.is_empty() { items.push(Item::new("── Conversas ──", "", "", vec![])); }
        for c in open {
            items.push(Item::new(format!("💬  {} com {} · {}/{} idas e voltas", c["conv"].as_str().unwrap_or(""), c["peer"].as_str().unwrap_or(""), c["hops"], c["max"]),
                "Enter: encerrar (os agentes param de trocar mensagens)", "peer_conv_menu", vec![c["conv"].clone(), c["peer"].clone()]));
        }
        if adv.is_some() {
            items.push(Item::new("   Desligar (para de escutar na rede)", "reinicia o server", "peer_disable", vec![]));
        }
        let mut o = Overlay::list("Computadores · agentes de computadores diferentes conversando (duck msg)", items);
        o.peers = true;
        // Redesenho (resultado de ping/busca): fica no item onde estava. Na
        // primeira vez, comeca no primeiro item que faz algo (nao no titulo).
        if let Some(old) = self.overlay.as_ref().filter(|o| o.peers && o.input.is_empty()) { o.sel = old.sel.min(o.shown.len().saturating_sub(1)); }
        else { o.sel = o.shown.iter().position(|&i| !matches!(&o.items[i].action, Action::Builtin(a, _) if a.is_empty())).unwrap_or(0); }
        self.overlay = Some(o);
        if ping { self.peers_refresh(); }
    }

    /// Em segundo plano: estado do server, conversas, firewall e ping de cada pareado.
    fn peers_refresh(&mut self) {
        if let Ok(v) = self.call(json!({"cmd": "peers-state"})) { self.peer_state = v; }
        if let Ok(v) = self.call(json!({"cmd": "convs"})) { self.peer_info.insert("\u{0}convs".into(), v["convs"].clone()); }
        let port = peer::port_of(&self.cfg.peer_listen);
        if !self.cfg.peer_listen.is_empty() && !cfg!(windows) {
            self.peers_bg("peer_info_done", vec![json!("\u{0}firewall")], move || Ok(json!(peer::firewalld_blocked(port))));
        }
        for (name, addrs) in self.cfg.peers.clone() {
            let list = peer::split_addrs(&addrs);
            self.peers_bg("peer_info_done", vec![json!(name)], move || Ok(match peer::ping_any(&list) {
                Ok((i, a)) => json!({"online": true, "state": format!("{} ms", i.ms), "version": i.version, "os": i.os, "addr": a}),
                Err(e) if e == "so pelo relay" => json!({"online": true, "state": "pelo relay"}),
                Err(e) => {
                    let first = e.split(" · ").next().unwrap_or(&e);
                    json!({"online": false, "state": peer::human_err(first.split_once(": ").map_or(first, |(_, x)| x))})
                }
            }));
        }
    }

    /// Roda `f` numa thread e devolve `{ok, ...}`/`{error}` como acao `done`.
    pub(super) fn peers_bg(&self, done: &str, extra: Vec<Value>, f: impl FnOnce() -> Result<Value, String> + Send + 'static) {
        let (q, proxy, done) = (self.queue.clone(), self.proxy.clone(), done.to_string());
        std::thread::spawn(move || {
            let r = f().unwrap_or_else(|e| json!({"error": e}));
            let mut args = extra;
            args.push(r);
            q.lock().unwrap().push((done, args));
            let _ = proxy.send_event(UserEv::Tick);
        });
    }

    /// Escuta ligada (ou relay): o outro precisa achar este computador para
    /// responder. Desligada: liga em todas as redes antes.
    fn peers_ensure_on(&mut self) -> bool {
        if peer::advertised(&self.cfg.peer_listen).is_some() || !self.cfg.peer_relay.is_empty() { return true; }
        let addr = format!("0.0.0.0:{}", peer::DEFAULT_PORT);
        if let Err(e) = peer::enable(&self.cfg.peer_name, &addr) { self.flash(&e); return false; }
        self.cfg.peer_listen = addr;
        let _ = self.call(json!({"cmd": "restart-server"}));
        std::thread::sleep(Duration::from_millis(600));
        true
    }

    fn peers_hint(err: &str) -> String {
        let fw = if err.contains("timed out") || err.contains("tempo") || err.contains("refused") || err.contains("recusad") {
            " · use Diagnosticar no painel Computadores"
        } else { "" };
        format!("{err}{fw}")
    }

    /// Descricao da escuta: "todas as redes" ou a placa e o IP.
    fn listen_label(listen: &str) -> String {
        let port = peer::port_of(listen);
        if peer::listens_all(listen) { return format!("todas as redes (porta {port})"); }
        let host = listen.rsplit_once(':').map_or(listen, |(h, _)| h);
        match peer::interfaces().into_iter().find(|i| i.ip == host) {
            Some(i) => format!("só {} · {}:{port}", i.name, i.ip),
            None => format!("só {host}:{port} (essa rede não existe mais aqui)"),
        }
    }

    /// `addr` e um IP deste computador (a busca acha o proprio Duck por cada rede).
    fn is_own_addr(&self, addr: &str) -> bool {
        let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
        peer::port_of(addr) == peer::port_of(&self.cfg.peer_listen) && peer::interfaces().iter().any(|i| i.ip == host)
    }

    /// Pergunta de aprovacao (conexao, arquivo, mensagem) como dialogo.
    pub(super) fn approval_dialog(&mut self, id: Value, kind: &str, text: &str) {
        let (yes, title) = match kind {
            "clipboard" => ("Permitir copiar", "Clipboard"),
            "peer" => ("Entregar ao agente", "Mensagem de outro computador"),
            "pair" => ("Conectar", "Pedido de conexão"),
            "file" => ("Aceitar o arquivo", "Arquivo de outro computador"),
            _ => ("Permitir", "Agente pede confirmação"),
        };
        // Negar primeiro: Enter distraido nao libera nada.
        let items = vec![
            Item::new("Negar", "Esc", "approve", vec![id.clone(), json!(false)]),
            if kind == "pair" { Item::new(yes, "guarda o outro computador dos dois lados", "peer_accept", vec![id]) }
            else { Item::new(yes, "", "approve", vec![id, json!(true)]) },
        ];
        self.overlay = Some(Overlay::dialog(&format!("{title}: {text}"), items));
    }

    fn peer_set_listen(&mut self, addr: String) {
        match config::set_local_setting("peer.listen", &format!("'{addr}'")) {
            Ok(_) => {
                self.cfg.peer_listen = addr.clone();
                let _ = self.call(json!({"cmd": "restart-server"}));
                self.flash(&format!("Escutando em {}", Self::listen_label(&addr)));
                self.open_peers(false);
            }
            Err(e) => self.flash(&e),
        }
    }

    pub(super) fn peers_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "peers" => self.open_peers(true),
            "peer_enable" => {
                if let Err(e) = peer::enable(&self.cfg.peer_name, &format!("0.0.0.0:{}", peer::DEFAULT_PORT)) { self.flash(&e); return true; }
                self.cfg.peer_listen = format!("0.0.0.0:{}", peer::DEFAULT_PORT);
                let _ = self.call(json!({"cmd": "restart-server"}));
                let fw = if cfg!(windows) { " · no Windows, libere a porta: item Firewall no painel" } else { "" };
                self.flash(&format!("Escutando em todas as redes{fw}"));
                self.open_peers(true);
            }
            "peer_disable" => match config::set_local_setting("peer.listen", "''") {
                Ok(_) => {
                    self.cfg.peer_listen.clear();
                    let _ = self.call(json!({"cmd": "restart-server"}));
                    self.flash("Desligado: este computador não recebe mais mensagens");
                    self.open_peers(false);
                }
                Err(e) => self.flash(&e),
            },
            "peer_listen_menu" => {
                let port = peer::port_of(&self.cfg.peer_listen);
                let cur = self.cfg.peer_listen.trim().to_string();
                let mark = |on: bool| if on { "●" } else { "○" };
                let mut items = vec![Item::new(format!("{}  Todas as redes", mark(peer::listens_all(&cur))),
                    "casa, VMs e VPN ao mesmo tempo · a conexão continua exigindo aprovação ou convite", "peer_listen_set", vec![json!(format!("0.0.0.0:{port}"))])];
                for i in peer::interfaces() {
                    let addr = format!("{}:{port}", i.ip);
                    items.push(Item::new(format!("{}  Só {} · {}", mark(cur == addr), i.name, i.ip),
                        "só os computadores desta rede alcançam este", "peer_listen_set", vec![json!(addr)]));
                }
                items.push(Item::new("Voltar", "", "peers", vec![]));
                self.overlay = Some(Overlay::list("Em qual rede este computador escuta? (reinicia o server; os shells continuam)", items));
            }
            "peer_listen_set" => {
                let addr = s(0);
                if addr.is_empty() || addr == self.cfg.peer_listen.trim() { self.open_peers(false); return true; }
                self.peer_set_listen(addr);
            }
            "peer_relay_menu" => {
                let mut items = vec![Item::new("Usar um relay (host:porta)", "o relay roda num computador que todos alcançam: duck relay --listen 0.0.0.0:7879", "peer_relay_ask", vec![])];
                if !self.cfg.peer_relay.is_empty() {
                    items.push(Item::new("Senha do relay", if self.cfg.peer_relay_secret.is_empty() { "nenhuma" } else { "definida" }, "peer_relay_secret_ask", vec![]));
                    items.push(Item::new("Desligar o relay", "", "peer_relay_set", vec![json!("")]));
                }
                items.push(Item::new("Voltar", "", "peers", vec![]));
                self.overlay = Some(Overlay::list("Relay: liga computadores em redes diferentes (tudo continua cifrado de ponta a ponta)", items));
            }
            "peer_relay_ask" => {
                let cur = self.cfg.peer_relay.clone();
                self.overlay = Some(Overlay::prompt("Relay (ex.: meu-vps.com:7879)", &cur, "peer_relay_set", vec![]));
            }
            "peer_relay_set" => {
                let mut r = s(0).trim().replace('\'', "");
                if !r.is_empty() && !r.contains(':') { r = format!("{r}:7879"); }
                match config::set_local_setting("peer.relay", &format!("'{r}'")) {
                    Ok(_) => {
                        self.cfg.peer_relay = r.clone();
                        let _ = self.call(json!({"cmd": "restart-server"}));
                        self.flash(&if r.is_empty() { "Relay desligado".to_string() } else { format!("Relay: {r}. Pareie de novo (ou use convite) para os outros aprenderem o caminho") });
                        self.open_peers(true);
                    }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_relay_secret_ask" => {
                let mut o = Overlay::prompt("Senha do relay (a mesma do duck relay --secret)", "", "peer_relay_secret_set", vec![]);
                o.secret = true;
                self.overlay = Some(o);
            }
            "peer_relay_secret_set" => {
                match config::set_local_setting("peer.relay_secret", &format!("'{}'", s(0).replace('\'', ""))) {
                    Ok(_) => { self.cfg.peer_relay_secret = s(0); let _ = self.call(json!({"cmd": "restart-server"})); self.flash("Senha do relay guardada"); self.open_peers(true); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_invite" => {
                if !self.peers_ensure_on() { return true; }
                match self.call(json!({"cmd": "peer-invite"})) {
                    Ok(v) => {
                        let inv = v["invite"].as_str().unwrap_or("").to_string();
                        let copied = clip_set(&inv).is_ok();
                        let items = vec![
                            Item::new(inv.clone(), if copied { "copiado · Enter copia de novo" } else { "Enter copia" }, "peer_copy", vec![json!(inv)]),
                            Item::new("Voltar", "", "peers", vec![]),
                        ];
                        self.overlay = Some(Overlay::list("Convite (vale 10 min, uma vez). No outro: Computadores > Usar convite, ou duck peer join '<convite>'", items));
                    }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_copy" => match clip_set(&s(0)) {
                Ok(_) => self.flash("Copiado"),
                Err(e) => self.flash(&format!("clipboard: {e}")),
            },
            "peer_join_ask" => {
                self.overlay = Some(Overlay::prompt("Convite do outro computador (duck:nome@...#...)", "", "peer_join", vec![]));
            }
            "peer_join" => {
                let inv = match peer::parse_invite(&s(0)) { Ok(i) => i, Err(e) => { self.flash(&e); return true } };
                self.flash(&format!("Conectando com {}…", inv.name));
                self.peers_bg("peer_join_done", vec![], move || {
                    let cfg = config::load_config().0;
                    let mut errs = vec![];
                    for addr in &inv.addrs {
                        match peer::pair_request(addr, Some(&inv.code), Some(&inv.key_prefix), |_| {}) {
                            Ok(p) if p.accepted => {
                                let name = peer::free_name(&cfg.peers, &p.name, &p.key);
                                let mut addrs = vec![addr.clone()];
                                addrs.extend(inv.addrs.iter().filter(|x| *x != addr).cloned());
                                peer::save(&name, &addrs, &p.key)?;
                                return Ok(json!({"name": name}));
                            }
                            Ok(_) => errs.push(format!("{addr}: convite não aceito")),
                            Err(e) => errs.push(format!("{addr}: {e}")),
                        }
                    }
                    Err(errs.join(" · "))
                });
            }
            "peer_join_done" => {
                let r = args.first().cloned().unwrap_or(Value::Null);
                match r["error"].as_str() {
                    None => {
                        let _ = self.call(json!({"cmd": "peers-reload"}));
                        self.cfg = config::load_config().0;
                        self.flash(&format!("Conectado com {}", r["name"].as_str().unwrap_or("?")));
                    }
                    Some(e) => self.flash(&Self::peers_hint(e)),
                }
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(true); }
            }
            "peer_rename" => {
                let cur = self.cfg.peer_name.clone();
                self.overlay = Some(Overlay::prompt("Nome deste computador (o outro usa em duck msg <nome>)", &cur, "peer_rename_set", vec![]));
            }
            "peer_rename_set" => {
                let n = peer::clean_name(&s(0));
                match config::set_local_setting("peer.name", &format!("'{n}'")) {
                    Ok(_) => { self.cfg.peer_name = n.clone(); self.flash(&format!("Nome: {n}. Os já pareados continuam te chamando pelo nome antigo até parear de novo.")); self.open_peers(false); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_discover" => {
                self.peer_status.insert("\u{0}busca".into(), String::new());
                let mut ports = vec![peer::DEFAULT_PORT];
                let p = peer::port_of(&self.cfg.peer_listen);
                if !self.cfg.peer_listen.is_empty() && p != ports[0] { ports.push(p); }
                self.peers_bg("peer_discover_done", vec![], move || {
                    let found = peer::discover(&ports, Duration::from_millis(1500));
                    // Pareado achado num endereco novo: guarda (o server relê abaixo).
                    if !peer::learn_from_discovery(&found).is_empty() { if let Ok(mut c) = crate::proto::Conn::connect() { let _ = c.call(json!({"cmd": "peers-reload"})); } }
                    Ok(json!(found))
                });
                self.open_peers(false);
            }
            "peer_discover_done" => {
                self.peer_status.remove("\u{0}busca");
                self.peer_found = args.first().and_then(|v| v.as_array().cloned());
                self.cfg.peers = config::load_config().0.peers;
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_connect_ask" => {
                self.overlay = Some(Overlay::prompt("IP do outro computador (ex.: 192.168.0.20, 100.64.0.5:7878 ou relay:host:7879 com a chave)", "", "peer_connect_ip", vec![]));
            }
            "peer_connect_ip" => {
                let ip = s(0).trim().to_string();
                if ip.is_empty() { return true; }
                let addr = if ip.contains(':') { ip.clone() } else { format!("{ip}:{}", peer::DEFAULT_PORT) };
                self.builtin("peer_connect", &[json!(""), json!(addr)]);
            }
            "peer_connect" => {
                let (pname, addr) = (s(0), s(1));
                if !self.peers_ensure_on() { return true; }
                let label = if pname.is_empty() { addr.clone() } else { pname.clone() };
                self.flash(&format!("Pedindo conexão a {label}…"));
                let a = addr.clone();
                self.peers_bg("peer_connect_done", vec![json!(label), json!(addr)], move || {
                    // Registra o pedido no server logo apos o handshake: ele
                    // consulta o outro ate aceitar e recebe o aviso de aceite.
                    let reg = |k: &str| { if let Ok(mut c) = crate::proto::Conn::connect() { let _ = c.call(json!({"cmd": "peer-pending", "key": k, "addr": a})); } };
                    let p = peer::pair_request(&a, None, None, reg)?;
                    Ok(json!({"name": p.name, "key": p.key, "confirm": p.confirm, "accepted": p.accepted}))
                });
                self.open_peers(false);
            }
            "peer_connect_done" => {
                let r = args.get(2).cloned().unwrap_or(Value::Null);
                match r["error"].as_str() {
                    None if r["accepted"] == true => {
                        let cfg = config::load_config().0;
                        let (k, n) = (r["key"].as_str().unwrap_or(""), r["name"].as_str().unwrap_or("duck"));
                        let name = peer::free_name(&cfg.peers, n, k);
                        let _ = peer::save(&name, &[s(1)], k);
                        let _ = self.call(json!({"cmd": "peer-pending-cancel", "key": k}));
                        let _ = self.call(json!({"cmd": "peers-reload"}));
                        self.cfg = config::load_config().0;
                        self.flash(&format!("Já estavam pareados: {name}"));
                    }
                    None => {
                        let n = r["name"].as_str().unwrap_or("?").to_string();
                        let code = r["confirm"].as_str().unwrap_or("?").to_string();
                        self.peer_info.insert(format!("\u{0}confirm:{}", r["key"].as_str().unwrap_or("")), json!(code));
                        if let Ok(v) = self.call(json!({"cmd": "peers-state"})) { self.peer_state = v; }
                        self.flash(&format!("Pedido enviado a {n}. Lá deve aparecer o número {code}: aceite só se for igual."));
                    }
                    Some(e) => self.flash(&format!("{}: {}", s(0), Self::peers_hint(e))),
                }
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_wait_cancel" => {
                let _ = self.call(json!({"cmd": "peer-pending-cancel", "key": s(0)}));
                if let Ok(v) = self.call(json!({"cmd": "peers-state"})) { self.peer_state = v; }
                self.open_peers(false);
            }
            "peer_ask" => {
                let (id, kind, text) = (args.first().cloned().unwrap_or(json!(0)), s(1), s(2));
                self.approval_dialog(id, &kind, &text);
            }
            "peer_accept" => {
                let id = args.first().cloned().unwrap_or(json!(0));
                self.builtin("approve", &[id, json!(true)]);
                self.flash("Conectado ✓ (o outro computador foi avisado)");
            }
            "peer_info_done" => {
                let v = args.get(1).cloned().unwrap_or(Value::Null);
                self.peer_info.insert(s(0), v);
                if self.overlay.as_ref().is_some_and(|o| o.peers) { self.open_peers(false); }
            }
            "peer_menu" => {
                let n = s(0);
                let addrs = peer::addrs_of(&n);
                let legacy = peer::key_of(&n).is_none();
                let mut items = vec![
                    Item::new("Agentes de lá", "lista as sessões abertas e manda mensagem para uma", "peer_sessions", vec![json!(n)]),
                    Item::new("Mandar mensagem ao agente de lá", format!("duck msg {n} \"...\" · vai para o agente da tela de lá"), "peer_msg", vec![json!(n), json!(null)]),
                    Item::new("Enviar arquivo", "até 64 MB · lá vai para Downloads/Duck depois de aceito", "peer_file_ask", vec![json!(n)]),
                    Item::new("Diagnosticar", "endereços, criptografia, versão e se o outro alcança este de volta", "peer_doctor", vec![json!(n)]),
                    Item::new(format!("Endereços ({})", addrs.len()), addrs.join(", "), "peer_addrs", vec![json!(n)]),
                ];
                if legacy || !addrs.is_empty() {
                    let addr = addrs.iter().find(|a| !a.starts_with("relay:")).cloned().unwrap_or_default();
                    if !addr.is_empty() {
                        items.push(Item::new(if legacy { "Parear de novo (com criptografia)" } else { "Parear de novo" }, "pede conexão de novo pelo primeiro endereço", "peer_connect", vec![json!(n), json!(addr)]));
                    }
                }
                items.push(Item::new("Remover", "esquece a chave e os endereços deste lado", "peer_remove", vec![json!(n)]));
                items.push(Item::new("Voltar", "", "peers", vec![]));
                let info = self.peer_info.get(&n).cloned().unwrap_or(Value::Null);
                let title = format!("{} {n} · {}{}", os_icon(info["os"].as_str().unwrap_or("")), info["state"].as_str().unwrap_or(""),
                    if legacy { " · sem criptografia" } else { " · cifrado" });
                self.overlay = Some(Overlay::list(&title, items));
            }
            "peer_addrs" => {
                let n = s(0);
                let mut items: Vec<Item> = peer::addrs_of(&n).into_iter().enumerate().map(|(i, a)| Item::new(format!("{}  {a}", i + 1),
                    if i == 0 { "o primeiro é o que funcionou por último · Enter remove" } else { "Enter remove" }, "peer_addr_remove", vec![json!(n), json!(a)])).collect();
                items.push(Item::new("Adicionar endereço", "ip:porta, ou relay:host:porta", "peer_addr_ask", vec![json!(n)]));
                items.push(Item::new("Voltar", "", "peer_menu", vec![json!(n)]));
                self.overlay = Some(Overlay::list(&format!("Endereços de {n} (tenta na ordem; os novos aparecem sozinhos quando ele fala daqui)"), items));
            }
            "peer_addr_ask" => {
                self.overlay = Some(Overlay::prompt(&format!("Endereço novo de {}", s(0)), "", "peer_addr_add", vec![json!(s(0))]));
            }
            "peer_addr_add" | "peer_addr_remove" => {
                let (n, a) = (s(0), s(1).trim().to_string());
                let mut list = peer::addrs_of(&n);
                if name == "peer_addr_add" {
                    if a.is_empty() { return true; }
                    let a = if a.contains(':') { a } else { format!("{a}:{}", peer::DEFAULT_PORT) };
                    if !list.contains(&a) { list.push(a); }
                } else {
                    if list.len() <= 1 { self.flash("Precisa ficar pelo menos um endereço (ou Remover o computador)"); return true; }
                    list.retain(|x| *x != a);
                }
                match peer::save_addrs(&n, &list) {
                    Ok(_) => { let _ = self.call(json!({"cmd": "peers-reload"})); self.cfg = config::load_config().0; self.builtin("peer_addrs", &[json!(n)]); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_doctor" => {
                let n = s(0);
                self.flash(if n.is_empty() { "Diagnosticando…" } else { "Diagnosticando (pode levar alguns segundos)…" });
                let who = n.clone();
                self.peers_bg("peer_doctor_done", vec![json!(n)], move || {
                    let cs = crate::cli::diagnose((!who.is_empty()).then_some(who.as_str()));
                    Ok(json!(cs.into_iter().map(|c| json!({"ok": c.ok, "text": c.text})).collect::<Vec<_>>()))
                });
            }
            "peer_doctor_done" => {
                let n = s(0);
                let r = args.get(1).cloned().unwrap_or(Value::Null);
                if let Some(e) = r["error"].as_str() { self.flash(e); return true; }
                let mut items: Vec<Item> = r.as_array().into_iter().flatten().map(|c| {
                    let t = c["text"].as_str().unwrap_or("").to_string();
                    let m = match c["ok"].as_bool() { Some(true) => "✓", Some(false) => "✗", None => "!" };
                    // Linha com comando: Enter copia o comando.
                    let cmd = t.split_once("Libere com: ").map(|(_, c)| c.to_string());
                    match cmd {
                        Some(c) => Item::new(format!("{m}  {t}"), "Enter copia o comando", "peer_copy", vec![json!(c)]),
                        None => Item::new(format!("{m}  {t}"), "", "peer_copy", vec![json!(t)]),
                    }
                }).collect();
                items.push(Item::new("Voltar", "", if n.is_empty() { "peers" } else { "peer_menu" }, if n.is_empty() { vec![] } else { vec![json!(n)] }));
                self.overlay = Some(Overlay::list(&format!("Diagnóstico{}", if n.is_empty() { " deste computador".to_string() } else { format!(" · {n}") }), items));
            }
            "peer_sessions" => {
                let n = s(0);
                self.flash(&format!("Lendo os agentes de {n}…"));
                let who = n.clone();
                self.peers_bg("peer_sessions_done", vec![json!(n)], move || {
                    let (v, _) = peer::request(&who, &json!({"sessions": 1}))?;
                    if v["ok"].as_bool() == Some(true) { Ok(v) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
                });
            }
            "peer_sessions_done" => {
                let n = s(0);
                let r = args.get(1).cloned().unwrap_or(Value::Null);
                if let Some(e) = r["error"].as_str() { self.flash(&format!("{n}: {}", Self::peers_hint(e))); return true; }
                let mut items: Vec<Item> = r["sessions"].as_array().into_iter().flatten().map(|x| session_item(&n, x, false)).collect();
                if items.is_empty() { items.push(Item::new("Nenhum agente aberto lá", "", "", vec![])); }
                items.push(Item::new("Voltar", "", "peer_menu", vec![json!(n)]));
                self.overlay = Some(Overlay::list(&format!("Agentes de {n}"), items));
            }
            "peer_msg_pick" => {
                let peers: Vec<String> = self.cfg.peers.iter().map(|(n, _)| n.clone()).collect();
                if peers.is_empty() { self.flash("Nenhum computador pareado: Ctrl+Shift+P > Computadores"); return true; }
                self.flash("Procurando agentes nos outros computadores…");
                self.peers_bg("peer_msg_pick_done", vec![], move || {
                    // Em paralelo: um computador desligado nao segura os outros.
                    let hs: Vec<_> = peers.into_iter().map(|n| std::thread::spawn(move || {
                        let r = peer::request(&n, &json!({"sessions": 1}));
                        match r {
                            Ok((v, _)) if v["ok"].as_bool() == Some(true) => json!({"peer": n, "sessions": v["sessions"]}),
                            Ok((v, _)) => json!({"peer": n, "error": v["error"]}),
                            Err(e) => json!({"peer": n, "error": e}),
                        }
                    })).collect();
                    Ok(json!(hs.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()))
                });
            }
            "peer_msg_pick_done" => {
                let r = args.first().cloned().unwrap_or(Value::Null);
                if let Some(e) = r["error"].as_str() { self.flash(e); return true; }
                let mut items: Vec<Item> = vec![];
                let mut off: Vec<String> = vec![];
                for p in r.as_array().into_iter().flatten() {
                    let n = p["peer"].as_str().unwrap_or("").to_string();
                    match p["sessions"].as_array() {
                        Some(ss) => items.extend(ss.iter().map(|x| session_item(&n, x, true))),
                        None => off.push(n),
                    }
                }
                if items.is_empty() { items.push(Item::new("Nenhum agente aberto nos outros computadores", "", "", vec![])); }
                if !off.is_empty() { items.push(Item::new(format!("Sem resposta: {}", off.join(", ")), "desligado, firewall ou versão antiga · Computadores > nome > Diagnosticar", "peers", vec![])); }
                self.overlay = Some(Overlay::list("Mandar mensagem para… (aba de cada computador)", items));
            }
            "peer_msg" => {
                let to = args.get(1).cloned().unwrap_or(Value::Null);
                let label = s(2);
                let title = if to.is_null() { format!("Mensagem para o agente de {}", s(0)) }
                    else if label.is_empty() { format!("Mensagem para o pane {} de {}", to, s(0)) }
                    else { format!("Mensagem para {} › {label}", s(0)) };
                self.overlay = Some(Overlay::prompt(&title, "Teste do Duck: responda só \"ok, recebi\".", "peer_msg_send", vec![json!(s(0)), to]));
            }
            "peer_msg_send" => {
                let n = s(0);
                let to = args.get(1).cloned().unwrap_or(Value::Null);
                let text = s(2);
                if text.trim().is_empty() { return true; }
                let out = match self.call(json!({"cmd": "peer-out", "peer": n, "text": text})) { Ok(v) => v, Err(e) => { self.flash(&e); return true } };
                let to = if to.is_null() { out["to"].clone() } else { to };
                let req = json!({"msg": 1, "conv": out["conv"], "hops": out["hops"], "to": to, "reply_pane": out["reply_pane"], "text": out["text"]});
                let who = n.clone();
                self.peers_bg("peer_msg_done", vec![json!(n)], move || {
                    let (v, _) = peer::request(&who, &req)?;
                    if v["ok"].as_bool() == Some(true) { Ok(v) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
                });
                self.flash(&format!("Enviando para {n}…"));
            }
            "peer_msg_done" | "peer_file_done" => {
                let r = args.get(1).cloned().unwrap_or(Value::Null);
                let what = if name == "peer_file_done" { "Arquivo" } else { "Mensagem" };
                match r["error"].as_str() {
                    None if r["approval"].as_bool() == Some(true) => self.flash(&format!("{what} chegou em {}: espera aprovar lá", s(0))),
                    None => self.flash(&format!("{what} entregue em {}", s(0))),
                    Some(e) => self.flash(&format!("{}: {}", s(0), Self::peers_hint(e))),
                }
            }
            "peer_file_ask" => {
                self.overlay = Some(Overlay::prompt(&format!("Arquivo para {} (caminho completo)", s(0)), "", "peer_file_send", vec![json!(s(0))]));
            }
            "peer_file_send" => {
                let (n, path) = (s(0), s(1).trim().trim_matches(['"', '\'']).to_string());
                if path.is_empty() { return true; }
                self.flash(&format!("Enviando {path} para {n}…"));
                let who = n.clone();
                self.peers_bg("peer_file_done", vec![json!(n)], move || {
                    use base64::Engine;
                    let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
                    if data.len() > 64 << 20 { return Err("máximo 64 MB".into()); }
                    let fname = std::path::Path::new(&path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "arquivo".into());
                    let req = json!({"file": 1, "name": fname, "data": base64::engine::general_purpose::STANDARD.encode(&data)});
                    let (v, _) = peer::request(&who, &req)?;
                    if v["ok"].as_bool() == Some(true) { Ok(v) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
                });
            }
            "peer_conv_menu" => {
                let (c, p) = (s(0), s(1));
                let items = vec![
                    Item::new("Encerrar a conversa", "mensagens novas dela são recusadas dos dois lados", "peer_conv_close", vec![json!(c)]),
                    Item::new("Voltar", "", "peers", vec![]),
                ];
                self.overlay = Some(Overlay::list(&format!("Conversa {c} com {p}"), items));
            }
            "peer_conv_close" => {
                match self.call(json!({"cmd": "conv-close", "conv": s(0)})) {
                    Ok(_) => { self.flash(&format!("Conversa {} encerrada", s(0))); self.open_peers(true); }
                    Err(e) => self.flash(&e),
                }
            }
            "peer_remove" => match peer::remove(&s(0)) {
                Ok(_) => {
                    let _ = self.call(json!({"cmd": "peers-reload"}));
                    self.cfg = config::load_config().0;
                    self.peer_info.remove(&s(0));
                    self.flash(&format!("{} removido", s(0)));
                    self.open_peers(false);
                }
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

/// Uma sessao de agente de outro computador na lista (Enter: escrever a
/// mensagem). `with_peer`: mostra tambem de qual computador e.
fn session_item(peer: &str, x: &Value, with_peer: bool) -> Item {
    let label = x["tab"].as_str().filter(|t| !t.is_empty()).or(x["title"].as_str()).unwrap_or("?").to_string();
    let (mark, st) = match x["state"].as_str() { Some("working") => ("◐", "trabalhando"), Some("blocked") => ("✋", "esperando você"), Some("done") => ("●", "terminou"), _ => ("○", "parado") };
    let who = if with_peer { format!("{peer} › ") } else { String::new() };
    Item::new(format!("{mark}  {who}{label}"),
        format!("{} · {st} · Enter: escrever a mensagem · no terminal: duck msg {peer}/{} \"...\"", x["dir"].as_str().unwrap_or(""), short_tab(&label)),
        "peer_msg", vec![json!(peer), x["id"].clone(), json!(label)])
}

/// Primeiro pedaco do nome da aba, para o exemplo do terminal ("Ferrum|Git" -> "Ferrum").
fn short_tab(label: &str) -> String {
    let t: String = label.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_' || *c == '|').collect();
    let t = t.trim().split(['|', ' ']).find(|w| !w.is_empty()).unwrap_or("").to_string();
    if t.is_empty() { "aba".into() } else { t }
}
