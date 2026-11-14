//! Duck: terminal com mux persistente e orquestracao de agentes.
//!
//! Dois executaveis, o mesmo codigo (como wezterm.exe / wezterm-gui.exe):
//!   duck.exe   console: CLI e subcomandos; sem argumentos abre a janela
//!               (via duckw.exe) e devolve o terminal na hora.
//!   duckw.exe  sem console: a janela (atalhos, boot) e os processos de fundo.
//!
//!   duck [--workspace NOME] [--restore] [--quick]   abre a janela
//!   duck server | ptyhost                           mux e host de pane (internos)
//!   duck cli <comando> [--chave valor]              API para agentes e scripts
//!   duck hook                                       hook do Claude Code
//!   duck doctor | run | fleet | mcp | view          ferramentas
//!   duck guard install|check|scan                   barra segredo/palavra no commit e no push
//!   duck why <pane> | queue ls|rm|up|now <pane>      estado do agente explicado; fila de recados
//!   duck mobile pair|list|revoke|scopes             celulares (QR, permissoes)
//!   duck init | install | plugin                    config, boot e plugins

pub mod cli;
pub mod config;
pub mod ext;
pub mod gui;
pub mod guard;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod peer;
pub mod secure;
pub mod relay;
pub mod fcm;
pub mod mobile;
pub mod plugins;
pub mod proto;
pub mod ptyhost;
pub mod claude;
pub mod colr;
pub mod render;
pub mod secret;
pub mod server;
pub mod setup;
pub mod toast;
pub mod translate;
pub mod update;

/// Executavel da janela (duckw.exe ao lado deste), se existir.
pub fn gui_exe() -> Option<std::path::PathBuf> {
    let me = proto::current_exe().ok()?;
    let name = if cfg!(windows) { "duckw.exe" } else { "duckw" };
    let p = me.with_file_name(name);
    p.is_file().then_some(p)
}

/// Roda `cmd` e deixa o shell aberto depois: pwsh -NoExit no Windows; fora
/// dele, sh e em seguida o $SHELL do usuario.
pub fn shell_keep(cmd: &str) -> serde_json::Value {
    if cfg!(windows) {
        serde_json::json!(["pwsh", "-NoLogo", "-NoProfile", "-NoExit", "-Command", cmd])
    } else {
        serde_json::json!(["sh", "-c", format!("{cmd}; exec \"${{SHELL:-sh}}\"")])
    }
}

pub fn entry(gui_binary: bool) -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    config::migrate_from_lasso();
    let code = match args.first().map(String::as_str) {
        Some("server") => { server::run(); 0 }
        Some("ptyhost") => ptyhost::run(args.get(1).map(String::as_str).unwrap_or("")),
        Some("cli") => cli::run(&args[1..]),
        Some("hook") => cli::hook(),
        Some("init") => cli::init(),
        Some("install") => cli::install(&args[1..]),
        Some("plugin") => cli::plugin(&args[1..]),
        Some("doctor") => cli::doctor(),
        Some("run") => cli::run_headless(&args[1..]),
        Some("fleet") => cli::fleet(&args[1..]),
        Some("mcp") => cli::mcp(),
        Some("view") => cli::view(&args[1..]),
        Some("play") => cli::play(&args[1..]),
        Some("url") => cli::url(&args[1..]),
        Some("quick") => cli::quick(),
        Some("secret") => cli::secret(&args[1..]),
        Some("msg") => cli::msg(&args[1..]),
        Some("why") => cli::why(&args[1..]),
        Some("queue") => cli::queue(&args[1..]),
        Some("peer") => cli::peer(&args[1..]),
        Some("mobile") => cli::mobile(&args[1..]),
        Some("send-file") => cli::send_file(&args[1..]),
        Some("relay") => relay::main(&args[1..]),
        Some("update") => update::main(&args[1..]),
        Some("statusline") => setup::statusline(),
        Some("config") => setup::config(&args[1..]),
        Some("guard") => guard::main(&args[1..]),
        Some("toast") => toast::run(args.get(1).map(String::as_str).unwrap_or("")),
        Some("--version" | "-V") => { println!("duck {}", env!("CARGO_PKG_VERSION")); 0 }
        Some("--help" | "-h") => {
            println!("duck [--workspace NOME] [--restore] [--quick] | cli <cmd> | hook | doctor | run --cmd C | fleet tarefas.txt | mcp | view <arq> | init | install [--hooks settings.json] [--statusline settings.json] [--quick] | config set <chave> <valor> | statusline | quick | plugin add|list|update | guard install|scan | why <pane> | queue ls|add|rm|up|now <pane> | msg <peer>[/aba] [--to aba] texto | send-file <peer> <arq> | peer list|find|connect|invite|join|doctor|sessions | mobile pair|list|revoke|scopes | relay | update [--check|--release|--source]");
            0
        }
        _ => {
            // Do console: abre a janela destacada e devolve o terminal.
            if !gui_binary {
                if let Some(w) = gui_exe() {
                    let mut argv = vec![w.to_string_lossy().to_string()];
                    argv.extend(args.iter().cloned());
                    std::process::exit(if proto::spawn_detached(&argv, &[]).is_ok() { 0 } else { 1 });
                }
            }
            let mut startup = Vec::new();
            let quick = args.iter().any(|a| a == "--quick");
            if quick { startup.push(serde_json::json!({"cmd": "workspace", "name": "quick"})); }
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--restore" => startup.push(serde_json::json!({"cmd": "checkpoint-restore"})),
                    "--workspace" if i + 1 < args.len() => {
                        startup.push(serde_json::json!({"cmd": "workspace", "name": args[i + 1]}));
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
            }
            // A janela age como o dono, mesmo aberta de dentro de um pane.
            std::env::remove_var("DUCK_TOKEN");
            std::env::remove_var("LASSO_TOKEN");
            gui::run(startup, quick);
            0
        }
    };
    std::process::exit(code);
}
