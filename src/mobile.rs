//! Celulares pareados (app duck-app, contrato em docs/MOBILE.md): permissoes,
//! guarda dos aparelhos, convite do QR e o QR em si. Usado pelo server (sessao,
//! push), pela CLI (`duck mobile`) e pela janela (painel Celulares).
//!
//! Guarda: a lista de nomes e a ultima conexao em `mobile.json` (dados do
//! Duck); chave publica, permissoes e token do FCM no cofre (`mobile/<nome>`),
//! como a chave dos computadores pareados. Nome na lista sem segredo no cofre
//! nao vale: editar o json nao cria celular nem da permissao.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Permissoes, na ordem do painel.
pub const SCOPES: [&str; 5] = ["view", "approve", "message", "control", "keyboard"];
/// Ligadas no pareamento; as outras o usuario liga no painel.
pub const DEFAULT_SCOPES: [&str; 3] = ["view", "approve", "message"];

/// Comandos que o celular pode chamar, por permissao. Cada um em uma so.
pub const SCOPE_CMDS: [(&str, &[&str]); 5] = [
    ("view", &["agents", "list", "read", "last-output", "blocks", "block-output", "why", "queue-ls", "inbox", "inbox-read", "loops", "changes",
        "timeline", "usage", "system", "routines", "recent-dirs", "file-get", "push-token"]),
    ("approve", &["approve", "agent-answer"]),
    ("message", &["queue", "queue-rm", "queue-up", "queue-now", "translate"]),
    ("control", &["spawn", "interrupt", "resume", "loop-start", "loop-stop", "routine-run", "file-put", "close"]),
    ("keyboard", &["send", "paste"]),
];

/// Permissao que o comando exige (None: o celular nao chama).
pub fn scope_of(cmd: &str) -> Option<&'static str> {
    SCOPE_CMDS.iter().find(|(_, cs)| cs.contains(&cmd)).map(|(s, _)| *s)
}

/// Comandos que mexem num pane: `pane` e obrigatorio e o pane nao pode ser privado.
pub const PANE_CMDS: [&str; 22] = ["read", "last-output", "blocks", "block-output", "why", "queue-ls", "changes", "timeline", "file-get",
    "agent-answer", "queue", "queue-rm", "queue-up", "queue-now", "translate", "interrupt", "loop-start", "loop-stop", "file-put", "close", "send", "paste"];

/// Maior arquivo que vai e vem do celular.
pub const MAX_FILE: usize = 20 << 20;
/// Validade do convite do QR.
pub const INVITE_SECS: u64 = 10 * 60;
/// Sessao sem nada do celular por este tempo: o Duck fecha.
pub const IDLE_SECS: u64 = 5 * 60;

pub fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

// ---------------------------------------------------------------- aparelhos

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub name: String,
    /// Chave publica do Noise (hexa).
    pub key: String,
    pub scopes: Vec<String>,
    /// Token do FCM (push); vazio sem push.
    pub token: String,
    pub last_seen: u64,
    pub paired_at: u64,
}

impl Device {
    pub fn has(&self, scope: &str) -> bool { self.scopes.iter().any(|s| s == scope) }
    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "scopes": self.scopes, "last_seen": self.last_seen, "paired_at": self.paired_at, "push": !self.token.is_empty(), "key": crate::secure::short(&self.key)})
    }
}

#[cfg(not(test))]
fn index_file() -> std::path::PathBuf { crate::proto::data_dir().join("mobile.json") }
fn secret_name(name: &str) -> String { format!("mobile/{name}") }

/// Testes: lista e cofre em memoria (nada do usuario e tocado).
#[cfg(test)]
type Mem = (Vec<(String, u64, u64)>, Vec<(String, String)>);
#[cfg(test)]
static MEM: std::sync::Mutex<Mem> = std::sync::Mutex::new((Vec::new(), Vec::new()));
#[cfg(test)]
fn mem() -> std::sync::MutexGuard<'static, Mem> { MEM.lock().unwrap_or_else(|e| e.into_inner()) }
#[cfg(test)]
fn secret_get(n: &str) -> Option<String> { mem().1.iter().find(|x| x.0 == n).map(|x| x.1.clone()) }
#[cfg(test)]
fn secret_set(n: &str, v: &str) -> Result<(), String> { let mut m = mem(); m.1.retain(|x| x.0 != n); m.1.push((n.into(), v.into())); Ok(()) }
#[cfg(test)]
fn secret_delete(n: &str) { mem().1.retain(|x| x.0 != n); }
#[cfg(test)]
fn read_index() -> Vec<(String, u64, u64)> { mem().0.clone() }
#[cfg(test)]
fn write_index(list: &[(String, u64, u64)]) -> Result<(), String> { mem().0 = list.to_vec(); Ok(()) }

#[cfg(not(test))]
use crate::secret::{delete as secret_delete, get as secret_get, set as secret_set};

/// Lista: (nome, ultima conexao, pareado em).
#[cfg(not(test))]
fn read_index() -> Vec<(String, u64, u64)> {
    let v: Value = std::fs::read(index_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v["devices"].as_array().into_iter().flatten()
        .filter_map(|d| Some((d["name"].as_str()?.to_string(), d["last_seen"].as_u64().unwrap_or(0), d["paired_at"].as_u64().unwrap_or(0)))).collect()
}

#[cfg(not(test))]
fn write_index(list: &[(String, u64, u64)]) -> Result<(), String> {
    let v = json!({"devices": list.iter().map(|(n, l, p)| json!({"name": n, "last_seen": l, "paired_at": p})).collect::<Vec<_>>()});
    let p = index_file();
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

/// Celulares pareados (so os que tem segredo no cofre).
pub fn list() -> Vec<Device> {
    read_index().into_iter().filter_map(|(name, last_seen, paired_at)| {
        let s: Value = serde_json::from_str(&secret_get(&secret_name(&name))?).ok()?;
        let key = s["key"].as_str().filter(|k| k.len() == 64)?.to_string();
        let scopes = s["scopes"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).filter(|x| SCOPES.contains(x)).map(String::from).collect();
        Some(Device { name, key, scopes, token: s["token"].as_str().unwrap_or("").to_string(), last_seen, paired_at })
    }).collect()
}

pub fn by_key(key: &str) -> Option<Device> { list().into_iter().find(|d| d.key == key) }
pub fn by_name(name: &str) -> Option<Device> { list().into_iter().find(|d| d.name == name) }

/// Grava (cria ou atualiza) o aparelho.
pub fn save(d: &Device) -> Result<(), String> {
    let mut scopes: Vec<&str> = SCOPES.iter().copied().filter(|s| d.has(s)).collect();
    scopes.dedup();
    secret_set(&secret_name(&d.name), &json!({"key": d.key, "scopes": scopes, "token": d.token}).to_string())?;
    let mut idx = read_index();
    match idx.iter_mut().find(|x| x.0 == d.name) {
        Some(x) => { x.1 = d.last_seen; x.2 = d.paired_at; }
        None => idx.push((d.name.clone(), d.last_seen, d.paired_at)),
    }
    write_index(&idx)
}

/// So a ultima conexao (nao reescreve o cofre).
pub fn touch(name: &str) {
    let mut idx = read_index();
    if let Some(x) = idx.iter_mut().find(|x| x.0 == name) { x.1 = now(); let _ = write_index(&idx); }
}

/// Esquece o aparelho: chave, permissoes e token saem do cofre.
pub fn remove(name: &str) -> Result<bool, String> {
    let mut idx = read_index();
    let had = idx.iter().any(|x| x.0 == name);
    idx.retain(|x| x.0 != name);
    secret_delete(&secret_name(name));
    write_index(&idx)?;
    Ok(had)
}

/// Nome livre: `nome`, `nome-2`... (outro aparelho com o mesmo nome nao sobrescreve).
pub fn free_name(taken: &[String], name: &str) -> String {
    let base = crate::peer::clean_name(name);
    if !taken.contains(&base) { return base; }
    (2..).map(|i| format!("{base}-{i}")).find(|n| !taken.contains(n)).unwrap_or(base)
}

/// Aplica `+escopo -escopo` (CLI) ou a lista inteira. Erro em escopo desconhecido.
pub fn apply_scopes(cur: &[String], changes: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = cur.to_vec();
    for c in changes {
        let (on, s) = match c.strip_prefix('+') { Some(s) => (true, s), None => match c.strip_prefix('-') { Some(s) => (false, s), None => (true, c.as_str()) } };
        if !SCOPES.contains(&s) { return Err(format!("permissao desconhecida: {s} (validas: {})", SCOPES.join(", "))); }
        out.retain(|x| x != s);
        if on { out.push(s.to_string()); }
    }
    Ok(SCOPES.iter().filter(|s| out.iter().any(|x| x == *s)).map(|s| s.to_string()).collect())
}

// ---------------------------------------------------------------- convites

/// Convites do QR: codigo -> vale ate (unix). Uso unico.
#[derive(Default)]
pub struct Invites(HashMap<String, u64>);

impl Invites {
    pub fn add(&mut self, code: &str, exp: u64) { self.0.retain(|_, e| *e > now()); self.0.insert(code.to_lowercase(), exp); }
    /// Gasta o convite: true se existia e nao venceu em `at`.
    pub fn take(&mut self, code: &str, at: u64) -> bool {
        let code = code.trim().to_lowercase();
        !code.is_empty() && self.0.remove(&code).is_some_and(|e| at < e)
    }
}

// ---------------------------------------------------------------- QR

/// O que vai no QR (docs/MOBILE.md, secao 2).
#[derive(Clone, Debug, PartialEq)]
pub struct Pairing {
    pub name: String,
    pub key: String,
    pub addrs: Vec<String>,
    pub relay: String,
    pub relay_secret: String,
    pub invite: String,
    pub exp: u64,
}

impl Pairing {
    pub fn to_json(&self) -> Value {
        let mut v = json!({"v": 1, "name": self.name, "key": self.key, "addrs": self.addrs, "invite": self.invite, "exp": self.exp});
        if !self.relay.is_empty() {
            v["relay"] = json!(self.relay);
            if !self.relay_secret.is_empty() { v["relay_secret"] = json!(self.relay_secret); }
        }
        v
    }
    /// `duck://pair/<base64url(JSON)>`.
    pub fn url(&self) -> String {
        format!("duck://pair/{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.to_json().to_string()))
    }
    pub fn parse(url: &str) -> Result<Pairing, String> {
        let b = url.trim().strip_prefix("duck://pair/").ok_or("nao e um QR do Duck")?;
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b.trim_end_matches('=')).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        if v["v"].as_u64() != Some(1) { return Err("versao do QR desconhecida".into()); }
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        Ok(Pairing {
            name: s("name"), key: s("key"), relay: s("relay"), relay_secret: s("relay_secret"), invite: s("invite"),
            addrs: v["addrs"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect(),
            exp: v["exp"].as_u64().unwrap_or(0),
        })
    }
}

/// Modulos do QR (true = escuro), linha a linha, e a largura.
pub fn qr_modules(text: &str) -> Result<(usize, Vec<bool>), String> {
    let c = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::L).map_err(|e| e.to_string())?;
    Ok((c.width(), c.to_colors().into_iter().map(|x| x == qrcode::Color::Dark).collect()))
}

/// QR em texto com meio-blocos (2 modulos por caractere na vertical) e
/// margem de 2 modulos. Escuro = espaco: no terminal de fundo escuro o
/// leitor ve o QR invertido do jeito certo (claro em volta).
pub fn qr_text(text: &str) -> Result<String, String> {
    let (w, m) = qr_modules(text)?;
    let q = 2i64;
    let dark = |x: i64, y: i64| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && m[y as usize * w + x as usize];
    let mut out = String::new();
    let n = w as i64;
    let mut y = -q;
    while y < n + q {
        for x in -q..n + q {
            out.push(match (dark(x, y), dark(x, y + 1)) { (true, true) => ' ', (true, false) => '▄', (false, true) => '▀', (false, false) => '█' });
        }
        out.push('\n');
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escopos_um_por_comando() {
        let mut seen = std::collections::HashSet::new();
        for (s, cmds) in SCOPE_CMDS {
            assert!(SCOPES.contains(&s));
            for c in cmds { assert!(seen.insert(*c), "{c} em mais de uma permissao"); assert_eq!(scope_of(c), Some(s)); }
        }
        for c in PANE_CMDS { assert!(scope_of(c).is_some(), "{c} sem permissao"); }
        for c in ["kill-server", "reload", "mobile-pair", "mobile-revoke", "ext-send", "gui-action", "peer-invite", "restart-server", "", "AGENTS"] {
            assert_eq!(scope_of(c), None, "{c}");
        }
        assert_eq!(scope_of("send"), Some("keyboard"));
        assert_eq!(scope_of("approve"), Some("approve"));
    }

    #[test]
    fn mudar_escopos() {
        let cur: Vec<String> = DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect();
        assert_eq!(apply_scopes(&cur, &["+control".into(), "-message".into()]).unwrap(), vec!["view", "approve", "control"]);
        assert_eq!(apply_scopes(&cur, &["keyboard".into(), "+keyboard".into()]).unwrap(), vec!["view", "approve", "message", "keyboard"]);
        assert!(apply_scopes(&cur, &["+root".into()]).is_err());
        assert_eq!(free_name(&["pixel".into(), "pixel-2".into()], "Pixel"), "pixel-3");
        assert_eq!(free_name(&[], "Meu Pixel 8"), "meupixel8");
    }

    #[test]
    fn convite_vence_e_vale_uma_vez() {
        let mut i = Invites::default();
        let t = now();
        i.add("ABCD", t + 600);
        assert!(!i.take("zzzz", t));
        assert!(i.take("abcd", t + 10));
        assert!(!i.take("abcd", t + 10), "uso unico");
        i.add("ee", t + 600);
        assert!(!i.take("ee", t + 600), "vencido");
        assert!(!i.take("ee", t), "vencido tambem sai");
        assert!(!i.take("", t));
    }

    #[test]
    fn qr_ida_e_volta() {
        let p = Pairing { name: "hd-fedora".into(), key: "ab".repeat(32), addrs: vec!["192.168.0.10:7878".into(), "100.64.0.2:7878".into()],
            relay: "relay.exemplo.com:7879".into(), relay_secret: "s3nha/+=".into(), invite: "0123456789abcdef".into(), exp: 1_800_000_000 };
        let url = p.url();
        assert!(url.starts_with("duck://pair/") && !url.contains('=') && !url.contains('+') && url.matches('/').count() == 3);
        assert_eq!(Pairing::parse(&url).unwrap(), p);
        let v = p.to_json();
        assert_eq!(v["v"], 1);
        assert_eq!(v["relay_secret"], "s3nha/+=");
        // Sem relay: nem relay nem segredo no QR.
        let q = Pairing { relay: String::new(), ..p.clone() };
        assert!(q.to_json().get("relay").is_none() && q.to_json().get("relay_secret").is_none());
        assert_eq!(Pairing::parse(&q.url()).unwrap(), Pairing { relay_secret: String::new(), ..q });
        assert!(Pairing::parse("duck:x@1.2.3.4").is_err());
        let txt = qr_text(&url).unwrap();
        let (w, _) = qr_modules(&url).unwrap();
        assert_eq!(txt.lines().count(), (w + 4).div_ceil(2));
        assert!(txt.lines().all(|l| l.chars().count() == w + 4));
    }
}
