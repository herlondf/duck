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
//!   duck init | install | plugin                    config, boot e plugins

pub mod cli;
pub mod config;
pub mod ext;
pub mod gui;
pub mod plugins;
pub mod proto;
pub mod ptyhost;
pub mod render;
pub mod secret;
pub mod server;

/// Executavel da janela (duckw.exe ao lado deste), se existir.
pub fn gui_exe() -> Option<std::path::PathBuf> {
    let me = std::env::current_exe().ok()?;
    let name = if cfg!(windows) { "duckw.exe" } else { "duckw" };
    let p = me.with_file_name(name);
    p.is_file().then_some(p)
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
        Some("secret") => cli::secret(&args[1..]),
        Some("--version" | "-V") => { println!("duck {}", env!("CARGO_PKG_VERSION")); 0 }
        Some("--help" | "-h") => {
            println!("duck [--workspace NOME] [--restore] [--quick] | cli <cmd> | hook | doctor | run --cmd C | fleet tarefas.txt | mcp | view <arq> | init | install [--hooks settings.json] [--quick] | plugin add|list|update");
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
