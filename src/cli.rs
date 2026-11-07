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
    // Rodando no WSL (duck.exe pelo interop): de qual distro.
    // O perfil so vale com caminho do Linux (o do Windows vem herdado do pwsh).
    if let Ok(d) = std::env::var("WSL_DISTRO_NAME") {
        req["distro"] = json!(d);
        if !req["config_dir"].as_str().unwrap_or("").starts_with('/') { req["config_dir"] = json!(""); }
    }
    if name == "UserPromptSubmit" {
        if let Some(p) = ev["prompt"].as_str() { req["prompt"] = json!(p.chars().take(400).collect::<String>()); }
    }
    if let Some(path) = ev["transcript_path"].as_str() {
        if let Some(t) = context_tokens(path) { req["context_tokens"] = json!(t); }
        // Passo do plano em andamento (previa da aba).
        if matches!(ev["tool_name"].as_str().unwrap_or(""), "TaskCreate" | "TaskUpdate" | "TodoWrite") {
            let plan = transcript_plan(path);
            req["step"] = plan.iter().find(|p| p.0 == "in_progress").map(|p| json!(p.1)).unwrap_or(Value::Null);
        }
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
    let exe = crate::proto::current_exe().unwrap();
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
    let script = crate::proto::data_dir().join("shell").join("duck.sh");
    if cfg!(windows) {
        let wsl = std::process::Command::new("wsl.exe").args(["-l", "-q"]).output()
            .map(|o| String::from_utf8_lossy(&o.stdout).replace('\0', "").lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        println!("{}WSL: {}", ok(!wsl.is_empty()), if wsl.is_empty() { "nenhuma distro" } else { &wsl });
        println!("    shell integration bash/zsh: no ~/.bashrc do WSL ponha  . {}", to_wsl_path(&script.to_string_lossy()));
    } else {
        // Linux/macOS: o duck.sh precisa ser carregado pelo rc do shell.
        let home = crate::config::home();
        let has = [".bashrc", ".zshrc"].iter()
            .any(|f| std::fs::read_to_string(home.join(f)).unwrap_or_default().contains("duck/shell/duck.sh"));
        println!("{}shell integration bash/zsh{}", ok(has),
            if has { String::new() } else { format!("  (no ~/.bashrc ou ~/.zshrc ponha  . {})", script.display()) });
    }
    let (cfg, _) = crate::config::load_config();
    let here = std::env::current_dir().unwrap_or_default();
    let repo = std::process::Command::new("git").arg("-C").arg(&here).args(["rev-parse", "--is-inside-work-tree"]).output().is_ok_and(|o| o.status.success());
    if repo {
        let on = crate::guard::installed(&here);
        println!("{}guarda de commit/push neste repo{}", ok(on), if on { format!(" ({} palavra(s) proibida(s))", cfg.guard_words.len()) } else { "  (duck guard install)".into() });
    }
    println!("{}mascara de segredos na tela{}", ok(cfg.secret_mask), if cfg.secret_mask { "" } else { "  (secret_mask = false na config)" });
    doctor_extensions();
    0
}

/// Extensoes: Node no PATH, quais estao ligadas, compiladas e com os tokens
/// das contas configuradas (segredo que falta = a extensao fica sem dados).
fn doctor_extensions() {
    let ok = |b: bool| if b { "ok " } else { "!! " };
    let node = std::process::Command::new(if cfg!(windows) { "node.exe" } else { "node" }).arg("--version").output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let settings = crate::ext::settings();
    let all: Vec<crate::ext::ExtInfo> = crate::ext::catalog().into_iter().filter(|i| !i.manifest["builtin"].as_bool().unwrap_or(false)).collect();
    let on: Vec<&crate::ext::ExtInfo> = all.iter().filter(|i| crate::ext::enabled(i, &settings)).collect();
    println!("extensoes: {} de {} ligadas (Ctrl+Shift+P > Extensoes liga, configura e pede os tokens)", on.len(), all.len());
    if !on.is_empty() {
        println!("{}node {}", ok(node.is_some()), node.as_deref().unwrap_or("nao encontrado: winget install OpenJS.NodeJS.LTS"));
    }
    for i in on {
        let mut problems: Vec<String> = Vec::new();
        if !i.built { problems.push("nao compilada (install.ps1 -SkipBuild -Extensions)".into()); }
        for name in missing_secrets(i, &settings[&i.name]) {
            problems.push(format!("falta o token {name} (echo <token> | duck secret set {name})"));
        }
        if problems.is_empty() { println!("ok  {}", i.name); } else { println!("!!  {}: {}", i.name, problems.join("; ")); }
    }
}

/// Segredos esperados pela config da extensao e que nao estao gravados.
/// Contas do manifesto ("accounts": {key, secret: "x/{name}"}) ou, sem isso,
/// listas de tabelas com `name` (ex.: azure orgs): `secret` da tabela ou "<ext>/<name>".
fn missing_secrets(i: &crate::ext::ExtInfo, cfg: &Value) -> Vec<String> {
    let mut want: Vec<String> = Vec::new();
    let acc = &i.manifest["accounts"];
    if let (Some(key), Some(pat)) = (acc["key"].as_str(), acc["secret"].as_str()) {
        for a in cfg[key].as_array().into_iter().flatten() {
            let mut name = pat.to_string();
            for (k, v) in a.as_object().into_iter().flatten() {
                if let Some(v) = v.as_str() { name = name.replace(&format!("{{{k}}}"), v); }
            }
            if !name.contains('{') { want.push(name); }
        }
    } else if acc.is_null() {
        for v in cfg.as_object().into_iter().flatten().filter_map(|(_, v)| v.as_array()) {
            for a in v.iter().filter(|a| a["name"].is_string() && a["url"].is_string()) {
                want.push(a["secret"].as_str().map(String::from).unwrap_or_else(|| format!("{}/{}", i.name, a["name"].as_str().unwrap_or(""))));
            }
        }
    }
    want.into_iter().filter(|n| crate::secret::get(n).is_none()).collect()
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
/// Tarefa do `duck fleet`. Linha do arquivo:
///   [id] after:a,b check:"cargo test"  texto da tarefa
/// Tudo opcional menos o texto: sem [id], o id e o numero da linha; `after`
/// espera essas tarefas passarem; `check` poe o agente em loop ate o comando
/// passar (sem check, passou = o agente parou).
#[derive(Clone, Debug, PartialEq)]
pub struct FleetTask { pub id: String, pub text: String, pub after: Vec<String>, pub check: Option<String>, pub status: String, pub pane: u64 }

pub fn parse_fleet_line(line: &str, n: usize) -> FleetTask {
    let mut rest = line.trim();
    let mut id = n.to_string();
    if let Some(r) = rest.strip_prefix('[') {
        if let Some((i, r2)) = r.split_once(']') { id = i.trim().to_string(); rest = r2; }
    }
    let (mut after, mut check) = (Vec::new(), None);
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("after:") {
            let (tok, r2) = r.split_once(char::is_whitespace).unwrap_or((r, ""));
            after = tok.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            rest = r2;
        } else if let Some(r) = rest.strip_prefix("check:") {
            let (val, r2) = match r.chars().next() {
                Some(q @ ('"' | '\'')) => match r[1..].split_once(q) { Some((v, r2)) => (v.to_string(), r2), None => (r[1..].to_string(), "") },
                _ => { let (v, r2) = r.split_once(char::is_whitespace).unwrap_or((r, "")); (v.to_string(), r2) }
            };
            check = Some(val).filter(|v| !v.trim().is_empty());
            rest = r2;
        } else { break; }
    }
    FleetTask { id, text: rest.trim().to_string(), after, check, status: "pendente".into(), pane: 0 }
}

/// Proxima tarefa pronta (dependencias passaram). Tarefa que depende de uma
/// que falhou (ou de um ciclo) e pulada. None + false = acabou tudo.
fn fleet_next(tasks: &mut [FleetTask]) -> (Option<usize>, bool) {
    loop {
        let st = |id: &str, t: &[FleetTask]| t.iter().find(|x| x.id == id).map(|x| x.status.clone()).unwrap_or_default();
        let mut changed = false;
        for i in 0..tasks.len() {
            if tasks[i].status != "pendente" { continue; }
            if tasks[i].after.iter().any(|d| matches!(st(d, tasks).as_str(), "falhou" | "pulada")) {
                tasks[i].status = "pulada".into();
                changed = true;
            }
        }
        if let Some(i) = tasks.iter().position(|t| t.status == "pendente" && t.after.iter().all(|d| st(d, tasks) == "passou")) {
            return (Some(i), true);
        }
        let pending = tasks.iter().any(|t| t.status == "pendente");
        let running = tasks.iter().any(|t| t.status == "rodando");
        if pending && !running && !changed {
            // Ninguem rodando e nada pronto: dependencia circular.
            for t in tasks.iter_mut().filter(|t| t.status == "pendente") { t.status = "pulada".into(); }
            continue;
        }
        if !changed { return (None, pending || running); }
    }
}

pub fn fleet(args: &[String]) -> i32 {
    let a = parse_args("fleet", args);
    let file = args.iter().rev().find(|x| !x.starts_with("--") && std::path::Path::new(x).is_file());
    let Some(file) = file else { eprintln!("uso: duck fleet [--agents 3] [--cmd claude] [--cwd D] tarefas.txt\n  linha: [id] after:a,b check:\"cargo test\"  texto da tarefa"); return 2 };
    let tasks: Vec<FleetTask> = std::fs::read_to_string(file).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'))
        .enumerate().map(|(i, l)| parse_fleet_line(l, i + 1)).collect();
    for t in &tasks {
        if let Some(d) = t.after.iter().find(|d| !tasks.iter().any(|x| &x.id == *d)) { eprintln!("tarefa [{}] depende de [{d}], que nao existe", t.id); return 2; }
        if t.text.is_empty() { eprintln!("tarefa [{}] sem texto", t.id); return 2; }
    }
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
    let deps = tasks.iter().filter(|t| !t.after.is_empty()).count();
    println!("fleet: {} agentes ({cmd}) para {} tarefas ({deps} com dependencia) no workspace 'fleet'", panes.len(), tasks.len());
    std::thread::sleep(std::time::Duration::from_secs(6)); // agente subindo
    let state = std::sync::Arc::new((std::sync::Mutex::new(tasks), std::sync::Condvar::new()));
    let handles: Vec<_> = panes.into_iter().map(|pane| {
        let st = state.clone();
        std::thread::spawn(move || {
            let Ok(mut c) = Conn::connect() else { return };
            loop {
                let task = {
                    let (lock, cv) = &*st;
                    let mut ts = lock.lock().unwrap();
                    loop {
                        match fleet_next(&mut ts) {
                            (Some(i), _) => { ts[i].status = "rodando".into(); ts[i].pane = pane; break ts[i].clone(); }
                            (None, false) => { cv.notify_all(); return; }
                            (None, true) => { ts = cv.wait_timeout(ts, std::time::Duration::from_secs(2)).unwrap().0; }
                        }
                    }
                };
                println!("[pane {pane}] [{}] {}", task.id, task.text);
                let ok = match &task.check {
                    Some(check) => fleet_loop(&mut c, pane, check, &task.text),
                    None => fleet_plain(&mut c, pane, &task.text),
                };
                println!("[pane {pane}] [{}] {}", task.id, if ok { "passou" } else { "falhou" });
                let (lock, cv) = &*st;
                if let Some(t) = lock.lock().unwrap().iter_mut().find(|t| t.id == task.id) { t.status = if ok { "passou" } else { "falhou" }.into(); }
                cv.notify_all();
            }
        })
    }).collect();
    for h in handles { let _ = h.join(); }
    let ts = state.0.lock().unwrap();
    println!("fleet: fim");
    for t in ts.iter() { println!("  [{}] {:<8} {}", t.id, t.status, t.text.chars().take(70).collect::<String>()); }
    if ts.iter().all(|t| t.status == "passou") { 0 } else { 1 }
}

/// Tarefa sem verificacao: manda e espera o agente parar.
fn fleet_plain(c: &mut Conn, pane: u64, text: &str) -> bool {
    let _ = c.call(json!({"cmd": "send", "pane": pane, "text": text}));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = c.call(json!({"cmd": "send", "pane": pane, "text": "\r"}));
    std::thread::sleep(std::time::Duration::from_secs(3));
    loop {
        match c.call(json!({"cmd": "wait", "pane": pane, "state": "done,idle,blocked", "timeout": 3600})) {
            Ok(v) if v["state"] == "blocked" => {
                println!("[pane {pane}] agente pede aprovacao: responda (Ctrl+Alt+A vai ate ele)");
                let _ = c.call(json!({"cmd": "wait", "pane": pane, "state": "done,idle", "timeout": 3600}));
                return true;
            }
            Ok(_) => return true,
            Err(e) => { println!("[pane {pane}] {e}"); return false; }
        }
    }
}

/// Tarefa com verificacao: loop do Duck ate o comando passar (ou acabar as voltas).
fn fleet_loop(c: &mut Conn, pane: u64, check: &str, text: &str) -> bool {
    let t0 = std::time::Instant::now();
    // O agente pode ainda nao ter sido detectado: tenta por ate 1 min.
    while let Err(e) = c.call(json!({"cmd": "loop-start", "pane": pane, "check": check, "max": 5, "task": text})) {
        if t0.elapsed() > std::time::Duration::from_secs(60) { println!("[pane {pane}] loop nao comecou: {e}"); return false; }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let Ok(v) = c.call(json!({"cmd": "loops"})) else { return false };
        let phase = v["loops"].as_array().into_iter().flatten().find(|l| l["pane"].as_u64() == Some(pane)).and_then(|l| l["phase"].as_str().map(String::from)).unwrap_or_default();
        match phase.as_str() {
            "passed" => return true,
            "failed" | "stopped" | "" => return false,
            _ => {}
        }
    }
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

/// `duck secret set <nome>` (valor pelo stdin, nunca na linha de comando),
/// `duck secret has <nome>`, `duck secret delete <nome>`.
pub fn secret(args: &[String]) -> i32 {
    let (Some(op), Some(name)) = (args.first(), args.get(1)) else {
        eprintln!("uso: duck secret set|has|delete <nome>   (set le o valor do stdin)");
        return 2;
    };
    match op.as_str() {
        "set" => {
            let mut v = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut v);
            let v = v.trim_end_matches(['\r', '\n']).to_string();
            if v.is_empty() { eprintln!("valor vazio no stdin"); return 1; }
            match crate::secret::set(name, &v) {
                Ok(_) => { println!("gravado: {}", crate::secret::target(name)); 0 }
                Err(e) => { eprintln!("{e}"); 1 }
            }
        }
        "has" => { let ok = crate::secret::get(name).is_some(); println!("{}", if ok { "sim" } else { "nao" }); if ok { 0 } else { 1 } }
        "delete" => if crate::secret::delete(name) { println!("apagado"); 0 } else { eprintln!("nao existe"); 1 },
        _ => { eprintln!("uso: duck secret set|has|delete <nome>"); 2 }
    }
}

/// `duck url duck://approve/12`: botoes dos toasts do Windows (protocolo duck:).
pub fn url(args: &[String]) -> i32 {
    let u = args.first().map(|s| s.trim_end_matches('/').to_string()).unwrap_or_default();
    let rest = u.trim_start_matches("duck://").trim_start_matches("duck:").trim_start_matches("lasso://").trim_start_matches("lasso:");
    // Botao de toast de extensao: duck://ext/<extensao>/<acao>/<item>
    if let Some(e) = rest.strip_prefix("ext/") {
        let mut it = e.splitn(3, '/');
        let dec = |s: &str| {
            let b = s.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 { if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; } }
                out.push(b[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).to_string()
        };
        let (ext, act, item) = (it.next().unwrap_or(""), dec(it.next().unwrap_or("")), dec(it.next().unwrap_or("")));
        let Ok(mut c) = Conn::connect() else { return 1 };
        // Direto para a extensao no server: funciona com a janela fechada.
        let _ = c.call(json!({"cmd": "ext-send", "ext": ext, "msg": {"type": "action", "action": act, "item": item, "view": ""}}));
        let _ = c.call(json!({"cmd": "raise", "pane": 0}));
        return 0;
    }
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
    use super::*;

    #[test]
    fn fleet_lines_and_order() {
        let t = parse_fleet_line("[api] check:\"cargo test --lib\" Criar o endpoint", 1);
        assert_eq!((t.id.as_str(), t.check.as_deref(), t.text.as_str()), ("api", Some("cargo test --lib"), "Criar o endpoint"));
        let t2 = parse_fleet_line("[tela] after:api,docs  Ligar a tela", 2);
        assert_eq!(t2.after, vec!["api", "docs"]);
        assert_eq!(parse_fleet_line("so o texto", 7).id, "7");
        // b depende de a; c depende de b; a falha -> b e c pulam.
        let mut ts = vec![parse_fleet_line("[a] x", 1), parse_fleet_line("[b] after:a y", 2), parse_fleet_line("[c] after:b z", 3), parse_fleet_line("[d] w", 4)];
        assert_eq!(fleet_next(&mut ts).0, Some(0));
        ts[0].status = "rodando".into();
        assert_eq!(fleet_next(&mut ts).0, Some(3));
        ts[3].status = "rodando".into();
        assert_eq!(fleet_next(&mut ts), (None, true));
        ts[0].status = "falhou".into();
        ts[3].status = "passou".into();
        assert_eq!(fleet_next(&mut ts), (None, false));
        assert_eq!(ts[1].status, "pulada");
        assert_eq!(ts[2].status, "pulada");
        // Ciclo: ninguem fica esperando para sempre.
        let mut cy = vec![parse_fleet_line("[a] after:b x", 1), parse_fleet_line("[b] after:a y", 2)];
        assert_eq!(fleet_next(&mut cy), (None, false));
        assert!(cy.iter().all(|t| t.status == "pulada"));
    }

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

/// `duck msg <peer> [--to SESSAO] [--conv ID] "texto"` (ou texto no stdin): manda para o
/// agente de outro computador. O server prepara; a rede sai daqui.
pub fn msg(args: &[String]) -> i32 {
    let (mut conv, mut to) = (None, None);
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--conv" { conv = it.next().cloned(); } else if a == "--to" { to = it.next().cloned(); } else { rest.push(a.clone()); }
    }
    let Some(peer) = rest.first().cloned() else { eprintln!("uso: duck msg <peer> [--to SESSAO] [--conv ID] \"texto\"   (sem texto: le do stdin)"); return 2 };
    let mut text = rest[1..].join(" ");
    if text.trim().is_empty() { let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text); }
    let mut c = match Conn::connect_or_spawn() { Ok(c) => c, Err(e) => { eprintln!("duck: {e}"); return 1 } };
    let out = match c.call(json!({"cmd": "peer-out", "peer": peer, "text": text, "conv": conv})) {
        Ok(v) => v, Err(e) => { eprintln!("duck msg: {e}"); return 1 }
    };
    let Some(token) = crate::secret::get(&format!("peer/{peer}")) else {
        eprintln!("duck msg: sem o token de {peer}. No {peer} rode `duck peer token` e aqui `duck peer add {peer} <token>`");
        return 1;
    };
    let addr = out["addr"].as_str().unwrap_or("").to_string();
    let req = json!({"token": token, "from": out["from"], "conv": out["conv"], "hops": out["hops"], "to": out["to"], "reply_pane": out["reply_pane"], "to_name": to, "text": out["text"]});
    match crate::peer::send(&addr, &req) {
        Ok(v) if v["ok"].as_bool() == Some(true) => {
            let how = if v["approval"].as_bool() == Some(true) { "chegou, espera o usuario de la aprovar" } else { "entregue" };
            println!("{peer}: {how} (conversa {}). A resposta chega neste pane.", out["conv"].as_str().unwrap_or(""));
            0
        }
        Ok(v) => { eprintln!("duck msg: {peer} recusou: {}", v["error"].as_str().unwrap_or("?")); 1 }
        Err(e) => { eprintln!("duck msg: {peer} ({addr}) nao respondeu: {e}"); 1 }
    }
}

/// `duck peer ...`: token, codigo de pareamento, conectar, procurar, listar e
/// remover. O mesmo que o painel Computadores (Ctrl+Shift+P).
pub fn peer(args: &[String]) -> i32 {
    let cfg = crate::config::load_config().0;
    let me = cfg.peer_name.clone();
    let mine = || crate::peer::advertised(&cfg.peer_listen);
    let off = "este computador nao escuta. Ligue no painel Computadores (Ctrl+Shift+P) ou ponha peer = { listen = 'ip:7878' } no ~/.duck-local.lua";
    match (args.first().map(String::as_str), args.get(1), args.get(2)) {
        (Some("token"), _, _) => { println!("{}", crate::peer::self_token()); 0 }
        (Some("add"), Some(name), Some(token)) => match crate::secret::set(&format!("peer/{name}"), token.trim()) {
            Ok(_) => { println!("ok. Na config: peers = {{ {name} = 'ip:porta' }}"); 0 }
            Err(e) => { eprintln!("{e}"); 1 }
        },
        (Some("code"), _, _) => match mine() {
            Some(addr) => { println!("{}", crate::peer::code(&me, &addr, &crate::peer::self_token())); 0 }
            None => { eprintln!("duck peer code: {off}"); 1 }
        },
        (Some("join"), Some(code), _) => {
            let c = match crate::peer::parse_code(code) { Ok(c) => c, Err(e) => { eprintln!("duck peer join: {e}"); return 2 } };
            if c.name == me { eprintln!("duck peer join: esse e o codigo deste computador ({me}). Cole o do outro."); return 1; }
            let Some(my_addr) = mine() else { eprintln!("duck peer join: {off}"); return 1 };
            if let Err(e) = crate::peer::save(&c.name, &c.addr, &c.token) { eprintln!("duck peer join: {e}"); return 1; }
            match crate::peer::pair_request(&me, &my_addr, &c.addr, Some(&c.token)) {
                Ok(_) => { println!("conectado com {} ({}). Teste: duck msg {} \"oi\"", c.name, c.addr, c.name); 0 }
                Err(e) => { eprintln!("duck peer join: {} ({}) nao aceitou: {e}", c.name, c.addr); 1 }
            }
        }
        (Some("find"), _, _) => {
            let port = crate::peer::port_of(&cfg.peer_listen);
            let mut ports = vec![crate::peer::DEFAULT_PORT];
            if port != ports[0] { ports.push(port); }
            let found = crate::peer::discover(&ports, std::time::Duration::from_millis(1500));
            let mine = mine().unwrap_or_default();
            let found: Vec<_> = found.into_iter().filter(|f| f["addr"] != mine.as_str()).collect();
            if found.is_empty() { println!("nenhum Duck respondeu na rede local (o outro precisa estar escutando, e no Windows com a porta UDP liberada)"); }
            for f in &found { println!("{}  {}  {}", f["name"].as_str().unwrap_or(""), f["addr"].as_str().unwrap_or(""), f["os"].as_str().unwrap_or("")); }
            0
        }
        (Some("connect"), Some(addr), _) => {
            let Some(my_addr) = mine() else { eprintln!("duck peer connect: {off}"); return 1 };
            let addr = if addr.contains(':') { addr.clone() } else { format!("{addr}:{}", crate::peer::DEFAULT_PORT) };
            match crate::peer::pair_request(&me, &my_addr, &addr, None) {
                Ok(v) => {
                    let n = v["name"].as_str().unwrap_or("?");
                    println!("pedido enviado para {n} ({addr}). Numero de confirmacao: {}", crate::peer::confirm_code(&crate::peer::self_token()));
                    println!("Aceite em {n} so se aparecer o mesmo numero. Depois ele aparece em duck peer list.");
                    0
                }
                Err(e) => { eprintln!("duck peer connect: {addr}: {e}"); 1 }
            }
        }
        (Some("list"), _, _) => {
            match mine() { Some(a) => println!("este computador: {me} em {a}"), None => println!("este computador: {me} (nao escuta)") }
            for (name, addr) in &cfg.peers {
                let tok = if crate::secret::get(&format!("peer/{name}")).is_some() { "" } else { "  (sem token)" };
                match crate::peer::ping(addr) {
                    Ok(ms) => println!("  online  {name}  {addr}  {ms} ms{tok}"),
                    Err(e) => println!("  offline {name}  {addr}  {e}{tok}"),
                }
            }
            0
        }
        (Some("remove"), Some(name), _) => match crate::peer::remove(name) {
            Ok(_) => { println!("{name} removido"); 0 }
            Err(e) => { eprintln!("{e}"); 1 }
        },
        _ => {
            eprintln!("uso: duck peer code | join <codigo> | find | connect <ip[:porta]> | list | remove <nome> | token | add <nome> <token>");
            2
        }
    }
}
