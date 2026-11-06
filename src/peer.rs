//! Computadores pareados (`duck msg`): codigo de pareamento, descoberta na
//! rede local e o pedido de conexao. Usado pela CLI, pela janela e pelo server.
//!
//! Protocolo (uma linha JSON por conexao TCP, na porta de `peer.listen`):
//! - `{"ping":1}`: responde o nome, sem token (o painel mostra online/offline).
//! - `{"pair":1,"from","addr","key","token"}`: pedido de conexao. Com o token
//!   certo do outro lado (veio do codigo) ele aceita na hora; sem token, o
//!   usuario de la aprova e o aceite volta como `pair_done`.
//! - `{"pair_done":1,"token","from","key"}`: o outro aceitou; `token` e o
//!   nosso, `key` e o dele.
//!
//! Descoberta: datagrama UDP `duck-discover/1` em broadcast na mesma porta; quem
//! escuta responde nome e endereco (nunca o token).

use serde_json::{json, Value};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 7878;
const PROBE: &[u8] = b"duck-discover/1";

/// Token deste computador (`peer/self` no cofre). Cria na primeira vez.
pub fn self_token() -> String {
    crate::secret::get("peer/self").map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
        let t = crate::proto::random_hex(32);
        let _ = crate::secret::set("peer/self", &t);
        t
    })
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

/// Codigo de pareamento: `duck:nome@ip:porta#token`. Quem tem o codigo
/// conecta sem o outro lado aprovar; trate como senha.
pub fn code(name: &str, addr: &str, token: &str) -> String { format!("duck:{name}@{addr}#{token}") }

pub struct Code { pub name: String, pub addr: String, pub token: String }

pub fn parse_code(s: &str) -> Result<Code, String> {
    let s = s.trim().trim_matches(['"', '\'', '`']);
    let s = s.strip_prefix("duck:").unwrap_or(s);
    let (left, token) = s.split_once('#').ok_or("codigo sem token (formato: duck:nome@ip:porta#token)")?;
    let (name, addr) = left.split_once('@').ok_or("codigo sem endereco (formato: duck:nome@ip:porta#token)")?;
    let token = token.trim();
    if token.len() < 16 || !token.chars().all(|c| c.is_ascii_hexdigit()) { return Err("token invalido no codigo".into()); }
    if !addr.contains(':') || addr.to_socket_addrs().map(|mut a| a.next().is_none()).unwrap_or(true) {
        return Err(format!("endereco invalido no codigo: {addr}"));
    }
    Ok(Code { name: clean_name(name), addr: addr.trim().to_string(), token: token.to_string() })
}

fn lua_str(s: &str) -> String { format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")) }

/// Guarda o outro computador: token no cofre, endereco no ~/.duck-local.lua.
pub fn save(name: &str, addr: &str, token: &str) -> Result<(), String> {
    crate::secret::set(&format!("peer/{name}"), token.trim())?;
    crate::config::set_local_setting(&format!("peers.{name}"), &lua_str(addr))
}

/// Esquece o computador (token e endereco).
pub fn remove(name: &str) -> Result<(), String> {
    crate::secret::delete(&format!("peer/{name}"));
    crate::config::set_local_setting(&format!("peers.{name}"), "nil")
}

/// Liga a escuta (`peer.listen`) neste IP e grava o nome. Vale ao reiniciar o server.
pub fn enable(name: &str, addr: &str) -> Result<(), String> {
    crate::config::set_local_setting("peer.listen", &lua_str(addr))?;
    crate::config::set_local_setting("peer.name", &lua_str(name))
}

/// Uma linha JSON para o outro Duck e a resposta.
pub fn send(addr: &str, req: &Value) -> Result<Value, String> {
    send_timeout(addr, req, Duration::from_secs(5))
}

fn send_timeout(addr: &str, req: &Value, connect: Duration) -> Result<Value, String> {
    use std::io::{BufRead, Write};
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("endereco invalido")?;
    let mut s = TcpStream::connect_timeout(&sa, connect).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    s.write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| format!("resposta invalida: {e}"))
}

/// Testa se o Duck de la responde. Ok(ms) ou o erro.
pub fn ping(addr: &str) -> Result<u128, String> {
    let t = Instant::now();
    let v = send_timeout(addr, &json!({"ping": 1}), Duration::from_millis(1500))?;
    if v["ok"].as_bool() == Some(true) { Ok(t.elapsed().as_millis()) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
}

/// Pede conexao. `their_token` (do codigo) faz o outro aceitar na hora; sem
/// ele, o usuario de la aprova. Resposta: `accepted` ou `approval`.
pub fn pair_request(me: &str, my_addr: &str, addr: &str, their_token: Option<&str>) -> Result<Value, String> {
    let req = json!({"pair": 1, "from": me, "addr": my_addr, "key": self_token(), "token": their_token.unwrap_or("")});
    let v = send(addr, &req)?;
    if v["ok"].as_bool() == Some(true) { Ok(v) } else { Err(v["error"].as_str().unwrap_or("recusou").to_string()) }
}

/// Procura Ducks na rede local (broadcast UDP). Cada item: {name, addr, os}.
pub fn discover(ports: &[u16], wait: Duration) -> Vec<Value> {
    let Ok(s) = UdpSocket::bind("0.0.0.0:0") else { return vec![] };
    let _ = s.set_broadcast(true);
    let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
    // Broadcast geral e o da /24 (alguns roteadores Wi-Fi so passam o direcionado).
    let mut targets: Vec<String> = vec!["255.255.255.255".into()];
    if let Some(ip) = local_ip().filter(|ip| ip.contains('.')) {
        if let Some((net, _)) = ip.rsplit_once('.') { targets.push(format!("{net}.255")); }
    }
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
        if out.iter().any(|o| o["addr"] == addr.as_str()) { continue; }
        out.push(json!({"name": clean_name(v["name"].as_str().unwrap_or("duck")), "addr": addr, "os": v["os"], "version": v["version"]}));
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
        let r = json!({"duck": 1, "name": name, "addr": addr, "port": port, "os": std::env::consts::OS, "version": env!("CARGO_PKG_VERSION")});
        let _ = s.send_to(r.to_string().as_bytes(), from);
    }
}

/// Comando que libera a porta no firewall do Windows (TCP e a descoberta UDP).
pub fn windows_firewall_cmd(port: u16) -> String {
    format!("New-NetFirewallRule -DisplayName 'Duck peers ({port})' -Direction Inbound -Protocol TCP -LocalPort {port} -Profile Private,Domain -Action Allow; \
             New-NetFirewallRule -DisplayName 'Duck peers descoberta ({port})' -Direction Inbound -Protocol UDP -LocalPort {port} -Profile Private,Domain -Action Allow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        let c = parse_code(&code("Legion", "192.168.0.5:7878", "ab12cd34ef56ab12cd34")).unwrap();
        assert_eq!((c.name.as_str(), c.addr.as_str(), c.token.as_str()), ("legion", "192.168.0.5:7878", "ab12cd34ef56ab12cd34"));
        assert!(parse_code("  'duck:x@10.0.0.1:7878#ab12cd34ef56ab12cd34' ").is_ok());
        assert!(parse_code("duck:x@10.0.0.1:7878").is_err());
        assert!(parse_code("duck:x@10.0.0.1:7878#naohex!").is_err());
        assert!(parse_code("duck:x#ab12cd34ef56ab12cd34").is_err());
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
