//! Transporte entre GUI, CLI e server: local socket (named pipe no Windows)
//! com JSON por linha.
//!
//! Autenticacao: o pipe aceita conexao de qualquer usuario local para leitura,
//! e a API manda texto para shells. Por isso a primeira linha de toda conexao
//! e um token aleatorio que so o dono le (fica no perfil dele, em
//! %LOCALAPPDATA%\duck). Sem o token o server fecha a conexao.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use interprocess::local_socket::{prelude::*, GenericNamespaced, Stream};
use serde_json::{json, Value};

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    let d = base.join("duck");
    // O Duck se chamava Lasso: na 1a vez herda dados, sessoes vivas e historico.
    if !d.exists() && base.join("lasso").is_dir() { let _ = copy_dir(&base.join("lasso"), &d); }
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() { copy_dir(&src, &dst)?; } else { let _ = std::fs::copy(&src, &dst); }
    }
    Ok(())
}

/// DUCK_<k>, ou LASSO_<k> dos shells abertos antes da troca de nome.
pub fn env(k: &str) -> Option<String> {
    std::env::var(format!("DUCK_{k}")).or_else(|_| std::env::var(format!("LASSO_{k}"))).ok()
}

pub fn state_dir() -> PathBuf {
    let d = data_dir().join("state");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    format!("duck-{}.sock", user.to_lowercase())
}

pub fn token_path() -> PathBuf { data_dir().join("token") }

pub fn read_token() -> String {
    std::fs::read_to_string(token_path()).unwrap_or_default().trim().to_string()
}

/// Token novo a cada start do server: 128 bits aleatorios.
pub fn new_token() -> String {
    let s = random_hex(16);
    std::fs::write(token_path(), &s).expect("gravar token");
    s
}

pub struct Conn {
    reader: BufReader<Stream>,
}

impl Conn {
    pub fn connect() -> std::io::Result<Conn> {
        let name = pipe_name().to_ns_name::<GenericNamespaced>()?;
        let mut s = Stream::connect(name)?;
        // Dentro de um pane vale o token do pane (escopo restrito); fora, o do dono.
        let token = env("TOKEN").unwrap_or_else(read_token);
        s.write_all(format!("{token}\n").as_bytes())?;
        Ok(Conn { reader: BufReader::new(s) })
    }

    /// Conecta; se nao houver server, sobe um destacado e tenta por ~3s.
    pub fn connect_or_spawn() -> std::io::Result<Conn> {
        if let Ok(c) = Conn::connect() { return Ok(c); }
        spawn_server()?;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            if let Ok(c) = Conn::connect() { return Ok(c); }
        }
        Conn::connect()
    }

    pub fn send(&mut self, v: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.reader.get_mut().write_all(&line)?;
        self.reader.get_mut().flush()
    }

    pub fn recv(&mut self) -> std::io::Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "server fechou"));
        }
        serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Request/response. Erro do server vira Err com a mensagem dele.
    pub fn call(&mut self, v: Value) -> Result<Value, String> {
        self.send(&v).map_err(|e| e.to_string())?;
        let r = self.recv().map_err(|e| e.to_string())?;
        match r.get("error") {
            Some(e) => Err(e.as_str().unwrap_or("erro").to_string()),
            None => Ok(r),
        }
    }
}

/// O proprio executavel. No Linux, depois de uma reinstalacao o binario em
/// uso e apagado e o caminho vira "... (deleted)": usa o novo no mesmo lugar.
pub fn current_exe() -> std::io::Result<PathBuf> {
    std::env::current_exe().map(undeleted)
}

fn undeleted(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(orig) => PathBuf::from(orig),
        None => p,
    }
}

pub fn spawn_server() -> std::io::Result<()> {
    // Processos de fundo pelo executavel sem console, quando existir.
    let exe = crate::gui_exe().map(Ok).unwrap_or_else(current_exe)?;
    spawn_detached(&[exe.to_string_lossy().to_string(), "server".into()], &[]).map(|_| ())
}

/// Sobe um processo destacado (sobrevive a quem chamou, sem console), com o
/// env atual menos o que vazou da sessao do Claude, mais `extra`. Devolve o pid.
pub fn spawn_detached(argv: &[String], extra: &[(String, String)]) -> std::io::Result<u32> {
    #[cfg(windows)]
    {
        // CreateProcessW direto: o Command do Rust herda os handles herdaveis
        // de quem chamou (ex.: o pipe do `duck cli list | head`), e o processo
        // destacado seguraria esse pipe aberto para sempre.
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW};
        use windows_sys::Win32::Foundation::CloseHandle;
        let line = argv.iter().map(|a| format!("\"{}\"", a.replace('"', "\\\""))).collect::<Vec<_>>().join(" ");
        let mut wline: Vec<u16> = std::ffi::OsStr::new(&line).encode_wide().chain([0]).collect();
        let mut vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| !leaked_env(k) && !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(k)))
            .collect();
        vars.extend(extra.iter().cloned());
        // O bloco de env do Windows precisa vir ordenado.
        vars.sort_by(|a, b| a.0.to_uppercase().cmp(&b.0.to_uppercase()));
        let mut env: Vec<u16> = Vec::new();
        for (k, v) in vars {
            env.extend(std::ffi::OsStr::new(&format!("{k}={v}")).encode_wide());
            env.push(0);
        }
        env.push(0);
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT
        let flags = 0x0000_0008 | 0x0000_0200 | 0x0800_0000 | 0x0000_0400;
        let ok = unsafe {
            CreateProcessW(std::ptr::null(), wline.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, flags,
                env.as_ptr() as _, std::ptr::null(), &si, &mut pi)
        };
        if ok == 0 { return Err(std::io::Error::last_os_error()); }
        let pid = pi.dwProcessId;
        unsafe { CloseHandle(pi.hProcess); CloseHandle(pi.hThread); }
        Ok(pid)
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        for (k, _) in std::env::vars() {
            if leaked_env(&k) { cmd.env_remove(k); }
        }
        for (k, v) in extra { cmd.env(k, v); }
        let mut child = cmd.spawn()?;
        let pid = child.id();
        // Recolhe o filho quando ele sair (senao fica zumbi ate quem chamou sair).
        std::thread::spawn(move || { let _ = child.wait(); });
        Ok(pid)
    }
}

/// Segredo aleatorio em hex (RandomState usa semente aleatoria do SO).
pub fn random_hex(bytes: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut s = String::new();
    let mut i = 0u64;
    while s.len() < bytes * 2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i ^ std::process::id() as u64);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        s.push_str(&format!("{:016x}", h.finish()));
        i += 1;
    }
    s.truncate(bytes * 2);
    s
}

/// Log do server em %LOCALAPPDATA%\duck\server.log (gira em 1 MB).
pub fn log(msg: &str) {
    let p = data_dir().join("server.log");
    if std::fs::metadata(&p).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&p, data_dir().join("server.log.1"));
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{ts} {msg}");
    }
}

/// Variaveis que uma sessao do Claude Code injeta e que nao podem vazar para
/// o mux nem para os shells (README do WezTerm, secao 6). As da segunda lista
/// so saem quando vieram junto com o Claude: fora dele sao escolha do usuario.
/// CLAUDE_CONFIG_DIR e a pior: o Duck aberto de uma sessao de outro perfil (~/.claude-trabalho)
/// fazia todo `claude` digitado num pane logar na conta desse perfil.
pub fn leaked_env(k: &str) -> bool {
    let k = k.to_uppercase();
    let from_claude = std::env::var_os("CLAUDECODE").is_some();
    k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_")
        || (from_claude && ["NO_COLOR", "CLAUDE_CONFIG_DIR", "CLAUDE_BROWSER_PROFILE", "CLAUDE_PID"].contains(&k.as_str()))
}

/// Variavel lida do registro: (nome, valor cru, REG_EXPAND_SZ).
pub type RegVar = (String, String, bool);

/// Variaveis de sessao do logon: o registro da maquina tem USERNAME=SYSTEM e
/// estas sempre vem certas do processo pai (como no Explorer).
const SESSION_VARS: &[&str] = &["USERNAME", "USERPROFILE", "USERDOMAIN", "HOMEDRIVE", "HOMEPATH", "APPDATA", "LOCALAPPDATA", "LOGONSERVER", "SESSIONNAME", "COMPUTERNAME"];

/// Troca %NOME% pelo valor em `vars` (sem diferenciar maiusculas); o que nao
/// existe fica como esta, igual ao ExpandEnvironmentStrings.
fn expand_vars(s: &str, vars: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) if j > 0 => {
                let name = &after[..j];
                match vars.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                    Some((_, v)) => out.push_str(v),
                    None => { out.push('%'); out.push_str(name); out.push('%'); }
                }
                rest = &after[j + 1..];
            }
            _ => { out.push('%'); rest = after; }
        }
    }
    out.push_str(rest);
    out
}

/// Ambiente de uma aba nova no Windows: o que ela herdaria do server mais o
/// que esta no registro agora (maquina, depois usuario). Um programa instalado
/// com o Duck aberto muda o registro, mas o server ficou com o env de quando
/// subiu; o Windows Terminal resolve igual, relendo o registro a cada aba.
/// PATH = maquina;usuario e depois as pastas herdadas que nao estao la.
/// Devolve so as variaveis que vem do registro (o resto continua herdado).
pub fn merge_registry_env(inherited: &[(String, String)], machine: &[RegVar], user: &[RegVar]) -> Vec<(String, String)> {
    let is_path = |k: &str| k.eq_ignore_ascii_case("PATH");
    let skip = |k: &str| SESSION_VARS.iter().any(|s| s.eq_ignore_ascii_case(k)) || leaked_env(k);
    let mut lookup: Vec<(String, String)> = inherited.to_vec();
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v, expand) in machine.iter().chain(user) {
        if is_path(k) || skip(k) { continue; }
        let v = if *expand { expand_vars(v, &lookup) } else { v.clone() };
        out.retain(|(o, _)| !o.eq_ignore_ascii_case(k));
        out.push((k.clone(), v.clone()));
        lookup.push((k.clone(), v));
    }
    let reg_path: Vec<String> = machine.iter().chain(user).filter(|(k, _, _)| is_path(k))
        .map(|(_, v, expand)| if *expand { expand_vars(v, &lookup) } else { v.clone() }).collect();
    if !reg_path.is_empty() {
        let norm = |p: &str| p.trim().trim_end_matches('\\').to_lowercase();
        let mut dirs: Vec<String> = Vec::new();
        let inherited_path = inherited.iter().rev().find(|(k, _)| is_path(k)).map(|(_, v)| v.as_str()).unwrap_or("");
        for d in reg_path.iter().flat_map(|p| p.split(';')).chain(inherited_path.split(';')) {
            if d.trim().is_empty() || dirs.iter().any(|x| norm(x) == norm(d)) { continue; }
            dirs.push(d.to_string());
        }
        let name = inherited.iter().find(|(k, _)| is_path(k)).map(|(k, _)| k.clone()).unwrap_or_else(|| "Path".into());
        out.push((name, dirs.join(";")));
    }
    out
}

/// Variaveis do registro para uma aba nova (vazio se a leitura falhar: a aba
/// abre com o env herdado, como antes).
#[cfg(windows)]
pub fn fresh_env() -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let machine = read_reg_env(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
    let user = read_reg_env(HKEY_CURRENT_USER, "Environment");
    if machine.is_none() && user.is_none() { return Vec::new(); }
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    merge_registry_env(&inherited, &machine.unwrap_or_default(), &user.unwrap_or_default())
}

#[cfg(not(windows))]
pub fn fresh_env() -> Vec<(String, String)> { Vec::new() }

#[cfg(windows)]
fn read_reg_env(root: windows_sys::Win32::System::Registry::HKEY, sub: &str) -> Option<Vec<RegVar>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{RegCloseKey, RegEnumValueW, RegOpenKeyExW, HKEY, KEY_READ, REG_EXPAND_SZ, REG_SZ};
    let wsub: Vec<u16> = std::ffi::OsStr::new(sub).encode_wide().chain([0]).collect();
    let mut key: HKEY = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(root, wsub.as_ptr(), 0, KEY_READ, &mut key) } != 0 { return None; }
    let mut vars = Vec::new();
    let mut i = 0u32;
    loop {
        let mut name = vec![0u16; 16384];
        let mut name_len = name.len() as u32;
        let mut data = vec![0u8; 65536];
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        let r = unsafe { RegEnumValueW(key, i, name.as_mut_ptr(), &mut name_len, std::ptr::null(), &mut kind, data.as_mut_ptr(), &mut data_len) };
        if r != 0 { break; } // ERROR_NO_MORE_ITEMS (ou valor grande demais: para por aqui)
        i += 1;
        if kind != REG_SZ && kind != REG_EXPAND_SZ { continue; }
        let wide: Vec<u16> = data[..data_len as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let value = String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string();
        vars.push((String::from_utf16_lossy(&name[..name_len as usize]), value, kind == REG_EXPAND_SZ));
    }
    unsafe { RegCloseKey(key) };
    Some(vars)
}

/// Atalho para respostas: {"ok":true,...}.
pub fn ok(extra: Value) -> Value {
    let mut v = json!({"ok": true});
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) { o.extend(e); }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_path_after_reinstall() {
        assert_eq!(undeleted("/home/u/.local/bin/duckw (deleted)".into()), PathBuf::from("/home/u/.local/bin/duckw"));
        assert_eq!(undeleted("/home/u/.local/bin/duckw".into()), PathBuf::from("/home/u/.local/bin/duckw"));
    }

    fn kv(v: &[(&str, &str)]) -> Vec<(String, String)> { v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
    fn reg(v: &[(&str, &str, bool)]) -> Vec<RegVar> { v.iter().map(|(k, v, e)| (k.to_string(), v.to_string(), *e)).collect() }
    fn get<'a>(v: &'a [(String, String)], k: &str) -> Option<&'a str> { v.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str()) }

    #[test]
    fn registry_path_new_dirs_and_inherited_kept() {
        let inh = kv(&[("Path", r"C:\Windows;C:\Users\u\AppData\Local\Duck\bin"), ("USERPROFILE", r"C:\Users\u")]);
        let machine = reg(&[("Path", r"C:\Windows;C:\Program Files\nodejs\", false)]);
        let user = reg(&[("Path", r"%USERPROFILE%\AppData\Roaming\npm;C:\windows\", true)]);
        let out = merge_registry_env(&inh, &machine, &user);
        assert_eq!(get(&out, "PATH").unwrap(), r"C:\Windows;C:\Program Files\nodejs\;C:\Users\u\AppData\Roaming\npm;C:\Users\u\AppData\Local\Duck\bin");
        // O nome fica como o processo ja usa (Path), sem duplicar.
        assert_eq!(out.iter().filter(|(k, _)| k.eq_ignore_ascii_case("path")).count(), 1);
        assert_eq!(out.iter().find(|(k, _)| k.eq_ignore_ascii_case("path")).unwrap().0, "Path");
    }

    #[test]
    fn registry_vars_override_and_expand() {
        let inh = kv(&[("Path", r"C:\Windows"), ("TEMP", r"C:\old"), ("USERPROFILE", r"C:\Users\u"), ("KEEP", "1")]);
        let machine = reg(&[("TEMP", r"%SystemRoot%\TEMP", true), ("USERNAME", "SYSTEM", false), ("JAVA_HOME", r"C:\jdk", false)]);
        let user = reg(&[("TEMP", r"%USERPROFILE%\AppData\Local\Temp", true), ("Path", r"%JAVA_HOME%\bin;%NADA%\x", true)]);
        let out = merge_registry_env(&inh, &machine, &user);
        assert_eq!(get(&out, "TEMP"), Some(r"C:\Users\u\AppData\Local\Temp"));   // usuario ganha da maquina
        assert_eq!(get(&out, "JAVA_HOME"), Some(r"C:\jdk"));                      // variavel nova aparece
        assert_eq!(get(&out, "USERNAME"), None);                                  // a da maquina (SYSTEM) nao entra
        assert_eq!(get(&out, "KEEP"), None);                                      // so herdada: continua herdada
        assert_eq!(get(&out, "PATH"), Some(r"C:\jdk\bin;%NADA%\x;C:\Windows"));  // sem valor: fica literal
    }

    #[test]
    fn registry_without_path_and_claude_vars() {
        let inh = kv(&[("Path", r"C:\Windows")]);
        let out = merge_registry_env(&inh, &reg(&[("CLAUDECODE", "1", false), ("CLAUDE_CODE_X", "1", false), ("FOO", "%%", true)]), &[]);
        assert_eq!(get(&out, "CLAUDECODE"), None);
        assert_eq!(get(&out, "CLAUDE_CODE_X"), None);
        assert_eq!(get(&out, "PATH"), None);
        assert_eq!(get(&out, "FOO"), Some("%%"));
    }
}
