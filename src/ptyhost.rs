//! `duck ptyhost <spec>`: um processo minimo por pane, dono do ConPTY e do
//! shell. O server conversa com ele por dois pipes de mao unica. Se o server
//! cair (crash, kill, update), os shells continuam aqui; o server novo
//! reconecta e recebe os ultimos 256 KB de saida para refazer a tela.
//!
//! Pipes (nomes com segredo aleatorio, guardados so no perfil do usuario):
//!   <nome>-in : server -> host  (teclas, resize, kill)
//!   <nome>-out: host -> server  (saida do shell; replay a cada reconexao)
//! Quadro: [tipo u8][tamanho u32 LE][dados].

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
#[cfg(not(windows))]
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};

pub const T_DATA: u8 = 1;
pub const T_RESIZE: u8 = 2;
pub const T_KILL: u8 = 3;
pub const T_REPLAY: u8 = 4;
pub const T_EXIT: u8 = 9;

const RING: usize = 256 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Spec {
    pub id: u64,
    pub argv: Vec<String>,
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    pub pipe: String,
}

/// Registro de um host vivo (o server novo acha os shells por aqui).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Reg {
    pub id: u64,
    pub pid: u32,
    pub pipe: String,
    pub argv: Vec<String>,
    pub cwd: String,
}

pub fn dir() -> PathBuf {
    let d = crate::proto::data_dir().join("pty");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn write_frame(w: &mut impl Write, t: u8, data: &[u8]) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(5 + data.len());
    buf.push(t);
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    r.read_exact(&mut h)?;
    let n = u32::from_le_bytes([h[1], h[2], h[3], h[4]]) as usize;
    let mut data = vec![0u8; n];
    r.read_exact(&mut data)?;
    Ok((h[0], data))
}

fn listen(name: &str) -> std::io::Result<interprocess::local_socket::Listener> {
    ListenerOptions::new().name(name.to_ns_name::<GenericNamespaced>()?).create_sync()
}

/// O PTY do pane, independente de plataforma.
struct Pty {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    resize: Box<dyn Fn(u16, u16) + Send + Sync>,
    /// Espera o shell sair; devolve o exit code (e fecha o PTY, liberando a leitora).
    wait: Box<dyn FnOnce() -> u32 + Send>,
}

#[cfg(not(windows))]
fn open_pty(spec: &Spec) -> Option<Pty> {
    let pty = native_pty_system().openpty(PtySize { rows: spec.rows, cols: spec.cols, pixel_width: 0, pixel_height: 0 }).ok()?;
    let mut cmd = CommandBuilder::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    cmd.cwd(&spec.cwd);
    let mut child = pty.slave.spawn_command(cmd).ok()?;
    drop(pty.slave);
    let reader = pty.master.try_clone_reader().ok()?;
    let writer = pty.master.take_writer().ok()?;
    let master = Arc::new(Mutex::new(pty.master));
    let m2 = master.clone();
    Some(Pty {
        reader, writer,
        resize: Box::new(move |cols, rows| { let _ = m2.lock().unwrap().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }); }),
        wait: Box::new(move || { let c = child.wait().map(|s| s.exit_code()).unwrap_or(1); drop(master); c }),
    })
}

/// ConPTY criado direto pela API do Windows. O portable-pty liga
/// PSEUDOCONSOLE_RESIZE_QUIRK: no resize o ConPTY nao repinta e confia que o
/// terminal refaz a quebra de linhas igual ao conhost -- o parser do
/// alacritty quebra diferente e a linha do prompt ficava uma abaixo. Sem a
/// flag, o ConPTY repinta a tela depois do resize e ele vira a referencia.
#[cfg(windows)]
fn open_pty(spec: &Spec) -> Option<Pty> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
        CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
    };
    unsafe {
        let (mut in_r, mut in_w, mut out_r, mut out_w): (HANDLE, HANDLE, HANDLE, HANDLE) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        if CreatePipe(&mut in_r, &mut in_w, std::ptr::null(), 0) == 0 || CreatePipe(&mut out_r, &mut out_w, std::ptr::null(), 0) == 0 { return None; }
        let mut hpc: HPCON = 0;
        let size = COORD { X: spec.cols.max(2) as i16, Y: spec.rows.max(2) as i16 };
        if CreatePseudoConsole(size, in_r, out_w, 0, &mut hpc) < 0 { return None; }
        // O ConPTY ficou com as pontas dele; as copias daqui fecham.
        CloseHandle(in_r);
        CloseHandle(out_w);

        let mut attr_size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size);
        let mut attr = vec![0u8; attr_size];
        let list = attr.as_mut_ptr() as _;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut attr_size) == 0 { return None; }
        if UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, hpc as *const _, std::mem::size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null()) == 0 { return None; }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        let line = spec.argv.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ");
        let mut wline: Vec<u16> = std::ffi::OsStr::new(&line).encode_wide().chain([0]).collect();
        let wcwd: Vec<u16> = std::ffi::OsStr::new(&spec.cwd).encode_wide().chain([0]).collect();
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(std::ptr::null(), wline.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT, std::ptr::null(),
            if spec.cwd.is_empty() { std::ptr::null() } else { wcwd.as_ptr() }, &si.StartupInfo, &mut pi);
        if ok == 0 { ClosePseudoConsole(hpc); return None; }
        CloseHandle(pi.hThread);
        let proc_h = pi.hProcess as usize;
        let reader = std::fs::File::from_raw_handle(out_r as RawHandle);
        let writer = std::fs::File::from_raw_handle(in_w as RawHandle);
        Some(Pty {
            reader: Box::new(reader),
            writer: Box::new(writer),
            resize: Box::new(move |cols, rows| { ResizePseudoConsole(hpc, COORD { X: cols.max(2) as i16, Y: rows.max(2) as i16 }); }),
            wait: Box::new(move || {
                let h = proc_h as HANDLE;
                WaitForSingleObject(h, INFINITE);
                let mut code = 1u32;
                GetExitCodeProcess(h, &mut code);
                CloseHandle(h);
                // Fechar o ConPTY entrega EOF para a leitora, que drena o resto.
                ClosePseudoConsole(hpc);
                drop(attr);
                code
            }),
        })
    }
}

/// Aspas de linha de comando do Windows (regras do CommandLineToArgvW).
#[cfg(windows)]
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) { return a.to_string(); }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => { out.push_str(&"\\".repeat(slashes * 2 + 1)); out.push('"'); slashes = 0; }
            c => { out.push_str(&"\\".repeat(slashes)); out.push(c); slashes = 0; }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

pub fn run(spec_path: &str) -> i32 {
    let Ok(raw) = std::fs::read(spec_path) else { return 2 };
    let _ = std::fs::remove_file(spec_path);
    let Ok(spec) = serde_json::from_slice::<Spec>(&raw) else { return 2 };

    let Some(pty) = open_pty(&spec) else { return 3 };
    let (Ok(lin), Ok(lout)) = (listen(&format!("{}-in", spec.pipe)), listen(&format!("{}-out", spec.pipe))) else { return 5 };

    let reg_path = dir().join(format!("{}.json", spec.id));
    let reg = Reg { id: spec.id, pid: std::process::id(), pipe: spec.pipe.clone(), argv: spec.argv.clone(), cwd: spec.cwd.clone() };
    let _ = std::fs::write(&reg_path, serde_json::to_vec(&reg).unwrap());

    // Saida: anel com o fim do historico + conexao atual (troca a cada reconexao).
    let out: Arc<Mutex<(VecDeque<u8>, Option<Stream>)>> = Arc::new(Mutex::new((VecDeque::new(), None)));
    {
        let out = out.clone();
        std::thread::spawn(move || {
            for conn in lout.incoming().flatten() {
                let mut g = out.lock().unwrap();
                let mut conn = conn;
                let replay: Vec<u8> = g.0.iter().copied().collect();
                if write_frame(&mut conn, T_REPLAY, &replay).is_ok() { g.1 = Some(conn); }
            }
        });
    }
    let Pty { mut reader, mut writer, resize, wait } = pty;
    {
        std::thread::spawn(move || {
            for conn in lin.incoming().flatten() {
                let mut conn = conn;
                while let Ok((t, data)) = read_frame(&mut conn) {
                    match t {
                        T_DATA => { let _ = writer.write_all(&data); let _ = writer.flush(); }
                        T_RESIZE if data.len() == 4 => {
                            let cols = u16::from_le_bytes([data[0], data[1]]);
                            let rows = u16::from_le_bytes([data[2], data[3]]);
                            resize(cols, rows);
                        }
                        T_KILL => { std::process::exit(0); }
                        _ => {}
                    }
                }
            }
        });
    }
    let reader_out = out.clone();
    let rt = std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 { break; }
            let mut g = reader_out.lock().unwrap();
            g.0.extend(&buf[..n]);
            let excess = g.0.len().saturating_sub(RING);
            g.0.drain(..excess);
            let failed = g.1.as_mut().is_some_and(|c| write_frame(c, T_DATA, &buf[..n]).is_err());
            if failed { g.1 = None; } // server caiu: segue guardando no anel
        }
    });
    let code = wait();
    // Drena o que o shell escreveu por ultimo antes de avisar a saida.
    let _ = rt.join();
    let _ = std::fs::remove_file(&reg_path);
    if let Some(c) = out.lock().unwrap().1.as_mut() { let _ = write_frame(c, T_EXIT, &code.to_le_bytes()); }
    0
}

/// Lado do server: conecta aos dois pipes de um host.
pub struct Client { pub input: Stream, pub output: Stream }

pub fn connect(pipe: &str) -> std::io::Result<Client> {
    let input = Stream::connect(format!("{pipe}-in").to_ns_name::<GenericNamespaced>()?)?;
    let output = Stream::connect(format!("{pipe}-out").to_ns_name::<GenericNamespaced>()?)?;
    Ok(Client { input, output })
}

/// Hosts vivos de uma execucao anterior do server.
pub fn alive() -> Vec<Reg> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "json") {
            match std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Reg>(&b).ok()) {
                Some(r) if process_alive(r.pid) => v.push(r),
                _ => { let _ = std::fs::remove_file(&p); }
            }
        }
    }
    v.sort_by_key(|r| r.id);
    v
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() { return false; }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0 && code == 259; // STILL_ACTIVE
        CloseHandle(h);
        ok
    }
    #[cfg(not(windows))]
    { std::path::Path::new(&format!("/proc/{pid}")).exists() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_quoting() {
        assert_eq!(quote_arg("pwsh.exe"), "pwsh.exe");
        assert_eq!(quote_arg(r"C:\a b\x.ps1"), r#""C:\a b\x.ps1""#);
        assert_eq!(quote_arg(r". 'C:\x\duck.ps1'"), r#"". 'C:\x\duck.ps1'""#);
        // barra antes de aspas e no fim: dobra (regra do CommandLineToArgvW)
        assert_eq!(quote_arg(r"a\b c\"), r#""a\b c\\""#);
        assert_eq!(quote_arg("say \"oi\""), "\"say \\\"oi\\\"\"");
        assert_eq!(quote_arg(""), "\"\"");
    }

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, T_RESIZE, &[1, 2, 3, 4]).unwrap();
        write_frame(&mut buf, T_DATA, b"oi").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap(), (T_RESIZE, vec![1, 2, 3, 4]));
        assert_eq!(read_frame(&mut r).unwrap(), (T_DATA, b"oi".to_vec()));
    }
}
