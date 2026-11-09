//! Computadores pareados (`duck msg`, painel Computadores): descoberta,
//! pareamento, convites e o envio de pedidos para outro Duck. Usado pela CLI,
//! pela janela e pelo server.
//!
//! Protocolo na porta de `peer.listen`:
//! - Texto puro, uma linha JSON (so o que pode ser publico):
//!   `{"ping":1}` responde nome, versao, sistema e chave publica.
//! - Cifrado (`secure::MAGIC` + Noise XX, ver secure.rs): todo o resto. Quem
//!   e quem sai da chave publica do handshake, nao de um token na rede.
//!   - `pair` (from, port, invite?): pedido de conexao. Com convite valido
//!     aceita na hora; senao o usuario de la aprova vendo o numero de
//!     confirmacao (igual nas duas telas).
//!   - `pair_status`: quem pediu pergunta se ja foi aceito (funciona mesmo
//!     quando o outro nao consegue abrir conexao de volta).
//!   - `pair_done` (from, port): quem aceitou avisa quem pediu.
//!   - `msg`, `sessions`, `file`, `callback`, `hello`: so de pareados.
//! - Legado (Duck ate 0.1.8): linha JSON com `token`. So recebemos, de
//!   pareados antigos que ainda nao tem chave; para enviar, o mesmo.
//!
//! Enderecos de um pareado: `peers.nome = 'ip:porta,ip2:porta,relay:host:porta'`
//! (tenta na ordem; o que funcionar vai para o comeco).
//!
//! Descoberta: datagrama UDP `duck-discover/1` em broadcast na mesma porta;
//! quem escuta responde nome, endereco, versao e o comeco da chave (nunca
//! segredo). Na Tailscale (sem broadcast) testa os computadores da tailnet.

use crate::secure::{self, Chan};
use serde_json::{json, Value};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 7878;
const PROBE: &[u8] = b"duck-discover/1";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Token legado deste computador (`peer/self`): so para receber de pareados
/// antigos (sem criptografia). Nao cria mais.
pub fn legacy_token() -> Option<String> {
    crate::secret::get("peer/self").map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Nome de peer: minusculo, letras/numeros/-/_, ate 32.
pub fn clean_name(s: &str) -> String {
    let n: String = s.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(32).collect();
    if n.is_empty() { "duck".into() } else { n }
}

/// IP desta maquina na rede de saida (nenhum pacote sai: so escolhe a rota).
pub fn local_ip() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

/// Uma rede IPv4 desta maquina (placa de rede, VM, VPN...).
#[derive(Clone, Debug, PartialEq)]
pub struct Iface { pub name: String, pub ip: String, pub broadcast: String }

/// Redes IPv4 desta maquina, sem loopback nem link-local (169.254).
pub fn interfaces() -> Vec<Iface> {
    let Ok(nets) = sysinfo::Networks::new_with_refreshed_list() else { return vec![] };
    let mut out = vec![];
    for (name, data) in nets.iter() {
        for n in data.ip_networks() {
            let std::net::IpAddr::V4(ip) = n.addr else { continue };
            if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() { continue; }
            out.push(Iface { name: name.to_string_lossy().into_owned(), ip: ip.to_string(), broadcast: broadcast_of(ip, n.prefix).to_string() });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.ip.cmp(&b.ip)));
    out
}

fn broadcast_of(ip: std::net::Ipv4Addr, prefix: u8) -> std::net::Ipv4Addr {
    let p = u32::from(prefix.min(32));
    let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
    std::net::Ipv4Addr::from(u32::from(ip) | !mask)
}

/// `listen` escuta em todas as redes (0.0.0.0, :: ou sem host).
pub fn listens_all(listen: &str) -> bool {
    let host = listen.trim().rsplit_once(':').map_or(listen.trim(), |(h, _)| h).trim_matches(['[', ']']);
    host.is_empty() || host == "0.0.0.0" || host == "::"
}

/// Porta de um "host:porta" (padrao 7878).
pub fn port_of(addr: &str) -> u16 {
    addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// Endereco que os outros usam para chegar aqui. `listen` vazio: desligado.
/// Escutando em todas as redes (0.0.0.0): o IP da rede de saida.
pub fn advertised(listen: &str) -> Option<String> {
    let listen = listen.trim();
    if listen.is_empty() { return None; }
    let (host, port) = listen.rsplit_once(':').unwrap_or((listen, ""));
    let port = port.parse::<u16>().unwrap_or(DEFAULT_PORT);
    let host = host.trim_matches(['[', ']']);
    if host.is_empty() || host == "0.0.0.0" || host == "::" {
        local_ip().map(|ip| format!("{ip}:{port}"))
    } else {
        Some(format!("{}:{port}", if host.contains(':') { format!("[{host}]") } else { host.to_string() }))
    }
}

fn lua_str(s: &str) -> String { format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")) }

/// Liga a escuta (`peer.listen`) e grava o nome. Vale ao reiniciar o server.
pub fn enable(name: &str, addr: &str) -> Result<(), String> {
    crate::config::set_local_setting("peer.listen", &lua_str(addr))?;
    crate::config::set_local_setting("peer.name", &lua_str(name))
}

// ---------------------------------------------------------------- pareados

/// Enderecos de um pareado, na ordem de tentativa.
pub fn split_addrs(s: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for a in s.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        if !v.iter().any(|x| x == a) { v.push(a.to_string()); }
    }
    v
}

/// Enderecos guardados de `name` (vazio se nao for pareado).
pub fn addrs_of(name: &str) -> Vec<String> {
    let cfg = crate::config::load_config().0;
    cfg.peers.iter().find(|(n, _)| n == name).map(|(_, a)| split_addrs(a)).unwrap_or_default()
}

pub fn save_addrs(name: &str, addrs: &[String]) -> Result<(), String> {
    crate::config::set_local_setting(&format!("peers.{name}"), &lua_str(&addrs.join(",")))
}

/// Poe `addr` no comeco da lista (o que funcionou por ultimo vem primeiro).
/// Devolve a lista nova se mudou.
pub fn promote(list: &[String], addr: &str) -> Option<Vec<String>> {
    if list.first().is_some_and(|a| a == addr) { return None; }
    let mut v = vec![addr.to_string()];
    v.extend(list.iter().filter(|a| *a != addr).cloned());
    Some(v)
}

/// Chave publica guardada de um pareado (sem ela: pareado antigo, sem criptografia).
pub fn key_of(name: &str) -> Option<String> {
    crate::secret::get(&format!("peerkey/{name}")).map(|k| k.trim().to_string()).filter(|k| k.len() == 64)
}

/// Guarda o pareado: chave no cofre, enderecos na config. O token legado sai.
pub fn save(name: &str, addrs: &[String], key: &str) -> Result<(), String> {
    crate::secret::set(&format!("peerkey/{name}"), key)?;
    crate::secret::delete(&format!("peer/{name}"));
    save_addrs(name, addrs)
}

/// Esquece o computador (chave, token legado e enderecos).
pub fn remove(name: &str) -> Result<(), String> {
    crate::secret::delete(&format!("peerkey/{name}"));
    crate::secret::delete(&format!("peer/{name}"));
    crate::config::set_local_setting(&format!("peers.{name}"), "nil")
}

/// Nome do pareado que tem esta chave.
pub fn name_by_key(peers: &[(String, String)], key: &str) -> Option<String> {
    peers.iter().find(|(n, _)| key_of(n).is_some_and(|k| k == key)).map(|(n, _)| n.clone())
}

/// Nome livre para um pareado novo: `nome`, `nome-2`... (outro computador
/// com o mesmo nome e outra chave nao sobrescreve o que ja existe).
pub fn free_name(peers: &[(String, String)], name: &str, key: &str) -> String {
    let taken = |n: &str| peers.iter().any(|(p, _)| p == n) && key_of(n).is_none_or(|k| k != key);
    if !taken(name) { return name.to_string(); }
    (2..).map(|i| format!("{name}-{i}")).find(|n| !taken(n)).unwrap_or_else(|| name.to_string())
}

// ---------------------------------------------------------------- conexao

/// Abre o canal cifrado por um endereco (`ip:porta` ou `relay:host:porta`)
/// e confere que do outro lado esta a chave esperada. `want_key` pode ser a
/// chave inteira ou o comeco dela (convite); pelo relay ela e obrigatoria.
pub fn open(addr: &str, want_key: Option<&str>) -> Result<Chan, String> {
    let s = if let Some(relay) = addr.strip_prefix("relay:") {
        let key = want_key.ok_or("relay precisa da chave do outro")?;
        crate::relay::dial(relay, &crate::config::load_config().0.peer_relay_secret, key)?
    } else {
        tcp(addr, Duration::from_millis(2500))?
    };
    let c = Chan::connect(s)?;
    if let Some(k) = want_key {
        if !c.remote().starts_with(k) { return Err("a chave do outro lado nao e a esperada (reinstalou? ou alguem no meio): pareie de novo".into()); }
    }
    Ok(c)
}

fn tcp(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("endereco invalido")?;
    TcpStream::connect_timeout(&sa, timeout).map_err(|e| e.to_string())
}

/// Pedido para um pareado: tenta cada endereco (direto e relay) ate um
/// responder. Sem chave guardada usa o protocolo legado com token.
/// Devolve a resposta e o endereco que funcionou.
pub fn request(name: &str, req: &Value) -> Result<(Value, String), String> {
    let cfg = crate::config::load_config().0;
    let list = cfg.peers.iter().find(|(n, _)| n == name).map(|(_, a)| split_addrs(a))
        .ok_or_else(|| format!("{name} nao esta pareado (duck peer list)"))?;
    if list.is_empty() { return Err(format!("{name} sem endereco")); }
    let mut req = req.clone();
    req["from"] = json!(cfg.peer_name);
    req["version"] = json!(VERSION);
    if !cfg.peer_listen.is_empty() { req["port"] = json!(port_of(&cfg.peer_listen)); }
    let key = key_of(name);
    let mut errs = vec![];
    for a in &list {
        let r = match &key {
            Some(k) => open(a, Some(k)).and_then(|mut c| { c.set_timeout(Duration::from_secs(120)); c.call(&req) }),
            None if a.starts_with("relay:") => Err("pareado antigo nao passa por relay".into()),
            None => {
                let Some(t) = crate::secret::get(&format!("peer/{name}")) else { errs.push(format!("{a}: sem chave nem token: pareie de novo")); continue };
                let mut r = req.clone();
                r["token"] = json!(t.trim());
                send_legacy(a, &r)
            }
        };
        match r {
            Ok(v) => {
                if let Some(n) = promote(&list, a) { let _ = save_addrs(name, &n); }
                return Ok((v, a.clone()));
            }
            Err(e) => errs.push(format!("{a}: {e}")),
        }
    }
    Err(errs.join(" · "))
}

/// Uma linha JSON em texto puro e a resposta (ping e pareados antigos).
pub fn send_legacy(addr: &str, req: &Value) -> Result<Value, String> {
    use std::io::{BufRead, Write};
    let mut s = tcp(addr, Duration::from_millis(2500))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    s.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| format!("resposta invalida: {e}"))
}

/// Erro de rede em palavras: o que provavelmente aconteceu.
pub fn human_err(e: &str) -> String {
    if e.contains("refused") || e.contains("recusad") { "não responde (Duck fechado, ou escuta em outra rede/porta)".into() }
    else if e.contains("timed out") || e.contains("tempo") || e.contains("WouldBlock") { "sem resposta (desligado, firewall ou outra rede)".into() }
    else if e.contains("unreachable") || e.contains("inacess") || e.contains("No route") { "rede inalcançável daqui".into() }
    else { e.to_string() }
}

/// Resposta do ping: quem esta do outro lado.
#[derive(Clone, Debug, Default)]
pub struct Info { pub ms: u128, pub name: String, pub version: String, pub os: String, pub key: String }

/// Testa se o Duck de la responde (texto puro, sem segredo).
pub fn ping(addr: &str) -> Result<Info, String> {
    if let Some(relay) = addr.strip_prefix("relay:") {
        // Pelo relay so da para saber se a chave esta registrada nele.
        let _ = relay;
        return Err("via relay: use Testar conexão".into());
    }
    let t = Instant::now();
    let v = send_legacy(addr, &json!({"ping": 1}))?;
    if v["ok"].as_bool() != Some(true) { return Err(v["error"].as_str().unwrap_or("recusou").to_string()); }
    Ok(Info {
        ms: t.elapsed().as_millis(),
        name: v["name"].as_str().unwrap_or("").to_string(),
        version: v["version"].as_str().unwrap_or("0.1.8 ou antes").to_string(),
        os: v["os"].as_str().unwrap_or("").to_string(),
        key: v["key"].as_str().unwrap_or("").to_string(),
    })
}

/// O primeiro endereco que responde ao ping (para o painel).
pub fn ping_any(addrs: &[String]) -> Result<(Info, String), String> {
    let mut errs = vec![];
    for a in addrs.iter().filter(|a| !a.starts_with("relay:")) {
        match ping(a) { Ok(i) => return Ok((i, a.clone())), Err(e) => errs.push(format!("{a}: {e}")) }
    }
    if addrs.iter().any(|a| a.starts_with("relay:")) && errs.len() == addrs.len() - 1 { return Err("so pelo relay".into()); }
    Err(if errs.is_empty() { "sem endereco".into() } else { errs.join(" · ") })
}

// ---------------------------------------------------------------- pareamento

/// Resultado do pedido de conexao.
pub struct PairStart {
    /// Nome que o outro informou.
    pub name: String,
    /// Chave publica do outro (do handshake).
    pub key: String,
    /// Numero de confirmacao desta conexao (o outro ve o mesmo).
    pub confirm: String,
    /// Aceito na hora (convite valido ou ja pareados).
    pub accepted: bool,
}

/// Pede conexao a `addr`. `invite`: codigo de convite do outro (aceita na
/// hora); `key_prefix`: comeco da chave que o convite promete. `on_key` roda
/// logo apos o handshake (registra o pedido pendente no server antes de o
/// outro poder aceitar).
pub fn pair_request(addr: &str, invite: Option<&str>, key_prefix: Option<&str>, on_key: impl FnOnce(&str)) -> Result<PairStart, String> {
    let cfg = crate::config::load_config().0;
    let mut c = open(addr, key_prefix).map_err(|e| if e.contains("nao e a esperada") { "a chave do outro nao bate com a do convite: nao conecte (alguem no meio?)".to_string() } else { e })?;
    on_key(c.remote());
    let mut req = json!({"pair": 1, "from": cfg.peer_name, "version": VERSION, "invite": invite.unwrap_or("")});
    if !cfg.peer_listen.is_empty() { req["port"] = json!(port_of(&cfg.peer_listen)); }
    if !cfg.peer_relay.is_empty() { req["relay"] = json!(cfg.peer_relay); }
    let v = c.call(&req)?;
    if v["ok"].as_bool() != Some(true) { return Err(v["error"].as_str().unwrap_or("recusou").to_string()); }
    Ok(PairStart {
        name: clean_name(v["name"].as_str().unwrap_or("duck")),
        key: c.remote().to_string(),
        confirm: c.confirm(),
        accepted: v["accepted"].as_bool() == Some(true),
    })
}

/// Estado de um pedido de conexao feito daqui.
pub enum PairState {
    /// Aceito: nome do outro e a resposta (traz o relay dele).
    Accepted(String, Value),
    Waiting,
    /// Negado, expirado ou o outro nao sabe do pedido.
    Denied,
}

/// Pergunta a quem recebeu o pedido se ja aceitou. Err: sem resposta (rede).
pub fn pair_status(addr: &str, key: &str) -> Result<PairState, String> {
    let cfg = crate::config::load_config().0;
    let mut c = open(addr, Some(key))?;
    let v = c.call(&json!({"pair_status": 1, "from": cfg.peer_name}))?;
    if v["ok"].as_bool() != Some(true) { return Err(v["error"].as_str().unwrap_or("recusou").to_string()); }
    if v["accepted"].as_bool() == Some(true) { return Ok(PairState::Accepted(clean_name(v["name"].as_str().unwrap_or("duck")), v)); }
    Ok(if v["waiting"].as_bool() == Some(true) { PairState::Waiting } else { PairState::Denied })
}

/// Enderecos para guardar de quem acabou de parear: o que funcionou e o relay dele.
pub fn addrs_for(addr: &str, their_relay: Option<&str>) -> Vec<String> {
    let mut v = vec![addr.to_string()];
    if let Some(r) = their_relay.map(str::trim).filter(|r| !r.is_empty()) { v.push(format!("relay:{r}")); }
    v
}

/// A busca achou pareados (pelo comeco da chave) em enderecos que nao
/// estavam na lista: guarda (DHCP mudou, outra rede). Devolve quem mudou.
pub fn learn_from_discovery(found: &[Value]) -> Vec<String> {
    let cfg = crate::config::load_config().0;
    let mut changed = vec![];
    for f in found {
        let (Some(k), Some(addr)) = (f["key"].as_str().filter(|k| k.len() >= 16), f["addr"].as_str()) else { continue };
        let Some((name, list)) = cfg.peers.iter().find(|(n, _)| key_of(n).is_some_and(|x| x.starts_with(k))) else { continue };
        let mut list = split_addrs(list);
        if list.iter().any(|a| a == addr) { continue; }
        let at = list.iter().position(|a| a.starts_with("relay:")).unwrap_or(list.len());
        list.insert(at, addr.to_string());
        if save_addrs(name, &list).is_ok() { changed.push(name.clone()); }
    }
    changed
}

// ---------------------------------------------------------------- convites

/// Convite: `duck:nome@end1,end2#codigo.chave` (chave = comeco da publica).
/// Vale uma vez, por pouco tempo; quem usa nao precisa de aprovacao, e a
/// chave no convite impede que outro se passe por este computador.
pub fn invite_string(name: &str, addrs: &[String], code: &str, key: &str) -> String {
    format!("duck:{name}@{}#{code}.{}", addrs.join(","), secure::short(key))
}

#[derive(Debug)]
pub struct Invite { pub name: String, pub addrs: Vec<String>, pub code: String, pub key_prefix: String }

pub fn parse_invite(s: &str) -> Result<Invite, String> {
    let s = s.trim().trim_matches(['"', '\'', '`']);
    let s = s.strip_prefix("duck:").unwrap_or(s);
    let (left, right) = s.split_once('#').ok_or("convite sem codigo (formato: duck:nome@ip:porta#codigo.chave)")?;
    let (name, addrs) = left.split_once('@').ok_or("convite sem endereco")?;
    let (code, key) = right.split_once('.').ok_or("convite de versao antiga: gere outro no painel Computadores (Gerar convite)")?;
    let hexok = |x: &str, min: usize| x.len() >= min && x.chars().all(|c| c.is_ascii_hexdigit());
    if !hexok(code, 8) || !hexok(key, 16) { return Err("convite invalido".into()); }
    let addrs = split_addrs(addrs);
    if addrs.is_empty() { return Err("convite sem endereco".into()); }
    Ok(Invite { name: clean_name(name), addrs, code: code.to_lowercase(), key_prefix: key.to_lowercase() })
}

// ---------------------------------------------------------------- rede

/// Procura Ducks na rede local (broadcast UDP). Cada item: {name, addr, os}.
pub fn discover(ports: &[u16], wait: Duration) -> Vec<Value> {
    let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return vec![] };
    let _ = s.set_broadcast(true);
    let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
    // Broadcast geral e o direcionado de cada rede desta maquina: o geral so
    // sai pela rota padrao, e assim a busca tambem acha VMs, VPNs e a outra placa.
    let mut targets: Vec<String> = vec!["255.255.255.255".into()];
    if let Some(ip) = local_ip().filter(|ip| ip.contains('.')) {
        if let Some((net, _)) = ip.rsplit_once('.') { targets.push(format!("{net}.255")); }
    }
    for i in interfaces() {
        if !targets.contains(&i.broadcast) { targets.push(i.broadcast); }
    }
    // A Tailscale nao tem broadcast: testa os computadores da tailnet em paralelo.
    let ts_port = ports.first().copied().unwrap_or(DEFAULT_PORT);
    let ts = std::thread::spawn(move || discover_tailscale(ts_port));
    let send_all = || for p in ports { for t in &targets { let _ = s.send_to(PROBE, (t.as_str(), *p)); } };
    send_all();
    let mut out: Vec<Value> = vec![];
    let deadline = Instant::now() + wait;
    let mut resent = false;
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline {
        // UDP perde pacote: manda de novo na metade do tempo.
        if !resent && Instant::now() + wait / 2 > deadline { send_all(); resent = true; }
        let Ok((n, from)) = s.recv_from(&mut buf) else { continue };
        let Ok(v) = serde_json::from_slice::<Value>(&buf[..n]) else { continue };
        if v["duck"].as_u64() != Some(1) { continue; }
        let addr = v["addr"].as_str().filter(|a| !a.is_empty()).map(String::from)
            .unwrap_or_else(|| format!("{}:{}", from.ip(), v["port"].as_u64().unwrap_or(DEFAULT_PORT as u64)));
        let name = clean_name(v["name"].as_str().unwrap_or("duck"));
        if out.iter().any(|o| o["addr"] == addr.as_str()) { continue; }
        out.push(json!({"name": name, "addr": addr, "os": v["os"], "version": v["version"], "key": v["key"]}));
    }
    for t in ts.join().unwrap_or_default() {
        if !out.iter().any(|o| o["addr"] == t["addr"] || (o["key"].is_string() && o["key"] == t["key"])) { out.push(t); }
    }
    out
}

/// Responde a descoberta (thread do server). `who` devolve (nome, endereco).
pub fn answer_discovery(port: u16, who: impl Fn() -> (String, String)) {
    let s = match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(s) => s,
        Err(e) => { crate::proto::log(&format!("peers: descoberta nao escutou na UDP {port}: {e}")); return; }
    };
    crate::proto::log(&format!("peers: descoberta na UDP {port}"));
    let mut buf = [0u8; 64];
    let mut last: Option<(SocketAddr, Instant)> = None;
    loop {
        let Ok((n, from)) = s.recv_from(&mut buf) else { continue };
        if &buf[..n] != PROBE { continue; }
        // A mesma origem repetindo em rajada: uma resposta basta.
        if last.is_some_and(|(a, t)| a == from && t.elapsed() < Duration::from_millis(200)) { continue; }
        last = Some((from, Instant::now()));
        let (name, addr) = who();
        let r = json!({"duck": 1, "name": name, "addr": addr, "port": port, "os": std::env::consts::OS, "version": VERSION, "key": secure::short(&secure::my_key())});
        let _ = s.send_to(r.to_string().as_bytes(), from);
    }
}

// ---------------------------------------------------------------- tailscale

/// Computadores online da tailnet: (nome, IP). Vazio sem Tailscale.
/// A Tailscale nao passa broadcast: a busca testa cada um direto.
pub fn tailscale_peers() -> Vec<(String, String)> {
    let bin = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let Ok(out) = cmd.output() else { return vec![] };
    parse_tailscale(&String::from_utf8_lossy(&out.stdout))
}

fn parse_tailscale(json_text: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(json_text) else { return vec![] };
    let mut out = vec![];
    for p in v["Peer"].as_object().into_iter().flat_map(|m| m.values()) {
        if p["Online"].as_bool() != Some(true) { continue; }
        let Some(ip) = p["TailscaleIPs"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).find(|x| x.contains('.')) else { continue };
        let name = p["HostName"].as_str().or(p["DNSName"].as_str()).unwrap_or("?");
        out.push((clean_name(name), ip.to_string()));
    }
    out
}

/// Pinga em paralelo os computadores da tailnet; os que tem Duck viram itens da busca.
fn discover_tailscale(port: u16) -> Vec<Value> {
    let peers = tailscale_peers();
    let hs: Vec<_> = peers.into_iter().map(|(n, ip)| std::thread::spawn(move || {
        let addr = format!("{ip}:{port}");
        let mut s = tcp(&addr, Duration::from_millis(900)).ok()?;
        use std::io::{BufRead, Write};
        let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
        s.write_all(b"{\"ping\":1}\n").ok()?;
        let mut line = String::new();
        std::io::BufReader::new(s).read_line(&mut line).ok()?;
        let v: Value = serde_json::from_str(&line).ok()?;
        (v["ok"].as_bool() == Some(true)).then(|| json!({
            "name": clean_name(v["name"].as_str().unwrap_or(&n)), "addr": addr, "os": v["os"], "version": v["version"],
            "key": v["key"].as_str().map(secure::short), "via": "tailscale",
        }))
    })).collect();
    hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

// ---------------------------------------------------------------- firewall

/// Comando que libera a porta no firewall do Windows (TCP e a descoberta UDP).
pub fn windows_firewall_cmd(port: u16) -> String {
    format!("New-NetFirewallRule -DisplayName 'Duck peers ({port})' -Direction Inbound -Protocol TCP -LocalPort {port} -Profile Private,Domain -Action Allow; \
             New-NetFirewallRule -DisplayName 'Duck peers descoberta ({port})' -Direction Inbound -Protocol UDP -LocalPort {port} -Profile Private,Domain -Action Allow")
}

/// Zonas do firewalld (Linux) com placa de rede ativa que nao liberam a
/// porta: (zona, placas). Vazio sem firewalld ou com tudo liberado.
pub fn firewalld_blocked(port: u16) -> Vec<(String, String)> {
    let run = |args: &[&str]| -> Option<String> {
        let o = std::process::Command::new("firewall-cmd").args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).to_string())
    };
    if cfg!(windows) || run(&["--state"]).is_none() { return vec![]; }
    let zones = run(&["--get-active-zones"]).unwrap_or_default();
    let mut out = vec![];
    let mut cur: Option<String> = None;
    for l in zones.lines() {
        if !l.starts_with(' ') && !l.starts_with('\t') { cur = Some(l.split_whitespace().next().unwrap_or("").to_string()); continue; }
        let Some(z) = cur.clone() else { continue };
        let Some(ifs) = l.trim().strip_prefix("interfaces:") else { continue };
        if ifs.trim().is_empty() { continue; }
        // target ACCEPT (zona trusted) libera tudo.
        let target = run(&["--zone", &z, "--get-target"]).unwrap_or_default();
        if target.trim() == "ACCEPT" && z == "trusted" { continue; }
        let tcp_ok = run(&["--zone", &z, &format!("--query-port={port}/tcp")]).is_some_and(|s| s.trim() == "yes");
        let range_ok = run(&["--zone", &z, "--list-ports"]).unwrap_or_default().split_whitespace().any(|r| port_in_range(r, port, "tcp"));
        if !tcp_ok && !range_ok { out.push((z, ifs.trim().to_string())); }
    }
    out
}

fn port_in_range(r: &str, port: u16, proto: &str) -> bool {
    let Some((range, p)) = r.split_once('/') else { return false };
    if p != proto { return false; }
    match range.split_once('-') {
        Some((a, b)) => a.parse::<u16>().is_ok_and(|a| a <= port) && b.parse::<u16>().is_ok_and(|b| port <= b),
        None => range.parse::<u16>() == Ok(port),
    }
}

/// Comando que libera a porta numa zona do firewalld (TCP e a busca UDP).
pub fn firewalld_cmd(zone: &str, port: u16) -> String {
    format!("sudo firewall-cmd --zone={zone} --add-port={port}/tcp --add-port={port}/udp --permanent && sudo firewall-cmd --reload")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_e_todas_as_redes() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        assert_eq!(broadcast_of(ip("192.168.122.1"), 24), ip("192.168.122.255"));
        assert_eq!(broadcast_of(ip("10.1.2.3"), 16), ip("10.1.255.255"));
        assert_eq!(broadcast_of(ip("10.1.2.3"), 32), ip("10.1.2.3"));
        assert_eq!(broadcast_of(ip("10.1.2.3"), 0), ip("255.255.255.255"));
        assert!(listens_all("0.0.0.0:7878") && listens_all("[::]:7878") && listens_all(":7878"));
        assert!(!listens_all("192.168.68.139:7878") && !listens_all("[fe80::1]:7878"));
    }

    #[test]
    fn enderecos() {
        assert_eq!(split_addrs(" a:1 , b:2,,a:1, relay:r:3 "), vec!["a:1", "b:2", "relay:r:3"]);
        let l: Vec<String> = split_addrs("a:1,b:2,c:3");
        assert_eq!(promote(&l, "a:1"), None);
        assert_eq!(promote(&l, "c:3"), Some(split_addrs("c:3,a:1,b:2")));
        assert_eq!(promote(&l, "d:4"), Some(split_addrs("d:4,a:1,b:2,c:3")));
        assert_eq!(addrs_for("1.2.3.4:7878", Some("vps:7879")), vec!["1.2.3.4:7878", "relay:vps:7879"]);
        assert_eq!(addrs_for("1.2.3.4:7878", Some(" ")), vec!["1.2.3.4:7878"]);
    }

    #[test]
    fn convite() {
        let k = "ab".repeat(32);
        let s = invite_string("fedora", &split_addrs("192.168.0.2:7878,100.64.0.2:7878"), "0123456789", &k);
        let i = parse_invite(&format!(" '{s}' ")).unwrap();
        assert_eq!(i.name, "fedora");
        assert_eq!(i.addrs, vec!["192.168.0.2:7878", "100.64.0.2:7878"]);
        assert_eq!(i.code, "0123456789");
        assert!(k.starts_with(&i.key_prefix));
        assert!(parse_invite("duck:x@1.2.3.4:7878#ab12cd34ef56ab12cd34").unwrap_err().contains("versao antiga"));
        assert!(parse_invite("duck:x#0123456789.abababababababab").is_err());
        assert!(parse_invite("duck:x@1.2.3.4:7878#zz.abababababababab").is_err());
    }

    #[test]
    fn tailscale_status() {
        let j = r#"{"Peer":{"a":{"HostName":"LEGION","TailscaleIPs":["100.101.1.2","fd7a::1"],"Online":true},
                            "b":{"HostName":"off","TailscaleIPs":["100.1.1.1"],"Online":false}}}"#;
        assert_eq!(parse_tailscale(j), vec![("legion".to_string(), "100.101.1.2".to_string())]);
        assert!(parse_tailscale("nao e json").is_empty());
    }

    #[test]
    fn faixa_de_portas() {
        assert!(port_in_range("1025-65535/tcp", 7878, "tcp"));
        assert!(port_in_range("7878/tcp", 7878, "tcp"));
        assert!(!port_in_range("7878/udp", 7878, "tcp"));
        assert!(!port_in_range("8000-9000/tcp", 7878, "tcp"));
    }

    #[test]
    fn names_and_addrs() {
        assert_eq!(clean_name(" Meu PC! "), "meupc");
        assert_eq!(clean_name("***"), "duck");
        assert_eq!(advertised(""), None);
        assert_eq!(advertised("100.64.0.1:7878").as_deref(), Some("100.64.0.1:7878"));
        assert_eq!(port_of("10.0.0.1:9999"), 9999);
        assert_eq!(port_of("10.0.0.1"), DEFAULT_PORT);
    }
}
