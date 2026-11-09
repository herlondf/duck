//! Canal cifrado entre Ducks: Noise XX (X25519, ChaCha20-Poly1305, BLAKE2s).
//!
//! Cada computador tem um par de chaves fixo (`peer/static` no cofre). O
//! handshake XX troca as chaves publicas ja cifrado; quem recebe sabe quem e
//! o outro pela chave publica, e nao por um token que passa pela rede.
//!
//! Na rede: quem conecta manda `MAGIC`, depois cada mensagem do Noise vai
//! como `u16` (big endian) + bytes. Uma mensagem da aplicacao (JSON) pode ser
//! maior que um quadro do Noise: o primeiro quadro comeca com o tamanho total
//! (`u32`) e os quadros seguintes completam.
//!
//! Numero de confirmacao do pareamento: sai do hash do handshake, que so e
//! igual dos dois lados se ninguem estiver no meio. Nove digitos: para forjar
//! um igual no tempo do pedido seria preciso gerar ~10^9 chaves.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const MAGIC: &[u8] = b"DUCK-NOISE/1\n";
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const FRAME: usize = 65535;
/// Texto puro por quadro (o Noise poe 16 bytes de tag).
const CHUNK: usize = FRAME - 16;
/// Maior mensagem aceita (arquivo em base64 cabe folgado).
pub const MAX_MSG: usize = 96 << 20;

fn params() -> snow::params::NoiseParams { PATTERN.parse().expect("padrao Noise") }

/// Chaves deste computador (privada, publica). Cria na primeira vez.
pub fn keypair() -> (Vec<u8>, Vec<u8>) {
    if let Some(s) = crate::secret::get("peer/static") {
        if let Some((a, b)) = s.trim().split_once(':') {
            if let (Some(p), Some(q)) = (unhex(a), unhex(b)) { if p.len() == 32 && q.len() == 32 { return (p, q); } }
        }
    }
    let kp = snow::Builder::new(params()).generate_keypair().expect("gerar chaves");
    let _ = crate::secret::set("peer/static", &format!("{}:{}", hex(&kp.private), hex(&kp.public)));
    (kp.private, kp.public)
}

/// Chave publica deste computador em hexa (identidade, pode circular).
pub fn my_key() -> String { hex(&keypair().1) }

/// Pedaco curto da chave para mostrar e para codigos de convite.
pub fn short(key: &str) -> String { key.chars().take(16).collect() }

pub fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 { return None; }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Numero de confirmacao (9 digitos) a partir do hash do handshake.
pub fn confirm_from_hash(h: &[u8]) -> String {
    let mut n: u64 = 0;
    for b in h.iter().take(8) { n = (n << 8) | *b as u64; }
    let n = n % 1_000_000_000;
    format!("{:03} {:03} {:03}", n / 1_000_000, (n / 1000) % 1000, n % 1000)
}

/// Bytes aleatorios do sistema em hexa (tokens, convites, conversas).
pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("gerador aleatorio do sistema");
    hex(&b)
}

pub struct Chan {
    s: TcpStream,
    t: snow::TransportState,
    remote: String,
    hash: Vec<u8>,
}

fn write_frame(s: &mut TcpStream, b: &[u8]) -> Result<(), String> {
    let mut out = Vec::with_capacity(b.len() + 2);
    out.extend_from_slice(&(b.len() as u16).to_be_bytes());
    out.extend_from_slice(b);
    s.write_all(&out).map_err(|e| e.to_string())
}

fn read_frame(s: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut n = [0u8; 2];
    s.read_exact(&mut n).map_err(|e| if e.kind() == std::io::ErrorKind::UnexpectedEof { "o outro fechou a conexao".into() } else { e.to_string() })?;
    let mut b = vec![0u8; u16::from_be_bytes(n) as usize];
    s.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

impl Chan {
    /// Lado de quem conecta: manda o MAGIC e faz o handshake.
    pub fn connect(s: TcpStream) -> Result<Chan, String> { Self::connect_key(s, &keypair().0) }

    fn connect_key(mut s: TcpStream, private: &[u8]) -> Result<Chan, String> {
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        let mut hs = snow::Builder::new(params()).local_private_key(private).and_then(|b| b.build_initiator()).map_err(|e| e.to_string())?;
        s.write_all(MAGIC).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; FRAME];
        let mut tmp = vec![0u8; FRAME];
        // -> e
        let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
        write_frame(&mut s, &buf[..n])?;
        // <- e, ee, s, es
        let m = read_frame(&mut s).map_err(|e| format!("handshake: {e} (Duck de la antigo, sem criptografia?)"))?;
        hs.read_message(&m, &mut tmp).map_err(|e| format!("handshake: {e}"))?;
        // -> s, se
        let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
        write_frame(&mut s, &buf[..n])?;
        Self::finish(s, hs)
    }

    /// Lado de quem escuta (o MAGIC ja foi lido).
    pub fn accept(s: TcpStream) -> Result<Chan, String> { Self::accept_key(s, &keypair().0) }

    fn accept_key(mut s: TcpStream, private: &[u8]) -> Result<Chan, String> {
        let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
        let mut hs = snow::Builder::new(params()).local_private_key(private).and_then(|b| b.build_responder()).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; FRAME];
        let mut tmp = vec![0u8; FRAME];
        let m = read_frame(&mut s)?;
        hs.read_message(&m, &mut tmp).map_err(|e| format!("handshake: {e}"))?;
        let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
        write_frame(&mut s, &buf[..n])?;
        let m = read_frame(&mut s)?;
        hs.read_message(&m, &mut tmp).map_err(|e| format!("handshake: {e}"))?;
        Self::finish(s, hs)
    }

    fn finish(s: TcpStream, hs: snow::HandshakeState) -> Result<Chan, String> {
        let remote = hs.get_remote_static().map(hex).ok_or("handshake sem a chave do outro")?;
        let hash = hs.get_handshake_hash().to_vec();
        let t = hs.into_transport_mode().map_err(|e| e.to_string())?;
        Ok(Chan { s, t, remote, hash })
    }

    /// Chave publica do outro (hexa): quem ele e.
    pub fn remote(&self) -> &str { &self.remote }

    /// Numero de confirmacao desta conexao (igual dos dois lados).
    pub fn confirm(&self) -> String { confirm_from_hash(&self.hash) }

    pub fn set_timeout(&self, d: Duration) { let _ = self.s.set_read_timeout(Some(d)); }

    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() > MAX_MSG { return Err(format!("mensagem grande demais ({} MB, maximo {} MB)", data.len() >> 20, MAX_MSG >> 20)); }
        let mut plain = Vec::with_capacity(data.len() + 4);
        plain.extend_from_slice(&(data.len() as u32).to_be_bytes());
        plain.extend_from_slice(data);
        let mut buf = vec![0u8; FRAME];
        for c in plain.chunks(CHUNK) {
            let n = self.t.write_message(c, &mut buf).map_err(|e| e.to_string())?;
            write_frame(&mut self.s, &buf[..n])?;
        }
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; FRAME];
        let first = read_frame(&mut self.s)?;
        let n = self.t.read_message(&first, &mut buf).map_err(|e| format!("mensagem adulterada ou fora de ordem: {e}"))?;
        if n < 4 { return Err("mensagem curta".into()); }
        let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if total > MAX_MSG { return Err("mensagem grande demais".into()); }
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&buf[4..n]);
        while out.len() < total {
            let f = read_frame(&mut self.s)?;
            let n = self.t.read_message(&f, &mut buf).map_err(|e| format!("mensagem adulterada ou fora de ordem: {e}"))?;
            out.extend_from_slice(&buf[..n]);
        }
        out.truncate(total);
        Ok(out)
    }

    pub fn send_json(&mut self, v: &Value) -> Result<(), String> { self.send(v.to_string().as_bytes()) }

    pub fn recv_json(&mut self) -> Result<Value, String> {
        let b = self.recv()?;
        serde_json::from_slice(&b).map_err(|e| format!("resposta invalida: {e}"))
    }

    /// Pedido e resposta numa chamada.
    pub fn call(&mut self, v: &Value) -> Result<Value, String> {
        self.send_json(v)?;
        self.recv_json()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_e_confirmacao() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
        let c = confirm_from_hash(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(c.len(), 11);
        assert_eq!(c, confirm_from_hash(&[1, 2, 3, 4, 5, 6, 7, 8, 0]));
        assert_ne!(c, confirm_from_hash(&[1, 2, 3, 4, 5, 6, 7, 9]));
        assert_eq!(random_hex(16).len(), 32);
        assert_ne!(random_hex(16), random_hex(16));
    }

    /// Handshake e mensagens grandes (varios quadros) entre dois lados com
    /// chaves diferentes, sem cofre: chaves passadas direto.
    #[test]
    fn canal_ponta_a_ponta() {
        let b = snow::Builder::new(params());
        let ka = b.generate_keypair().unwrap();
        let kb = snow::Builder::new(params()).generate_keypair().unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let kb_priv = kb.private.clone();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut m = vec![0u8; MAGIC.len()];
            s.read_exact(&mut m).unwrap();
            assert_eq!(m, MAGIC);
            let mut c = Chan::accept_key(s, &kb_priv).unwrap();
            let v = c.recv_json().unwrap();
            c.send_json(&serde_json::json!({"eco": v["x"].as_str().unwrap().len()})).unwrap();
            c.remote().to_string()
        });
        let s = TcpStream::connect(addr).unwrap();
        let mut c = Chan::connect_key(s, &ka.private).unwrap();
        assert_eq!(c.remote(), hex(&kb.public));
        let big = "x".repeat(200_000);
        let r = c.call(&serde_json::json!({"x": big})).unwrap();
        assert_eq!(r["eco"], 200_000);
        assert_eq!(srv.join().unwrap(), hex(&ka.public));
    }
}
