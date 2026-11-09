//! Atualizador: `duck update` e o aviso de versao nova na janela.
//!
//! So atualiza instalacao feita pelo release (get.sh / get.ps1). Instalacao
//! pelo codigo (clone + install) continua com git pull + install, para nao
//! misturar as duas. Quem instalou grava o tipo em <data_dir>/install-kind.
//! A troca dos binarios: no Linux/macOS e o get.sh da versao nova; no
//! Windows e feita aqui mesmo, sem PowerShell (ver `install_platform`).

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

/// Baixa e instala a versao `ver`. No Windows ja reinicia o server; no
/// Linux/macOS quem chamou reinicia.
pub fn install(ver: &str) -> Result<String, String> {
    install_platform(&format!("v{}", ver.trim_start_matches('v')))
}

#[cfg(not(windows))]
fn install_platform(tag: &str) -> Result<String, String> {
    let base = format!("https://raw.githubusercontent.com/{}/{tag}", repo());
    let out = quiet(std::process::Command::new("sh").args(["-c", &format!("curl -fsSL {base}/get.sh | sh")])
        .env("DUCK_VERSION", tag).env("DUCK_REPO", repo()).stdin(std::process::Stdio::null()))
        .output().map_err(|e| format!("sh: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("instalador falhou").trim().to_string()) }
}

/// Windows sem PowerShell: o Defender acusa como trojan um exe que chama
/// `pwsh` para baixar e rodar script (`irm | iex`, `-ExecutionPolicy Bypass`).
/// Aqui e o que o get.ps1 + install.ps1 fazem numa atualizacao: baixa o
/// pacote com curl.exe, confere o SHA256, extrai com tar.exe, troca os
/// binarios (exe em uso: renomeia e copia o novo) e reinicia o server.
#[cfg(windows)]
fn install_platform(tag: &str) -> Result<String, String> {
    use std::path::Path;
    const PKG: &str = "duck-windows-x86_64.zip";
    let exe = crate::proto::current_exe().map_err(|e| e.to_string())?;
    let bin = exe.parent().ok_or("pasta do duck.exe")?.to_path_buf();
    let root = bin.parent().ok_or("pasta da instalacao")?.to_path_buf();
    if !root.join("install.ps1").is_file() { return Err(format!("{} nao e uma instalacao do release", root.display())); }

    let tmp = TempDir(std::env::temp_dir().join(format!("duck-update-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&tmp.0);
    let new = tmp.0.join("pkg");
    std::fs::create_dir_all(&new).map_err(|e| e.to_string())?;
    let sys = std::env::var("SystemRoot").map(|r| Path::new(&r).join("System32")).unwrap_or_else(|_| PathBuf::from(r"C:\Windows\System32"));
    let run = |c: &mut std::process::Command, what: &str| -> Result<(), String> {
        let out = quiet(c.stdin(std::process::Stdio::null())).output().map_err(|e| format!("{what}: {e}"))?;
        if out.status.success() { Ok(()) } else { Err(format!("{what}: {}", String::from_utf8_lossy(&out.stderr).trim())) }
    };
    let base = format!("https://github.com/{}/releases/download/{tag}", repo());
    for f in [PKG, "SHA256SUMS"] {
        run(std::process::Command::new(sys.join("curl.exe")).args(["-fsSL", "--max-time", "300", "-A", "duck-update", "-o"]).arg(tmp.0.join(f)).arg(format!("{base}/{f}")), &format!("baixar {f}"))?;
    }
    let sums = std::fs::read_to_string(tmp.0.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let want = sums.lines().find_map(|l| { let mut it = l.split_whitespace(); let h = it.next()?; (it.next()?.trim_start_matches('*') == PKG).then(|| h.to_lowercase()) })
        .ok_or("SHA256SUMS sem o pacote do Windows")?;
    let got = sha256_hex(&std::fs::read(tmp.0.join(PKG)).map_err(|e| e.to_string())?);
    if got != want { return Err(format!("checksum nao confere: {PKG}")); }
    run(std::process::Command::new(sys.join("tar.exe")).arg("-xf").arg(tmp.0.join(PKG)).arg("-C").arg(&new), "extrair o pacote")?;
    if !new.join("duck.exe").is_file() || !new.join("duckw.exe").is_file() { return Err("pacote sem duck.exe/duckw.exe".into()); }

    // Arquivos da instalacao (extensoes antigas saem, como no get.ps1).
    let _ = std::fs::remove_dir_all(root.join("extensions"));
    copy_tree(&new, &root).map_err(|e| format!("copiar para {}: {e}", root.display()))?;
    // Binarios: um exe em uso nao pode ser sobrescrito, mas pode ser renomeado.
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for n in ["duck", "duckw"] {
        let dst = bin.join(format!("{n}.exe"));
        if dst.exists() { std::fs::rename(&dst, bin.join(format!("{n}.old-{stamp}.exe"))).map_err(|e| format!("{n}.exe: {e}"))?; }
        std::fs::copy(new.join(format!("{n}.exe")), &dst).map_err(|e| format!("{n}.exe: {e}"))?;
    }
    if let Ok(rd) = std::fs::read_dir(&bin) {
        for e in rd.flatten() { if e.file_name().to_string_lossy().contains(".old-") { let _ = std::fs::remove_file(e.path()); } }
    }
    let _ = std::fs::copy(new.join("assets").join("duck.ico"), bin.join("duck.ico"));
    let _ = std::fs::copy(new.join("extensions").join("claude-usage").join("statusline.ps1"), bin.join("duck-statusline.ps1"));

    // Server rodando: reinicia com o binario novo (os shells continuam). De
    // dentro de um pane o token do pane nao pode reiniciar: sai do ambiente.
    let _ = quiet(std::process::Command::new(bin.join("duck.exe")).args(["cli", "restart-server"])
        .env_remove("DUCK_TOKEN").env_remove("DUCK_PANE").env_remove("LASSO_TOKEN").env_remove("LASSO_PANE")
        .stdin(std::process::Stdio::null())).output();
    Ok(format!("Duck {tag} instalado em {}", root.display()))
}

#[cfg(windows)]
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() { copy_tree(&e.path(), &dst)?; } else { std::fs::copy(e.path(), dst)?; }
    }
    Ok(())
}

/// Apaga a pasta quando sai de escopo (deu certo ou nao).
#[cfg(windows)]
struct TempDir(PathBuf);
#[cfg(windows)]
impl Drop for TempDir {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// SHA-256 em hexa (minusculo), para conferir o pacote sem depender de ferramenta externa.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 { w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]); }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 { h[i] = h[i].wrapping_add(v[i]); }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
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
    fn sha256() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(sha256_hex(long), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(sha256_hex(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }

    #[test]
    fn repo_from_cargo() {
        assert_eq!(repo(), "herlondf/duck");
    }
}
