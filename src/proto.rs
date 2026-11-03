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

pub fn spawn_server() -> std::io::Result<()> {
    // Processos de fundo pelo executavel sem console, quando existir.
    let exe = crate::gui_exe().map(Ok).unwrap_or_else(std::env::current_exe)?;
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
        cmd.spawn().map(|c| c.id())
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
/// o mux nem para os shells (README do WezTerm, secao 6). NO_COLOR so sai
/// quando veio junto com o Claude: fora dele e preferencia legitima do usuario.
pub fn leaked_env(k: &str) -> bool {
    let from_claude = std::env::var_os("CLAUDECODE").is_some();
    k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_") || (from_claude && k == "NO_COLOR")
}

/// Atalho para respostas: {"ok":true,...}.
pub fn ok(extra: Value) -> Value {
    let mut v = json!({"ok": true});
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) { o.extend(e); }
    v
}
