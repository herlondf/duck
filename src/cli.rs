//! `duck cli <cmd> [--chave valor]...`: a API do server na linha de comando,
//! para agentes e scripts. Responde JSON.
//!
//!   duck cli list
//!   duck cli spawn --where right --cmd "claude"        (pane novo ao lado do atual)
//!   duck cli send --pane 3 --text "rode os testes" --enter
//!   duck cli read --pane 3 --lines 40
//!   duck cli wait --pane 3 --state blocked,done --timeout 900
//!
//! Sem --pane, comandos de pane usam $DUCK_PANE (o pane de quem chamou).

use std::io::Write;

use serde_json::{json, Map, Value};

use crate::proto::Conn;

pub fn parse_args(cmd: &str, args: &[String]) -> Value {
    let mut o = Map::new();
    o.insert("cmd".into(), json!(cmd));
    let mut i = 0;
    while i < args.len() {
        let Some(k) = args[i].strip_prefix("--") else { i += 1; continue };
        let next = args.get(i + 1).filter(|v| !v.starts_with("--"));
        let v = match next {
            None => json!(true),
            Some(v) => {
                i += 1;
                if let Ok(n) = v.parse::<i64>() { json!(n) }
                else if let Ok(f) = v.parse::<f64>() { json!(f) }
                else if v == "true" || v == "false" { json!(v == "true") }
                else { json!(v) }
            }
        };
        // --cmd e o argv do spawn; "cmd" no protocolo e o nome do comando.
        let k = if k == "cmd" { "argv".to_string() } else { k.replace('-', "_") };
        o.insert(k, v);
        i += 1;
    }
    // Pane de quem chamou: um agente controla os vizinhos sem saber ids.
    let pane_cmds = ["send", "read", "close", "paste", "scroll", "search", "set-state", "clear-attention"];
    if !o.contains_key("pane") && (pane_cmds.contains(&cmd) || (cmd == "spawn" && o.get("where").and_then(|w| w.as_str()).is_some_and(|w| w != "tab"))) {
        if let Some(p) = crate::proto::env("PANE") { o.insert("pane".into(), json!(p.parse::<u64>().unwrap_or(0))); }
    }
    Value::Object(o)
}

pub fn run(args: &[String]) -> i32 {
    let Some(cmd) = args.first() else {
        eprintln!("uso: duck cli <comando> [--chave valor]...\ncomandos: list spawn send read wait close focus tab \
            workspace rename-tab zoom adjust scroll search set-state save states restore delete-state \
            checkpoint checkpoint-restore events reload kill-server ping");
        return 2;
    };
    let req = parse_args(cmd, &args[1..]);
    let conn = if ["kill-server", "restart-server", "ping"].contains(&cmd.as_str()) { Conn::connect() } else { Conn::connect_or_spawn() };
    let mut conn = match conn {
        Ok(c) => c,
        Err(e) => { eprintln!("duck: server indisponivel: {e}"); return 1; }
    };
    match conn.call(req) {
        Ok(v) => { println!("{}", serde_json::to_string_pretty(&v).unwrap()); 0 }
        Err(e) if cmd == "kill-server" && e.contains("server fechou") => 0,
        Err(e) => { eprintln!("duck: {e}"); 1 }
    }
}

/// Hook do Claude Code (`duck hook` em settings.json, todos os eventos).
/// Le o JSON do evento no stdin e informa o estado do pane. Nunca falha: um
/// hook que da erro atrapalharia o agente.
pub fn hook() -> i32 {
    let Some(pane) = crate::proto::env("PANE") else { return 0 };
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    let ev: Value = serde_json::from_str(&input).unwrap_or(json!({}));
    let name = ev["hook_event_name"].as_str().unwrap_or("");
    let state = match name {
        "SessionStart" => "idle",
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => "working",
        "Notification" | "PermissionRequest" => "blocked",
        "Stop" => "done",
        "SessionEnd" => "none",
        _ => return 0,
    };
    let mut req = json!({
        "cmd": "set-state", "pane": pane.parse::<u64>().unwrap_or(0), "state": state,
        "agent": "claude", "source": "hook",
        "session": ev["session_id"], "cwd": ev["cwd"],
        "config_dir": std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default(),
    });
    if let Some(path) = ev["transcript_path"].as_str() {
        if let Some(t) = context_tokens(path) { req["context_tokens"] = json!(t); }
        // Plano do agente: so quando uma ferramenta de tarefas rodou (le o transcript inteiro).
        let tool = ev["tool_name"].as_str().unwrap_or("");
        if matches!(tool, "TaskCreate" | "TaskUpdate" | "TodoWrite") || name == "Stop" {
            if let Some((done, total, tokens)) = transcript_stats(path) {
                if total > 0 { req["todo_done"] = json!(done); req["todo_total"] = json!(total); }
                if name == "Stop" { req["session_tokens"] = json!(tokens); }
            }
        }
    }
    if let Ok(mut c) = Conn::connect() { let _ = c.call(req); }
    0
}

/// Escreve a config de exemplo em ~/.duck.lua (sem sobrescrever).
pub fn init() -> i32 {
    let p = crate::config::config_path();
    if p.exists() { println!("ja existe: {}", p.display()); return 0; }
    match std::fs::write(&p, crate::config::DEFAULT_CONFIG) {
        Ok(_) => { println!("criado: {}", p.display()); 0 }
        Err(e) => { eprintln!("{e}"); 1 }
    }
}

/// `duck plugin add <url-git>` / `duck plugin list`.
pub fn plugin(args: &[String]) -> i32 {
    let dir = crate::config::plugins_dir();
    let _ = std::fs::create_dir_all(&dir);
    match args.first().map(String::as_str) {
        Some("add") => {
            let Some(url) = args.get(1) else { eprintln!("uso: duck plugin add <url-git>"); return 2 };
            let name = url.trim_end_matches('/').trim_end_matches(".git").rsplit('/').next().unwrap_or("plugin").to_string();
            let st = std::process::Command::new("git").args(["clone", "--depth", "1", url]).arg(dir.join(&name)).status();
            match st {
                Ok(s) if s.success() => { println!("instalado: {name}  ->  use require '{name}' na config"); 0 }
                _ => { eprintln!("git clone falhou"); 1 }
            }
        }
        Some("update") => {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let _ = std::process::Command::new("git").arg("-C").arg(e.path()).args(["pull", "--ff-only"]).status();
            }
            0
        }
        _ => {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                println!("{}", e.file_name().to_string_lossy());
            }
            0
        }
    }
}

/// `duck install`: atalho no Iniciar do Windows que sobe o Duck restaurando
/// o checkpoint. Os hooks do Claude ficam em `duck install --hooks <settings.json>`.
pub fn install(args: &[String]) -> i32 {
    let exe = std::env::current_exe().unwrap();
    let gui = crate::gui_exe().unwrap_or_else(|| exe.clone());
    if let Some(i) = args.iter().position(|a| a == "--hooks") {
        let Some(path) = args.get(i + 1) else { eprintln!("uso: duck install --hooks <settings.json>"); return 2 };
        return install_hooks(path, &exe.to_string_lossy());
    }
    let quick = args.iter().any(|a| a == "--quick");
    let home = crate::config::home();
    // Linux: autostart do desktop (XDG). macOS: LaunchAgent.
    if cfg!(target_os = "linux") {
        let dir = home.join(".config/autostart");
        let _ = std::fs::create_dir_all(&dir);
        let body = format!("[Desktop Entry]
Type=Application
Name=Duck
Exec=\"{}\" --restore
Icon=duck
X-GNOME-Autostart-enabled=true
", gui.display());
        return match std::fs::write(dir.join("duck.desktop"), body) {
            Ok(_) => { println!("restauracao no login: {}", dir.join("duck.desktop").display()); 0 }
            Err(e) => { eprintln!("{e}"); 1 }
        };
    }
    if cfg!(target_os = "macos") {
        let dir = home.join("Library/LaunchAgents");
        let _ = std::fs::create_dir_all(&dir);
        let body = format!(concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
",
            "<plist version=\"1.0\"><dict><key>Label</key><string>dev.duck.restore</string>",
            "<key>ProgramArguments</key><array><string>{}</string><string>--restore</string></array>",
            "<key>RunAtLoad</key><true/></dict></plist>
"), gui.display());
        let path = dir.join("dev.duck.restore.plist");
        return match std::fs::write(&path, body) {
            Ok(_) => { println!("restauracao no login: {}", path.display()); 0 }
            Err(e) => { eprintln!("{e}"); 1 }
        };
    }
    let Some(appdata) = std::env::var_os("APPDATA") else { eprintln!("sem APPDATA"); return 1 };
    let startup = std::path::PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup\duck.cmd");
    let body = if quick {
        format!("@echo off\r\nstart \"\" \"{}\" --restore\r\nstart \"\" \"{}\" --quick\r\n", gui.display(), gui.display())
    } else {
        format!("@echo off\r\nstart \"\" \"{}\" --restore\r\n", gui.display())
    };
    match std::fs::write(&startup, body) {
        Ok(_) => { println!("restauracao no boot: {}", startup.display()); 0 }
        Err(e) => { eprintln!("{e}"); 1 }
    }
}

fn install_hooks(path: &str, exe: &str) -> i32 {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
    let mut v: Value = match serde_json::from_str(&raw) { Ok(v) => v, Err(e) => { eprintln!("{path}: {e}"); return 1 } };
    let _ = std::fs::write(format!("{path}.bak-duck"), &raw);
    let cmd = format!("\"{}\" hook", exe.replace('\\', "/"));
    let hooks = v.as_object_mut().unwrap().entry("hooks").or_insert(json!({}));
    for ev in ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Notification", "Stop", "SessionEnd"] {
        let list = hooks.as_object_mut().unwrap().entry(ev).or_insert(json!([]));
        let present = list.to_string().contains(" hook\"") && list.to_string().contains("duck");
        if !present {
            list.as_array_mut().unwrap().push(json!({"hooks": [{"type": "command", "command": cmd, "timeout": 5}]}));
        }
    }
    match std::fs::write(path, serde_json::to_string_pretty(&v).unwrap()) {
        Ok(_) => { println!("hooks do Duck em {path} (backup .bak-duck)"); 0 }
        Err(e) => { eprintln!("{e}"); 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn args() {
        let v = parse_args("send", &["--pane".into(), "3".into(), "--text".into(), "oi".into(), "--enter".into()]);
        assert_eq!(v["pane"], 3);
        assert_eq!(v["text"], "oi");
        assert_eq!(v["enter"], true);
    }
}


/// Contexto usado pela ultima resposta do agente: soma dos tokens de entrada
/// (com cache) do ultimo `usage` no fim do transcript do Claude Code.
fn context_tokens(path: &str) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(256 * 1024))).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    buf.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| {
        let u = &v["message"]["usage"];
        u.is_object().then(|| ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
            .iter().map(|k| u[k].as_u64().unwrap_or(0)).sum())
    })
}

/// `duck doctor`: o que costuma dar errado, numa tela so.
pub fn doctor() -> i32 {
    let ok = |b: bool| if b { "ok " } else { "!! " };
    println!("duck {}", env!("CARGO_PKG_VERSION"));
    let (_cfg, err) = crate::config::load_config();
    println!("{}config {} {}", ok(err.is_none()), crate::config::config_path().display(), err.unwrap_or_default());
    match Conn::connect().and_then(|mut c| c.call(json!({"cmd": "doctor"})).map_err(std::io::Error::other)) {
        Ok(v) => {
            println!("{}server {} (pid {}), {} panes, {} hosts vivos", ok(v["version"] == env!("CARGO_PKG_VERSION")), v["version"], v["pid"], v["panes"], v["hosts_vivos"]);
            println!("    log: {}", v["log"].as_str().unwrap_or(""));
            println!("    auditoria: {}", v["audit"].as_str().unwrap_or(""));
            println!("    api_scope={} clipboard_write={} shell_integration={}", v["api_scope"], v["clipboard_write"], v["shell_integration"]);
        }
        Err(e) => println!("-- server nao esta rodando ({e}); sobe sozinho ao abrir o Duck"),
    }
    // Hooks do Claude em cada perfil.
    for e in std::fs::read_dir(crate::config::home()).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let settings = e.path().join("settings.json");
        if name.starts_with(".claude") && settings.is_file() {
            let has = std::fs::read_to_string(&settings).unwrap_or_default().contains("duck");
            println!("{}hooks do Duck em ~/{name}/settings.json{}", ok(has), if has { "" } else { "  (duck install --hooks <arquivo>)" });
        }
    }
    let wsl = std::process::Command::new("wsl.exe").args(["-l", "-q"]).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).replace('\0', "").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    println!("{}WSL: {}", ok(!wsl.is_empty()), if wsl.is_empty() { "nenhuma distro" } else { &wsl });
    let script = crate::proto::data_dir().join("shell").join("duck.sh");
    println!("    shell integration bash/zsh: no ~/.bashrc do WSL ponha  . {}", to_wsl_path(&script.to_string_lossy()));
    0
}

fn to_wsl_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    match p.split_once(":/") { Some((d, rest)) if d.len() == 1 => format!("/mnt/{}/{rest}", d.to_lowercase()), _ => p }
}

/// `duck run --cmd "..." [--timeout 600] [--domain X] [--cwd D]`: roda um
/// comando num pane sem janela (CI, scripts), espera acabar e imprime a saida.
pub fn run_headless(args: &[String]) -> i32 {
    let a = parse_args("spawn", args);
    let Some(cmd) = a["argv"].as_str().map(String::from) else { eprintln!("uso: duck run --cmd \"comando\" [--timeout s] [--domain X] [--cwd D]"); return 2 };
    let timeout = a["timeout"].as_f64().unwrap_or(600.0);
    let marker = format!("__DUCK_FIM_{}__", crate::proto::random_hex(4));
    // O shell fica vivo depois do comando (para ler a saida) e imprime um
    // marcador com o exit code no fim.
    let wsl = a["domain"].as_str().is_some();
    let line = if wsl { format!("{cmd}; echo \"{marker} $?\"") } else { format!("{cmd}; Write-Host \"{marker} $LASTEXITCODE\"") };
    let mut req = json!({"cmd": "spawn", "where": "tab", "workspace": "headless", "focus": false});
    for k in ["domain", "cwd"] { if let Some(v) = a.get(k) { req[k] = v.clone(); } }
    let mut c = match Conn::connect_or_spawn() { Ok(c) => c, Err(e) => { eprintln!("{e}"); return 1 } };
    let pane = match c.call(req) { Ok(v) => v["pane"].as_u64().unwrap_or(0), Err(e) => { eprintln!("{e}"); return 1 } };
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let _ = c.call(json!({"cmd": "send", "pane": pane, "text": format!("{line}\r")}));
    let start = std::time::Instant::now();
    let mut code = 1;
    let text = loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let t = c.call(json!({"cmd": "read", "pane": pane, "all": true})).ok().and_then(|v| v["text"].as_str().map(String::from)).unwrap_or_default();
        if let Some(l) = t.lines().rev().find(|l| l.starts_with(&marker)) {
            code = l[marker.len()..].trim().parse().unwrap_or(0);
            break t;
        }
        if start.elapsed().as_secs_f64() > timeout { eprintln!("duck run: timeout"); break t; }
    };
    let _ = c.call(json!({"cmd": "close", "pane": pane}));
    // So a saida: do comando ecoado ate o marcador.
    let lines: Vec<&str> = text.lines().collect();
    let to = lines.iter().rposition(|l| l.starts_with(&marker)).unwrap_or(lines.len());
    // O eco do comando pode quebrar em varias linhas: a saida comeca depois
    // da linha que fecha o eco (termina com o exit code pedido).
    let from = lines[..to].iter().rposition(|l| { let t = l.trim_end(); t.ends_with("$LASTEXITCODE\"") || t.ends_with("$?\"") })
        .map(|i| i + 1).unwrap_or(0);
    for l in &lines[from..to.max(from)] { println!("{l}"); }
    code
}

/// `duck fleet --agents 3 --cmd "claude" tarefas.txt`: abre N agentes em
/// panes e distribui as tarefas (uma por linha); cada agente pega a proxima
/// quando para (done/idle/blocked e avisado no painel).
pub fn fleet(args: &[String]) -> i32 {
    let a = parse_args("fleet", args);
    let file = args.iter().rev().find(|x| !x.starts_with("--") && std::path::Path::new(x).is_file());
    let Some(file) = file else { eprintln!("uso: duck fleet [--agents 3] [--cmd claude] [--cwd D] tarefas.txt"); return 2 };
    let tasks: Vec<String> = std::fs::read_to_string(file).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    let n = a["agents"].as_u64().unwrap_or(2).max(1) as usize;
    let cmd = a["argv"].as_str().unwrap_or("claude").to_string();
    let mut c = match Conn::connect_or_spawn() { Ok(c) => c, Err(e) => { eprintln!("{e}"); return 1 } };
    let mut panes = Vec::new();
    for i in 0..n.min(tasks.len()) {
        let mut req = json!({"cmd": "spawn", "where": if i == 0 { "tab" } else { "right" }, "workspace": "fleet", "argv": cmd, "focus": false, "title": "fleet"});
        if i > 0 { req["pane"] = json!(panes[0]); req.as_object_mut().unwrap().remove("workspace"); }
        if let Some(cwd) = a.get("cwd") { req["cwd"] = cwd.clone(); }
        match c.call(req) { Ok(v) => panes.push(v["pane"].as_u64().unwrap_or(0)), Err(e) => { eprintln!("{e}"); return 1 } }
    }
    println!("fleet: {} agentes ({cmd}) para {} tarefas no workspace 'fleet'", panes.len(), tasks.len());
    std::thread::sleep(std::time::Duration::from_secs(6)); // agente subindo
    let queue = std::sync::Arc::new(std::sync::Mutex::new(tasks.into_iter().enumerate().collect::<std::collections::VecDeque<_>>()));
    let handles: Vec<_> = panes.into_iter().map(|pane| {
        let q = queue.clone();
        std::thread::spawn(move || {
            let Ok(mut c) = Conn::connect() else { return };
            loop {
                let Some((i, task)) = q.lock().unwrap().pop_front() else { return };
                println!("[pane {pane}] tarefa {}: {task}", i + 1);
                let _ = c.call(json!({"cmd": "send", "pane": pane, "text": task}));
                std::thread::sleep(std::time::Duration::from_millis(300));
                let _ = c.call(json!({"cmd": "send", "pane": pane, "text": "\r"}));
                std::thread::sleep(std::time::Duration::from_secs(3));
                match c.call(json!({"cmd": "wait", "pane": pane, "state": "done,idle,blocked", "timeout": 3600})) {
                    Ok(v) => println!("[pane {pane}] tarefa {} -> {}", i + 1, v["state"]),
                    Err(e) => { println!("[pane {pane}] {e}"); return; }
                }
                if c.call(json!({"cmd": "list"})).map(|v| v.to_string().contains("\"blocked\"")).unwrap_or(false) {
                    println!("[pane {pane}] agente bloqueado: responda no painel (Alt+A) para seguir");
                    let _ = c.call(json!({"cmd": "wait", "pane": pane, "state": "done,idle", "timeout": 3600}));
                }
            }
        })
    }).collect();
    for h in handles { let _ = h.join(); }
    println!("fleet: todas as tarefas entregues");
    0
}

/// `duck mcp`: servidor MCP (stdio) para agentes controlarem o terminal
/// sem a CLI: listar, abrir pane, mandar texto, ler, esperar.
pub fn mcp() -> i32 {
    use std::io::BufRead;
    let tools = json!([
        {"name": "duck_list", "description": "Lista workspaces, abas, panes e o estado dos agentes do Duck.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "duck_spawn", "description": "Abre um pane novo (tab, right, down) rodando um comando; devolve o id.",
         "inputSchema": {"type": "object", "properties": {"cmd": {"type": "string"}, "where": {"type": "string", "enum": ["tab", "right", "down"]}, "cwd": {"type": "string"}, "domain": {"type": "string"}}}},
        {"name": "duck_send", "description": "Digita texto num pane (enter=true aperta Enter).",
         "inputSchema": {"type": "object", "properties": {"pane": {"type": "integer"}, "text": {"type": "string"}, "enter": {"type": "boolean"}}, "required": ["pane", "text"]}},
        {"name": "duck_read", "description": "Le as ultimas linhas da tela de um pane.",
         "inputSchema": {"type": "object", "properties": {"pane": {"type": "integer"}, "lines": {"type": "integer"}}, "required": ["pane"]}},
        {"name": "duck_wait", "description": "Espera o agente de um pane chegar a um estado (blocked,done,idle).",
         "inputSchema": {"type": "object", "properties": {"pane": {"type": "integer"}, "state": {"type": "string"}, "timeout": {"type": "number"}}, "required": ["pane"]}},
        {"name": "duck_last_output", "description": "Saida e exit code do ultimo comando terminado num pane (shell integration).",
         "inputSchema": {"type": "object", "properties": {"pane": {"type": "integer"}}, "required": ["pane"]}}
    ]);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req["id"].clone();
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({"protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}}, "serverInfo": {"name": "duck", "version": env!("CARGO_PKG_VERSION")}}),
            "tools/list" => json!({"tools": tools}),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let mut args = req["params"]["arguments"].clone();
                if !args.is_object() { args = json!({}); }
                let cmd = match name {
                    "duck_list" => "list", "duck_spawn" => "spawn", "duck_send" => "send", "duck_read" => "read",
                    "duck_wait" => "wait", "duck_last_output" => "last-output", _ => "",
                };
                if let Some(c) = args.get("cmd").cloned() { args["argv"] = c; }
                args["cmd"] = json!(cmd);
                if cmd == "spawn" && args.get("where").is_none() { args["where"] = json!("right"); }
                if args.get("pane").is_none() && cmd == "spawn" { if let Some(p) = crate::proto::env("PANE") { args["pane"] = json!(p.parse::<u64>().unwrap_or(0)); } }
                let r = Conn::connect_or_spawn().map_err(|e| e.to_string()).and_then(|mut c| c.call(args));
                match r {
                    Ok(v) => json!({"content": [{"type": "text", "text": v.get("text").and_then(|t| t.as_str()).map(String::from).unwrap_or_else(|| v.to_string())}]}),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                }
            }
            m if m.starts_with("notifications/") => continue,
            "ping" => json!({}),
            _ => { let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "metodo desconhecido"}})); continue; }
        };
        let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let _ = out.flush();
    }
    0
}

/// `duck view <arquivo>`: mostra texto/markdown com cor (o "widget" de
/// arquivo: abra num flutuante com Alt+F e `duck view README.md`).
pub fn view(args: &[String]) -> i32 {
    let Some(path) = args.first() else { eprintln!("uso: duck view <arquivo|pasta>"); return 2 };
    let p = std::path::Path::new(path);
    if p.is_dir() {
        let mut v: Vec<_> = std::fs::read_dir(p).into_iter().flatten().flatten().collect();
        v.sort_by_key(|e| (!e.path().is_dir(), e.file_name()));
        for e in v {
            let dir = e.path().is_dir();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            println!("{}{}\x1b[0m{}", if dir { "\x1b[1;34m" } else { "" }, e.file_name().to_string_lossy(), if dir { "/".into() } else { format!("  \x1b[2m{size} B\x1b[0m") });
        }
        return 0;
    }
    let Ok(text) = std::fs::read_to_string(p) else { eprintln!("nao consegui ler {path} (binario?)"); return 1 };
    let md = path.to_lowercase().ends_with(".md");
    let mut code = false;
    for (i, line) in text.lines().enumerate() {
        if md {
            if line.trim_start().starts_with("```") { code = !code; println!("\x1b[2m{line}\x1b[0m"); continue; }
            if code { println!("\x1b[36m{line}\x1b[0m"); }
            else if line.starts_with('#') { println!("\x1b[1;35m{line}\x1b[0m"); }
            else if line.trim_start().starts_with(['-', '*']) || line.trim_start().starts_with("1.") { println!("\x1b[33m{}\x1b[0m{}", &line[..line.len() - line.trim_start().len() + 1], &line[line.len() - line.trim_start().len() + 1..]); }
            else { println!("{line}"); }
        } else {
            println!("\x1b[2m{:>5}\x1b[0m  {line}", i + 1);
        }
    }
    0
}


/// Progresso do plano (feitas, total) e tokens gastos na sessao, lendo o
/// transcript do Claude Code. Entende TaskCreate/TaskUpdate (atual) e
/// TodoWrite (antigo).
pub fn transcript_stats(path: &str) -> Option<(u32, u32, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut tasks: Vec<(String, String)> = Vec::new(); // (id, status)
    let mut todo: Option<(u32, u32)> = None;
    let mut tokens: u64 = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let u = &v["message"]["usage"];
        if u.is_object() {
            tokens += ["input_tokens", "output_tokens", "cache_creation_input_tokens"].iter().map(|k| u[k].as_u64().unwrap_or(0)).sum::<u64>();
        }
        let Some(blocks) = v["message"]["content"].as_array() else { continue };
        for b in blocks {
            if b["type"] != "tool_use" { continue; }
            let input = &b["input"];
            match b["name"].as_str().unwrap_or("") {
                "TaskCreate" => tasks.push(((tasks.len() + 1).to_string(), "pending".into())),
                "TaskUpdate" => {
                    let id = input["taskId"].as_str().map(String::from).or_else(|| input["taskId"].as_u64().map(|n| n.to_string())).unwrap_or_default();
                    if let Some(st) = input["status"].as_str() {
                        if let Some(t) = tasks.iter_mut().find(|t| t.0 == id) { t.1 = st.into(); }
                    }
                }
                "TodoWrite" => if let Some(list) = input["todos"].as_array() {
                    let done = list.iter().filter(|t| t["status"] == "completed").count() as u32;
                    todo = Some((done, list.len() as u32));
                },
                _ => {}
            }
        }
    }
    tasks.retain(|t| t.1 != "deleted");
    let (done, total) = if !tasks.is_empty() {
        (tasks.iter().filter(|t| t.1 == "completed").count() as u32, tasks.len() as u32)
    } else { todo.unwrap_or((0, 0)) };
    Some((done, total, tokens))
}

/// Plano do agente para o painel: (status, titulo) de cada tarefa.
pub fn transcript_plan(path: &str) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else { return vec![] };
    let mut tasks: Vec<(String, String, String)> = Vec::new(); // (id, status, titulo)
    let mut todo: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if !line.contains("\"tool_use\"") { continue; }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(blocks) = v["message"]["content"].as_array() else { continue };
        for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
            let input = &b["input"];
            match b["name"].as_str().unwrap_or("") {
                "TaskCreate" => tasks.push(((tasks.len() + 1).to_string(), "pending".into(), input["subject"].as_str().unwrap_or("").to_string())),
                "TaskUpdate" => {
                    let id = input["taskId"].as_str().map(String::from).or_else(|| input["taskId"].as_u64().map(|n| n.to_string())).unwrap_or_default();
                    if let Some(t) = tasks.iter_mut().find(|t| t.0 == id) {
                        if let Some(st) = input["status"].as_str() { t.1 = st.into(); }
                        if let Some(sub) = input["subject"].as_str() { t.2 = sub.into(); }
                    }
                }
                "TodoWrite" => if let Some(list) = input["todos"].as_array() {
                    todo = list.iter().map(|t| (t["status"].as_str().unwrap_or("").to_string(), t["content"].as_str().unwrap_or("").to_string())).collect();
                },
                _ => {}
            }
        }
    }
    tasks.retain(|t| t.1 != "deleted");
    if tasks.is_empty() { todo } else { tasks.into_iter().map(|t| (t.1, t.2)).collect() }
}

/// `duck url duck://approve/12`: botoes dos toasts do Windows (protocolo duck:).
pub fn url(args: &[String]) -> i32 {
    let u = args.first().map(|s| s.trim_end_matches('/').to_string()).unwrap_or_default();
    let rest = u.trim_start_matches("duck://").trim_start_matches("duck:").trim_start_matches("lasso://").trim_start_matches("lasso:");
    let (what, id) = rest.split_once('/').unwrap_or((rest, "0"));
    let Ok(pane) = id.parse::<u64>() else { return 2 };
    let Ok(mut c) = Conn::connect() else { return 1 };
    match what {
        "approve" => { let _ = c.call(json!({"cmd": "send", "pane": pane, "text": "1"})); }
        "deny" => { let _ = c.call(json!({"cmd": "send", "pane": pane, "text": "\x1b"})); }
        _ => {
            let _ = c.call(json!({"cmd": "focus", "pane": pane}));
            let _ = c.call(json!({"cmd": "raise", "pane": pane}));
        }
    }
    0
}

/// `duck play <arquivo.cast>`: reproduz uma gravacao com o tempo original
/// (Ctrl+C para). `--speed 2` acelera.
pub fn play(args: &[String]) -> i32 {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else { eprintln!("uso: duck play <arquivo.cast> [--speed 2]"); return 2 };
    let speed = parse_args("play", args)["speed"].as_f64().unwrap_or(1.0).max(0.1);
    let Ok(text) = std::fs::read_to_string(path) else { eprintln!("nao li {path}"); return 1 };
    let mut out = std::io::stdout();
    let mut last = 0.0;
    for line in text.lines().skip(1) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let t = v[0].as_f64().unwrap_or(last);
        let wait = ((t - last) / speed).clamp(0.0, 2.0);
        std::thread::sleep(std::time::Duration::from_secs_f64(wait));
        last = t;
        if v[1] == "o" { let _ = out.write_all(v[2].as_str().unwrap_or("").as_bytes()); let _ = out.flush(); }
    }
    0
}

#[cfg(test)]
mod stats_tests {
    #[test]
    fn counts_tasks_and_tokens() {
        let path = std::env::temp_dir().join(format!("duck-transcript-{}.jsonl", std::process::id()));
        let lines = [
            r#"{"message":{"usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"tool_use","name":"TaskCreate","input":{"subject":"a"}}]}}"#,
            r#"{"message":{"content":[{"type":"tool_use","name":"TaskCreate","input":{"subject":"b"}}]}}"#,
            r#"{"message":{"content":[{"type":"tool_use","name":"TaskUpdate","input":{"taskId":"1","status":"completed"}}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(super::transcript_stats(path.to_str().unwrap()), Some((1, 2, 15)));
        let _ = std::fs::remove_file(path);
    }
}
