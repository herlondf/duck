//! Pecas do instalador (install.sh / install.ps1) que precisam do Duck:
//!
//!   duck statusline                       statusline do Claude Code (stdin JSON)
//!   duck install --statusline <settings>  liga a statusline do Duck num perfil
//!   duck config set <chave> <valor-lua>   grava no ~/.duck-local.lua (ex.: update_auto false)
//!
//! A statusline grava um JSON por sessao em ~/AppData/Local/Temp/claude-usage
//! (o mesmo caminho em todo sistema, e o que a extensao "Uso do Claude" e as
//! Rotinas leem) e imprime a linha: modelo, 5h, 7d, custo. No Windows o
//! install.ps1 usa o duck-statusline.ps1, que faz o mesmo pelo pwsh.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Pasta dos JSONs de uso por sessao.
pub fn usage_dir() -> PathBuf { crate::config::home().join("AppData").join("Local").join("Temp").join("claude-usage") }

/// "five_hour" -> "5h", "seven_day" -> "7d" (os nomes ja mudaram entre versoes).
pub fn window_label(k: &str) -> String {
    let l = k.to_lowercase();
    if l.contains("five_hour") || l.contains("5h") || l.contains("5_h") { "5h".into() }
    else if l.contains("seven_day") || l.contains("7d") || l.contains("7_d") || l.contains("week") { "7d".into() }
    else { k.to_string() }
}

/// (linha da statusline, registro da sessao para gravar). Sem `session_id`
/// nao grava nada.
pub fn statusline_parts(d: &Value, ts: &str) -> (String, Option<Value>) {
    let mut windows = Vec::new();
    if let Some(rl) = d["rate_limits"].as_object() {
        for (k, v) in rl {
            if let Some(p) = v["used_percentage"].as_f64() {
                windows.push(json!({"Key": k, "Pct": p, "Resets": v["resets_at"]}));
            }
        }
    }
    let mut segs: Vec<String> = Vec::new();
    if let Some(m) = d["model"]["display_name"].as_str() { segs.push(m.to_string()); }
    for w in &windows {
        segs.push(format!("{} {:.0}%", window_label(w["Key"].as_str().unwrap_or("")), w["Pct"].as_f64().unwrap_or(0.0)));
    }
    let cost = d["cost"]["total_cost_usd"].as_f64();
    if let Some(c) = cost.filter(|c| *c > 0.0) { segs.push(format!("${c:.2}")); }
    let rec = d["session_id"].as_str().filter(|s| !s.is_empty()).map(|sid| json!({
        "ts": ts, "session_id": sid, "model": d["model"]["display_name"], "version": d["version"],
        "cwd": d["workspace"]["current_dir"], "cost_usd": cost, "rate_limits": d["rate_limits"], "windows": windows,
    }));
    (segs.join(" · "), rec)
}

/// Data e hora UTC em ISO 8601 (o `Date.parse` das extensoes le).
pub fn iso_utc(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Dias desde 1970 -> data civil (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `duck statusline`: le o JSON do Claude Code no stdin.
pub fn statusline() -> i32 {
    let mut raw = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw);
    let Ok(d) = serde_json::from_str::<Value>(&raw) else { return 0 };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (line, rec) = statusline_parts(&d, &iso_utc(now));
    if let Some(rec) = rec {
        let dir = usage_dir();
        let sid: String = rec["session_id"].as_str().unwrap_or("").chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        if !sid.is_empty() && std::fs::create_dir_all(&dir).is_ok() {
            // Atomico: a extensao nunca le arquivo pela metade.
            let (f, tmp) = (dir.join(format!("{sid}.json")), dir.join(format!("{sid}.json.tmp")));
            if std::fs::write(&tmp, rec.to_string()).is_ok() { let _ = std::fs::rename(&tmp, &f); }
        }
    }
    println!("{line}");
    0
}

/// Comando da statusline do Duck neste sistema. No Windows o Claude Code pode
/// rodar o comando pelo PowerShell, onde `"C:/.../duck.exe" statusline` e
/// erro de sintaxe: la vai o script pwsh ao lado do duck.exe.
pub fn statusline_cmd(exe: &Path, windows: bool) -> String {
    if windows {
        let script = exe.parent().unwrap_or(Path::new(".")).join("duck-statusline.ps1");
        format!("pwsh -NoProfile -File \"{}\"", script.display())
    } else {
        format!("\"{}\" statusline", exe.display())
    }
}

/// A statusline e a do Duck? (`duck statusline` ou o duck-statusline.ps1).
pub fn is_duck_statusline(v: &Value) -> bool {
    let c = v["statusLine"]["command"].as_str().unwrap_or("");
    c.contains("duck-statusline.ps1") || c.trim().strip_suffix(" statusline").is_some_and(|p| {
        let p = p.trim().trim_matches('"');
        let base = p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase();
        base == "duck" || base == "duck.exe"
    })
}

/// Poe a statusline do Duck so onde nao ha nenhuma (a sua, se existir, fica)
/// ou troca a do proprio Duck (caminho mudou). Devolve se mudou algo.
pub fn apply_statusline(v: &mut Value, cmd: &str) -> Result<bool, String> {
    let Some(o) = v.as_object_mut() else { return Err("settings.json nao e um objeto JSON".into()) };
    let want = json!({"type": "command", "command": cmd, "padding": 0});
    let ours = o.get("statusLine").is_some_and(|s| is_duck_statusline(&json!({"statusLine": s})));
    match o.get("statusLine") {
        Some(s) if s == &want => Ok(false),
        Some(_) if !ours => Ok(false),
        _ => { o.insert("statusLine".into(), want); Ok(true) }
    }
}

/// Estado da statusline num perfil, para o `duck doctor`.
#[derive(Debug, PartialEq)]
pub enum StatusState { Duck, Other, Missing }

pub fn statusline_state(settings: &Path) -> StatusState {
    let v: Value = std::fs::read_to_string(settings).ok().and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null);
    if v.get("statusLine").is_none() { StatusState::Missing }
    else if is_duck_statusline(&v) { StatusState::Duck }
    else { StatusState::Other }
}

/// `duck install --statusline <settings.json>`.
pub fn install_statusline(path: &str, exe: &Path) -> i32 {
    let p = Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) { eprintln!("{}: {e}", dir.display()); return 1; }
    }
    let raw = std::fs::read_to_string(p).unwrap_or_default();
    let raw = if raw.trim().is_empty() { "{}".to_string() } else { raw };
    let mut v: Value = match serde_json::from_str(&raw) { Ok(v) => v, Err(e) => { eprintln!("{path}: {e}"); return 1 } };
    let had_other = v.get("statusLine").is_some() && !is_duck_statusline(&v);
    match apply_statusline(&mut v, &statusline_cmd(exe, cfg!(windows))) {
        Ok(false) if had_other => { println!("statusline sua mantida em {path} (para o Uso do Claude, chame `duck statusline` de dentro dela)"); 0 }
        Ok(false) => { println!("statusline do Duck ja estava em {path}"); 0 }
        Ok(true) => {
            let _ = std::fs::write(format!("{path}.bak-duck"), &raw);
            match std::fs::write(p, serde_json::to_string_pretty(&v).unwrap()) {
                Ok(_) => { println!("statusline do Duck em {path}"); 0 }
                Err(e) => { eprintln!("{e}"); 1 }
            }
        }
        Err(e) => { eprintln!("{path}: {e}"); 1 }
    }
}

/// `duck config set <chave> <valor-lua>`: o valor vai como esta (true, 13,
/// 'texto'). Uma palavra que nao e numero nem booleano vira texto.
pub fn config(args: &[String]) -> i32 {
    match args {
        [cmd, key, value] if cmd == "set" => {
            let v = lua_value(value);
            match crate::config::set_local_setting(key, &v) {
                Ok(()) => { println!("{key} = {v} em {}", crate::config::local_path().display()); 0 }
                Err(e) => { eprintln!("duck config: {e}"); 1 }
            }
        }
        _ => { eprintln!("uso: duck config set <chave> <valor>   (ex.: duck config set update_auto false)"); 2 }
    }
}

fn lua_value(v: &str) -> String {
    let t = v.trim();
    let quoted = (t.starts_with('\'') && t.ends_with('\'') && t.len() > 1) || (t.starts_with('"') && t.ends_with('"') && t.len() > 1);
    if quoted || t == "true" || t == "false" || t == "nil" || t.parse::<f64>().is_ok() { t.to_string() }
    else { format!("'{}'", t.replace('\\', "\\\\").replace('\'', "\\'")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statusline_line_and_record() {
        let d = json!({"session_id": "abc-1", "model": {"display_name": "Opus"}, "version": "2.1.90",
            "workspace": {"current_dir": "/w"}, "cost": {"total_cost_usd": 1.234},
            "rate_limits": {"five_hour": {"used_percentage": 41.6, "resets_at": 100}, "seven_day": {"used_percentage": 7.0, "resets_at": 200}}});
        let (line, rec) = statusline_parts(&d, "2026-01-01T00:00:00Z");
        assert_eq!(line, "Opus · 5h 42% · 7d 7% · $1.23");
        let rec = rec.unwrap();
        assert_eq!(rec["session_id"], "abc-1");
        assert_eq!(rec["rate_limits"]["five_hour"]["resets_at"], 100);
        assert_eq!(rec["ts"], "2026-01-01T00:00:00Z");
        assert_eq!(rec["windows"].as_array().unwrap().len(), 2);
        // Sem sessao: so a linha.
        let (line, rec) = statusline_parts(&json!({"model": {"display_name": "Sonnet"}}), "x");
        assert_eq!((line.as_str(), rec), ("Sonnet", None));
    }

    #[test]
    fn iso() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_793_580_245), "2026-11-02T00:44:05Z");
    }

    #[test]
    fn statusline_settings() {
        let cmd = statusline_cmd(Path::new("/home/u/.local/bin/duck"), false);
        assert_eq!(cmd, "\"/home/u/.local/bin/duck\" statusline");
        let mut v = json!({"model": "opus"});
        assert!(apply_statusline(&mut v, &cmd).unwrap());
        assert!(is_duck_statusline(&v));
        assert!(!apply_statusline(&mut v, &cmd).unwrap());
        // Caminho novo do Duck: troca a do Duck.
        assert!(apply_statusline(&mut v, "\"/opt/duck\" statusline").unwrap());
        // A do usuario fica.
        let mut mine = json!({"statusLine": {"type": "command", "command": "~/bin/minha.sh"}});
        assert!(!apply_statusline(&mut mine, &cmd).unwrap());
        assert_eq!(mine["statusLine"]["command"], "~/bin/minha.sh");
        // A do install.ps1 conta como do Duck.
        assert!(is_duck_statusline(&json!({"statusLine": {"command": "pwsh -NoProfile -File \"C:\\D\\bin\\duck-statusline.ps1\""}})));
        assert!(!is_duck_statusline(&json!({"statusLine": {"command": "\"/x/duckdns\" statusline"}})));
        assert!(apply_statusline(&mut json!([]), &cmd).is_err());
        let w = statusline_cmd(Path::new("C:/D/bin/duck.exe"), true);
        assert!(w.starts_with("pwsh -NoProfile -File \"") && w.contains("duck-statusline.ps1"));
    }

    #[test]
    fn lua_values() {
        assert_eq!(lua_value("false"), "false");
        assert_eq!(lua_value("13"), "13");
        assert_eq!(lua_value("'x'"), "'x'");
        assert_eq!(lua_value("dark mode"), "'dark mode'");
        assert_eq!(lua_value("it's"), "'it\\'s'");
    }
}
