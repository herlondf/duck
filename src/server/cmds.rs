//! Comandos novos do server feitos para o celular (docs/MOBILE.md, os ★),
//! mas abertos a CLI e aos scripts como os outros. Os que leem disco, rodam
//! processo ou esperam (traducao, CPU) pegam o lock do Mux so para copiar o
//! que precisam: um celular lento nao trava os outros clientes.

use super::*;
use base64::Engine;

/// Comandos daqui (o resto vai para o `match` de `handle`).
const CMDS: [&str; 18] = ["timeline", "usage", "system", "routines", "recent-dirs", "file-get", "agent-answer", "translate", "interrupt",
    "resume", "routine-run", "file-put", "push-token", "mobile-pair", "mobile-list", "mobile-revoke", "mobile-scopes", "mobile-invite-cancel"];

pub(super) fn handles(cmd: &str) -> bool { CMDS.contains(&cmd) }

/// Pane pedido (obrigatorio aqui: nada de "o pane ativo da janela").
fn pane_arg(a: &Value) -> Result<Id, String> { arg_id(a, "pane").ok_or_else(|| "falta pane".to_string()) }

/// Copia algo do pane com o lock (o pane existe?).
fn with_pane<T>(srv: &Arc<Server>, id: Id, f: impl FnOnce(&Mux, &Pane) -> T) -> Result<T, String> {
    let m = srv.lock();
    let p = m.panes.get(&id).ok_or_else(|| format!("pane {id} nao existe"))?;
    Ok(f(&m, p))
}

pub(super) fn run(srv: &Arc<Server>, a: &Value, caller: Caller, cmd: &str) -> Result<Value, String> {
    let owner = caller.0.is_none();
    match cmd {
        "timeline" => {
            let id = pane_arg(a)?;
            let (session, dir) = with_pane(srv, id, |_, p| p.agent.as_ref().map(|g| (g.session.clone(), g.config_dir.clone())))?
                .ok_or("o pane nao tem agente")?;
            let session = session.ok_or("sessao do agente desconhecida (hooks do Duck instalados?)")?;
            let path = crate::claude::find_transcript(dir.as_deref().unwrap_or(""), &session).ok_or("transcript da sessao nao encontrado")?;
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(200).clamp(1, 600) as usize;
            let ev = crate::claude::timeline(&text);
            let n = ev.len();
            Ok(proto::ok(json!({"pane": id, "session": session, "total": n, "events": ev.into_iter().skip(n.saturating_sub(limit)).collect::<Vec<_>>()})))
        }
        "usage" => Ok(proto::ok(json!({"accounts": crate::claude::usage(&config::home(), &crate::setup::usage_dir())}))),
        "system" => {
            let (name, panes, agents, working) = {
                let m = srv.lock();
                let ags = m.panes.values().filter_map(|p| p.agent.as_ref()).collect::<Vec<_>>();
                (m.cfg.peer_name.clone(), m.panes.len(), ags.len(), ags.iter().filter(|g| g.state == "working").count())
            };
            let mut v = json!({"name": name, "version": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS, "panes": panes, "agents": agents, "working": working});
            if let Ok(mut sys) = sysinfo::System::new() {
                // Duas leituras para ter o uso da CPU (fora do lock).
                sys.refresh_cpu_all();
                std::thread::sleep(Duration::from_millis(250));
                sys.refresh_cpu_all();
                sys.refresh_memory();
                v["cpu"] = json!((sys.global_cpu_usage() * 10.0).round() / 10.0);
                v["cores"] = json!(sys.cpus().len());
                v["ram_used"] = json!(sys.used_memory());
                v["ram_total"] = json!(sys.total_memory());
                v["swap_used"] = json!(sys.used_swap());
                v["swap_total"] = json!(sys.total_swap());
                v["uptime"] = json!(sysinfo::System::uptime().unwrap_or(0));
            }
            Ok(proto::ok(v))
        }
        "routines" => Ok(proto::ok(json!({"routines": routines(&config::home().join(".duck").join("rotinas")), "running": srv.lock().ext.is_running("rotinas")}))),
        "routine-run" => {
            let id = arg_str(a, "id").filter(|i| !i.is_empty() && i.chars().all(|c| c.is_alphanumeric() || "-_. ".contains(c))).ok_or("falta id da rotina")?;
            if !config::home().join(".duck").join("rotinas").join(format!("{id}.md")).is_file() { return Err(format!("rotina {id} nao existe")); }
            let mut m = srv.lock();
            if !m.ext.is_running("rotinas") { return Err("a extensao Rotinas nao esta ligada".into()); }
            m.ext.send("rotinas", json!({"type": "action", "action": "run", "item": id, "view": ""}));
            Ok(proto::ok(json!({"id": id, "queued": true})))
        }
        "recent-dirs" => {
            let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(40).clamp(1, 200) as usize;
            let hist: Vec<String> = { let m = srv.lock(); m.history.iter().rev().filter_map(|h| h["cwd"].as_str().map(String::from)).take(2000).collect() };
            Ok(proto::ok(json!({"dirs": recent_dirs(zoxide(), hist, limit)})))
        }
        "file-get" => {
            let id = pane_arg(a)?;
            let cwd = with_pane(srv, id, |_, p| p.cwd.clone())?;
            let want = arg_str(a, "path").filter(|p| !p.is_empty()).ok_or("falta path")?;
            let path = inside(&cwd, want)?;
            let meta = std::fs::metadata(&path).map_err(|e| format!("{want}: {e}"))?;
            if !meta.is_file() { return Err(format!("{want} nao e arquivo")); }
            if meta.len() as usize > crate::mobile::MAX_FILE { return Err(format!("arquivo grande demais ({}, maximo 20 MB)", human_size(meta.len() as usize))); }
            let data = std::fs::read(&path).map_err(|e| e.to_string())?;
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            Ok(proto::ok(json!({"pane": id, "name": name, "path": plain(&path), "size": data.len(), "data": base64::engine::general_purpose::STANDARD.encode(&data)})))
        }
        "file-put" => {
            let id = pane_arg(a)?;
            let cwd = with_pane(srv, id, |_, p| p.cwd.clone())?;
            let dir = std::fs::canonicalize(&cwd).map_err(|_| format!("pasta do pane inacessivel daqui: {cwd}"))?;
            let name = safe_file_name(arg_str(a, "name").unwrap_or(""));
            let data = base64::engine::general_purpose::STANDARD.decode(arg_str(a, "data").unwrap_or("")).map_err(|e| format!("data: {e}"))?;
            if data.len() > crate::mobile::MAX_FILE { return Err("arquivo grande demais (maximo 20 MB)".into()); }
            let dst = free_path(&dir, &name);
            // So na pasta do pane: o nome saneado nao tem pasta, mas confere.
            if dst.parent() != Some(dir.as_path()) { return Err("nome de arquivo invalido".into()); }
            std::fs::write(&dst, &data).map_err(|e| format!("{}: {e}", dst.display()))?;
            Ok(proto::ok(json!({"pane": id, "name": dst.file_name().map(|n| n.to_string_lossy().to_string()), "path": plain(&dst), "size": data.len()})))
        }
        "agent-answer" => {
            let id = pane_arg(a)?;
            let yes = a.get("yes").and_then(|v| v.as_bool()).ok_or("falta yes (true ou false)")?;
            let mut m = srv.lock();
            let p = m.panes.get_mut(&id).ok_or_else(|| format!("pane {id} nao existe"))?;
            let keys = answer_keys(p.agent.as_ref().map(|g| g.state.as_str()), yes)?;
            p.vt().term.scroll_display(Scroll::Bottom);
            p.write(keys.as_bytes());
            p.attention = false;
            Ok(proto::ok(json!({"pane": id, "sent": if yes { "1" } else { "esc" }})))
        }
        "interrupt" => {
            let id = pane_arg(a)?;
            let keys = interrupt_keys(arg_str(a, "key").unwrap_or("esc"))?;
            let m = srv.lock();
            m.panes.get(&id).ok_or_else(|| format!("pane {id} nao existe"))?.write(keys.as_bytes());
            Ok(proto::ok(json!({"pane": id})))
        }
        "translate" => {
            let id = pane_arg(a)?;
            let text = arg_str(a, "text").filter(|t| !t.trim().is_empty()).ok_or("falta text")?;
            if text.len() > 16_000 { return Err("texto grande demais".into()); }
            let (agent, dir, private, tr) = with_pane(srv, id, |m, p| (p.agent.as_ref().map(|g| g.name.clone()).unwrap_or_default(),
                p.agent.as_ref().and_then(|g| g.config_dir.clone()), p.private, m.cfg.translate.clone()))?;
            if private { return Err("pane privado: nada sai dele".into()); }
            let plan = tr.resolve(&agent)?;
            let mut env = vec![];
            if plan.engine == "claude" {
                if let Some(d) = dir.filter(|d| !d.is_empty() && (cfg!(not(windows)) || !d.starts_with('/'))) { env.push(("CLAUDE_CONFIG_DIR".to_string(), d)); }
            }
            let prompt = crate::translate::compose_prompt(&tr.from, &tr.to, &crate::translate::mask(text));
            let out = crate::translate::run(&plan.argv, &env, &prompt, crate::translate::TIMEOUT).map_err(|e| format!("Tradutor: {e}"))?;
            let c = crate::translate::parse_compose(&out).map_err(|e| format!("Tradutor: {e}"))?;
            Ok(proto::ok(json!({"pane": id, "translation": c.translation, "engine": plan.engine, "model": plan.model, "from": tr.from, "to": tr.to,
                "notes": c.notes.iter().map(|n| json!({"en": n.en, "pt": n.pt, "why": n.why})).collect::<Vec<_>>()})))
        }
        "resume" => {
            let home = config::home();
            let mut all = crate::claude::scan_profiles(&home);
            all.sort_by(|a, b| b.0.cmp(&a.0));
            let Some(want) = arg_str(a, "session").filter(|s| !s.is_empty()) else {
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 200) as usize;
                let list: Vec<Value> = all.iter().filter_map(|(t, prof, sid, path)| {
                    let (cwd, title) = crate::claude::session_head(path);
                    (!cwd.is_empty()).then(|| json!({"session": sid, "profile": crate::claude::profile_label(prof), "cwd": cwd, "title": title, "updated": t}))
                }).take(limit).collect();
                return Ok(proto::ok(json!({"sessions": list})));
            };
            if !want.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("sessao invalida".into()); }
            let (_, prof, sid, path) = all.iter().find(|x| x.2 == want).ok_or_else(|| format!("sessao {want} nao encontrada"))?;
            let (cwd, _) = crate::claude::session_head(path);
            let cfg_dir = home.join(prof).display().to_string();
            let env = match (prof == ".claude", cfg!(windows)) {
                (true, _) => String::new(),
                (false, true) => format!("$env:CLAUDE_CONFIG_DIR='{cfg_dir}'; "),
                (false, false) => format!("CLAUDE_CONFIG_DIR='{cfg_dir}' "),
            };
            let line = format!("{env}claude --resume {sid}");
            let argv = if cfg!(windows) { json!(["pwsh", "-NoLogo", "-NoExit", "-Command", line]) } else { crate::shell_keep(&line) };
            let title = cwd.rsplit(['/', '\\']).next().unwrap_or("").to_string();
            let mut req = json!({"cmd": "spawn", "where": "tab", "cwd": cwd, "title": title, "argv": argv, "focus": false});
            if let Some(w) = arg_str(a, "workspace") { req["workspace"] = json!(w); }
            handle(srv, &req, caller)
        }
        "push-token" => Err("push-token: so pela sessao do celular".into()),
        _ if !owner => Err(format!("{cmd}: so fora dos panes")),
        "mobile-pair" => mobile::invite(srv),
        "mobile-invite-cancel" => { mobile::cancel_invites(); Ok(proto::ok(json!({}))) }
        "mobile-list" => Ok(proto::ok(json!({"devices": crate::mobile::list().iter().map(|d| {
            let mut v = d.to_json();
            v["online"] = json!(mobile::online(&d.key));
            v
        }).collect::<Vec<_>>()}))),
        "mobile-revoke" => {
            let name = arg_str(a, "name").ok_or("falta name")?;
            let d = crate::mobile::by_name(name).ok_or_else(|| format!("celular {name} nao existe"))?;
            crate::mobile::remove(name)?;
            let n = mobile::kill_sessions(&d.key);
            proto::log(&format!("celular: {name} revogado ({n} sessao(oes) derrubada(s))"));
            Ok(proto::ok(json!({"name": name, "sessions_closed": n})))
        }
        "mobile-scopes" => {
            let name = arg_str(a, "name").ok_or("falta name")?;
            let mut d = crate::mobile::by_name(name).ok_or_else(|| format!("celular {name} nao existe"))?;
            let ch: Vec<String> = a.get("changes").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
            d.scopes = match a.get("scopes").and_then(|v| v.as_array()) {
                Some(all) => crate::mobile::apply_scopes(&[], &all.iter().filter_map(|x| x.as_str().map(|s| format!("+{s}"))).collect::<Vec<_>>())?,
                None => crate::mobile::apply_scopes(&d.scopes, &ch)?,
            };
            crate::mobile::save(&d)?;
            Ok(proto::ok(json!({"name": name, "scopes": d.scopes})))
        }
        _ => Err(format!("comando desconhecido: {cmd}")),
    }
}

/// `changes {pane, file}`: o diff do arquivo contra o HEAD (novo: o arquivo
/// inteiro como adicionado). Ate 200 KB.
pub(super) fn diff(srv: &Arc<Server>, a: &Value) -> Result<Value, String> {
    let id = pane_arg(a)?;
    let cwd = with_pane(srv, id, |_, p| p.cwd.clone())?;
    let file = arg_str(a, "file").filter(|f| !f.is_empty()).ok_or("falta file")?;
    let path = inside(&cwd, file)?;
    let mut c = std::process::Command::new("git");
    c.arg("-C").arg(&cwd).args(["diff", "--no-color", "HEAD", "--"]).arg(&path).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    let out = c.output().map_err(|e| format!("git: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.is_empty() && path.is_file() {
        // Fora do indice: tudo e linha nova.
        let body = std::fs::read(&path).map(|b| String::from_utf8_lossy(&b[..b.len().min(200 << 10)]).to_string()).unwrap_or_default();
        text = format!("+++ {file}\n{}", body.lines().map(|l| format!("+{l}")).collect::<Vec<_>>().join("\n"));
    }
    let cut = text.len() > 200 << 10;
    if cut { let mut n = 200 << 10; while !text.is_char_boundary(n) { n -= 1; } text.truncate(n); }
    Ok(proto::ok(json!({"pane": id, "cwd": cwd, "file": file, "diff": text, "truncated": cut})))
}

/// Teclas da resposta ao agente que pergunta: so com ele em `blocked`.
/// Sim = "1" (primeira opcao do menu do Claude Code); nao = Esc.
pub(super) fn answer_keys(state: Option<&str>, yes: bool) -> Result<&'static str, String> {
    match state {
        Some("blocked") => Ok(if yes { "1" } else { "\x1b" }),
        Some(s) => Err(format!("o agente nao esta esperando resposta (estado {s})")),
        None => Err("o pane nao tem agente".into()),
    }
}

fn interrupt_keys(key: &str) -> Result<&'static str, String> {
    match key { "esc" => Ok("\x1b"), "ctrl-c" => Ok("\x03"), k => Err(format!("key: esc ou ctrl-c (veio {k})")) }
}

/// Caminho para mostrar (sem o \\?\ do canonicalize no Windows).
fn plain(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Caminho dentro da pasta do pane (relativo a ela ou absoluto la dentro).
fn inside(cwd: &str, want: &str) -> Result<std::path::PathBuf, String> {
    let base = std::fs::canonicalize(cwd).map_err(|_| format!("pasta do pane inacessivel daqui: {cwd}"))?;
    let p = std::fs::canonicalize(base.join(want)).map_err(|e| format!("{want}: {e}"))?;
    if !p.starts_with(&base) { return Err("so arquivos dentro da pasta do pane".into()); }
    Ok(p)
}

fn zoxide() -> Vec<String> {
    let mut c = std::process::Command::new("zoxide");
    c.args(["query", "-l"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); }
    c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).lines().take(200).map(String::from).collect()).unwrap_or_default()
}

/// Pastas recentes: zoxide primeiro (frecencia), depois as do historico de comandos.
fn recent_dirs(zo: Vec<String>, hist: Vec<String>, limit: usize) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for (src, list) in [("zoxide", zo), ("history", hist)] {
        for d in list {
            let d = d.trim().to_string();
            if d.is_empty() || !seen.insert(d.clone()) { continue; }
            let name = d.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(&d).to_string();
            out.push(json!({"path": d, "name": name, "source": src}));
            if out.len() >= limit { return out; }
        }
    }
    out
}

/// Rotinas (extensao rotinas): arquivos ~/.duck/rotinas/*.md e o ultimo
/// resultado de cada uma em .estado.json.
fn routines(dir: &std::path::Path) -> Vec<Value> {
    let st: Value = std::fs::read(dir.join(".estado.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md")).collect();
    files.sort();
    files.iter().map(|f| {
        let id = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let text = std::fs::read_to_string(f).unwrap_or_default().replace('\r', "");
        let mut head: HashMap<String, String> = HashMap::new();
        if let Some(rest) = text.strip_prefix("---\n") {
            for l in rest.split("\n---").next().unwrap_or("").lines() {
                if let Some((k, v)) = l.split_once(':') { head.insert(k.trim().to_lowercase(), v.trim().trim_matches(['"', '\'']).to_string()); }
            }
        }
        let off = |k: &str| head.get(k).is_some_and(|v| ["false", "nao", "não", "no", "0", "off"].contains(&v.to_lowercase().as_str()));
        let last = st["runs"].as_array().into_iter().flatten().rev().find(|r| r["task"] == id.as_str()).map(|r| json!({
            "at": r["at"].as_u64().map(|ms| ms / 1000), "ok": r["ok"], "secs": r["ms"].as_u64().map(|ms| ms / 1000),
            "text": r["text"].as_str().map(|t| t.chars().take(2000).collect::<String>()), "error": r["error"],
        }));
        json!({"id": id, "title": head.get("title").cloned().unwrap_or_else(|| id.clone()),
            "schedule": head.get("cron").or(head.get("every")).cloned().unwrap_or_default(),
            "enabled": !off("enabled"), "paused": st["paused"][&id] == true, "last": last})
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resposta_so_com_agente_bloqueado() {
        assert_eq!(answer_keys(Some("blocked"), true), Ok("1"));
        assert_eq!(answer_keys(Some("blocked"), false), Ok("\x1b"));
        assert!(answer_keys(Some("working"), true).unwrap_err().contains("nao esta esperando"));
        assert!(answer_keys(Some("idle"), false).is_err());
        assert!(answer_keys(None, true).is_err());
        assert_eq!(interrupt_keys("ctrl-c"), Ok("\x03"));
        assert!(interrupt_keys("ctrl-z").is_err());
        // Pelo comando: pane com agente trabalhando recusa, bloqueado manda "1".
        let srv = testkit::server((80, 24));
        let id = testkit::add_panes(&srv, 1)[0];
        set_state(&mut srv.lock(), id, "working", Some("claude"), "hook");
        let e = run(&srv, &json!({"pane": id, "yes": true}), Caller(None), "agent-answer").unwrap_err();
        assert!(e.contains("nao esta esperando"), "{e}");
        set_state(&mut srv.lock(), id, "blocked", Some("claude"), "hook");
        assert!(run(&srv, &json!({"pane": id, "yes": true}), Caller(None), "agent-answer").is_ok());
    }

    #[test]
    fn nome_de_arquivo_saneado_e_dentro_da_pasta() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Windows\\system32\\x.dll"), "x.dll");
        assert_eq!(safe_file_name("..\\..\\a"), "a");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("foto\u{0}:*?<>|.jpg"), "foto.jpg");
        assert_eq!(safe_file_name(""), "arquivo");
        assert_eq!(safe_file_name(".."), "arquivo");
        let dir = std::env::temp_dir().join(format!("duck-fileput-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("a.txt"), "oi").unwrap();
        let d = dir.to_string_lossy().to_string();
        assert!(inside(&d, "sub/a.txt").is_ok());
        assert!(inside(&d, "../").unwrap_err().contains("dentro da pasta"));
        assert!(inside(&d, "/etc/hostname").is_err());
        // file-put pelo comando: cai na pasta do pane, sem sobrescrever.
        let srv = testkit::server((80, 24));
        let id = testkit::add_panes(&srv, 1)[0];
        srv.lock().panes.get_mut(&id).unwrap().cwd = d.clone();
        let data = base64::engine::general_purpose::STANDARD.encode(b"conteudo");
        let r = run(&srv, &json!({"pane": id, "name": "../../a.txt", "data": data}), Caller(None), "file-put").unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"conteudo");
        assert_eq!(r["name"], "a.txt");
        let r = run(&srv, &json!({"pane": id, "name": "a.txt", "data": data}), Caller(None), "file-put").unwrap();
        assert_eq!(r["name"], "a (2).txt");
        let g = run(&srv, &json!({"pane": id, "path": "a.txt"}), Caller(None), "file-get").unwrap();
        assert_eq!(g["data"], data.as_str());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pastas_e_rotinas() {
        let v = recent_dirs(vec!["/a/b".into(), "/c".into()], vec!["/c".into(), "/d/e/".into()], 10);
        let paths: Vec<&str> = v.iter().map(|x| x["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/a/b", "/c", "/d/e/"]);
        assert_eq!(v[2]["name"], "e");
        assert_eq!(v[2]["source"], "history");
        let dir = std::env::temp_dir().join(format!("duck-rotinas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("noticias.md"), "---\ntitle: Notícias\ncron: 0 9 * * 1-5\nenabled: false\n---\nPesquise").unwrap();
        std::fs::write(dir.join(".estado.json"), r#"{"last":{},"paused":{"noticias":true},"runs":[{"task":"noticias","title":"N","at":1700000000000,"ms":5000,"ok":true,"text":"ok"}]}"#).unwrap();
        let r = routines(&dir);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["title"], "Notícias");
        assert_eq!(r[0]["schedule"], "0 9 * * 1-5");
        assert_eq!(r[0]["enabled"], false);
        assert_eq!(r[0]["paused"], true);
        assert_eq!(r[0]["last"]["at"], 1_700_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
