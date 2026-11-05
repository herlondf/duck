//! `duck guard`: barra commit e push com segredo ou palavra proibida.
//!
//!   duck guard install [--path DIR] [--force]   pre-commit, commit-msg e pre-push no repo
//!   duck guard uninstall [--path DIR]
//!   duck guard check --pre-commit | --pre-push   (chamado pelos hooks)
//!   duck guard scan [--path DIR] [--all]         procura no historico inteiro
//!
//! Segredos: os formatos de token de `config::SCREEN_SECRETS`. Palavras: a
//! lista `guard = { words = {...} }` do ~/.duck.lua (nomes de cliente, de
//! projeto...), que fica fora do repositorio. Casa a palavra inteira, sem
//! diferenciar maiusculas. Passar por cima: `git push --no-verify` ou
//! `DUCK_GUARD=off`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const MARK: &str = "# duck-guard";

pub fn main(args: &[String]) -> i32 {
    let flag = |k: &str| args.iter().any(|a| a == k);
    let opt = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let dir = PathBuf::from(opt("--path").unwrap_or_else(|| ".".into()));
    let r = match args.first().map(String::as_str) {
        Some("install") => install(&dir, flag("--force")),
        Some("uninstall") => uninstall(&dir),
        Some("check") => return match opt("--commit-msg") {
            Some(file) => check_msg(&file),
            None => check(&dir, if flag("--pre-push") { Mode::Push } else { Mode::Commit }),
        },
        Some("scan") => return scan(&dir, flag("--all")),
        _ => Err("uso: duck guard install|uninstall|check|scan [--path DIR]".into()),
    };
    match r {
        Ok(msg) => { println!("{msg}"); 0 }
        Err(e) => { eprintln!("{e}"); 1 }
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() { return Err(String::from_utf8_lossy(&out.stderr).trim().to_string()); }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn hooks_dir(dir: &Path) -> Result<PathBuf, String> {
    let p = git(dir, &["rev-parse", "--git-path", "hooks"])?.trim().to_string();
    let p = PathBuf::from(p);
    Ok(if p.is_absolute() { p } else { dir.join(p) })
}

/// duck.exe (o de console: o hook roda num shell, nao pode ser o duckw).
fn exe() -> String {
    let me = std::env::current_exe().unwrap_or_default();
    let name = if cfg!(windows) { "duck.exe" } else { "duck" };
    let p = me.with_file_name(name);
    let p = if p.is_file() { p } else { me };
    p.to_string_lossy().replace('\\', "/")
}

fn hook_text(kind: &str) -> String {
    if kind == "commit-msg" {
        return format!(r#"#!/bin/sh
{MARK} (duck guard install). Passar por cima: git commit --no-verify
"{exe}" guard check --commit-msg "$1" || exit 1
if [ -x "$0.local" ]; then exec "$0.local" "$@"; fi
"#, exe = exe());
    }
    if kind == "pre-commit" {
        // Sem stdin aqui (num terminal, ler o stdin travaria o commit).
        return format!(r#"#!/bin/sh
{MARK} (duck guard install). Passar por cima: git commit --no-verify
"{exe}" guard check --pre-commit < /dev/null || exit 1
if [ -x "$0.local" ]; then exec "$0.local" "$@"; fi
"#, exe = exe());
    }
    // O pre-push recebe as refs pelo stdin: guarda para passar ao hook antigo tambem.
    format!(r#"#!/bin/sh
{MARK} (duck guard install). Passar por cima: git {cmd} --no-verify
input=$(cat)
printf '%s\n' "$input" | "{exe}" guard check --{kind} || exit 1
if [ -x "$0.local" ]; then printf '%s\n' "$input" | "$0.local" "$@"; fi
"#, exe = exe(), cmd = "push")
}

pub fn install(dir: &Path, force: bool) -> Result<String, String> {
    let hooks_path = git(dir, &["config", "--get", "core.hooksPath"]).unwrap_or_default();
    if !hooks_path.trim().is_empty() && !force {
        return Err(format!("o repo usa core.hooksPath={} (husky?); rode com --force para instalar la mesmo assim", hooks_path.trim()));
    }
    let hooks = hooks_dir(dir)?;
    std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;
    for kind in ["pre-commit", "commit-msg", "pre-push"] {
        let path = hooks.join(kind);
        if let Ok(old) = std::fs::read_to_string(&path) {
            // Hook de outra ferramenta continua rodando depois do nosso.
            if !old.contains(MARK) { std::fs::rename(&path, hooks.join(format!("{kind}.local"))).map_err(|e| e.to_string())?; }
        }
        std::fs::write(&path, hook_text(kind)).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)); }
    }
    let n = words().len();
    Ok(format!("Guarda instalada em {} (pre-commit, commit-msg e pre-push): segredos{}.", hooks.display(),
        if n == 0 { " (sem palavras proibidas: ponha guard = { words = {...} } no ~/.duck.lua)".to_string() } else { format!(" e {n} palavra(s) proibida(s)") }))
}

fn uninstall(dir: &Path) -> Result<String, String> {
    let hooks = hooks_dir(dir)?;
    for kind in ["pre-commit", "commit-msg", "pre-push"] {
        let path = hooks.join(kind);
        if std::fs::read_to_string(&path).is_ok_and(|t| t.contains(MARK)) {
            let _ = std::fs::remove_file(&path);
            let local = hooks.join(format!("{kind}.local"));
            if local.exists() { let _ = std::fs::rename(&local, &path); }
        }
    }
    Ok("Guarda removida".into())
}

/// Instalada neste repo?
pub fn installed(dir: &Path) -> bool {
    hooks_dir(dir).is_ok_and(|h| std::fs::read_to_string(h.join("pre-push")).is_ok_and(|t| t.contains(MARK)))
}

fn words() -> Vec<String> {
    let (cfg, _) = crate::config::load_config();
    cfg.guard_words.into_iter().map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect()
}

/// Regras: (nome, regex). Segredo mostra so o comeco; palavra mostra ela.
fn rules() -> Vec<(String, regex::Regex)> {
    let mut v: Vec<(String, regex::Regex)> = crate::config::SCREEN_SECRETS.iter()
        .filter_map(|p| Some(("segredo".to_string(), regex::Regex::new(p).ok()?))).collect();
    for w in words() {
        let e = regex::escape(&w);
        // Borda de palavra so onde a palavra comeca/termina com letra ou numero.
        let a = if w.starts_with(|c: char| c.is_alphanumeric()) { r"\b" } else { "" };
        let b = if w.ends_with(|c: char| c.is_alphanumeric()) { r"\b" } else { "" };
        if let Ok(re) = regex::Regex::new(&format!("(?i){a}{e}{b}")) { v.push((format!("palavra \"{w}\""), re)); }
    }
    v
}

#[derive(PartialEq)]
enum Mode { Commit, Push }

struct Hit { place: String, rule: String, text: String }

/// Varre a saida de `git diff`/`git log -p`: linhas adicionadas, nomes de
/// arquivo e mensagens de commit.
fn scan_patch(patch: &str, rules: &[(String, regex::Regex)], out: &mut Vec<Hit>) {
    let mut file = String::new();
    let mut line_no = 0i64;
    let mut commit = String::new();
    for l in patch.lines() {
        let (what, text): (String, &str) = if let Some(c) = l.strip_prefix("commit ") {
            commit = c.chars().take(8).collect();
            continue;
        } else if let Some(p) = l.strip_prefix("diff --git ") {
            (format!("caminho {p}"), p)
        } else if let Some(p) = l.strip_prefix("+++ ") {
            file = p.trim_start_matches("b/").to_string();
            continue;
        } else if l.starts_with("@@") {
            // @@ -a,b +c,d @@
            line_no = l.split('+').nth(1).and_then(|s| s.split([',', ' ']).next()).and_then(|n| n.parse().ok()).unwrap_or(0) - 1;
            continue;
        } else if let Some(t) = l.strip_prefix('+') {
            line_no += 1;
            (format!("{file}:{line_no}"), t)
        } else if l.starts_with("    ") && !commit.is_empty() {
            (format!("mensagem do commit {commit}"), l.trim())
        } else {
            if l.starts_with(' ') { line_no += 1; }
            continue;
        };
        for (name, re) in rules {
            if let Some(m) = re.find(text) {
                let shown = if name == "segredo" { format!("{}…", m.as_str().chars().take(6).collect::<String>()) } else { m.as_str().to_string() };
                let place = if commit.is_empty() || what.starts_with("mensagem") { what.clone() } else { format!("{what} ({commit})") };
                out.push(Hit { place, rule: name.clone(), text: shown });
            }
        }
    }
}

fn report(hits: &[Hit], what: &str) -> i32 {
    if hits.is_empty() { return 0; }
    eprintln!("duck guard: {what} barrado ({} ocorrencia(s)):", hits.len());
    for h in hits.iter().take(30) { eprintln!("  {}  {}: {}", h.place, h.rule, h.text); }
    if hits.len() > 30 { eprintln!("  ... e mais {}", hits.len() - 30); }
    eprintln!("Corrija e tente de novo. Se for falso positivo: git {} --no-verify (ou DUCK_GUARD=off).", if what == "push" { "push" } else { "commit" });
    1
}

fn check(dir: &Path, mode: Mode) -> i32 {
    if std::env::var("DUCK_GUARD").is_ok_and(|v| v == "off" || v == "0") { return 0; }
    let rules = rules();
    let mut hits = Vec::new();
    if mode == Mode::Commit {
        match git(dir, &["diff", "--cached", "-U0", "--no-color", "--no-ext-diff"]) {
            Ok(p) => scan_patch(&p, &rules, &mut hits),
            Err(e) => { eprintln!("duck guard: {e}"); return 0; }
        }
        // Mensagem do commit ainda nao existe aqui; o pre-push confere depois.
        return report(&hits, "commit");
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    for l in input.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 || f[1].chars().all(|c| c == '0') { continue; } // apagando a ref remota
        let range = if f[3].chars().all(|c| c == '0') { vec![f[1].to_string(), "--not".into(), "--remotes".into()] } else { vec![format!("{}..{}", f[3], f[1])] };
        let mut a = vec!["log", "-p", "-U0", "--no-color", "--no-ext-diff", "--format=commit %H%n%n%w(0,4,4)%B"];
        a.extend(range.iter().map(String::as_str));
        match git(dir, &a) {
            Ok(p) => scan_patch(&p, &rules, &mut hits),
            Err(e) => eprintln!("duck guard: {e}"),
        }
    }
    report(&hits, "push")
}

/// Mensagem do commit (hook commit-msg): as linhas que nao sao comentario.
fn check_msg(file: &str) -> i32 {
    if std::env::var("DUCK_GUARD").is_ok_and(|v| v == "off" || v == "0") { return 0; }
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let rules = rules();
    let mut hits = Vec::new();
    for l in text.lines().filter(|l| !l.starts_with('#')) {
        for (name, re) in &rules {
            if let Some(m) = re.find(l) {
                let shown = if name == "segredo" { format!("{}…", m.as_str().chars().take(6).collect::<String>()) } else { m.as_str().to_string() };
                hits.push(Hit { place: "mensagem do commit".into(), rule: name.clone(), text: shown });
            }
        }
    }
    report(&hits, "commit")
}

fn scan(dir: &Path, all: bool) -> i32 {
    let rules = rules();
    let mut a = vec!["log", "-p", "-U0", "--no-color", "--no-ext-diff", "--format=commit %H%n%n%w(0,4,4)%B"];
    if all { a.push("--all"); }
    let mut hits = Vec::new();
    match git(dir, &a) {
        Ok(p) => scan_patch(&p, &rules, &mut hits),
        Err(e) => { eprintln!("{e}"); return 1; }
    }
    if hits.is_empty() { println!("Nada encontrado no historico ({} regra(s)).", rules.len()); return 0; }
    for h in &hits { println!("{}  {}: {}", h.place, h.rule, h.text); }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acha_segredo_e_palavra_no_patch() {
        let rules = vec![
            ("segredo".to_string(), regex::Regex::new(crate::config::SCREEN_SECRETS[0]).unwrap()),
            ("palavra \"acme\"".to_string(), regex::Regex::new(r"(?i)\bacme\b").unwrap()),
        ];
        let patch = "commit 0123456789abcdef\n\n    Ajuste para a ACME\n\ndiff --git a/x.txt b/x.txt\n+++ b/x.txt\n@@ -0,0 +1,2 @@\n+ok\n+token ghp_abcdefghijklmnopqrstuvwxyz0123456789\n";
        let mut hits = Vec::new();
        scan_patch(patch, &rules, &mut hits);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].place, "mensagem do commit 01234567");
        assert_eq!(hits[1].place, "x.txt:2 (01234567)");
        assert_eq!(hits[1].text, "ghp_ab…");
        // "acmes" e "macme" nao casam (palavra inteira).
        let mut h2 = Vec::new();
        scan_patch("+++ b/y\n@@ -0,0 +1 @@\n+acmes macme\n", &rules, &mut h2);
        assert!(h2.is_empty());
    }
}
