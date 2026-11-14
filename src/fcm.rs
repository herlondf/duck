//! Push pelo FCM HTTP v1 (usado pelo `duck relay`): a conta de servico do
//! Firebase (JSON baixado do console) assina um JWT RS256, que vira um
//! access token em oauth2.googleapis.com/token (guardado ate vencer). Cada
//! push e uma mensagem so de dados, prioridade alta, sem texto visivel.
//!
//! HTTP pelo curl, como o `duck update`: nada de TLS dentro do Duck. O token
//! e o corpo vao pelo stdin do curl (`-K -`), fora da linha de comando.

use base64::Engine;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// O que importa da conta de servico.
#[derive(Clone, Debug)]
pub struct Creds { pub email: String, pub private_key: String, pub token_uri: String, pub project: String }

pub fn parse_creds(text: &str) -> Result<Creds, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("credenciais do FCM: {e}"))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let c = Creds { email: s("client_email"), private_key: s("private_key"), token_uri: s("token_uri"), project: s("project_id") };
    if c.email.is_empty() || c.private_key.is_empty() || c.project.is_empty() {
        return Err("credenciais do FCM sem client_email, private_key ou project_id (use o JSON da conta de servico)".into());
    }
    Ok(Creds { token_uri: if c.token_uri.is_empty() { TOKEN_URI.into() } else { c.token_uri.clone() }, ..c })
}

fn b64(b: &[u8]) -> String { base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b) }

/// Claims do JWT que pede o access token (vale 1 h).
pub fn claims(c: &Creds, now: u64) -> Value {
    json!({"iss": c.email, "scope": SCOPE, "aud": c.token_uri, "iat": now, "exp": now + 3600})
}

/// JWT assinado com a chave da conta (RS256 = PKCS#1 v1.5 + SHA-256).
pub fn jwt(c: &Creds, now: u64) -> Result<String, String> {
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::signature::{SignatureEncoding, Signer};
    let key = rsa::RsaPrivateKey::from_pkcs8_pem(&c.private_key).map_err(|e| format!("chave da conta de servico: {e}"))?;
    let head = b64(json!({"alg": "RS256", "typ": "JWT"}).to_string().as_bytes());
    let body = b64(claims(c, now).to_string().as_bytes());
    let input = format!("{head}.{body}");
    let sig = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(key).sign(input.as_bytes());
    Ok(format!("{input}.{}", b64(&sig.to_bytes())))
}

/// Corpo do messages:send: so dados (valores em texto, como o FCM exige),
/// prioridade alta no Android e acordar em segundo plano no iOS.
pub fn message(token: &str, data: &Value) -> Value {
    let data: serde_json::Map<String, Value> = data.as_object().into_iter().flatten()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), json!(v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))).collect();
    json!({"message": {
        "token": token, "data": data,
        "android": {"priority": "high"},
        "apns": {"headers": {"apns-push-type": "background", "apns-priority": "5"}, "payload": {"aps": {"content-available": 1}}},
    }})
}

/// Texto entre aspas no arquivo de config do curl.
pub fn curl_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c { '\\' => o.push_str("\\\\"), '"' => o.push_str("\\\""), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"), c => o.push(c) }
    }
    o.push('"');
    o
}

/// POST pelo curl; devolve (status HTTP, corpo).
fn post(url: &str, headers: &[String], body: &str) -> Result<(u16, String), String> {
    use std::io::Write;
    let mut cfg = format!("url = {}\nrequest = \"POST\"\nmax-time = 20\nsilent\nshow-error\nwrite-out = \"\\n%{{http_code}}\"\n", curl_quote(url));
    for h in headers { cfg.push_str(&format!("header = {}\n", curl_quote(h))); }
    cfg.push_str(&format!("data-binary = {}\n", curl_quote(body)));
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let mut c = std::process::Command::new(curl);
    c.args(["-K", "-"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let mut ch = c.spawn().map_err(|e| format!("curl: {e}"))?;
    ch.stdin.take().ok_or("curl sem stdin")?.write_all(cfg.as_bytes()).map_err(|e| e.to_string())?;
    let out = ch.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    let code = code.trim().parse::<u16>().unwrap_or(0);
    if code == 0 { return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim())); }
    Ok((code, body.to_string()))
}

/// Cliente do FCM com o access token guardado.
pub struct Fcm { creds: Creds, token: Mutex<Option<(String, Instant)>> }

impl Fcm {
    pub fn load(path: &str) -> Result<Fcm, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let creds = parse_creds(&text)?;
        jwt(&creds, 0)?; // chave invalida aparece ja na partida
        Ok(Fcm { creds, token: Mutex::new(None) })
    }

    pub fn project(&self) -> &str { &self.creds.project }

    fn access_token(&self) -> Result<String, String> {
        let mut g = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, until)) = g.as_ref() { if Instant::now() + Duration::from_secs(60) < *until { return Ok(t.clone()); } }
        let assertion = jwt(&self.creds, crate::mobile::now())?;
        let form = format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}");
        let (code, body) = post(&self.creds.token_uri, &["Content-Type: application/x-www-form-urlencoded".into()], &form)?;
        let v: Value = serde_json::from_str(&body).unwrap_or_default();
        let t = v["access_token"].as_str().filter(|_| code == 200).ok_or_else(|| format!("token do Google ({code}): {}", v["error_description"].as_str().or(v["error"].as_str()).unwrap_or(&body)))?;
        let secs = v["expires_in"].as_u64().unwrap_or(3600);
        *g = Some((t.to_string(), Instant::now() + Duration::from_secs(secs)));
        Ok(t.to_string())
    }

    /// Manda um push de dados para um aparelho.
    pub fn send(&self, token: &str, data: &Value) -> Result<(), String> {
        let at = self.access_token()?;
        let url = format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", self.creds.project);
        let (code, body) = post(&url, &[format!("Authorization: Bearer {at}"), "Content-Type: application/json".into()], &message(token, data).to_string())?;
        if code == 401 { *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None; }
        if code == 200 { Ok(()) } else { Err(format!("FCM {code}: {}", body.chars().take(300).collect::<String>())) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Creds {
        use rsa::pkcs8::EncodePrivateKey;
        let k = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 1024).unwrap();
        let pem = k.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap().to_string();
        let j = json!({"type": "service_account", "project_id": "duck-app", "private_key": pem, "client_email": "push@duck-app.iam.gserviceaccount.com"});
        parse_creds(&j.to_string()).unwrap()
    }

    #[test]
    fn jwt_assinado_e_claims() {
        use rsa::signature::Verifier;
        let c = creds();
        assert_eq!(c.token_uri, TOKEN_URI);
        let cl = claims(&c, 1000);
        assert_eq!(cl, json!({"iss": "push@duck-app.iam.gserviceaccount.com", "scope": SCOPE, "aud": TOKEN_URI, "iat": 1000, "exp": 4600}));
        let t = jwt(&c, 1000).unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let dec = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&dec(parts[0])).unwrap(), json!({"alg": "RS256", "typ": "JWT"}));
        assert_eq!(serde_json::from_slice::<Value>(&dec(parts[1])).unwrap(), cl);
        // A assinatura confere com a chave publica.
        use rsa::pkcs8::DecodePrivateKey;
        let pubk = rsa::RsaPrivateKey::from_pkcs8_pem(&c.private_key).unwrap().to_public_key();
        let vk = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(pubk);
        let sig = rsa::pkcs1v15::Signature::try_from(dec(parts[2]).as_slice()).unwrap();
        assert!(vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).is_ok());
        assert!(vk.verify(b"outra coisa", &sig).is_err());
        assert!(parse_creds("{}").is_err());
    }

    #[test]
    fn corpo_da_mensagem() {
        let m = message("tok123", &json!({"duck": "hd-fedora", "kind": "approval", "pane": 12, "approval": 34, "n": 2, "nada": null}));
        assert_eq!(m["message"]["token"], "tok123");
        assert_eq!(m["message"]["data"], json!({"duck": "hd-fedora", "kind": "approval", "pane": "12", "approval": "34", "n": "2"}));
        assert_eq!(m["message"]["android"]["priority"], "high");
        assert!(m["message"].get("notification").is_none(), "so dados, sem texto visivel");
        assert_eq!(curl_quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }
}
