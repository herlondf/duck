//! Atualizador: `duck update` e o aviso de versao nova na janela.
//!
//! So atualiza instalacao feita pelo release (get.sh / get.ps1). Instalacao
//! pelo codigo (clone + install) continua com git pull + install, para nao
//! misturar as duas. Quem instalou grava o tipo em <data_dir>/install-kind.
//! A troca dos binarios e o proprio get.sh / get.ps1 da versao nova: baixa,
//! confere o SHA256 e roda o instalador.

use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// owner/repo, do `repository` do Cargo.toml.
pub fn repo() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_end_matches('/').trim_start_matches("https://github.com/")
}

pub fn current() -> &'static str { env!("CARGO_PKG_VERSION") }

#[derive(Debug, PartialEq)]
pub enum Kind {
    Release,
    /// Pelo codigo: a pasta do clone.
    Source(String),
}

fn kind_file() -> PathBuf { crate::proto::data_dir().join("install-kind") }

/// Tipo da instalacao. Sem o arquivo (instalado antes dele existir), pela
/// pasta: no Windows o release fica em <raiz>\bin com install.ps1 e sem
/// Cargo.toml na raiz; um clone tem Cargo.toml. No resto, pelo codigo.
pub fn kind() -> Kind {
    if let Ok(s) = std::fs::read_to_string(kind_file()) {
        let s = s.trim();
        if s == "release" { return Kind::Release; }
        if let Some(root) = s.strip_prefix("source") { return Kind::Source(root.trim().to_string()); }
    }
    let root = crate::proto::current_exe().ok().and_then(|e| e.parent()?.parent().map(|p| p.to_path_buf()));
    match root {
        Some(r) if r.join("Cargo.toml").is_file() => Kind::Source(r.display().to_string()),
        Some(r) if cfg!(windows) && r.join("install.ps1").is_file() => Kind::Release,
        _ => Kind::Source(String::new()),
    }
}

/// "0.1.10" > "0.1.9" (numerico, parte a parte; "v" na frente e ignorado).
pub fn newer(latest: &str, cur: &str) -> bool {
    let parts = |v: &str| v.trim().trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (parts(latest), parts(cur));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y { return x > y; }
    }
    false
}

fn quiet(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: a janela do Duck nao pisca um console
    }
    cmd
}

/// Ultima versao publicada ("0.1.4"), pela API do GitHub (curl vem no Linux,
/// no macOS e no Windows 10+).
pub fn latest() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let curl = if cfg!(windows) { "curl.exe" } else { "curl" };
    let out = quiet(std::process::Command::new(curl).args(["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: duck-update", &url]))
        .stdin(std::process::Stdio::null()).output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() { return Err(format!("GitHub nao respondeu ({})", String::from_utf8_lossy(&out.stderr).trim())); }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    v["tag_name"].as_str().map(|t| t.trim_start_matches('v').to_string()).ok_or_else(|| "release sem tag".into())
}

/// Versao nova, consultando no maximo uma vez a cada `max_age` (guarda em
/// <data_dir>/update-check.json). None: em dia, ou sem rede.
pub fn check_cached(max_age: Duration) -> Option<String> {
    let p = crate::proto::data_dir().join("update-check.json");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let cached: Value = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    let fresh = cached["at"].as_u64().is_some_and(|at| now.saturating_sub(at) < max_age.as_secs());
    let latest = match cached["latest"].as_str() {
        Some(l) if fresh => l.to_string(),
        _ => {
            let l = latest().ok()?;
            let _ = std::fs::write(&p, serde_json::json!({"at": now, "latest": l}).to_string());
            l
        }
    };
    newer(&latest, current()).then_some(latest)
}

/// Baixa e instala a versao `ver` pelo get.sh / get.ps1 dela. No Windows o
/// instalador ja reinicia o server; no Linux/macOS quem chamou reinicia.
pub fn install(ver: &str) -> Result<String, String> {
    let tag = format!("v{}", ver.trim_start_matches('v'));
    let base = format!("https://raw.githubusercontent.com/{}/{tag}", repo());
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("pwsh");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &format!("irm {base}/get.ps1 | iex")]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &format!("curl -fsSL {base}/get.sh | sh")]);
        c
    };
    let out = quiet(cmd.env("DUCK_VERSION", &tag).env("DUCK_REPO", repo()).stdin(std::process::Stdio::null()))
        .output().map_err(|e| format!("{}: {e}", if cfg!(windows) { "pwsh (PowerShell 7)" } else { "sh" }))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("instalador falhou").trim().to_string()) }
}

fn source_hint(root: &str) -> String {
    let at = if root.is_empty() { "na pasta do clone".to_string() } else { format!("em {root}") };
    let inst = if cfg!(windows) { "pwsh -File .\\install.ps1" } else { "./install.sh" };
    format!("instalado pelo codigo: atualize {at} com git pull e {inst}. Para atualizar sozinho, instale pelo release: https://github.com/{}#install", repo())
}

/// O que dizer quando ha versao nova (aviso da janela e `duck update --check`).
pub fn how_to(latest: &str) -> String {
    match kind() {
        Kind::Release => format!("Duck {latest} disponivel (esta: {}) · Ctrl+Shift+P > Atualizar o Duck", current()),
        Kind::Source(root) => format!("Duck {latest} disponivel (esta: {}) · {}", current(), source_hint(&root)),
    }
}

/// `duck update [--check]`.
pub fn main(args: &[String]) -> i32 {
    let check = args.iter().any(|a| a == "--check");
    let latest = match latest() { Ok(l) => l, Err(e) => { eprintln!("duck update: {e}"); return 1 } };
    if !newer(&latest, current()) { println!("Duck {} ja e a ultima versao", current()); return 0; }
    if check { println!("{}", how_to(&latest)); return 0; }
    if let Kind::Source(root) = kind() { eprintln!("duck update: {}", source_hint(&root)); return 1; }
    println!("Atualizando {} -> {latest}...", current());
    match install(&latest) {
        Ok(_) => {
            println!("Duck {latest} instalado.");
            if !cfg!(windows) { println!("Reinicie o server para usar: Ctrl+Shift+P > Reiniciar server (os shells continuam)."); }
            0
        }
        Err(e) => { eprintln!("duck update: {e}"); 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("v0.2.0", "0.1.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.1.3", "0.1.3"));
        assert!(!newer("0.1.2", "0.1.3"));
        assert!(!newer("0.1.3", "0.1.3.0"));
    }

    #[test]
    fn repo_from_cargo() {
        assert_eq!(repo(), "herlondf/duck");
    }
}
