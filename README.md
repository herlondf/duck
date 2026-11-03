<p align="center"><img src="assets/duck.png" width="112" alt="Duck"></p>

<h1 align="center">Duck</h1>

<p align="center"><b>A GPU terminal with a built-in multiplexer, made for working with AI agents.</b><br>
Rust · wgpu · Lua config · Windows first, Linux and macOS builds</p>

<p align="center"><a href="README.pt-BR.md">Leia em português</a></p>

---

Duck is a terminal emulator and multiplexer in one program. Closing the window
detaches instead of killing your shells, and the terminal knows which panes run
an AI agent (Claude Code, Codex, Kiro, Gemini, aider…) and what each one is
doing.

## Why Duck

- **Sessions that survive.** Every shell lives in its own `ptyhost` process.
  Closing the window, or even a crash of the server, keeps everything running;
  the next server reconnects. A checkpoint every 5 minutes restores workspaces,
  tabs and splits after a reboot, and **resumes the Claude Code conversations**
  that were open (`claude --resume`).
- **Agent aware.** Each tab shows whether its agent is working, waiting for
  your approval or done. Approve or deny from a card or from a Windows
  notification without switching tabs. Side panel with the agent's plan,
  context tokens and cost; timeline of the session; message queue delivered
  when the agent stops; "fix with agent" sends a failed command and its output.
- **Command blocks.** With shell integration (OSC 133), every command becomes a
  card with exit code and duration, copy/re-run/collapse/pin buttons, and a
  minimap that marks failures.
- **Its own look.** Custom title bar, tabs as pills (or vertical tabs), panes as
  rounded cards, smooth cursor, command palette that also searches tabs,
  sessions, history and files, 22 built-in themes plus a theme generated from
  your wallpaper, Mica/Acrylic on Windows 11, post-processing shaders.
- **Scriptable.** Config in Lua with hot reload, key tables, workflows,
  layouts, WASM plugins, a CLI and an MCP server so agents can drive the
  terminal (`duck cli spawn`, `read`, `send`, `blocks`, `last-failure`…).
- **Light.** One native binary (~19 MB), idle CPU ~0%, about 12k lines/s of
  output throughput.

## Install

You need [Rust](https://rustup.rs) (stable). The first build takes about 5
minutes.

### Windows 10/11

```powershell
winget install Rustlang.Rustup        # also needs the MSVC Build Tools
git clone https://github.com/herlondf/duck.git
cd duck
pwsh -File .\install.ps1               # build, PATH, ~/.duck.lua, Start menu shortcut
pwsh -File .\install.ps1 -Boot         # + open at logon restoring the last checkpoint
pwsh -File .\install.ps1 -ClaudeHooks  # + exact agent state via Claude Code hooks
```

### Linux

```bash
curl https://sh.rustup.rs -sSf | sh
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev   # Debian/Ubuntu
git clone https://github.com/herlondf/duck.git && cd duck
./install.sh                 # build, ~/.local/bin, ~/.duck.lua, app menu entry
./install.sh --boot --claude-hooks
```

### macOS

```bash
curl https://sh.rustup.rs -sSf | sh
git clone https://github.com/herlondf/duck.git && cd duck
./install.sh                 # build, ~/.local/bin, ~/Applications/Duck.app
```

Then run `duck` (opens the window) or `duck doctor` (checks config, server,
hooks and shell integration).

**Platform status:** Windows is the daily driver. Linux builds and passes the
test suite, but has had little interactive use. macOS is untested. The global
quick-terminal hotkey and Mica/Acrylic are Windows-only.

## First steps

| Key | Action |
|---|---|
| `Ctrl+Shift+P` | Command palette (commands, tabs, sessions, history, files) |
| `Ctrl+Shift+Home` | Home: recent projects, Claude sessions, actions |
| `Alt+\` / `Alt+-` | Split right / down |
| `Ctrl+Shift+T` / `Ctrl+Shift+W` | New tab / close tab |
| `Ctrl+Shift+S` / `Ctrl+Alt+N` | Switch / create workspace |
| `Ctrl+Alt+M` | Move the tab to another workspace |
| `Ctrl+Shift+A` | Agent Manager: every agent in a live grid |
| `Ctrl+Shift+I` / `Ctrl+Shift+Y` | Agent panel / session timeline |
| `Ctrl+Shift+E` | Fix the last failed command with an agent |
| `Ctrl+Shift+Enter` | Multi-line command editor (undo, selection) |
| `Ctrl+,` | Settings: themes, sliders, shaders, fonts |

Click CPU/RAM in the title bar for a system panel (per-core usage, memory,
top processes). Everything is configurable in `~/.duck.lua`; see
[duck.example.lua](duck.example.lua).

## Extensions

Extensions are separate programs (official SDK in TypeScript) that send data to
Duck; Duck draws them as side panels, floating details, title-bar items,
palette commands and notifications. A crashing extension never takes the
terminal down, and an extension that is turned off costs no memory.

### Bundled

| Area | Extensions |
|---|---|
| Code | **GitHub** (notifications, review requests, PRs, issues; every `gh auth login` account), **Azure DevOps** (PRs and builds), **Git** (branch, changes, worktrees and stashes of the active pane) |
| Issues and errors | **Jira** (Server/DC and Cloud), **Rollbar** (active errors, resolve, mute), **Grafana** (LogQL queries on Loki, alert when a count goes up) |
| Infrastructure | **Containers** (Docker and Podman, also inside WSL), **Database** (saved Postgres and Redis queries, read-only by default), **Ports** (listening TCP ports and their process) |
| Daily use | **Calendar** (Google/Outlook via iCal URL, reminders and Join button), **Notes** (per project and global), **Snippets** (command snippets and clipboard history), **Claude usage** (5h/7d limits and cost per Claude Code profile) |
| Others | **Google Play** (releases and reviews, reply to reviews), **Home Assistant** (lights, switches, covers, alerts) |

Build them with `install.ps1 -Extensions` (or `./install.sh --extensions`).
They start turned off; turn them on in `Ctrl+Shift+P` → "Extensões", where you
also change their settings and add accounts.

### Accounts and secrets

Tokens never go into a config file. The Extensions menu asks for them, or you
store them yourself:

```sh
echo <token> | duck secret set rollbar/<account>
duck secret has rollbar/<account>
```

On Windows they live in the Credential Manager (`duck:<name>`, per user); on
Linux/macOS in `~/.duck/secrets` with mode 600. The GitHub extension needs no
secret: it reads the accounts from the `gh` CLI.

### Write your own

Put a folder with a `duck-extension.json` manifest in `~/.duck/extensions/<name>`
(personal) or in `<duck>/extensions/<name>` (shared by every user of the
machine; a personal one with the same name wins). Any language works: Duck
starts the manifest's `command` and talks JSON lines over stdin/stdout. The
manifest can declare settings, accounts and secrets, and the Extensions menu
builds the forms from it. See [docs/EXTENSIONS.md](docs/EXTENSIONS.md); the
GitHub extension is the reference implementation.

## How it compares

A snapshot of the default, built-in capabilities of each tool (plugins and
external scripts can add more). ✅ yes · 🟡 partial · ❌ no

| | Duck | WezTerm | Windows Terminal | Ghostty | kitty | Alacritty | Warp | Wave | tmux | Zellij |
|---|---|---|---|---|---|---|---|---|---|---|
| Windows (native) | ✅ | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | ✅ | ❌ | ❌ |
| Linux / macOS | 🟡 | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| GPU rendering | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| Tabs and splits | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Shells survive closing the window | ✅ | 🟡 | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ✅ |
| Restore layout after reboot | ✅ | ❌ | 🟡 | ❌ | 🟡 | ❌ | 🟡 | 🟡 | 🟡 | ✅ |
| Resume AI agent sessions after reboot | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Knows agent state per tab (working / needs approval / done) | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | 🟡 | ❌ | ❌ | ❌ |
| Approve agent requests from another tab | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Command blocks (exit code, duration) | ✅ | 🟡 | 🟡 | ❌ | 🟡 | ❌ | ✅ | ✅ | ❌ | ❌ |
| Scriptable config | Lua | Lua | JSON | text | text/Python | TOML | ❌ | JSON | text | KDL |
| Plugins | Lua, WASM, extensions (any language) | Lua | ❌ | ❌ | Python | ❌ | ❌ | widgets | shell | WASM |
| API/CLI for agents to drive panes | ✅ (CLI + MCP) | ✅ (CLI) | 🟡 | ❌ | ✅ | ❌ | ❌ | 🟡 | ✅ | ✅ |
| Built-in AI chat | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ❌ | ❌ |
| Open source | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |

Duck does not try to be an AI chat app: it is a terminal where the agents you
already use (in the CLI) become visible and controllable. If you want an
integrated chat, Warp and Wave do that; if you only need a fast plain terminal
on Linux/macOS, Ghostty, kitty and Alacritty are more mature there.

## Documentation

- [Full manual (Portuguese)](docs/MANUAL.pt-BR.md): every key, the CLI, the
  agent features, security, persistence and the architecture.
- [Feature list and status](docs/FUNCIONALIDADES.md) and the
  [research notes](docs/PESQUISA.md) that shaped the design.

## Credits

- Terminal emulation: [alacritty_terminal](https://github.com/alacritty/alacritty).
  Rendering: [wgpu](https://wgpu.rs) and [glyphon](https://github.com/grovesNL/glyphon).
- Bundled font: JetBrains Mono (SIL Open Font License 1.1) patched by
  [Nerd Fonts](https://www.nerdfonts.com).
- Duck icon: based on a CC0 (public domain) SVG from [SVG Repo](https://www.svgrepo.com).
- Inspired by WezTerm, herdr, Warp, Ghostty, kitty, Zellij, Wave and Kiro.

## License

MIT. See [LICENSE](LICENSE). The bundled font keeps its own license ([fonts/OFL.txt](fonts/OFL.txt)).
