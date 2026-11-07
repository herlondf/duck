//! Config em Lua: `~/.duck.lua` (devolve uma tabela) + `~/.duck-local.lua`
//! opcional, rodado depois com a tabela em `config` para ajustes da maquina.
//!
//! O server so le os dados (shell, dominios, hooks). A GUI le os dados e
//! guarda a VM Lua viva, porque atalhos, barra de status e paleta podem ser
//! funcoes.

use std::collections::HashMap;
use std::path::PathBuf;

use mlua::{Lua, Table, Value as LuaValue};

pub const DEFAULT_CONFIG: &str = include_str!("../duck.example.lua");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim_start_matches('#');
        if h.len() != 6 { return None; }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
    pub fn f(self) -> [f32; 3] { [self.0 as f32 / 255.0, self.1 as f32 / 255.0, self.2 as f32 / 255.0] }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub palette: [Rgb; 16],
    pub tab_bar: Rgb,
    pub tab_active: Rgb,
    pub muted: Rgb,
    pub border: Rgb,
    pub accent: Rgb,
    pub working: Rgb,
    pub blocked: Rgb,
    pub done: Rgb,
    pub idle: Rgb,
}

impl Theme {
    /// Cor codificada pelo server: >=0 RGB; -1 fg; -2 bg; -10-i paleta i.
    pub fn resolve(&self, v: i64) -> Rgb {
        match v {
            -1 => self.fg,
            -2 => self.bg,
            v if v <= -10 => self.palette[((-10 - v) as usize).min(15)],
            v if v < 0 => self.fg,
            v => Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
        }
    }
    /// Nome de cor da config ('accent', 'muted', 'working'...) ou '#rrggbb'.
    pub fn named(&self, name: &str) -> Rgb {
        match name {
            "accent" => self.accent, "muted" => self.muted, "working" => self.working,
            "blocked" => self.blocked, "done" => self.done, "idle" => self.idle,
            "fg" | "foreground" => self.fg, "bg" | "background" => self.bg,
            n => Rgb::parse(n).unwrap_or(self.fg),
        }
    }
}

fn hexes(v: &[&str]) -> Vec<Rgb> { v.iter().map(|s| Rgb::parse(s).unwrap()).collect() }

/// Temas embutidos (galeria e Configuracoes).
pub const THEMES: [&str; 22] = ["Duck", "Catppuccin Mocha", "Catppuccin Macchiato", "Catppuccin Frappe", "Catppuccin Latte", "Tokyo Night", "Tokyo Night Storm", "Dracula", "Nord", "One Dark", "Gruvbox Dark", "Gruvbox Light", "Solarized Dark", "Solarized Light", "Rose Pine", "Kanagawa", "Everforest", "GitHub Dark", "GitHub Light", "Monokai Pro", "Ayu Dark", "Night Owl"];

pub fn builtin_theme(name: &str) -> Theme {
    // (fg, bg, cursor, selection, tab_bar, tab_active, muted, accent, 16 cores)
    let (base, pal): (Vec<Rgb>, Vec<Rgb>) = match name.to_lowercase().as_str() {
        "catppuccin latte" => (
            hexes(&["#4c4f69", "#eff1f5", "#dc8a78", "#acb0be", "#dce0e8", "#ccd0da", "#8c8fa1", "#1e66f5"]),
            hexes(&["#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be",
                    "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc"]),
        ),
        "tokyo night" => (
            hexes(&["#c0caf5", "#1a1b26", "#c0caf5", "#33467c", "#16161e", "#292e42", "#565f89", "#7aa2f7"]),
            hexes(&["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
                    "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]),
        ),
        "dracula" => (
            hexes(&["#f8f8f2", "#282a36", "#f8f8f2", "#44475a", "#21222c", "#44475a", "#6272a4", "#bd93f9"]),
            hexes(&["#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
                    "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff"]),
        ),
        "gruvbox dark" => (
            hexes(&["#ebdbb2", "#282828", "#ebdbb2", "#504945", "#1d2021", "#3c3836", "#928374", "#fabd2f"]),
            hexes(&["#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
                    "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2"]),
        ),
        "duck" => (
            hexes(&["#e3eef2", "#121c24", "#ffc843", "#2c4a57", "#0c141a", "#1c2b35", "#6f8794", "#6ccfe2"]),
            hexes(&["#1c2b35", "#ff6b7a", "#8fd694", "#ffc843", "#6ccfe2", "#c39bf0", "#58d6c0", "#c7d4da", "#3c5260", "#ff8a96", "#a9e4ad", "#ffd773", "#8fdcec", "#d4b5f5", "#7fe3d1", "#e3eef2"]),
        ),
        "catppuccin frappe" => (
            hexes(&["#c6d0f5", "#303446", "#f2d5cf", "#51576d", "#292c3c", "#414559", "#838ba7", "#8caaee"]),
            hexes(&["#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2", "#626880", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#a5adce"]),
        ),
        "catppuccin macchiato" => (
            hexes(&["#cad3f5", "#24273a", "#f4dbd6", "#494d64", "#1e2030", "#363a4f", "#8087a2", "#8aadf4"]),
            hexes(&["#494d64", "#ed8796", "#a6da95", "#eed49f", "#8aadf4", "#f5bde6", "#8bd5ca", "#b8c0e0", "#5b6078", "#ed8796", "#a6da95", "#eed49f", "#8aadf4", "#f5bde6", "#8bd5ca", "#a5adcb"]),
        ),
        "tokyo night storm" => (
            hexes(&["#c0caf5", "#24283b", "#c0caf5", "#364a82", "#1f2335", "#292e42", "#565f89", "#7aa2f7"]),
            hexes(&["#1d202f", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]),
        ),
        "nord" => (
            hexes(&["#d8dee9", "#2e3440", "#d8dee9", "#434c5e", "#242933", "#3b4252", "#616e88", "#88c0d0"]),
            hexes(&["#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0", "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4"]),
        ),
        "one dark" => (
            hexes(&["#abb2bf", "#282c34", "#528bff", "#3e4451", "#21252b", "#2c313a", "#5c6370", "#61afef"]),
            hexes(&["#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf", "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff"]),
        ),
        "solarized dark" => (
            hexes(&["#839496", "#002b36", "#93a1a1", "#073642", "#00212b", "#073642", "#586e75", "#268bd2"]),
            hexes(&["#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3"]),
        ),
        "solarized light" => (
            hexes(&["#657b83", "#fdf6e3", "#586e75", "#eee8d5", "#eee8d5", "#e4ddc8", "#93a1a1", "#268bd2"]),
            hexes(&["#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3"]),
        ),
        "rose pine" => (
            hexes(&["#e0def4", "#191724", "#524f67", "#403d52", "#1f1d2e", "#26233a", "#6e6a86", "#c4a7e7"]),
            hexes(&["#26233a", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4", "#6e6a86", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4"]),
        ),
        "kanagawa" => (
            hexes(&["#dcd7ba", "#1f1f28", "#c8c093", "#2d4f67", "#16161d", "#2a2a37", "#727169", "#7e9cd8"]),
            hexes(&["#090618", "#c34043", "#76946a", "#c0a36e", "#7e9cd8", "#957fb8", "#6a9589", "#c8c093", "#727169", "#e82424", "#98bb6c", "#e6c384", "#7fb4ca", "#938aa9", "#7aa89f", "#dcd7ba"]),
        ),
        "everforest" => (
            hexes(&["#d3c6aa", "#2d353b", "#d3c6aa", "#475258", "#232a2e", "#3d484d", "#859289", "#a7c080"]),
            hexes(&["#475258", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092", "#d3c6aa", "#475258", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092", "#d3c6aa"]),
        ),
        "gruvbox light" => (
            hexes(&["#3c3836", "#fbf1c7", "#3c3836", "#d5c4a1", "#ebdbb2", "#d5c4a1", "#928374", "#b57614"]),
            hexes(&["#fbf1c7", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#7c6f64", "#928374", "#9d0006", "#79740e", "#b57614", "#076678", "#8f3f71", "#427b58", "#3c3836"]),
        ),
        "github dark" => (
            hexes(&["#c9d1d9", "#0d1117", "#58a6ff", "#264f78", "#010409", "#161b22", "#8b949e", "#58a6ff"]),
            hexes(&["#484f58", "#ff7b72", "#3fb950", "#d29922", "#58a6ff", "#bc8cff", "#39c5cf", "#b1bac4", "#6e7681", "#ffa198", "#56d364", "#e3b341", "#79c0ff", "#d2a8ff", "#56d4dd", "#ffffff"]),
        ),
        "github light" => (
            hexes(&["#24292f", "#ffffff", "#0969da", "#b6e3ff", "#f6f8fa", "#eaeef2", "#6e7781", "#0969da"]),
            hexes(&["#24292f", "#cf222e", "#116329", "#4d2d00", "#0969da", "#8250df", "#1b7c83", "#6e7781", "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9", "#3192aa", "#8c959f"]),
        ),
        "monokai pro" => (
            hexes(&["#fcfcfa", "#2d2a2e", "#fcfcfa", "#5b595c", "#221f22", "#403e41", "#727072", "#ffd866"]),
            hexes(&["#403e41", "#ff6188", "#a9dc76", "#ffd866", "#fc9867", "#ab9df2", "#78dce8", "#fcfcfa", "#727072", "#ff6188", "#a9dc76", "#ffd866", "#fc9867", "#ab9df2", "#78dce8", "#fcfcfa"]),
        ),
        "ayu dark" => (
            hexes(&["#bfbdb6", "#0b0e14", "#e6b450", "#1b3a5b", "#07090d", "#131721", "#565b66", "#e6b450"]),
            hexes(&["#1e232b", "#ea6c73", "#7fd962", "#f9af4f", "#53bdfa", "#cda1fa", "#90e1c6", "#c7c7c7", "#686868", "#f07178", "#aad94c", "#ffb454", "#59c2ff", "#d2a6ff", "#95e6cb", "#ffffff"]),
        ),
        "night owl" => (
            hexes(&["#d6deeb", "#011627", "#80a4c2", "#1d3b53", "#010e1a", "#0b2942", "#637777", "#82aaff"]),
            hexes(&["#011627", "#ef5350", "#22da6e", "#addb67", "#82aaff", "#c792ea", "#21c7a8", "#ffffff", "#575656", "#ef5350", "#22da6e", "#ffeb95", "#82aaff", "#c792ea", "#7fdbca", "#ffffff"]),
        ),
        _ => (
            // Catppuccin Mocha (padrao, o mesmo do setup do WezTerm)
            hexes(&["#cdd6f4", "#1e1e2e", "#f5e0dc", "#45475a", "#181825", "#313244", "#7f849c", "#89b4fa"]),
            hexes(&["#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
                    "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8"]),
        ),
    };
    let mut palette = [Rgb(0, 0, 0); 16];
    palette.copy_from_slice(&pal);
    Theme {
        fg: base[0], bg: base[1], cursor: base[2], selection: base[3],
        tab_bar: base[4], tab_active: base[5], muted: base[6],
        border: base[5], accent: base[7],
        working: palette[11], blocked: palette[9], done: palette[10], idle: base[6],
        palette,
    }
}

#[derive(Clone, Debug)]
pub struct Domain {
    pub name: String,
    pub cmd: Vec<String>,
    /// Perfil visual: fundo e cor de aba proprios (ex.: producao em vermelho).
    pub background: Option<Rgb>,
    pub tab_color: Option<Rgb>,
    /// Caiu com erro (rede): abre de novo no mesmo lugar.
    pub reconnect: bool,
}

/// Perfil de agente: como abrir um agente (comando, modelo, permissoes).
#[derive(Clone, Debug)]
pub struct AgentProfile { pub name: String, pub cmd: String }

/// Popup: comando num pane flutuante que some quando o comando sai.
#[derive(Clone, Debug)]
pub struct Popup { pub name: String, pub cmd: String }

#[derive(Clone, Debug, Default)]
pub struct Background {
    pub image: Option<String>,
    pub opacity: f32,
    pub gradient: Vec<Rgb>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub font_family: String,
    pub font_dirs: Vec<String>,
    pub font_size: f32,
    pub line_height: f32,
    pub theme: Theme,
    pub opacity: f32,
    pub backdrop: String,
    pub tab_bar: String,
    pub padding: f32,
    pub shell: Vec<String>,
    pub cwd: Option<String>,
    pub domains: Vec<Domain>,
    pub scrollback: usize,
    pub hooks: HashMap<String, String>,
    pub autosave_minutes: u64,
    pub hyperlinks: Vec<(String, String)>,
    pub notify: bool,
    pub agents: Vec<String>,
    // ---- seguranca
    /// OSC 52 (app escrevendo no clipboard): "ask" | "allow" | "deny".
    pub clipboard_write: String,
    pub paste_confirm: bool,
    /// O que um pane pode fazer pela API: "all" | "children" | "read".
    pub api_scope: String,
    pub confirm_agent_actions: bool,
    pub audit: bool,
    pub secret_patterns: Vec<String>,
    /// Token que aparece na tela (ghp_, sk-, AKIA, JWT, PAT do Azure...) vira
    /// bolinhas e gera um aviso. `secret_mask = false` desliga.
    pub secret_mask: bool,
    /// Palavras que nao podem ir num commit/push (duck guard): nomes de
    /// cliente, de projeto... `guard = { words = {'acme'} }`.
    pub guard_words: Vec<String>,
    // ---- estabilidade / performance
    pub scrollback_mb: f64,
    pub max_fps: u32,
    pub gpu_backend: String,
    // ---- ux
    pub notify_command_secs: u64,
    pub shell_integration: bool,
    /// Windows: aba nova rele PATH e variaveis do registro (programa instalado
    /// com o Duck aberto ja aparece, sem reiniciar nada).
    pub refresh_env: bool,
    /// Procura versao nova do Duck (uma vez por dia) e avisa na janela.
    pub update_check: bool,
    pub theme_light: Option<Theme>,
    pub background: Background,
    pub tab_rules: Vec<(String, Rgb)>,
    pub hints: Vec<String>,
    pub quick_key: String,
    pub quick_height: f32,
    pub shader: Option<String>,
    pub visual_bell: bool,
    pub token_budget: u64,
    /// Mensagens entre computadores (`duck msg`): onde escutar ("" = desligado),
    /// nome deste computador, entrega sem aprovacao e limite de idas e voltas.
    pub peer_listen: String,
    pub peer_name: String,
    pub peer_trust: bool,
    pub peer_max_hops: u32,
    /// Outros computadores: nome -> "ip:porta" (o token fica em `duck secret`).
    pub peers: Vec<(String, String)>,
    pub agent_profiles: Vec<AgentProfile>,
    pub popups: Vec<Popup>,
    /// Editor para links arquivo:linha (padrao: VS Code).
    pub editor: String,
    /// Sugestao inline do historico: "auto" (fora do pwsh, que ja tem a do
    /// PSReadLine) | "always" | "off".
    pub inline_suggestions: String,
    // ---- visual (tabela `ui`)
    pub ui: Ui,
    /// Cabecalho por pane: "multi" (com mais de um pane) | "always" | "off".
    pub pane_header: String,
    /// Opcoes extras ao retomar sessoes do agente no restore (ex.: "--dangerously-skip-permissions").
    pub resume_args: String,
}

/// Visual da janela: o que faz o Duck nao parecer um WezTerm.
#[derive(Clone, Debug)]
pub struct Ui {
    /// "custom" = barra de titulo propria (abas + botoes); "native" = do Windows.
    pub titlebar: String,
    /// Panes como cartoes: cantos arredondados, sombra e respiro entre eles.
    pub cards: bool,
    pub radius: f32,
    /// Borda com brilho no pane ativo, na cor da aba.
    pub glow: bool,
    pub animations: bool,
    pub cursor_smooth: bool,
    pub cursor_blink: bool,
    /// Popup de autocompletar: "auto" | "off".
    pub autocomplete: String,
    /// Realce do comando enquanto digita: "auto" (fora do pwsh) | "always" | "off".
    pub syntax: String,
    /// Ctrl+clique em arquivo mostra uma previa antes de abrir no editor.
    pub link_preview: bool,
    /// Peso da fonte do terminal (100-900).
    pub font_weight: u16,
    /// Fonte da interface (abas, menus, cartoes).
    pub font: String,
    /// Recursos OpenType: {"-calt"} desliga ligaduras; {"ss01"} liga variante.
    pub font_features: Vec<String>,
    /// Saida com mais linhas que isso nasce recolhida (0 = nunca).
    pub collapse_lines: u32,
    /// Preco por milhao de tokens para estimar custo no painel do agente (0 = nao mostra).
    pub token_price: f64,
    pub sidebar_width: f32,
    /// Cores de avatar por perfil de agente: { trabalho = '#f38ba8' }.
    pub agent_colors: HashMap<String, Rgb>,
    /// Barra de rodape com os badges (sistema, agentes, extensoes) e o status.
    pub footer: bool,
    /// Barra de atalhos abaixo do rodape (lista em `shortcuts`).
    pub shortcuts: bool,
    /// "comfortable" (padrao) | "compact": barras e respiros menores (notebook).
    pub density: String,
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default()
}
/// O Duck se chamava Lasso: na 1a vez copia ~/.lasso.lua, ~/.lasso-local.lua
/// e ~/.lasso (temas, plugins, shaders) para os nomes novos.
pub fn migrate_from_lasso() {
    let h = home();
    for (old, new) in [(".lasso.lua", ".duck.lua"), (".lasso-local.lua", ".duck-local.lua")] {
        let (o, n) = (h.join(old), h.join(new));
        if !n.exists() {
            if let Ok(t) = std::fs::read_to_string(&o) {
                let t = t.replace("LASSO", "DUCK").replace("Lasso", "Duck").replace("lasso", "duck");
                let _ = std::fs::write(&n, t);
            }
        }
    }
    if !h.join(".duck").exists() && h.join(".lasso").is_dir() { let _ = crate::proto::copy_dir(&h.join(".lasso"), &h.join(".duck")); }
}

pub fn config_path() -> PathBuf {
    std::env::var_os("DUCK_CONFIG").map(PathBuf::from).unwrap_or_else(|| home().join(".duck.lua"))
}
pub fn local_path() -> PathBuf { home().join(".duck-local.lua") }
pub fn plugins_dir() -> PathBuf {
    std::env::var_os("DUCK_PLUGINS").map(PathBuf::from).unwrap_or_else(|| home().join(".duck").join("plugins"))
}

/// Modulo `duck` disponivel para a config e para os plugins.
fn install_module(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;
    m.set("home", home().to_string_lossy().to_string())?;
    m.set("os", std::env::consts::OS)?;
    m.set("hostname", std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default())?;
    m.set("username", std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default())?;
    // duck.action('split', 'right') -> {action='split', args={'right'}}: o jeito
    // de declarar atalho sem escrever funcao.
    m.set("action", lua.create_function(|lua, args: mlua::MultiValue| {
        let mut it = args.into_iter();
        let t = lua.create_table()?;
        t.set("action", it.next().unwrap_or(LuaValue::Nil))?;
        let a = lua.create_table()?;
        for (i, v) in it.enumerate() { a.set(i + 1, v)?; }
        t.set("args", a)?;
        Ok(t)
    })?)?;
    m.set("exists", lua.create_function(|_, p: String| Ok(std::path::Path::new(&p).exists()))?)?;
    m.set("getenv", lua.create_function(|_, k: String| Ok(std::env::var(k).ok()))?)?;
    lua.globals().set("duck", m.clone())?;
    // Configs da epoca do Lasso continuam funcionando.
    lua.globals().set("lasso", m.clone())?;
    lua.register_module("lasso", m.clone())?;
    lua.register_module("duck", m)?;

    // Plugins: require 'nome' acha ~/.duck/plugins/nome.lua ou nome/init.lua.
    let pkg: Table = lua.globals().get("package")?;
    let pd = plugins_dir().to_string_lossy().replace('\\', "/");
    let path: String = pkg.get("path")?;
    pkg.set("path", format!("{pd}/?.lua;{pd}/?/init.lua;{path}"))?;
    Ok(())
}

/// Roda a config e devolve (vm, tabela). Erro na config cai no padrao e
/// devolve a mensagem para a GUI mostrar, em vez de nao abrir.
pub fn load() -> (Lua, Table, Option<String>) {
    let lua = Lua::new();
    install_module(&lua).expect("modulo duck");
    let mut err = None;
    let src = std::fs::read_to_string(config_path()).unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    let table = match lua.load(&src).set_name("duck.lua").eval::<Table>() {
        Ok(t) => t,
        Err(e) => {
            err = Some(format!("~/.duck.lua: {e}"));
            lua.load(DEFAULT_CONFIG).eval::<Table>().expect("config padrao")
        }
    };
    if let Ok(local) = std::fs::read_to_string(local_path()) {
        lua.globals().set("config", table.clone()).ok();
        if let Err(e) = lua.load(&local).set_name("duck-local.lua").exec() {
            err = Some(format!("~/.duck-local.lua: {e}"));
        }
    }
    (lua, table, err)
}

fn s(t: &Table, k: &str, d: &str) -> String { t.get::<Option<String>>(k).ok().flatten().unwrap_or_else(|| d.to_string()) }
fn n(t: &Table, k: &str, d: f64) -> f64 { t.get::<Option<f64>>(k).ok().flatten().unwrap_or(d) }
fn list(t: &Table, k: &str) -> Option<Vec<String>> { t.get::<Option<Vec<String>>>(k).ok().flatten() }

pub fn parse(t: &Table) -> Config {
    let font: Option<Table> = t.get("font").ok();
    let (font_family, font_size, line_height) = match &font {
        Some(f) => (s(f, "family", "JetBrainsMono Nerd Font Mono"), n(f, "size", 11.0), n(f, "line_height", 1.2)),
        None => ("JetBrainsMono Nerd Font Mono".into(), 11.0, 1.2),
    };
    let (mut theme, theme_light) = match t.get::<LuaValue>("theme") {
        Ok(LuaValue::Table(th)) => (named_theme(&s(&th, "dark", "Catppuccin Mocha")),
            th.get::<Option<String>>("light").ok().flatten().map(|l| named_theme(&l))),
        _ => (named_theme(&s(t, "theme", "Catppuccin Mocha")), None),
    };
    if let Ok(c) = t.get::<Table>("colors") {
        let set = |k: &str, dst: &mut Rgb| {
            if let Some(v) = c.get::<Option<String>>(k).ok().flatten().and_then(|h| Rgb::parse(&h)) { *dst = v; }
        };
        set("foreground", &mut theme.fg);
        set("background", &mut theme.bg);
        set("cursor", &mut theme.cursor);
        set("selection", &mut theme.selection);
        set("tab_bar", &mut theme.tab_bar);
        set("tab_active", &mut theme.tab_active);
        set("muted", &mut theme.muted);
        set("border", &mut theme.border);
        set("accent", &mut theme.accent);
        set("working", &mut theme.working);
        set("blocked", &mut theme.blocked);
        set("done", &mut theme.done);
        set("idle", &mut theme.idle);
        if let Ok(p) = c.get::<Vec<String>>("ansi") {
            for (i, h) in p.iter().take(16).enumerate() {
                if let Some(v) = Rgb::parse(h) { theme.palette[i] = v; }
            }
        }
    }
    let default_shell = if cfg!(windows) { vec!["pwsh.exe".to_string(), "-NoLogo".into()] } else { vec!["bash".into(), "-l".into()] };
    let mut domains = Vec::new();
    if let Ok(d) = t.get::<Table>("domains") {
        // Valor: lista (o comando) ou tabela {cmd=..., background=, tab_color=}.
        for (name, v) in d.pairs::<String, LuaValue>().flatten() {
            let color = |t: &Table, k: &str| t.get::<Option<String>>(k).ok().flatten().and_then(|h| Rgb::parse(&h));
            match v {
                // SSH com tmux remoto: a sessao sobrevive a queda da rede e o
                // Duck reconecta sozinho no mesmo lugar.
                LuaValue::Table(tb) if tb.contains_key("ssh").unwrap_or(false) => {
                    let host: String = tb.get("ssh").unwrap_or_default();
                    let session = tb.get::<Option<String>>("session").ok().flatten().unwrap_or_else(|| "duck".into());
                    let mut cmd = vec!["ssh".to_string(), "-t".into(), host];
                    if tb.get::<Option<bool>>("tmux").ok().flatten().unwrap_or(true) {
                        cmd.extend(["tmux".into(), "new-session".into(), "-A".into(), "-s".into(), session]);
                    }
                    domains.push(Domain { cmd, background: color(&tb, "background"), tab_color: color(&tb, "tab_color"),
                        reconnect: tb.get::<Option<bool>>("reconnect").ok().flatten().unwrap_or(true), name });
                }
                LuaValue::Table(tb) if tb.contains_key("cmd").unwrap_or(false) => domains.push(Domain {
                    cmd: tb.get("cmd").unwrap_or_default(), background: color(&tb, "background"),
                    tab_color: color(&tb, "tab_color"), reconnect: tb.get::<Option<bool>>("reconnect").ok().flatten().unwrap_or(false), name,
                }),
                LuaValue::Table(tb) => domains.push(Domain {
                    cmd: tb.sequence_values::<String>().flatten().collect(), background: None, tab_color: None, reconnect: false, name,
                }),
                _ => {}
            }
        }
    }
    domains.sort_by(|a, b| a.name.cmp(&b.name));
    let mut hooks = HashMap::new();
    if let Ok(h) = t.get::<Table>("hooks") {
        for (k, v) in h.pairs::<String, String>().flatten() { hooks.insert(k, v); }
    }
    let mut hyperlinks = vec![(r"https?://[^\s<>()\x22']+".to_string(), "$0".to_string())];
    if let Ok(h) = t.get::<Table>("hyperlinks") {
        for r in h.sequence_values::<Table>().flatten() {
            hyperlinks.push((s(&r, "regex", ""), s(&r, "format", "$0")));
        }
    }
    let mut font_dirs: Vec<String> = font.as_ref().and_then(|f| list(f, "dirs")).unwrap_or_default();
    // Fontes instaladas "so para mim" no Windows nao estao em C:\Windows\Fonts.
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        font_dirs.push(PathBuf::from(l).join(r"Microsoft\Windows\Fonts").to_string_lossy().to_string());
    }
    Config {
        font_family,
        font_dirs,
        font_size: font_size as f32,
        line_height: line_height as f32,
        theme,
        opacity: n(t, "opacity", 1.0) as f32,
        backdrop: s(t, "backdrop", "none"),
        tab_bar: s(t, "tab_bar", "top"),
        padding: n(t, "padding", 6.0) as f32,
        shell: list(t, "shell").unwrap_or(default_shell),
        cwd: t.get::<Option<String>>("cwd").ok().flatten().filter(|p| std::path::Path::new(p).exists()),
        domains,
        scrollback: n(t, "scrollback", 10000.0) as usize,
        hooks,
        autosave_minutes: n(t, "autosave_minutes", 5.0) as u64,
        hyperlinks,
        notify: t.get::<Option<bool>>("notify").ok().flatten().unwrap_or(true),
        agents: list(t, "agents").unwrap_or_else(|| {
            ["claude", "codex", "cursor-agent", "opencode", "gemini", "aider", "copilot", "grok", "kiro-cli"]
                .iter().map(|s| s.to_string()).collect()
        }),
        clipboard_write: s(t, "clipboard_write", "ask"),
        paste_confirm: t.get::<Option<bool>>("paste_confirm").ok().flatten().unwrap_or(true),
        api_scope: s(t, "api_scope", "all"),
        confirm_agent_actions: t.get::<Option<bool>>("confirm_agent_actions").ok().flatten().unwrap_or(true),
        audit: t.get::<Option<bool>>("audit").ok().flatten().unwrap_or(true),
        secret_patterns: list(t, "secret_patterns").unwrap_or_else(|| DEFAULT_SECRETS.iter().map(|s| s.to_string()).collect()),
        secret_mask: t.get::<Option<bool>>("secret_mask").ok().flatten().unwrap_or(true),
        guard_words: t.get::<Table>("guard").ok().and_then(|g| list(&g, "words")).unwrap_or_default(),
        scrollback_mb: n(t, "scrollback_mb", 64.0),
        max_fps: n(t, "max_fps", 0.0) as u32,
        gpu_backend: s(t, "gpu_backend", "dx12"),
        notify_command_secs: n(t, "notify_command_secs", 30.0) as u64,
        shell_integration: t.get::<Option<bool>>("shell_integration").ok().flatten().unwrap_or(true),
        refresh_env: t.get::<Option<bool>>("refresh_env").ok().flatten().unwrap_or(true),
        update_check: t.get::<Option<bool>>("update_check").ok().flatten().unwrap_or(true),
        theme_light,
        background: t.get::<Table>("background").map(|b| Background {
            image: b.get::<Option<String>>("image").ok().flatten(),
            opacity: n(&b, "opacity", 0.25) as f32,
            gradient: list(&b, "gradient").unwrap_or_default().iter().filter_map(|h| Rgb::parse(h)).collect(),
        }).unwrap_or_default(),
        tab_rules: t.get::<Table>("tab_rules").map(|r| r.sequence_values::<Table>().flatten()
            .filter_map(|x| Some((s(&x, "match", ""), Rgb::parse(&s(&x, "color", ""))?))).collect()).unwrap_or_default(),
        hints: list(t, "hints").unwrap_or_else(|| DEFAULT_HINTS.iter().map(|s| s.to_string()).collect()),
        quick_key: t.get::<Table>("quick_terminal").ok().map(|q| s(&q, "key", "CTRL+'")).unwrap_or_else(|| "CTRL+'".into()),
        quick_height: t.get::<Table>("quick_terminal").ok().map(|q| n(&q, "height", 0.45) as f32).unwrap_or(0.45),
        shader: t.get::<Option<String>>("shader").ok().flatten(),
        visual_bell: t.get::<Option<bool>>("visual_bell").ok().flatten().unwrap_or(true),
        token_budget: n(t, "token_budget", 0.0) as u64,
        peer_listen: t.get::<Table>("peer").ok().map(|p| s(&p, "listen", "")).unwrap_or_default(),
        peer_name: t.get::<Table>("peer").ok().map(|p| s(&p, "name", "")).filter(|n| !n.is_empty())
            .unwrap_or_else(|| std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "duck".into()).to_lowercase()),
        peer_trust: t.get::<Table>("peer").ok().and_then(|p| p.get::<Option<bool>>("trust").ok().flatten()).unwrap_or(false),
        peer_max_hops: t.get::<Table>("peer").ok().map(|p| n(&p, "max_hops", 10.0) as u32).unwrap_or(10),
        peers: t.get::<Table>("peers").map(|p| p.pairs::<String, String>().flatten().collect()).unwrap_or_default(),
        agent_profiles: t.get::<Table>("agent_profiles").map(|p| p.sequence_values::<Table>().flatten()
            .map(|x| AgentProfile { name: s(&x, "name", "agente"), cmd: s(&x, "cmd", "claude") }).collect())
            .unwrap_or_else(|_| vec![
                AgentProfile { name: "Claude".into(), cmd: "claude".into() },
                AgentProfile { name: "Claude (modo plano)".into(), cmd: "claude --permission-mode plan".into() },
                AgentProfile { name: "Claude (sem pedir permissao em edicao)".into(), cmd: "claude --permission-mode acceptEdits".into() },
                AgentProfile { name: "Codex".into(), cmd: "codex".into() },
            ]),
        popups: t.get::<Table>("popups").map(|p| p.sequence_values::<Table>().flatten()
            .map(|x| Popup { name: s(&x, "name", "popup"), cmd: s(&x, "cmd", "") }).collect())
            .unwrap_or_else(|_| vec![
                Popup { name: "lazygit".into(), cmd: "lazygit".into() },
                Popup { name: "git log".into(), cmd: "git -c color.ui=always log --oneline --graph -40".into() },
                Popup { name: "processos (btop/htop)".into(), cmd: "btop".into() },
                Popup { name: "containers".into(), cmd: "docker ps -a".into() },
            ]),
        editor: s(t, "editor", "code -g {file}:{line}:{col}"),
        inline_suggestions: s(t, "inline_suggestions", "auto"),
        ui: parse_ui(t.get::<Table>("ui").ok()),
        resume_args: s(t, "resume_args", ""),
        pane_header: t.get::<Table>("ui").ok().map(|u| s(&u, "pane_header", "multi")).unwrap_or_else(|| "multi".into()),
    }
}

fn parse_ui(u: Option<Table>) -> Ui {
    let lua = Lua::new();
    let u = u.unwrap_or_else(|| lua.create_table().unwrap());
    let b = |k: &str, d: bool| u.get::<Option<bool>>(k).ok().flatten().unwrap_or(d);
    Ui {
        titlebar: s(&u, "titlebar", "custom"),
        cards: b("cards", true),
        radius: n(&u, "radius", 8.0) as f32,
        glow: b("glow", true),
        animations: b("animations", true),
        cursor_smooth: b("cursor_smooth", true),
        cursor_blink: b("cursor_blink", false),
        autocomplete: s(&u, "autocomplete", "auto"),
        syntax: s(&u, "syntax", "auto"),
        link_preview: b("link_preview", true),
        font_weight: n(&u, "font_weight", 400.0) as u16,
        font: s(&u, "font", "Segoe UI Variable Text"),
        font_features: list(&u, "font_features").unwrap_or_default(),
        collapse_lines: n(&u, "collapse_lines", 300.0) as u32,
        token_price: n(&u, "token_price", 0.0),
        sidebar_width: n(&u, "sidebar_width", 230.0) as f32,
        agent_colors: u.get::<Table>("agent_colors").map(|c| c.pairs::<String, String>().flatten()
            .filter_map(|(k, v)| Some((k, Rgb::parse(&v)?))).collect()).unwrap_or_default(),
        footer: b("footer", true),
        shortcuts: b("shortcuts", true),
        density: s(&u, "density", "comfortable"),
    }
}

/// Segredos que nao vao para o disco no autosave/checkpoint.
pub const DEFAULT_SECRETS: [&str; 7] = [
    r"gh[pousr]_[A-Za-z0-9]{20,}", r"sk-[A-Za-z0-9_\-]{20,}", r"AKIA[0-9A-Z]{16}", r"xox[abpr]-[A-Za-z0-9-]{10,}",
    r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
    r"(?i)(password|senha|passwd|secret|token|api[_-]?key)\s*[:=]\s*\S+", r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
];

/// Tokens com formato proprio: mascarados na tela, nas gravacoes e no que
/// vai para agentes. So formatos inequivocos (o "senha=" generico fica de
/// fora da tela: codigo-fonte cheio de `token:` viraria bolinha).
pub const SCREEN_SECRETS: [&str; 14] = [
    r"\bgh[pousr]_[A-Za-z0-9]{30,}", r"\bgithub_pat_[A-Za-z0-9_]{40,}", r"\bglpat-[A-Za-z0-9_\-]{20,}",
    r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_\-]{24,}", r"\b(?:sk|rk)_live_[A-Za-z0-9]{20,}", r"\bAKIA[0-9A-Z]{16}\b",
    r"\bxox[abpr]-[A-Za-z0-9-]{10,}", r"\bAIza[0-9A-Za-z_\-]{35}", r"\bnpm_[A-Za-z0-9]{36}\b",
    r"\beyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}",
    // PAT do Azure DevOps: o novo (84 com a assinatura AZDO) e o antigo (52 em base32).
    r"\b[A-Za-z0-9]{52}AZDO[A-Za-z0-9]{28}\b", r"\b[a-z2-7]{52}\b",
    r"(?i)\b(?:AccountKey|SharedAccessSignature|sig)=[A-Za-z0-9%+/=]{20,}", r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
];

/// Os padroes de tela juntos numa regex so.
pub fn screen_secrets() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(&SCREEN_SECRETS.join("|")).unwrap())
}

/// Padroes do modo "hints" (selecao por teclado): URL, caminho, hash, IP, issue, numero.
pub const DEFAULT_HINTS: [&str; 6] = [
    r"https?://[^\s<>()\x22']+", r"(?:[A-Za-z]:)?(?:[\\/][\w.\-]+){2,}", r"\b[0-9a-f]{7,40}\b",
    r"\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b", r"#\d+\b", r"\b\d{4,}\b",
];

/// Tema de arquivo em ~/.duck/themes: formato do Ghostty (`chave = valor`,
/// `palette = N=#hex`) ou do WezTerm (TOML com ansi/brights). Ha centenas
/// prontos nos dois formatos.
pub fn theme_file(name: &str) -> Option<Theme> {
    let dir = home().join(".duck").join("themes");
    let path = ["", ".conf", ".toml"].iter().map(|e| dir.join(format!("{name}{e}"))).find(|p| p.is_file())?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut t = builtin_theme("");
    let mut ansi: Vec<Rgb> = Vec::new();
    let mut brights: Vec<Rgb> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim().trim_matches('"');
        let v = v.trim().trim_matches(['"', '\'']);
        let col = || Rgb::parse(v);
        match k {
            "background" => if let Some(c) = col() { t.bg = c },
            "foreground" => if let Some(c) = col() { t.fg = c },
            "cursor-color" | "cursor_bg" => if let Some(c) = col() { t.cursor = c },
            "selection-background" | "selection_bg" => if let Some(c) = col() { t.selection = c },
            "palette" => if let Some((i, c)) = v.split_once('=') {
                if let (Ok(i), Some(c)) = (i.trim().parse::<usize>(), Rgb::parse(c.trim())) { if i < 16 { t.palette[i] = c; } }
            },
            "ansi" | "brights" => {
                let cols: Vec<Rgb> = v.trim_matches(['[', ']']).split(',').filter_map(|x| Rgb::parse(x.trim().trim_matches(['"', '\'']))).collect();
                if k == "ansi" { ansi = cols } else { brights = cols }
            }
            _ => {}
        }
    }
    for (i, c) in ansi.into_iter().take(8).enumerate() { t.palette[i] = c; }
    for (i, c) in brights.into_iter().take(8).enumerate() { t.palette[8 + i] = c; }
    t.tab_bar = mix_rgb(t.bg, Rgb(0, 0, 0), 0.25);
    t.tab_active = mix_rgb(t.bg, t.fg, 0.15);
    t.border = t.tab_active;
    t.muted = mix_rgb(t.fg, t.bg, 0.45);
    t.accent = t.palette[4];
    t.working = t.palette[11];
    t.blocked = t.palette[9];
    t.done = t.palette[10];
    t.idle = t.muted;
    Some(t)
}

pub fn mix_rgb(a: Rgb, b: Rgb, f: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - f) + y as f32 * f) as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

pub fn named_theme(name: &str) -> Theme { theme_file(name).unwrap_or_else(|| builtin_theme(name)) }

pub fn load_config() -> (Config, Option<String>) {
    let (_lua, t, err) = load();
    (parse(&t), err)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_config_parses() {
        let lua = Lua::new();
        install_module(&lua).unwrap();
        let t: Table = lua.load(DEFAULT_CONFIG).eval().unwrap();
        let c = parse(&t);
        assert!(c.font_size > 5.0);
        assert!(!c.shell.is_empty());
        assert_eq!(Rgb::parse("#1e1e2e"), Some(Rgb(0x1e, 0x1e, 0x2e)));
    }

    #[test]
    fn example_status_and_tab_title_run() {
        let lua = Lua::new();
        install_module(&lua).unwrap();
        let t: Table = lua.load(DEFAULT_CONFIG).eval().unwrap();
        let info: Table = lua.load(r#"return { workspace = 'w', cpu = 10, ram = 1.5, ram_total = 32, time = '10:00',
            agents = { working = 1, blocked = 0, done = 0, idle = 0 }, ports = { 3000, 5173 }, tokens = 12345, mode = nil }"#).eval().unwrap();
        let segs: Table = t.get::<mlua::Function>("status").unwrap().call(info).unwrap();
        let joined: String = segs.sequence_values::<Table>().flatten().map(|s| s.get::<String>("text").unwrap()).collect();
        // Portas e tokens sairam da barra (extensao Portas, badges); sem modo, nada.
        assert!(joined.is_empty(), "{joined}");
        let info: Table = lua.load(r#"return { workspace = 'w', agents = {}, mode = 'copia' }"#).eval().unwrap();
        let segs: Table = t.get::<mlua::Function>("status").unwrap().call(info).unwrap();
        let joined: String = segs.sequence_values::<Table>().flatten().map(|s| s.get::<String>("text").unwrap()).collect();
        assert!(joined.contains("COPIA"), "{joined}");
        let tab: Table = lua.load(r#"return { title = '', pane_title = 'pwsh', cwd = 'D:/projetos/app', branch = 'feat-x' }"#).eval().unwrap();
        let title: String = t.get::<mlua::Function>("tab_title").unwrap().call(tab).unwrap();
        assert_eq!(title, "app ⎇ feat-x");
    }

    #[test]
    fn ssh_domain_and_profiles() {
        let lua = Lua::new();
        install_module(&lua).unwrap();
        let t: Table = lua.load("return { domains = { srv = { ssh = 'eu@servidor', session = 'dev' } }, token_budget = 1000 }").eval().unwrap();
        let c = parse(&t);
        let d = &c.domains[0];
        assert_eq!(d.cmd, vec!["ssh", "-t", "eu@servidor", "tmux", "new-session", "-A", "-s", "dev"]);
        assert!(d.reconnect);
        assert_eq!(c.token_budget, 1000);
        assert!(c.agent_profiles.len() >= 2);
    }

    #[test]
    fn plugin_require() {
        let dir = std::env::temp_dir().join(format!("duck-plugins-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("saudacao")).unwrap();
        std::fs::write(dir.join("saudacao").join("init.lua"),
            "local duck = require 'duck'
return { oi = function() return 'oi ' .. duck.os end }").unwrap();
        std::env::set_var("DUCK_PLUGINS", &dir);
        let lua = Lua::new();
        install_module(&lua).unwrap();
        let r: String = lua.load("return require('saudacao').oi()").eval().unwrap();
        assert_eq!(r, format!("oi {}", std::env::consts::OS));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Grava `chave = valor` (Lua) num bloco gerenciado do ~/.duck-local.lua; o
/// hot reload aplica. Chave com ponto vira campo aninhado (font.size).
pub fn set_local_setting(key: &str, value: &str) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')) {
        return Err(format!("chave invalida: {key}"));
    }
    // Nome com hifen (claude-usage) nao e identificador Lua: vira ["claude-usage"].
    let lua_path = |ps: &[&str]| -> String {
        ps.iter().fold("config".to_string(), |acc, p| {
            let ident = p.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && p.chars().all(|c| c.is_alphanumeric() || c == '_');
            if ident { format!("{acc}.{p}") } else { format!("{acc}[\"{p}\"]") }
        })
    };
    let path = local_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let (begin, end) = ("-- duck:configuracoes (gerado pela tela de configuracoes)", "-- duck:fim");
    let (head, mut block, tail) = match (text.find(begin), text.find(end)) {
        (Some(a), Some(b)) if b > a => (text[..a].to_string(), text[a + begin.len()..b].to_string(), text[b + end.len()..].to_string()),
        _ => (text.clone(), String::new(), String::new()),
    };
    let full = lua_path(&parts);
    let line_key = format!("{full} =");
    block = block.lines().filter(|l| !l.trim_start().starts_with(&line_key) && !l.trim().is_empty()).map(|l| format!("{l}\n")).collect();
    // Garante as tabelas pai, em todos os niveis (config.extensions.jira...).
    // As guardas ficam no topo, a mais curta primeiro: o pai vem antes do filho.
    let is_guard = |l: &str| l.split_once(" = ").is_some_and(|(a, b)| b == format!("{a} or {{}}"));
    let mut guards: Vec<String> = block.lines().filter(|l| is_guard(l)).map(str::to_string).collect();
    let rest: String = block.lines().filter(|l| !is_guard(l)).map(|l| format!("{l}\n")).collect();
    for k in 1..parts.len() {
        let parent = lua_path(&parts[..k]);
        let guard = format!("{parent} = {parent} or {{}}");
        if !guards.contains(&guard) { guards.push(guard); }
    }
    guards.sort_by_key(|g| g.len());
    block = guards.iter().map(|g| format!("{g}\n")).collect::<String>() + &rest;
    block.push_str(&format!("{full} = {value}\n"));
    let out = format!("{}{begin}\n{block}{end}{}", head.trim_end().to_string() + if head.trim().is_empty() { "" } else { "\n\n" }, tail);
    std::fs::write(&path, out).map_err(|e| e.to_string())
}
