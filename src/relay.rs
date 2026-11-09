//! Relay: liga dois Ducks que nao se alcancam direto (redes diferentes, NAT).
//!
//! `duck relay --listen 0.0.0.0:7879 [--secret S]` roda num computador que os
//! dois alcancam (um VPS, por exemplo). Ele so emenda conexoes TCP: o canal
//! Noise vai de ponta a ponta por dentro, entao o relay nao le nem altera
//! nada (alterar quebra a autenticacao do Noise).
//!
//! Protocolo (uma linha JSON por conexao, depois bytes crus):
//! - `{"register": chave, "secret"}`: conexao de controle de um Duck. O relay
//!   manda `{"incoming": id}` quando alguem quer falar com essa chave e
//!   `{}` de tempos em tempos (se a escrita falha, o registro sai).
//! - `{"dial": chave, "secret"}`: quem quer falar. Espera o outro aceitar.
//! - `{"accept": id, "secret"}`: o Duck registrado abre esta conexao para o
//!   pedido `id`; o relay responde `{"ok":true}` aos dois e emenda.
//!
//! Registrar a chave de outro so faz as conexoes para ele falharem (sem a
//! chave privada o handshake nao fecha): o pior caso e negar servico.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Le uma linha byte a byte (sem buffer: o que vem depois e do Noise).
pub fn read_line(s: &mut TcpStream) -> Result<String, String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b) {
            Ok(0) => return Err("conexao fechada".into()),
            Ok(_) if b[0] == b'\n' => break,
            Ok(_) => { out.push(b[0]); if out.len() > 4096 { return Err("linha grande demais".into()); } }
            Err(e) => return Err(e.to_string()),
        }
    }
    String::from_utf8(out).map_err(|e| e.to_string())
}

fn send_line(s: &mut TcpStream, v: &Value) -> Result<(), String> {
    s.write_all(format!("{v}\n").as_bytes()).map_err(|e| e.to_string())
}

fn connect(addr: &str) -> Result<TcpStream, String> {
    let sa = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("endereco do relay invalido")?;
    TcpStream::connect_timeout(&sa, Duration::from_secs(5)).map_err(|e| format!("relay {addr}: {e}"))
}

/// Pede ao relay uma conexao com o Duck da chave `key`. Devolve o TCP ja
/// emendado: o handshake Noise vai por ele.
pub fn dial(relay: &str, secret: &str, key: &str) -> Result<TcpStream, String> {
    let mut s = connect(relay)?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    send_line(&mut s, &json!({"dial": key, "secret": secret}))?;
    let v: Value = serde_json::from_str(&read_line(&mut s)?).map_err(|e| e.to_string())?;
    if v["ok"].as_bool() != Some(true) { return Err(format!("relay: {}", v["error"].as_str().unwrap_or("recusou"))); }
    let _ = s.set_read_timeout(None);
    Ok(s)
}

/// Mantem este Duck registrado no relay e entrega cada conexao que chega
/// para `handle` (a mesma rotina das conexoes diretas). Volta a conectar
/// sozinho; `status` guarda o estado para o painel.
pub fn client_loop(relay: String, secret: String, key: String, status: Arc<Mutex<String>>, handle: Arc<dyn Fn(TcpStream) + Send + Sync>) {
    let mut wait = Duration::from_secs(2);
    loop {
        let r = (|| -> Result<(), String> {
            let mut s = connect(&relay)?;
            send_line(&mut s, &json!({"register": key, "secret": secret}))?;
            let _ = s.set_read_timeout(Some(Duration::from_secs(90)));
            let v: Value = serde_json::from_str(&read_line(&mut s)?).map_err(|e| e.to_string())?;
            if v["ok"].as_bool() != Some(true) { return Err(v["error"].as_str().unwrap_or("recusou").to_string()); }
            *status.lock().unwrap() = format!("conectado ao relay {relay}");
            crate::proto::log(&format!("relay: registrado em {relay}"));
            loop {
                let v: Value = serde_json::from_str(&read_line(&mut s)?).map_err(|e| e.to_string())?;
                let Some(id) = v["incoming"].as_str().map(String::from) else { continue };
                let (relay, secret, handle) = (relay.clone(), secret.clone(), handle.clone());
                std::thread::spawn(move || {
                    let r = (|| -> Result<TcpStream, String> {
                        let mut c = connect(&relay)?;
                        let _ = c.set_read_timeout(Some(Duration::from_secs(15)));
                        send_line(&mut c, &json!({"accept": id, "secret": secret}))?;
                        let v: Value = serde_json::from_str(&read_line(&mut c)?).map_err(|e| e.to_string())?;
                        if v["ok"].as_bool() != Some(true) { return Err(v["error"].as_str().unwrap_or("recusou").to_string()); }
                        Ok(c)
                    })();
                    match r { Ok(c) => handle(c), Err(e) => crate::proto::log(&format!("relay: aceitar {id}: {e}")) }
                });
            }
        })();
        let e = r.err().unwrap_or_default();
        *status.lock().unwrap() = format!("sem relay ({e})");
        crate::proto::log(&format!("relay: {relay}: {e}; tento de novo em {}s", wait.as_secs()));
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

#[derive(Default)]
struct Hub {
    /// chave -> conexao de controle
    regs: HashMap<String, TcpStream>,
    /// id -> conexao de quem discou (esperando o aceite)
    waiting: HashMap<String, (TcpStream, Instant)>,
}

/// Copia bytes nos dois sentidos ate um lado fechar.
fn splice(a: TcpStream, b: TcpStream) {
    let (Ok(a2), Ok(b2)) = (a.try_clone(), b.try_clone()) else { return };
    let pump = |mut from: TcpStream, mut to: TcpStream| {
        let _ = std::io::copy(&mut from, &mut to);
        let _ = to.shutdown(std::net::Shutdown::Both);
        let _ = from.shutdown(std::net::Shutdown::Both);
    };
    let t = std::thread::spawn(move || pump(a, b2));
    pump(b, a2);
    let _ = t.join();
}

/// `duck relay [--listen ip:porta] [--secret S]`.
pub fn main(args: &[String]) -> i32 {
    let mut listen = "0.0.0.0:7879".to_string();
    let mut secret = std::env::var("DUCK_RELAY_SECRET").unwrap_or_default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => if let Some(v) = it.next() { listen = v.clone() },
            "--secret" => if let Some(v) = it.next() { secret = v.clone() },
            _ => { eprintln!("uso: duck relay [--listen 0.0.0.0:7879] [--secret SENHA]  (ou DUCK_RELAY_SECRET)"); return 2; }
        }
    }
    let l = match TcpListener::bind(&listen) {
        Ok(l) => l,
        Err(e) => { eprintln!("duck relay: {listen}: {e}"); return 1; }
    };
    println!("duck relay escutando em {listen}{}", if secret.is_empty() { " (sem senha)" } else { " (com senha)" });
    println!("Nos Ducks: painel Computadores > Relay, ou peer = {{ relay = 'ESTE_IP:{}' }} no ~/.duck-local.lua", crate::peer::port_of(&listen));
    serve(l, secret);
    0
}

/// Roda o relay num listener ja aberto (tambem usado pelos testes).
pub fn serve(l: TcpListener, secret: String) {
    let hub = Arc::new(Mutex::new(Hub::default()));
    // Batimento: derruba registros mortos e pedidos que ninguem aceitou.
    {
        let hub = hub.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let mut h = hub.lock().unwrap();
            h.regs.retain(|_, s| s.write_all(b"{}\n").is_ok());
            h.waiting.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(20));
        });
    }
    for s in l.incoming().flatten() {
        let (hub, secret) = (hub.clone(), secret.clone());
        std::thread::spawn(move || relay_conn(hub, s, &secret));
    }
}

fn relay_conn(hub: Arc<Mutex<Hub>>, mut s: TcpStream, secret: &str) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    let Ok(line) = read_line(&mut s) else { return };
    let Ok(v) = serde_json::from_str::<Value>(&line) else { let _ = send_line(&mut s, &json!({"ok": false, "error": "json"})); return };
    if !secret.is_empty() && v["secret"].as_str() != Some(secret) {
        std::thread::sleep(Duration::from_millis(500));
        let _ = send_line(&mut s, &json!({"ok": false, "error": "senha do relay errada"}));
        return;
    }
    let hexkey = |x: &str| x.len() == 64 && x.chars().all(|c| c.is_ascii_hexdigit());
    // Discagem aceita o comeco da chave (convite traz 16 hexas), se for unico.
    let prefix = |x: &str| (16..=64).contains(&x.len()) && x.chars().all(|c| c.is_ascii_hexdigit());
    if let Some(k) = v["register"].as_str().filter(|k| hexkey(k)) {
        if send_line(&mut s, &json!({"ok": true})).is_err() { return; }
        let _ = s.set_read_timeout(None);
        if let Ok(c) = s.try_clone() { hub.lock().unwrap().regs.insert(k.to_lowercase(), c); }
        // Fica parado ate o Duck fechar; o batimento descobre se morreu.
        let mut b = [0u8; 64];
        while matches!(s.read(&mut b), Ok(n) if n > 0) {}
        let mut h = hub.lock().unwrap();
        if h.regs.get(&k.to_lowercase()).and_then(|r| r.peer_addr().ok()) == s.peer_addr().ok() { h.regs.remove(&k.to_lowercase()); }
    } else if let Some(want) = v["dial"].as_str().filter(|k| prefix(k)) {
        let id = crate::secure::random_hex(8);
        {
            let mut h = hub.lock().unwrap();
            let want = want.to_lowercase();
            let found: Vec<String> = h.regs.keys().filter(|k| k.starts_with(&want)).cloned().collect();
            if found.len() > 1 { drop(h); let _ = send_line(&mut s, &json!({"ok": false, "error": "chave ambigua no relay"})); return; }
            let k = found.into_iter().next().unwrap_or(want);
            let ok = h.regs.get_mut(&k).is_some_and(|c| send_line(c, &json!({"incoming": id})).is_ok());
            if !ok {
                h.regs.remove(&k);
                drop(h);
                let _ = send_line(&mut s, &json!({"ok": false, "error": "o outro Duck nao esta conectado a este relay"}));
                return;
            }
            let Ok(c) = s.try_clone() else { return };
            h.waiting.insert(id, (c, Instant::now()));
        }
        // A conexao fica com o hub; o accept responde e emenda.
    } else if let Some(id) = v["accept"].as_str() {
        let Some((mut d, _)) = hub.lock().unwrap().waiting.remove(id) else {
            let _ = send_line(&mut s, &json!({"ok": false, "error": "pedido expirou"}));
            return;
        };
        if send_line(&mut d, &json!({"ok": true})).is_err() || send_line(&mut s, &json!({"ok": true})).is_err() { return; }
        let _ = s.set_read_timeout(None);
        let _ = d.set_read_timeout(None);
        splice(d, s);
    } else {
        let _ = send_line(&mut s, &json!({"ok": false, "error": "pedido desconhecido"}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registro, discagem e bytes indo e voltando pelo relay.
    #[test]
    fn relay_emenda() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || serve(l, "s3nha".into()));
        let key = "ab".repeat(32);
        let got = Arc::new(Mutex::new(String::new()));
        let g = got.clone();
        let handle: Arc<dyn Fn(TcpStream) + Send + Sync> = Arc::new(move |mut c: TcpStream| {
            let mut b = [0u8; 5];
            c.read_exact(&mut b).unwrap();
            *g.lock().unwrap() = String::from_utf8_lossy(&b).to_string();
            c.write_all(b"pong!").unwrap();
        });
        let status = Arc::new(Mutex::new(String::new()));
        {
            let (a, k, st) = (addr.clone(), key.clone(), status.clone());
            std::thread::spawn(move || client_loop(a, "s3nha".into(), k, st, handle));
        }
        // Espera registrar.
        for _ in 0..50 { if status.lock().unwrap().starts_with("conectado") { break; } std::thread::sleep(Duration::from_millis(20)); }
        assert!(dial(&addr, "errada", &key).unwrap_err().contains("senha"));
        assert!(dial(&addr, "s3nha", &"cd".repeat(32)).unwrap_err().contains("nao esta conectado"));
        // Pelo comeco da chave (convite).
        let mut p = dial(&addr, "s3nha", &key[..16]).unwrap();
        p.write_all(b"oi!!!").unwrap();
        let mut b0 = [0u8; 5];
        p.read_exact(&mut b0).unwrap();
        assert_eq!(&b0, b"pong!");
        let mut s = dial(&addr, "s3nha", &key).unwrap();
        s.write_all(b"ping!").unwrap();
        let mut b = [0u8; 5];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"pong!");
        assert_eq!(*got.lock().unwrap(), "ping!");
    }
}
