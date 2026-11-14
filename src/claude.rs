//! Arquivos do Claude Code que o Duck le: transcripts das sessoes nos perfis
//! (~/.claude*/projects/*/<sessao>.jsonl) e o uso gravado pela statusline.
//! A janela (Inicio, linha do tempo) e o server (celular) usam os mesmos.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

/// Transcripts do Claude Code nos perfis (~/.claude*/projects) de uma home:
/// (mtime, perfil, sessao, caminho). Copias de seguranca ficam de fora.
pub fn scan_profiles(home: &Path) -> Vec<(u64, String, String, String)> {
    let mut found = Vec::new();
    for prof in std::fs::read_dir(home).into_iter().flatten().flatten() {
        let pname = prof.file_name().to_string_lossy().to_string();
        if !pname.starts_with(".claude") || pname.contains(".bak") || pname.contains("backup") || !prof.path().join("projects").is_dir() { continue; }
        for proj in std::fs::read_dir(prof.path().join("projects")).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(proj.path()).into_iter().flatten().flatten() {
                let p = f.path();
                if p.extension().is_none_or(|e| e != "jsonl") { continue; }
                found.push((mtime(&p), pname.clone(), p.file_stem().unwrap().to_string_lossy().to_string(), p.to_string_lossy().to_string()));
            }
        }
    }
    found
}

/// Transcript da sessao: <perfil>/projects/*/<sessao>.jsonl.
pub fn find_transcript(config_dir: &str, session: &str) -> Option<PathBuf> {
    let base = if config_dir.is_empty() { crate::config::home().join(".claude") } else { PathBuf::from(config_dir) };
    std::fs::read_dir(base.join("projects")).ok()?.flatten().map(|d| d.path().join(format!("{session}.jsonl"))).find(|p| p.is_file())
}

/// Nome do perfil para mostrar: ".claude" -> "pessoal", ".claude-totvs" -> "totvs".
pub fn profile_label(dir_name: &str) -> String {
    let p = dir_name.trim_start_matches(".claude").trim_start_matches('-');
    if p.is_empty() { "pessoal".into() } else { p.to_string() }
}

/// Comeco do arquivo (o cabecalho do transcript basta para pasta e titulo).
pub fn read_head(path: &str, max: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; max];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).to_string()
}

/// Pasta e primeiro pedido da sessao (titulo), pelo comeco do transcript.
pub fn session_head(path: &str) -> (String, String) {
    let (mut cwd, mut title) = (String::new(), String::new());
    for line in read_head(path, 96 * 1024).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if cwd.is_empty() { if let Some(c) = v["cwd"].as_str() { cwd = c.to_string(); } }
        if title.is_empty() && v["type"] == "user" {
            let c = &v["message"]["content"];
            let t = c.as_str().map(String::from).or_else(|| c.as_array().and_then(|a| a.iter().find_map(|x| x["text"].as_str().map(String::from)))).unwrap_or_default();
            if !t.starts_with('<') { title = t.replace('\n', " ").chars().take(80).collect(); }
        }
        if !cwd.is_empty() && !title.is_empty() { break; }
    }
    (cwd, title)
}

fn short(s: &str, n: usize) -> String { s.replace('\n', " ").chars().take(n).collect() }

/// Linha do tempo de um transcript: pedidos, ferramentas, arquivos e erros.
/// Cada item: {ts (ISO UTC do transcript), kind, text, file?}. kind:
/// prompt, error, edit, read, bash, task, tool.
pub fn timeline(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ts = v["timestamp"].as_str().unwrap_or("");
        let content = &v["message"]["content"];
        let item = |kind: &str, text: String| json!({"ts": ts, "kind": kind, "text": text});
        if v["type"] == "user" {
            if let Some(t) = content.as_str() {
                if !t.starts_with('<') { out.push(item("prompt", short(t, 120))); }
            }
            for b in content.as_array().into_iter().flatten() {
                if b["type"] == "tool_result" && b["is_error"] == true {
                    let msg = b["content"].as_str().map(String::from).or_else(|| b["content"].as_array().and_then(|a| a.iter().find_map(|x| x["text"].as_str().map(String::from)))).unwrap_or_default();
                    out.push(item("error", short(msg.lines().next().unwrap_or(""), 120)));
                } else if b["type"] == "text" {
                    let t = b["text"].as_str().unwrap_or("");
                    if !t.starts_with('<') && !t.is_empty() { out.push(item("prompt", short(t, 120))); }
                }
            }
        }
        if v["type"] == "assistant" {
            for b in content.as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                let name = b["name"].as_str().unwrap_or("");
                let i = &b["input"];
                let file = i["file_path"].as_str().or_else(|| i["notebook_path"].as_str()).unwrap_or("");
                let mut it = match name {
                    "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => item("edit", format!("{name} {file}")),
                    "Read" => item("read", format!("Read {file}")),
                    "Bash" | "PowerShell" => item("bash", format!("{name}: {}", short(i["command"].as_str().unwrap_or(""), 110))),
                    "TaskCreate" => item("task", format!("tarefa: {}", i["subject"].as_str().unwrap_or(""))),
                    "TaskUpdate" => item("task", format!("tarefa {} → {}", i["taskId"], i["status"].as_str().unwrap_or(""))),
                    _ => {
                        let d = i["pattern"].as_str().or_else(|| i["description"].as_str()).or_else(|| i["query"].as_str()).unwrap_or("");
                        item("tool", format!("{name} {}", short(d, 100)))
                    }
                };
                if !file.is_empty() { it["file"] = json!(file); }
                out.push(it);
            }
        }
    }
    out
}

/// Perfil dono da sessao (pasta ~/.claude*), procurando o transcript.
pub fn profile_of_session(home: &Path, session: &str) -> Option<String> {
    for prof in std::fs::read_dir(home).ok()?.flatten() {
        let pname = prof.file_name().to_string_lossy().to_string();
        if !pname.starts_with(".claude") || pname.contains(".bak") { continue; }
        let Ok(projs) = std::fs::read_dir(prof.path().join("projects")) else { continue };
        if projs.flatten().any(|d| d.path().join(format!("{session}.jsonl")).is_file()) { return Some(pname); }
    }
    None
}

/// Uso por conta (perfil do Claude): o registro mais novo da statusline de
/// cada perfil nos ultimos 8 dias. Cada item: {profile, config_dir, model,
/// updated, session, cost_usd, windows: [{key: "5h"|"7d", pct, resets}]}.
pub fn usage(home: &Path, usage_dir: &Path) -> Vec<Value> {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut files: Vec<(u64, PathBuf)> = std::fs::read_dir(usage_dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json")).map(|p| (mtime(&p), p)).filter(|(t, _)| now.saturating_sub(*t) < 8 * 86400).collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<Value> = Vec::new();
    for (t, p) in files.into_iter().take(200) {
        let Ok(v) = serde_json::from_slice::<Value>(&std::fs::read(&p).unwrap_or_default()) else { continue };
        let Some(sid) = v["session_id"].as_str() else { continue };
        let prof = profile_of_session(home, sid).unwrap_or_else(|| ".claude".into());
        let label = profile_label(&prof);
        if out.iter().any(|o| o["profile"] == label.as_str()) { continue; }
        let windows: Vec<Value> = v["windows"].as_array().into_iter().flatten().map(|w| json!({
            "key": crate::setup::window_label(w["Key"].as_str().unwrap_or("")), "pct": w["Pct"], "resets": w["Resets"],
        })).collect();
        out.push(json!({"profile": label, "config_dir": home.join(&prof), "model": v["model"], "updated": t, "session": sid, "cost_usd": v["cost_usd"], "windows": windows}));
    }
    out.sort_by(|a, b| a["profile"].as_str().cmp(&b["profile"].as_str()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linha_do_tempo() {
        let t = [
            r#"{"type":"user","timestamp":"2026-11-14T10:00:00Z","message":{"content":"arrume o build"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-14T10:00:05Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"src/a.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"error[E0308]: tipos\nmais"}]}}"#,
            "lixo",
        ].join("\n");
        let v = timeline(&t);
        let kinds: Vec<&str> = v.iter().map(|x| x["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["prompt", "edit", "bash", "error"]);
        assert_eq!(v[0]["ts"], "2026-11-14T10:00:00Z");
        assert_eq!(v[1]["file"], "src/a.rs");
        assert_eq!(v[2]["text"], "Bash: cargo build");
        assert_eq!(v[3]["text"], "error[E0308]: tipos");
        assert_eq!(profile_label(".claude"), "pessoal");
        assert_eq!(profile_label(".claude-totvs"), "totvs");
    }
}
