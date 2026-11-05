//! Melhor de N: a mesma tarefa para N agentes, cada um na sua worktree (e
//! com a sua porta), lado a lado num workspace. Com verificacao, cada um
//! entra no loop ate passar. Depois a tela de comparacao mostra, por
//! candidato, o loop, o estado do agente e o tamanho da mudanca; escolher
//! um faz commit do que ficou solto, mescla na branch base e remove as
//! outras worktrees.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{git, worktree_port, Gui, Item, Overlay, WorktreeCfg};
use crate::proto::Conn;

#[derive(Serialize, Deserialize, Clone, Default)]
pub(super) struct Run {
    pub task: String,
    pub check: String,
    /// Repo principal e a branch onde o vencedor entra.
    pub main: String,
    pub base: String,
    pub workspace: String,
    pub at: u64,
    pub entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(super) struct Entry { pub pane: u64, pub branch: String, pub dir: String, pub port: u16 }

fn runs_file() -> std::path::PathBuf { crate::proto::data_dir().join("best-of.json") }

pub(super) fn load_runs() -> Vec<Run> {
    std::fs::read(runs_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_runs(v: &[Run]) {
    let keep: Vec<&Run> = v.iter().rev().take(10).rev().collect();
    if let Ok(b) = serde_json::to_vec_pretty(&keep) { let _ = std::fs::write(runs_file(), b); }
}

/// "3 cargo test" -> (3, "cargo test"); "" -> (2, ""); "cargo test" -> (2, "cargo test").
pub(super) fn parse_spec(spec: &str) -> (usize, String) {
    let spec = spec.trim();
    match spec.split_once(' ').map(|(a, b)| (a.parse::<usize>(), b)) {
        Some((Ok(n), rest)) => (n.clamp(2, 6), rest.trim().to_string()),
        _ => match spec.parse::<usize>() { Ok(n) => (n.clamp(2, 6), String::new()), Err(_) => (2, spec.to_string()) },
    }
}

impl Gui {
    /// Cria a worktree `<repo>-<branch>` com a branch nova a partir do HEAD
    /// do repo do pane ativo: (pasta, linha que sobe o agente, porta, copiados).
    pub(super) fn make_worktree(&self, cwd: &str, profile: usize, branch: &str) -> Result<(String, String, u16, usize, std::path::PathBuf), String> {
        let top = git(cwd, &["rev-parse", "--show-toplevel"]).ok_or("O pane ativo nao esta num repositorio git")?;
        let top = std::path::PathBuf::from(top.trim());
        let name = top.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "repo".into());
        let dest = top.with_file_name(format!("{name}-{}", branch.replace('/', "-")));
        let dest_s = dest.to_string_lossy().to_string();
        if git(&top.to_string_lossy(), &["worktree", "add", "-b", branch, &dest_s]).is_none() {
            return Err(format!("git worktree add falhou (a branch {branch} ja existe?)"));
        }
        let cmd = self.cfg.agent_profiles.get(profile).map(|p| p.cmd.clone()).unwrap_or_else(|| "claude".into());
        let port = worktree_port(&dest_s);
        let wt = WorktreeCfg::load(&top);
        let copied = wt.copy_into(&top, &dest);
        Ok((dest_s, wt.command(port, &cmd), port, copied, top))
    }

    pub(super) fn best_prompt(&mut self) {
        self.overlay = Some(Overlay::prompt("Melhor de N: a tarefa (vai igual para todos os agentes)", "", "best_task", vec![]));
    }

    pub(super) fn best_task(&mut self, task: String) {
        if task.trim().is_empty() { return; }
        self.overlay = Some(Overlay::prompt("Quantos agentes e o comando que verifica (ex.: 3 cargo test; so o numero = sem verificacao)", "3", "best_go", vec![json!(task)]));
    }

    pub(super) fn best_start(&mut self, task: String, spec: String) {
        let (n, check) = parse_spec(&spec);
        let cwd = self.active().map(|p| p.cwd.clone()).unwrap_or_default();
        let Some(top) = git(&cwd, &["rev-parse", "--show-toplevel"]) else { return self.flash("O pane ativo nao esta num repositorio git") };
        let main = top.trim().to_string();
        let base = git(&main, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
        let stamp = chrono_stamp();
        let workspace = format!("melhor de {n} · {stamp}");
        let mut run = Run { task: task.clone(), check: check.clone(), main: main.clone(), base, workspace: workspace.clone(), at: now_secs(), entries: vec![] };
        let mut first: Option<u64> = None;
        for i in 0..n {
            let letter = (b'a' + i as u8) as char;
            let branch = format!("best/{stamp}-{letter}");
            let (dir, line, port, _, _) = match self.make_worktree(&cwd, 0, &branch) { Ok(x) => x, Err(e) => { self.flash(&e); break; } };
            let mut req = json!({"cmd": "spawn", "cwd": dir, "type": line, "focus": false, "title": format!("⎇ {letter}")});
            match first {
                None => { req["where"] = json!("tab"); req["workspace"] = json!(workspace); }
                Some(p) => { req["where"] = json!("right"); req["pane"] = json!(p); }
            }
            match self.call(req) {
                Ok(v) => {
                    let pane = v["pane"].as_u64().unwrap_or(0);
                    first.get_or_insert(pane);
                    run.entries.push(Entry { pane, branch, dir, port });
                }
                Err(e) => { self.flash(&e); break; }
            }
        }
        if run.entries.is_empty() { return; }
        self.cmd(json!({"cmd": "workspace", "name": workspace}));
        let mut runs = load_runs();
        runs.push(run.clone());
        save_runs(&runs);
        // Entrega a tarefa quando cada agente subir (fora da thread da janela).
        std::thread::spawn(move || deliver(run));
        self.flash(&format!("Melhor de {n}: {} agentes subindo{}; Ctrl+Shift+P → \"comparar\" quando terminarem",
            n, if check.is_empty() { String::new() } else { format!(", cada um em loop ate `{check}` passar") }));
    }

    /// Comparacao da ultima rodada: loop, estado e tamanho da mudanca de cada um.
    pub(super) fn best_compare(&mut self) {
        let Some(run) = load_runs().pop() else { return self.flash("Nenhuma rodada de melhor de N (Agentes: melhor de N)") };
        let states = self.call(json!({"cmd": "list"})).unwrap_or_default();
        let loops = self.call(json!({"cmd": "loops"})).unwrap_or_default();
        let mut items = Vec::new();
        for (i, e) in run.entries.iter().enumerate() {
            let letter = (b'a' + i as u8) as char;
            let state = find_agent_state(&states, e.pane);
            let lp = loops["loops"].as_array().into_iter().flatten().find(|l| l["pane"].as_u64() == Some(e.pane));
            let lp_s = lp.map(|l| match l["phase"].as_str().unwrap_or("") {
                "passed" => format!("✓ passou em {}", l["iter"]),
                "failed" => format!("✗ falhou {} vez(es)", l["iter"]),
                "stopped" => "loop parado".to_string(),
                _ => format!("⟳ volta {}/{}", l["iter"].as_u64().unwrap_or(0) + 1, l["max"]),
            }).unwrap_or_else(|| if run.check.is_empty() { "sem verificacao".into() } else { "loop ainda nao comecou".into() });
            // Mudanca contra o commit de partida (commits + o que esta solto).
            let from = git(&e.dir, &["merge-base", &run.base, "HEAD"]).map(|s| s.trim().to_string()).unwrap_or_else(|| run.base.clone());
            let stat = git(&e.dir, &["diff", "--shortstat", &from]).map(|s| short_stat(&s)).unwrap_or_else(|| "?".into());
            let untracked = git(&e.dir, &["ls-files", "--others", "--exclude-standard"]).map(|s| s.lines().count()).unwrap_or(0);
            let st = match state.as_str() { "working" => "trabalhando", "blocked" => "pede aprovacao", "done" => "terminou", "idle" => "parado", "" => "sem agente", s => s };
            items.push(Item::new(format!("{letter}: {lp_s} · {stat}{} · {st}", if untracked > 0 { format!(" +{untracked} novo(s)") } else { String::new() }),
                format!("{} · porta {}", e.branch, e.port), "best_pick", vec![json!(i)]));
        }
        let title = format!("Melhor de {}: {} (Enter: diff, abrir ou escolher)", run.entries.len(), run.task.chars().take(60).collect::<String>());
        self.overlay = Some(Overlay::list(&title, items));
    }

    pub(super) fn best_pick(&mut self, i: usize) {
        let Some(run) = load_runs().pop() else { return };
        let Some(e) = run.entries.get(i).cloned() else { return };
        let ps = |c: String| json!(["pwsh", "-NoLogo", "-NoProfile", "-NoExit", "-Command", c]);
        let dir = &e.dir;
        let base = &run.base;
        let main = &run.main;
        let msg = run.task.replace('\'', "''").chars().take(70).collect::<String>();
        // Escolher: commit do que ficou solto, merge na base, remove as outras.
        let mut losers = String::new();
        for o in run.entries.iter().filter(|o| o.pane != e.pane) {
            losers.push_str(&format!("git -C '{main}' worktree remove --force '{}'; git -C '{main}' branch -D {}; ", o.dir, o.branch));
        }
        let pick = format!("Start-Sleep 2; Set-Location '{dir}'; git add -A; if (git status --porcelain) {{ git commit -m 'melhor de N: {msg}' }}; \
            git -C '{main}' merge --no-ff {} -m 'Melhor de N: {msg}'; if ($LASTEXITCODE -eq 0) {{ {losers}'Pronto: {} mesclada em {base}. A worktree dela continua (Git: finalizar worktree remove).' }}",
            e.branch, e.branch);
        let items = vec![
            Item::new("Ver o diff contra a base", "flutuante", "popup",
                vec![ps(format!("git -C '{dir}' -c color.ui=always --no-pager diff --stat {base}; git -C '{dir}' -c color.ui=always --no-pager diff {base}"))]),
            Item::new("Ir para o pane deste agente", "", "focus_pane", vec![json!(e.pane)]),
            Item::new(format!("Escolher este: commit, mesclar em {base} e remover os outros"), main.clone(), "best_choose", vec![json!(i), ps(pick)]),
            Item::new("Cancelar", "Esc", "", vec![]),
        ];
        self.overlay = Some(Overlay::dialog(&format!("{} ({})", e.branch, e.dir), items));
    }

    pub(super) fn best_choose(&mut self, i: usize, argv: Value) {
        let Some(run) = load_runs().pop() else { return };
        // Os panes dos outros fecham (as worktrees deles vao embora no script).
        for (k, o) in run.entries.iter().enumerate() { if k != i { self.cmd(json!({"cmd": "close", "pane": o.pane})); } }
        self.builtin("popup", &[argv]);
    }
}

fn find_agent_state(list: &Value, pane: u64) -> String {
    for w in list["workspaces"].as_array().into_iter().flatten() {
        for t in w["tabs"].as_array().into_iter().flatten() {
            for p in t["panes"].as_array().into_iter().flatten() {
                if p["id"].as_u64() == Some(pane) { return p["agent"]["state"].as_str().unwrap_or("").to_string(); }
            }
        }
    }
    String::new()
}

/// " 3 files changed, 40 insertions(+), 2 deletions(-)" -> "3 arq +40 −2".
fn short_stat(s: &str) -> String {
    let num = |k: &str| s.split(',').find(|p| p.contains(k)).and_then(|p| p.split_whitespace().next()).unwrap_or("0").to_string();
    if s.trim().is_empty() { return "sem mudanca".into(); }
    format!("{} arq +{} −{}", num("file"), num("insertion"), num("deletion"))
}

fn now_secs() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

/// MMDD-HHMM local (nome curto e unico o bastante para a branch).
fn chrono_stamp() -> String {
    let out = std::process::Command::new("pwsh").args(["-NoProfile", "-Command", "Get-Date -Format MMdd-HHmm"]).output();
    out.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| s.len() == 9)
        .unwrap_or_else(|| format!("{}", now_secs() % 1_000_000))
}

/// Espera cada agente subir e entrega a tarefa (em loop, se tem verificacao).
fn deliver(run: Run) {
    let Ok(mut c) = Conn::connect() else { return };
    for e in &run.entries {
        let start = std::time::Instant::now();
        loop {
            let req = if run.check.is_empty() {
                // Sem verificacao: recado (entregue quando o agente estiver parado).
                let st = c.call(json!({"cmd": "list"})).map(|l| find_agent_state(&l, e.pane)).unwrap_or_default();
                if st.is_empty() { None } else { Some(json!({"cmd": "queue", "pane": e.pane, "text": run.task})) }
            } else {
                Some(json!({"cmd": "loop-start", "pane": e.pane, "check": run.check, "max": 5, "task": run.task}))
            };
            if let Some(r) = req {
                if c.call(r).is_ok() { break; }
            }
            if start.elapsed() > std::time::Duration::from_secs(60) { break; }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spec_e_stat() {
        assert_eq!(parse_spec("3 cargo test"), (3, "cargo test".into()));
        assert_eq!(parse_spec("4"), (4, String::new()));
        assert_eq!(parse_spec(""), (2, String::new()));
        assert_eq!(parse_spec("npm test"), (2, "npm test".into()));
        assert_eq!(parse_spec("9 x"), (6, "x".into()));
        assert_eq!(short_stat(" 3 files changed, 40 insertions(+), 2 deletions(-)\n"), "3 arq +40 −2");
        assert_eq!(short_stat(" 1 file changed, 1 deletion(-)"), "1 arq +0 −1");
    }
}
