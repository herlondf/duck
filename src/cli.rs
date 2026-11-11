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
        // "Claude is waiting for your input" (idle_prompt) tambem e Notification,
        // mas ele esta parado, nao pedindo nada: como blocked, os recados da
        // fila nunca eram entregues.
        "Notification" if notification_idle(&ev) => "idle",
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

/// Notification de "parado esperando voce" (nao de permissao).
fn notification_idle(ev: &Value) -> bool {
    match ev["notification_type"].as_str() {
        Some(t) => t == "idle_prompt",
        None => ev["message"].as_str().is_some_and(|m| m.to_lowercase().contains("waiting for your input")),
    }
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

/// Eventos do Claude Code em que o Duck registra `duck hook`.
const HOOK_EVENTS: [&str; 7] = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Notification", "Stop", "SessionEnd"];

/// Entrada do hook do Duck. No Windows o Claude Code roda `command` pelo Git
/// Bash; sem ele cai no PowerShell, onde `"C:/.../duck.exe" hook` e erro de
/// sintaxe e o hook que falha trava as ferramentas (PreToolUse). Por isso
/// la vai programa em `command` e argumentos em `args` (sem shell). Sem
/// `onFailure`: o hook do Duck e informativo, falhar nao pode travar o Claude.
fn hook_entry(exe: &str, windows: bool) -> Value {
    let exe = exe.replace('\\', "/");
    if windows { json!({"type": "command", "command": exe, "args": ["hook"], "timeout": 5}) }
    else { json!({"type": "command", "command": format!("\"{exe}\" hook"), "timeout": 5}) }
}

/// Nome do programa (sem pasta, aspas nem .exe), minusculo.
fn prog_base(p: &str) -> String {
    let p = p.trim().trim_matches('"').trim_matches('\'');
    let base = p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_string()
}

/// O hook e do Duck? (`duck hook` numa string, antigo, ou duck + args ["hook"]).
/// Hooks de terceiros nunca casam.
fn is_duck_hook(h: &Value) -> bool {
    let cmd = h["command"].as_str().unwrap_or("");
    if let Some(args) = h["args"].as_array() {
        return prog_base(cmd) == "duck" && args.len() == 1 && args[0] == "hook";
    }
    let Some(prog) = cmd.trim().strip_suffix(" hook") else { return false };
    prog_base(prog) == "duck"
}

/// Hook do Duck no formato que quebra no Windows sem Git Bash (string unica).
fn is_legacy_duck_hook(h: &Value) -> bool { is_duck_hook(h) && h.get("args").is_none() }

/// Poe o hook do Duck em cada evento: troca a entrada antiga (migra), tira
/// duplicadas e nao mexe nas de terceiros. Devolve se mudou algo.
fn apply_hooks(v: &mut Value, exe: &str, windows: bool) -> Result<bool, String> {
    if !v.is_object() { return Err("settings.json nao e um objeto JSON".into()); }
    let want = hook_entry(exe, windows);
    let before = v.clone();
    let hooks = v.as_object_mut().unwrap().entry("hooks").or_insert(json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return Err("\"hooks\" nao e um objeto".into()) };
    for ev in HOOK_EVENTS {
        let list = hooks.entry(ev).or_insert(json!([]));
        let Some(groups) = list.as_array_mut() else { return Err(format!("hooks.{ev} nao e uma lista")) };
        let mut placed = false;
        for g in groups.iter_mut() {
            let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) else { continue };
            let mut out = Vec::with_capacity(hs.len());
            for h in hs.drain(..) {
                if !is_duck_hook(&h) { out.push(h); }
                else if !placed { out.push(want.clone()); placed = true; }
            }
            *hs = out;
        }
        // Grupo que so tinha o Duck (duplicado) e ficou vazio sai.
        groups.retain(|g| g.get("hooks").and_then(|h| h.as_array()).is_none_or(|h| !h.is_empty()));
        if !placed { groups.push(json!({"hooks": [want.clone()]})); }
    }
    Ok(*v != before)
}

/// Estado dos hooks do Duck num settings.json.
#[derive(Debug, PartialEq)]
pub enum HookState {
    Ok,
    /// Arquivo nao existe ou nao tem o hook do Duck.
    Missing,
    /// Formato antigo (string unica) no Windows: trava o Claude sem Git Bash.
    Legacy,
    /// JSON invalido.
    Invalid(String),
}

fn hook_state_of(v: &Value, windows: bool) -> HookState {
    let entries: Vec<&Value> = v["hooks"].as_object().into_iter().flat_map(|o| o.values())
        .filter_map(|l| l.as_array()).flatten()
        .filter_map(|g| g["hooks"].as_array()).flatten().filter(|h| is_duck_hook(h)).collect();
    if entries.is_empty() { HookState::Missing }
    else if windows && entries.iter().any(|h| is_legacy_duck_hook(h)) { HookState::Legacy }
    else { HookState::Ok }
}

pub fn hook_state(settings: &std::path::Path) -> HookState {
    let Ok(raw) = std::fs::read_to_string(settings) else { return HookState::Missing };
    if raw.trim().is_empty() { return HookState::Missing; }
    match serde_json::from_str::<Value>(&raw) { Ok(v) => hook_state_of(&v, cfg!(windows)), Err(e) => HookState::Invalid(e.to_string()) }
}

/// Perfis do Claude Code: pastas ~/.claude* (o settings.json pode nao existir ainda).
pub fn claude_profiles() -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(crate::config::home()).into_iter().flatten().flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".claude") && e.path().is_dir())
        .map(|e| e.path()).collect();
    v.sort();
    v
}

/// Comando que instala (ou migra) os hooks num perfil.
pub fn hooks_fix_cmd(settings: &std::path::Path) -> String { format!("duck install --hooks \"{}\"", settings.display()) }

fn install_hooks(path: &str, exe: &str) -> i32 {
    let p = std::path::Path::new(path);
    // Perfil sem settings.json (perfil novo do Claude): cria.
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) { eprintln!("{}: {e}", dir.display()); return 1; }
    }
    let raw = std::fs::read_to_string(p).unwrap_or_default();
    let raw = if raw.trim().is_empty() { "{}".to_string() } else { raw };
    let mut v: Value = match serde_json::from_str(&raw) { Ok(v) => v, Err(e) => { eprintln!("{path}: {e}"); return 1 } };
    let legacy = hook_state_of(&v, cfg!(windows)) == HookState::Legacy;
    match apply_hooks(&mut v, exe, cfg!(windows)) {
        Ok(false) => { println!("hooks do Duck ja estavam em {path}"); 0 }
        Ok(true) => {
            let _ = std::fs::write(format!("{path}.bak-duck"), &raw);
            match std::fs::write(p, serde_json::to_string_pretty(&v).unwrap()) {
                Ok(_) => { println!("hooks do Duck {} em {path} (backup .bak-duck)", if legacy { "migrados para o formato sem shell" } else { "instalados" }); 0 }
                Err(e) => { eprintln!("{e}"); 1 }
            }
        }
        Err(e) => { eprintln!("{path}: {e}"); 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_idle_nao_e_bloqueio() {
        assert!(notification_idle(&json!({"notification_type": "idle_prompt", "message": "x"})));
        assert!(!notification_idle(&json!({"notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"})));
        assert!(notification_idle(&json!({"message": "Claude is waiting for your input"})));
        assert!(!notification_idle(&json!({"message": "Claude needs your permission to use Bash"})));
    }

    #[test]
    fn hooks_windows_e_linux() {
        let exe = r"C:\Users\herlo\AppData\Local\Duck\bin\duck.exe";
        let ev = |v: &Value, e: &str| v["hooks"][e].as_array().unwrap().clone();
        // Novo, Windows: programa + args, sem shell.
        let mut v = json!({});
        assert!(apply_hooks(&mut v, exe, true).unwrap());
        let h = &ev(&v, "PreToolUse")[0]["hooks"][0];
        assert_eq!(h["command"], "C:/Users/herlo/AppData/Local/Duck/bin/duck.exe");
        assert_eq!(h["args"], json!(["hook"]));
        assert!(h.get("onFailure").is_none());
        assert_eq!(hook_state_of(&v, true), HookState::Ok);
        // Rodar de novo nao muda nada.
        assert!(!apply_hooks(&mut v, exe, true).unwrap());
        // Antigo (string unica) + duplicado + hook de terceiro: migra, tira o
        // duplicado e mantem o de terceiro no lugar.
        let old = json!({"type": "command", "command": "\"C:/Users/herlo/AppData/Local/Duck/bin/duck.exe\" hook", "timeout": 5});
        let other = json!({"type": "command", "command": "node C:/x/meu-hook.js hook"});
        let mut v = json!({"model": "opus", "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [other.clone(), old.clone()]}, {"hooks": [old.clone()]}],
            "Stop": [{"hooks": [old.clone()]}]}});
        assert_eq!(hook_state_of(&v, true), HookState::Legacy);
        assert_eq!(hook_state_of(&v, false), HookState::Ok);
        assert!(apply_hooks(&mut v, exe, true).unwrap());
        let pre = ev(&v, "PreToolUse");
        assert_eq!(pre.len(), 1, "grupo so com duplicado sai: {pre:?}");
        assert_eq!(pre[0]["matcher"], "Bash");
        assert_eq!(pre[0]["hooks"][0], other);
        assert_eq!(pre[0]["hooks"][1]["args"], json!(["hook"]));
        assert_eq!(ev(&v, "Stop").len(), 1);
        assert_eq!(v["model"], "opus");
        assert_eq!(hook_state_of(&v, true), HookState::Ok);
        // Linux: string unica (o shell e POSIX), sem regressao.
        let mut v = json!({});
        assert!(apply_hooks(&mut v, "/home/u/.local/bin/duck", false).unwrap());
        assert_eq!(ev(&v, "Stop")[0]["hooks"][0]["command"], "\"/home/u/.local/bin/duck\" hook");
        assert!(!apply_hooks(&mut v, "/home/u/.local/bin/duck", false).unwrap());
        // Nao confunde com outros programas chamados "...hook".
        assert!(!is_duck_hook(&json!({"command": "\"/x/duckdns\" hook"})));
        assert!(!is_duck_hook(&json!({"command": "/x/ruff", "args": ["hook"]})));
        assert!(is_duck_hook(&json!({"command": "duck hook"})));
        assert_eq!(hook_state_of(&json!({"hooks": {}}), true), HookState::Missing);
        assert!(apply_hooks(&mut json!([]), exe, true).is_err());
    }

    #[test]
    fn install_hooks_perfil_sem_settings() {
        // Perfil novo do Claude (pasta sem settings.json, ou nem a pasta): cria e instala.
        let dir = std::env::temp_dir().join(format!("duck-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join(".claude-totvs").join("settings.json");
        let path = f.to_string_lossy().to_string();
        assert_eq!(hook_state(&f), HookState::Missing);
        assert_eq!(install_hooks(&path, "/opt/duck/bin/duck"), 0);
        assert_eq!(hook_state(&f), HookState::Ok);
        let first = std::fs::read_to_string(&f).unwrap();
        // De novo: nada muda.
        assert_eq!(install_hooks(&path, "/opt/duck/bin/duck"), 0);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), first);
        // JSON invalido: nao sobrescreve, acusa.
        std::fs::write(&f, "{ quebrado").unwrap();
        assert!(matches!(hook_state(&f), HookState::Invalid(_)));
        assert_eq!(install_hooks(&path, "/opt/duck/bin/duck"), 1);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ quebrado");
        assert_eq!(hooks_fix_cmd(&f), format!("duck install --hooks \"{}\"", f.display()));
        let _ = std::fs::remove_dir_all(&dir);
    }

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
    // Hooks do Claude em cada perfil (~/.claude*), com o comando que corrige.
    for dir in claude_profiles() {
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let settings = dir.join("settings.json");
        match hook_state(&settings) {
            HookState::Ok => println!("ok hooks do Duck em ~/{name}/settings.json"),
            HookState::Missing => println!("!! hooks do Duck faltando em ~/{name}{}: {}",
                if settings.is_file() { "/settings.json" } else { " (sem settings.json)" }, hooks_fix_cmd(&settings)),
            HookState::Legacy => println!("!! hooks do Duck em ~/{name}/settings.json no formato antigo (trava o Claude sem Git Bash): {}", hooks_fix_cmd(&settings)),
            HookState::Invalid(e) => println!("!! ~/{name}/settings.json invalido: {e}"),
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

/// `duck msg <peer>[/aba] [--to SESSAO] [--conv ID] "texto"` (ou texto no stdin): manda para o
/// agente de outro computador. O server prepara; a rede sai daqui.
pub fn msg(args: &[String]) -> i32 {
    let (mut conv, mut to) = (None, None);
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--conv" { conv = it.next().cloned(); } else if a == "--to" { to = it.next().cloned(); } else { rest.push(a.clone()); }
    }
    let Some(mut peer) = rest.first().cloned() else { eprintln!("uso: duck msg <peer>[/aba] [--to SESSAO] [--conv ID] \"texto\"   (sem texto: le do stdin)"); return 2 };
    // Forma curta: computador/aba ("legion/Ferrum"); nome de computador nao tem "/".
    if let Some((p, t)) = peer.split_once('/').map(|(p, t)| (p.to_string(), t.to_string())) {
        peer = p;
        if to.is_none() && !t.trim().is_empty() { to = Some(t); }
    }
    let mut text = rest[1..].join(" ");
    if text.trim().is_empty() { let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text); }
    let mut c = match Conn::connect_or_spawn() { Ok(c) => c, Err(e) => { eprintln!("duck: {e}"); return 1 } };
    let out = match c.call(json!({"cmd": "peer-out", "peer": peer, "text": text, "conv": conv})) {
        Ok(v) => v, Err(e) => { eprintln!("duck msg: {e}"); return 1 }
    };
    let req = json!({"msg": 1, "conv": out["conv"], "hops": out["hops"], "to": out["to"], "reply_pane": out["reply_pane"], "to_name": to, "text": out["text"]});
    match crate::peer::request(&peer, &req) {
        Ok((v, _)) if v["ok"].as_bool() == Some(true) => {
            let how = if v["approval"].as_bool() == Some(true) { "chegou, espera o usuario de la aprovar" } else { "entregue" };
            println!("{peer}: {how} (conversa {}). A resposta chega neste pane.", out["conv"].as_str().unwrap_or(""));
            0
        }
        Ok((v, _)) => { eprintln!("duck msg: {peer} recusou: {}", v["error"].as_str().unwrap_or("?")); 1 }
        Err(e) => { eprintln!("duck msg: {peer} nao respondeu: {e}\nDiagnostico: duck peer doctor {peer}"); 1 }
    }
}

/// `duck send-file <peer> <arquivo>`: manda um arquivo (ate 64 MB) para o
/// outro computador; la ele vai para Downloads/Duck depois de aprovado.
pub fn send_file(args: &[String]) -> i32 {
    use base64::Engine;
    let (Some(peer), Some(path)) = (args.first(), args.get(1)) else { eprintln!("uso: duck send-file <peer> <arquivo>"); return 2 };
    let data = match std::fs::read(path) { Ok(d) => d, Err(e) => { eprintln!("duck send-file: {path}: {e}"); return 1 } };
    if data.len() > 64 << 20 { eprintln!("duck send-file: maximo 64 MB"); return 1; }
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "arquivo".into());
    let req = json!({"file": 1, "name": name, "data": base64::engine::general_purpose::STANDARD.encode(&data)});
    match crate::peer::request(peer, &req) {
        Ok((v, _)) if v["ok"].as_bool() == Some(true) => {
            if v["approval"].as_bool() == Some(true) { println!("{peer}: {name} chegou, espera o usuario de la aceitar"); } else { println!("{peer}: {name} salvo la em Downloads/Duck"); }
            0
        }
        Ok((v, _)) => { eprintln!("duck send-file: {peer} recusou: {}", v["error"].as_str().unwrap_or("?")); 1 }
        Err(e) => { eprintln!("duck send-file: {peer}: {e}"); 1 }
    }
}

/// Uma linha do diagnostico: ok, aviso ou erro, e o que fazer.
pub struct Check { pub ok: Option<bool>, pub text: String }

fn check(ok: Option<bool>, text: impl Into<String>) -> Check { Check { ok, text: text.into() } }

/// Diagnostico da conexao com `name` (ou, sem nome, so deste computador):
/// escuta, firewall, relay, cada endereco, criptografia, versao e se o
/// outro alcanca este de volta.
pub fn diagnose(name: Option<&str>) -> Vec<Check> {
    let cfg = crate::config::load_config().0;
    let mut out = vec![];
    let port = crate::peer::port_of(&cfg.peer_listen);
    match crate::peer::advertised(&cfg.peer_listen) {
        None if cfg.peer_relay.is_empty() => out.push(check(Some(false), "Este computador não escuta nem usa relay: ninguém consegue falar com ele (painel Computadores > Ligar).")),
        None => out.push(check(None, "Este computador não escuta na rede: só recebe pelo relay.")),
        Some(_) if crate::peer::listens_all(&cfg.peer_listen) => out.push(check(Some(true), format!("Escutando em todas as redes, porta {port}."))),
        Some(a) => out.push(check(None, format!("Escutando só em {a}: computadores de outras redes (VMs, VPN) não chegam aqui. Painel Computadores > Rede > Todas as redes."))),
    }
    if let Some(conf) = crate::peer::reserve_of().filter(|_| !cfg.peer_listen.is_empty()) {
        out.push(check(None, format!("A porta {} está com outro Duck deste computador (outro usuário logado?): este usa a reserva {port}, com o nome {}. Os outros acham este pela busca (Procurar computadores), que também atualiza o endereço de quem já pareou.",
            crate::peer::port_of(&conf), cfg.peer_name)));
    }
    if !cfg.peer_listen.is_empty() {
        let blocked = crate::peer::firewalld_blocked(port);
        if blocked.is_empty() { if !cfg!(windows) { out.push(check(Some(true), "Firewall deste computador libera a porta (ou não há firewalld).")); } }
        for (z, ifs) in blocked {
            out.push(check(Some(false), format!("Firewall: a zona {z} ({ifs}) bloqueia a porta {port}. Libere com: {}", crate::peer::firewalld_cmd(&z, port))));
        }
        if cfg!(windows) { out.push(check(None, format!("Windows: confira a porta {port} no firewall (painel Computadores > Firewall do Windows copia o comando)."))); }
    }
    if !cfg.peer_relay.is_empty() {
        match crate::relay::dial(&cfg.peer_relay, &cfg.peer_relay_secret, &crate::secure::my_key()) {
            Ok(_) => out.push(check(Some(true), format!("Relay {} alcançável e este computador registrado nele.", cfg.peer_relay))),
            Err(e) if e.contains("nao esta conectado") => out.push(check(Some(false), format!("Relay {} responde, mas este computador não está registrado (o server daqui conecta ao iniciar: reinicie o server).", cfg.peer_relay))),
            Err(e) => out.push(check(Some(false), format!("Relay {}: {e}", cfg.peer_relay))),
        }
    }
    let Some(name) = name else { return out };
    let list = crate::peer::addrs_of(name);
    if list.is_empty() { out.push(check(Some(false), format!("{name} não está pareado."))); return out; }
    let key = crate::peer::key_of(name);
    if key.is_none() { out.push(check(Some(false), format!("{name} foi pareado numa versão antiga: conexão sem criptografia. Painel Computadores > {name} > Parear de novo."))); }
    let mut reached = None;
    for a in &list {
        if a.starts_with("relay:") {
            match crate::peer::open(a, key.as_deref()) {
                Ok(_) => { out.push(check(Some(true), format!("{a}: {name} responde pelo relay (cifrado)."))); reached.get_or_insert(a.clone()); }
                Err(e) => out.push(check(Some(false), format!("{a}: {e}"))),
            }
            continue;
        }
        match crate::peer::ping(a) {
            Ok(i) => {
                let mut t = format!("{a}: online, {} ms, Duck {} ({})", i.ms, i.version, i.os);
                if crate::update::newer(crate::peer::VERSION, &i.version) { t.push_str(&format!(" · desatualizado: rode duck update em {name}")); }
                if crate::update::newer(&i.version, crate::peer::VERSION) { t.push_str(" · este computador está desatualizado: duck update"); }
                if let Some(k) = &key { if !i.key.is_empty() && &i.key != k { t.push_str(" · CHAVE DIFERENTE da pareada (reinstalou? pareie de novo)"); out.push(check(Some(false), t)); continue; } }
                out.push(check(Some(true), t));
                reached.get_or_insert(a.clone());
            }
            Err(e) => {
                let hint = if e.contains("refused") || e.contains("recusad") { " (porta fechada: o Duck de lá não escuta nesse IP)" }
                    else if e.contains("timed out") || e.contains("tempo") { " (sem resposta: firewall de lá ou outra rede)" } else { "" };
                out.push(check(Some(false), format!("{a}: {e}{hint}")));
            }
        }
    }
    let Some(addr) = reached else {
        out.push(check(Some(false), format!("Nenhum endereço de {name} respondeu. Se estão em redes diferentes: Tailscale nos dois, ou um relay (duck relay).")));
        return out;
    };
    // Volta: ele consegue abrir conexao ate aqui? (precisa da conexao cifrada)
    if key.is_some() {
        let mine: Vec<String> = crate::peer::interfaces().into_iter().map(|i| format!("{}:{port}", i.ip)).collect();
        match crate::peer::open(&addr, key.as_deref()).and_then(|mut c| {
            c.set_timeout(std::time::Duration::from_secs(30));
            c.call(&json!({"callback": 1, "from": cfg.peer_name, "port": port, "addrs": mine, "version": crate::peer::VERSION}))
        }) {
            Ok(v) if v["ok"].as_bool() == Some(true) => {
                let rs = v["results"].as_array().cloned().unwrap_or_default();
                let any = rs.iter().any(|r| r["ok"].as_bool() == Some(true));
                if any {
                    let ok: Vec<String> = rs.iter().filter(|r| r["ok"].as_bool() == Some(true)).filter_map(|r| r["addr"].as_str().map(String::from)).collect();
                    out.push(check(Some(true), format!("Volta: {name} alcança este computador ({}).", ok.join(", "))));
                } else if cfg.peer_listen.is_empty() {
                    out.push(check(if cfg.peer_relay.is_empty() { Some(false) } else { None }, format!("Volta: {name} não alcança este computador (aqui não escuta). Respostas só pelo relay.")));
                } else {
                    let first = rs.first().and_then(|r| r["error"].as_str()).unwrap_or("?");
                    out.push(check(Some(false), format!("Volta: {name} NÃO alcança este computador ({first}). Mensagens de lá para cá falham: firewall daqui, escuta só numa rede, ou NAT. Veja as linhas acima; fora da rede, use relay.")));
                }
            }
            Ok(v) => out.push(check(Some(false), format!("Volta: {name} recusou o teste: {}", v["error"].as_str().unwrap_or("?")))),
            Err(e) => out.push(check(Some(false), format!("Conexão cifrada com {name} falhou: {e}"))),
        }
    }
    out
}

fn print_checks(cs: &[Check]) {
    for c in cs {
        let m = match c.ok { Some(true) => "ok   ", Some(false) => "ERRO ", None => "aviso" };
        println!("{m} {}", c.text);
    }
}

/// Espera o outro aceitar o pedido (a CLI consulta direto, o server tambem).
fn wait_accept(addr: &str, key: &str, secs: u64) -> Result<String, String> {
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < secs {
        std::thread::sleep(std::time::Duration::from_secs(2));
        // O server pode ter fechado antes (pair_done chegou la).
        if let Some(n) = crate::config::load_config().0.peers.iter().map(|(n, _)| n.clone()).find(|n| crate::peer::key_of(n).as_deref() == Some(key)) { return Ok(n); }
        match crate::peer::pair_status(addr, key) {
            Ok(crate::peer::PairState::Accepted(n, v)) => {
                // Guarda daqui mesmo (o server tambem guarda quando consulta).
                let cfg = crate::config::load_config().0;
                if crate::peer::name_by_key(&cfg.peers, key).is_none() {
                    let name = crate::peer::free_name(&cfg.peers, &n, key);
                    crate::peer::save(&name, &crate::peer::addrs_for(addr, v["relay"].as_str()), key)?;
                }
                if let Ok(mut c) = Conn::connect() {
                    let _ = c.call(json!({"cmd": "peer-pending-cancel", "key": key}));
                    let _ = c.call(json!({"cmd": "peers-reload"}));
                }
                return Ok(crate::peer::name_by_key(&crate::config::load_config().0.peers, key).unwrap_or(n));
            }
            Ok(crate::peer::PairState::Denied) => return Err("negado".into()),
            _ => {}
        }
    }
    Err("sem resposta ainda (o pedido continua valendo por 15 min; aparece em duck peer list quando aceitarem)".into())
}

/// `duck peer ...`: o mesmo que o painel Computadores (Ctrl+Shift+P).
pub fn peer(args: &[String]) -> i32 {
    let cfg = crate::config::load_config().0;
    let me = cfg.peer_name.clone();
    let off = "este computador nao escuta. Ligue no painel Computadores (Ctrl+Shift+P) ou ponha peer = { listen = '0.0.0.0:7878' } no ~/.duck-local.lua";
    let a = |i: usize| args.get(i).map(String::as_str);
    let server = |req: Value| -> Result<Value, String> { Conn::connect_or_spawn().map_err(|e| e.to_string())?.call(req) };
    match (a(0), a(1), a(2)) {
        (Some("invite"), _, _) => match server(json!({"cmd": "peer-invite"})) {
            Ok(v) => {
                println!("{}", v["invite"].as_str().unwrap_or(""));
                eprintln!("Vale uma vez, por {} min. No outro computador: duck peer join '<convite>' (ou painel Computadores > Usar convite).", v["minutes"]);
                0
            }
            Err(e) => { eprintln!("duck peer invite: {e}"); 1 }
        },
        (Some("join"), Some(code), _) => {
            let inv = match crate::peer::parse_invite(code) { Ok(c) => c, Err(e) => { eprintln!("duck peer join: {e}"); return 2 } };
            if inv.name == me && crate::secure::my_key().starts_with(&inv.key_prefix) { eprintln!("duck peer join: esse convite e deste computador. Use no outro."); return 1; }
            let mut errs = vec![];
            for addr in &inv.addrs {
                match crate::peer::pair_request(addr, Some(&inv.code), Some(&inv.key_prefix), |_| {}) {
                    Ok(p) if p.accepted => {
                        let name = crate::peer::free_name(&cfg.peers, &p.name, &p.key);
                        let mut addrs = vec![addr.clone()];
                        addrs.extend(inv.addrs.iter().filter(|x| *x != addr).cloned());
                        if let Err(e) = crate::peer::save(&name, &addrs, &p.key) { eprintln!("duck peer join: {e}"); return 1; }
                        let _ = server(json!({"cmd": "peers-reload"}));
                        println!("conectado com {name} ({addr}). Teste: duck msg {name} \"oi\"");
                        return 0;
                    }
                    Ok(_) => { errs.push(format!("{addr}: o outro pediu aprovacao (convite nao aceito)")); }
                    Err(e) => errs.push(format!("{addr}: {e}")),
                }
            }
            eprintln!("duck peer join: {}", errs.join(" · "));
            1
        }
        (Some("find"), _, _) => {
            let port = crate::peer::port_of(&cfg.peer_listen);
            let mut ports = vec![crate::peer::DEFAULT_PORT];
            if !cfg.peer_listen.is_empty() && port != ports[0] { ports.push(port); }
            let found = crate::peer::discover(&ports, std::time::Duration::from_millis(1500));
            for n in crate::peer::learn_from_discovery(&found) { println!("(endereco novo de {n} guardado)"); let _ = server(json!({"cmd": "peers-reload"})); }
            let cfg = crate::config::load_config().0;
            let mine = crate::secure::short(&crate::secure::my_key());
            let found: Vec<_> = found.into_iter().filter(|f| f["key"].as_str() != Some(&mine)).collect();
            if found.is_empty() { println!("nenhum outro Duck respondeu (nem na rede local nem na Tailscale)"); }
            for f in &found {
                let known = f["key"].as_str().and_then(|k| cfg.peers.iter().find(|(n, _)| crate::peer::key_of(n).is_some_and(|x| x.starts_with(k)))).map(|(n, _)| format!("  (pareado: {n})")).unwrap_or_default();
                println!("{}  {}  {} {}{}{}", f["name"].as_str().unwrap_or(""), f["addr"].as_str().unwrap_or(""), f["os"].as_str().unwrap_or(""),
                    f["version"].as_str().unwrap_or(""), if f["via"] == "tailscale" { "  tailscale" } else { "" }, known);
            }
            0
        }
        (Some("connect"), Some(addr), _) => {
            if cfg.peer_listen.is_empty() && cfg.peer_relay.is_empty() { eprintln!("duck peer connect: {off}"); return 1; }
            let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{}", crate::peer::DEFAULT_PORT) };
            let reg = |k: &str| { let _ = server(json!({"cmd": "peer-pending", "key": k, "addr": addr})); };
            match crate::peer::pair_request(&addr, None, None, reg) {
                Ok(p) if p.accepted => {
                    let name = crate::peer::free_name(&cfg.peers, &p.name, &p.key);
                    if let Err(e) = crate::peer::save(&name, &[addr.clone()], &p.key) { eprintln!("{e}"); return 1; }
                    let _ = server(json!({"cmd": "peers-reload"}));
                    println!("ja estavam pareados: {name} ({addr})");
                    0
                }
                Ok(p) => {
                    println!("pedido enviado para {} ({addr}). Numero de confirmacao: {}", p.name, p.confirm);
                    println!("Aceite em {} so se aparecer o mesmo numero. Esperando...", p.name);
                    match wait_accept(&addr, &p.key, 120) {
                        Ok(n) => { println!("{n} aceitou. Teste: duck msg {n} \"oi\""); 0 }
                        Err(e) => { eprintln!("{e}"); 1 }
                    }
                }
                Err(e) => { eprintln!("duck peer connect: {addr}: {e}"); 1 }
            }
        }
        (Some("list"), _, _) => {
            let mode = match crate::peer::advertised(&cfg.peer_listen) { Some(x) if crate::peer::listens_all(&cfg.peer_listen) => format!("todas as redes ({x})"), Some(x) => x, None => "nao escuta".into() };
            println!("este computador: {me} · {mode}{} · chave {}", if cfg.peer_relay.is_empty() { String::new() } else { format!(" · relay {}", cfg.peer_relay) }, crate::secure::short(&crate::secure::my_key()));
            for (name, addrs) in &cfg.peers {
                let list = crate::peer::split_addrs(addrs);
                let lock = if crate::peer::key_of(name).is_some() { "cifrado" } else { "SEM CRIPTOGRAFIA (pareie de novo)" };
                match crate::peer::ping_any(&list) {
                    Ok((i, a)) => {
                        let old = if crate::update::newer(crate::peer::VERSION, &i.version) { " · desatualizado" } else { "" };
                        println!("  online  {name}  {a}  {} ms  Duck {} {}{old}  {lock}", i.ms, i.version, i.os);
                    }
                    Err(e) => println!("  offline {name}  {}  {e}  {lock}", list.join(",")),
                }
            }
            if let Ok(v) = server(json!({"cmd": "peers-state"})) {
                for p in v["pending"].as_array().into_iter().flatten() { println!("  esperando aceite: {} (ha {}s)", p["addr"].as_str().unwrap_or(""), p["secs"]); }
                if let Some(r) = v["relay"].as_str().filter(|r| !r.is_empty()) { println!("  relay: {r}"); }
            }
            0
        }
        (Some("doctor"), n, _) => { print_checks(&diagnose(n)); 0 }
        (Some("sessions"), Some(name), _) => match crate::peer::request(name, &json!({"sessions": 1})) {
            Ok((v, _)) if v["ok"].as_bool() == Some(true) => {
                let ss = v["sessions"].as_array().cloned().unwrap_or_default();
                if ss.is_empty() { println!("{name}: nenhum agente aberto"); }
                for s in ss {
                    println!("  {:>3}  {:<8}  {}  {}  ({})", s["id"], s["state"].as_str().unwrap_or(""), s["tab"].as_str().filter(|t| !t.is_empty()).unwrap_or(s["title"].as_str().unwrap_or("")),
                        s["dir"].as_str().unwrap_or(""), s["agent"].as_str().unwrap_or(""));
                }
                println!("Mandar para um: duck msg {name}/<aba> \"texto\"   (aba, pasta, titulo ou numero; parte do nome basta)");
                0
            }
            Ok((v, _)) => { eprintln!("duck peer sessions: {}", v["error"].as_str().unwrap_or("?")); 1 }
            Err(e) => { eprintln!("duck peer sessions: {e}"); 1 }
        },
        (Some("addr"), Some(name), Some(op)) => {
            let mut list = crate::peer::addrs_of(name);
            if list.is_empty() { eprintln!("{name} nao esta pareado"); return 1; }
            match (op, a(3)) {
                ("add", Some(x)) => { let x = if x.contains(':') { x.to_string() } else { format!("{x}:{}", crate::peer::DEFAULT_PORT) }; if !list.contains(&x) { list.push(x); } }
                ("remove", Some(x)) => list.retain(|y| y != x),
                ("list", _) => { for x in &list { println!("{x}"); } return 0; }
                _ => { eprintln!("uso: duck peer addr <nome> add|remove <ip:porta|relay:host:porta> | list"); return 2; }
            }
            if list.is_empty() { eprintln!("precisa ficar pelo menos um endereco"); return 1; }
            match crate::peer::save_addrs(name, &list) { Ok(_) => { let _ = server(json!({"cmd": "peers-reload"})); println!("{name}: {}", list.join(", ")); 0 } Err(e) => { eprintln!("{e}"); 1 } }
        }
        (Some("relay"), Some("off"), _) => {
            let r = crate::config::set_local_setting("peer.relay", "''");
            match r { Ok(_) => { let _ = server(json!({"cmd": "restart-server"})); println!("relay desligado"); 0 } Err(e) => { eprintln!("{e}"); 1 } }
        }
        (Some("relay"), Some(host), secret) => {
            let host = if host.contains(':') { host.to_string() } else { format!("{host}:7879") };
            let r = crate::config::set_local_setting("peer.relay", &format!("'{}'", host.replace('\'', ""))).and_then(|_| match secret {
                Some(s) => crate::config::set_local_setting("peer.relay_secret", &format!("'{}'", s.replace('\'', ""))),
                None => Ok(()),
            });
            match r { Ok(_) => { let _ = server(json!({"cmd": "restart-server"})); println!("relay: {host}. Os pareados aprendem o relay ao parear de novo, ou: duck peer addr <nome> add relay:<host do relay deles>"); 0 } Err(e) => { eprintln!("{e}"); 1 } }
        }
        (Some("convs"), _, _) => match server(json!({"cmd": "convs"})) {
            Ok(v) => {
                let cs = v["convs"].as_array().cloned().unwrap_or_default();
                if cs.is_empty() { println!("nenhuma conversa"); }
                for c in cs { println!("  {}  com {}  {}/{} idas e voltas{}", c["conv"].as_str().unwrap_or(""), c["peer"].as_str().unwrap_or(""), c["hops"], c["max"], if c["closed"] == true { "  (encerrada)" } else { "" }); }
                0
            }
            Err(e) => { eprintln!("{e}"); 1 }
        },
        (Some("close"), Some(conv), _) => match server(json!({"cmd": "conv-close", "conv": conv})) { Ok(_) => { println!("conversa {conv} encerrada"); 0 } Err(e) => { eprintln!("{e}"); 1 } },
        (Some("remove"), Some(name), _) => match crate::peer::remove(name) {
            Ok(_) => { let _ = server(json!({"cmd": "peers-reload"})); println!("{name} removido"); 0 }
            Err(e) => { eprintln!("{e}"); 1 }
        },
        _ => {
            eprintln!("uso: duck peer list | find | connect <ip[:porta]> | invite | join <convite> | doctor [nome] | sessions <nome>\n          \
                       | addr <nome> add|remove|list [end] | relay <host[:porta]> [senha] | relay off | convs | close <conv> | remove <nome>\n\
                       tambem: duck msg <nome> \"texto\" · duck send-file <nome> <arquivo> · duck relay (servidor de relay)");
            2
        }
    }
}
