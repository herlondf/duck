#!/usr/bin/env bash
# Duck installer for Linux and macOS / instalador do Duck para Linux e macOS.
#
#   ./install.sh                 build + install to ~/.local/bin, PATH and shell integration
#                                (offers to install Rust and the build packages when missing).
#                                First install: a checklist of what to set up (see below)
#   ./install.sh --boot          also open Duck at login, restoring the last checkpoint
#   ./install.sh --claude-hooks  also register `duck hook` and the Duck statusline in
#                                ~/.claude/settings.json and every ~/.claude-*/settings.json
#   ./install.sh --skip-build    reuse target/release
#   ./install.sh --extensions    build the bundled extensions (Node 18+); enable them in Ctrl+Shift+P
#   ./install.sh --all           same as --boot --claude-hooks --extensions
#   ./install.sh --yes           answer yes to every question; with no other option it also
#                                applies the checklist defaults without asking (unattended)
#   ./install.sh --no-shell      leave ~/.bashrc and ~/.zshrc alone
#   ./install.sh --wizard        show the checklist again (not only on the first install)
#   ./install.sh --no-wizard     never show it (DUCK_NO_WIZARD=1 does the same)
#
# Checklist (first install, none of --boot/--claude-hooks/--extensions/--all/--no-shell):
# numbers toggle, Enter confirms. With stdin not a terminal (curl | sh) it reads
# /dev/tty; without one, the defaults apply. At the end it runs `duck doctor`.
# Checklist na primeira instalacao: numeros ligam/desligam, Enter confirma.
#
# From a release package (duck-linux-x86_64.tar.gz) the binaries are already built:
# nothing is compiled and the bundled extensions are installed as they are.
# Num pacote de release os binarios ja vem prontos: nada e compilado.
#
# Only system packages (with sudo, after asking) change anything outside your home.
# So os pacotes do sistema (com sudo, depois de perguntar) mexem fora do seu home.
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
case "${LANG:-}${LC_ALL:-}" in pt*) pt=1 ;; *) pt=0 ;; esac
say() { if [ "$pt" = 1 ]; then printf '%s\n' "$1"; else printf '%s\n' "$2"; fi; }

boot=0; hooks=0; status=0; build=1; exts=0; yes=0; shellrc=1; init=1; menu=1
explicit=0; wizard=auto
for a in "$@"; do
  case "$a" in
    --boot) boot=1; explicit=1 ;;
    --claude-hooks) hooks=1; status=1; explicit=1 ;;
    --skip-build) build=0 ;;
    --extensions) exts=1; explicit=1 ;;
    --all) boot=1; hooks=1; status=1; exts=1; explicit=1 ;;
    --yes|-y) yes=1 ;;
    --no-shell) shellrc=0; explicit=1 ;;
    --wizard) wizard=1 ;;
    --no-wizard) wizard=0 ;;
    -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
    *) say "opcao desconhecida: $a" "unknown option: $a"; exit 2 ;;
  esac
done

# De onde ler as respostas: o stdin, se for terminal; senao o /dev/tty (no
# `curl | sh` o stdin e o proprio script). A atualizacao automatica
# (DUCK_NO_WIZARD) nunca pergunta.
tty=""
if [ -t 0 ]; then tty=/dev/stdin
elif [ -z "${DUCK_NO_WIZARD:-}" ] && { : </dev/tty; } 2>/dev/null; then tty=/dev/tty
fi

# Pergunta sim/nao. --yes responde sim; sem terminal (CI) responde nao.
ask() {
  [ "$yes" = 1 ] && return 0
  [ -n "$tty" ] || return 1
  local r
  if [ "$pt" = 1 ]; then printf '%s [S/n] ' "$1"; else printf '%s [Y/n] ' "$2"; fi
  read -r r <"$tty" || return 1
  case "$r" in ""|[sSyY]*) return 0 ;; *) return 1 ;; esac
}

# Pacote de release: binarios prontos ao lado deste script, sem Cargo.toml.
prebuilt=0; src="$root/target/release"
if [ -x "$root/duck" ] && [ ! -f "$root/Cargo.toml" ]; then prebuilt=1; build=0; src="$root"; fi

os="$(uname -s)"
data="$HOME/.local/share/duck"
home_rel() { case "$1" in "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;; *) printf '%s' "$1" ;; esac; }

# Perfis do Claude Code: ~/.claude e ~/.claude-*. Sem nenhum, ~/.claude (o
# `duck install --hooks` cria): vale quando o Claude Code for instalado.
profiles=()
for d in "$HOME/.claude" "$HOME"/.claude-*; do if [ -d "$d" ]; then profiles+=("$d"); fi; done
noprofile=0
if [ "${#profiles[@]}" = 0 ]; then profiles=("$HOME/.claude"); noprofile=1; fi

# Assistente da primeira instalacao. As opcoes (--all, --boot...) pulam;
# reinstalar ou atualizar so mostra com --wizard (--yes: aplica o padrao).
run_wizard=0; autoupd=1; exts_on=0
if [ "$explicit" = 0 ] && [ "$wizard" != 0 ] && [ -z "${DUCK_NO_WIZARD:-}" ]; then
  if [ "$wizard" = 1 ] || [ "$yes" = 1 ] || [ ! -f "$data/install-kind" ]; then run_wizard=1; fi
fi
if [ "$run_wizard" = 1 ]; then
  plist=""; for d in "${profiles[@]}"; do plist="$plist $(home_rel "$d")"; done
  if [ "$noprofile" = 1 ]; then plist="$plist$(say ' (nenhum perfil ainda: cria)' ' (no profile yet: creates it)')"; fi
  rcs_l=""; for f in .bashrc .zshrc; do if [ -f "$HOME/$f" ]; then rcs_l="$rcs_l ~/$f"; fi; done
  if [ -z "$rcs_l" ]; then case "${SHELL:-}" in *zsh) rcs_l=" ~/.zshrc" ;; *) rcs_l=" ~/.bashrc" ;; esac; fi
  node=""; command -v node >/dev/null 2>&1 || node="$(say ', nao encontrado' ', not found')"
  lua_l=""; if [ -f "$HOME/.duck.lua" ]; then lua_l="$(say ' (ja existe: fica como esta)' ' (exists: kept as is)')"; fi
  if [ "$os" = Darwin ]; then menu_l="~/Applications/Duck.app"; else menu_l="$(say 'atalho no menu de aplicativos' 'app menu shortcut')"; fi
  if [ "$prebuilt" = 1 ]; then
    upd_l="$(say 'atualizacoes automaticas (baixa, confere e instala; reinicia o server quando nenhum agente trabalha)' 'automatic updates (download, verify, install; restart the server when no agent is working)')"
    ext_l="$(say "extensoes: ligar o Uso do Claude (precisa do Node 18+$node; as outras em Ctrl+Shift+P)" "extensions: enable Claude usage (needs Node 18+$node; the rest in Ctrl+Shift+P)")"
  else
    upd_l="$(say 'aviso de versao nova (pelo codigo nao se atualiza sozinho)' 'new version notice (a source install does not update itself)')"
    ext_l="$(say "extensoes: compilar e ligar o Uso do Claude (precisa do Node 18+$node)" "extensions: build them and enable Claude usage (needs Node 18+$node)")"
  fi
  labels=(
    "$(say "hooks do Claude Code em:$plist" "Claude Code hooks in:$plist")"
    "$(say 'statusline do Claude Code (uso de 5h/7d para o Uso do Claude e as Rotinas; a sua fica)' 'Claude Code statusline (5h/7d usage for Claude usage and Routines; yours is kept)')"
    "$(say "shell integration bash/zsh e PATH em$rcs_l" "bash/zsh shell integration and PATH in$rcs_l")"
    "~/.duck.lua$lua_l"
    "$menu_l"
    "$(say 'abrir no login, restaurando a ultima sessao' 'start on login, restoring the last session')"
    "$upd_l"
    "$ext_l"
  )
  on=(1 1 1 1 1 1 1 0)
  # Desligado antes (--wizard de novo): comeca desmarcado.
  if [ "$prebuilt" = 1 ]; then ukey=update_auto; else ukey=update_check; fi
  if grep -q "^config\.$ukey = false" "$HOME/.duck-local.lua" 2>/dev/null; then on[6]=0; fi
  if [ "$yes" = 1 ] || [ -z "$tty" ]; then
    say "Sem terminal para perguntar (ou --yes): usando o padrao." "No terminal to ask (or --yes): using the defaults."
  else
    while :; do
      echo
      say "Duck: o que configurar?" "Duck: what to set up?"
      i=0
      while [ "$i" -lt "${#labels[@]}" ]; do
        if [ "${on[$i]}" = 1 ]; then mark=x; else mark=' '; fi
        printf '  %d [%s] %s\n' "$((i + 1))" "$mark" "${labels[$i]}"
        i=$((i + 1))
      done
      if [ "$pt" = 1 ]; then printf 'Numeros ligam/desligam (ex.: 6 8), Enter confirma: '; else printf 'Numbers toggle (e.g. 6 8), Enter confirms: '; fi
      line=""
      read -r line <"$tty" || break
      [ -z "$line" ] && break
      for n in $(printf '%s' "$line" | tr ',;' '  '); do
        case "$n" in ''|*[!0-9]*) continue ;; esac
        if [ "$n" -ge 1 ] && [ "$n" -le "${#labels[@]}" ]; then
          j=$((n - 1)); on[$j]=$((1 - on[$j]))
        fi
      done
    done
  fi
  hooks=${on[0]}; status=${on[1]}; shellrc=${on[2]}; init=${on[3]}; menu=${on[4]}; boot=${on[5]}; autoupd=${on[6]}; exts_on=${on[7]}
  if [ "$prebuilt" = 0 ]; then exts=$exts_on; fi
fi
summary=()
done_item() { summary+=("$1"); }

if [ "$build" = 1 ]; then
  # rustup instala em ~/.cargo/bin, mas so entra no PATH num terminal novo.
  if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    if command -v curl >/dev/null 2>&1 && ask "Rust nao encontrado. Instalar agora (rustup, no seu home)?" "Rust not found. Install it now (rustup, in your home)?"; then
      curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal
      . "$HOME/.cargo/env"
    else
      say "Rust nao encontrado. Instale com: curl https://sh.rustup.rs -sSf | sh" \
          "Rust not found. Install it with: curl https://sh.rustup.rs -sSf | sh"
      exit 1
    fi
  fi

  # Compilador C (linker e Lua embutido), pkg-config e libs de janela/teclado do winit/wgpu.
  missing=""
  command -v cc >/dev/null 2>&1 || missing="$missing cc"
  if [ "$os" = Linux ]; then
    if command -v pkg-config >/dev/null 2>&1; then
      for lib in xkbcommon wayland-client x11; do
        pkg-config --exists "$lib" || missing="$missing $lib"
      done
    else
      missing="$missing pkg-config"
    fi
  fi
  if [ -n "$missing" ]; then
    if [ "$os" = Darwin ]; then
      say "Faltando:$missing. Rode: xcode-select --install (e depois este script de novo)" \
          "Missing:$missing. Run: xcode-select --install (then this script again)"
      exit 1
    fi
    pkgs=""; inst=""
    if command -v apt-get >/dev/null 2>&1; then
      pkgs="build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev"; inst="apt-get install -y"
    elif command -v dnf >/dev/null 2>&1; then
      pkgs="gcc pkgconf-pkg-config libxkbcommon-devel wayland-devel libX11-devel"; inst="dnf install -y"
    elif command -v pacman >/dev/null 2>&1; then
      pkgs="base-devel pkgconf libxkbcommon wayland libx11"; inst="pacman -S --needed --noconfirm"
    elif command -v zypper >/dev/null 2>&1; then
      pkgs="gcc pkg-config libxkbcommon-devel wayland-devel libX11-devel"; inst="zypper install -y"
    else
      say "Faltando:$missing. Instale gcc, pkg-config e os pacotes de desenvolvimento de libxkbcommon, wayland e libX11." \
          "Missing:$missing. Install gcc, pkg-config and the dev packages for libxkbcommon, wayland and libX11."
      exit 1
    fi
    sudo=""; [ "$(id -u)" = 0 ] || sudo="sudo "
    say "Faltando:$missing" "Missing:$missing"
    if ask "Instalar agora? $sudo$inst $pkgs" "Install now? $sudo$inst $pkgs"; then
      case "$inst" in apt-get*) ${sudo}apt-get update -qq ;; esac
      ${sudo}$inst $pkgs
    else
      say "Instale com: $sudo$inst $pkgs" "Install with: $sudo$inst $pkgs"
      exit 1
    fi
  fi
  say "Compilando (release, ~5 min na primeira vez)..." "Building (release, ~5 min the first time)..."
  cargo build --release --manifest-path "$root/Cargo.toml"
fi

bin="$HOME/.local/bin"
mkdir -p "$bin"
install -m 755 "$src/duck" "$bin/duck"
install -m 755 "$src/duckw" "$bin/duckw"

# PATH e shell integration no ~/.bashrc e ~/.zshrc, num bloco que e trocado
# a cada instalacao (rodar de novo nao duplica).
if [ "$shellrc" = 1 ]; then
  rcs=""
  for f in .bashrc .zshrc; do [ -f "$HOME/$f" ] && rcs="$rcs $HOME/$f"; done
  if [ -z "$rcs" ]; then
    case "${SHELL:-}" in *zsh) rcs="$HOME/.zshrc" ;; *) rcs="$HOME/.bashrc" ;; esac
  fi
  for rc in $rcs; do
    if [ -f "$rc" ]; then
      # Backup so na primeira vez: guarda o arquivo de antes do Duck.
      [ -f "$rc.bak-duck" ] || cp "$rc" "$rc.bak-duck"
      tmp="$(mktemp)"
      awk '/^# >>> duck >>>$/{skip=1} !skip{print} /^# <<< duck <<<$/{skip=0}' "$rc" > "$tmp"
      cat "$tmp" > "$rc"; rm -f "$tmp"
    fi
    {
      echo '# >>> duck >>>'
      echo 'case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac'
      echo '[ -f "$HOME/.local/share/duck/shell/duck.sh" ] && . "$HOME/.local/share/duck/shell/duck.sh"'
      echo '# <<< duck <<<'
    } >> "$rc"
    say "PATH e shell integration em $rc (backup: $rc.bak-duck)" "PATH and shell integration in $rc (backup: $rc.bak-duck)"
    done_item "$(say "shell integration e PATH: $(home_rel "$rc")" "shell integration and PATH: $(home_rel "$rc")")"
  done
else
  case ":$PATH:" in
    *":$bin:"*) ;;
    *) say "Adicione ao PATH: export PATH=\"$bin:\$PATH\" (no ~/.bashrc ou ~/.zshrc)" \
           "Add to PATH: export PATH=\"$bin:\$PATH\" (in ~/.bashrc or ~/.zshrc)" ;;
  esac
fi

if [ "$init" = 1 ]; then
  "$bin/duck" init
  done_item "~/.duck.lua"
fi

# Tipo da instalacao para o `duck update`: so a do release se atualiza sozinha.
mkdir -p "$data"
if [ "$prebuilt" = 1 ]; then echo release; else echo "source $root"; fi > "$data/install-kind"

# Atualizacoes (o padrao e ligado): desmarcado no assistente grava no
# ~/.duck-local.lua; marcado so desfaz um "false" gravado antes.
if [ "$run_wizard" = 1 ]; then
  if [ "$autoupd" = 0 ]; then
    "$bin/duck" config set "$ukey" false >/dev/null
  elif grep -q "^config\.$ukey = " "$HOME/.duck-local.lua" 2>/dev/null; then
    "$bin/duck" config set "$ukey" true >/dev/null
  fi
  if [ "$prebuilt" = 1 ] && [ "$autoupd" = 1 ]; then done_item "$(say 'atualizacoes automaticas' 'automatic updates')"
  elif [ "$prebuilt" = 1 ]; then done_item "$(say 'atualizacoes: so avisa (update_auto = false)' 'updates: notify only (update_auto = false)')"
  elif [ "$autoupd" = 1 ]; then done_item "$(say 'aviso de versao nova' 'new version notice')"
  fi
fi

if [ "$menu" != 1 ]; then
  :
elif [ "$os" = Linux ]; then
  icons="$HOME/.local/share/icons/hicolor/256x256/apps"
  apps="$HOME/.local/share/applications"
  mkdir -p "$icons" "$apps"
  cp "$root/assets/duck.png" "$icons/duck.png"
  cat > "$apps/duck.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Duck
Comment=Terminal for AI agents
Exec=$bin/duckw
Icon=duck
Terminal=false
Categories=System;TerminalEmulator;
EOF
  say "Atalho no menu de aplicativos: $apps/duck.desktop" "App menu entry: $apps/duck.desktop"
  done_item "$(say 'atalho no menu de aplicativos' 'app menu shortcut')"
elif [ "$os" = Darwin ]; then
  # App minimo em ~/Applications (abre o duckw; icone gerado do PNG).
  app="$HOME/Applications/Duck.app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cat > "$app/Contents/MacOS/Duck" <<EOF
#!/bin/sh
exec "$bin/duckw" "\$@"
EOF
  chmod +x "$app/Contents/MacOS/Duck"
  if command -v sips >/dev/null && command -v iconutil >/dev/null; then
    set_dir="$(mktemp -d)/duck.iconset"; mkdir -p "$set_dir"
    for s in 16 32 128 256; do
      sips -z $s $s "$root/assets/duck.png" --out "$set_dir/icon_${s}x${s}.png" >/dev/null
      sips -z $((s*2)) $((s*2)) "$root/assets/duck.png" --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
    done
    iconutil -c icns "$set_dir" -o "$app/Contents/Resources/duck.icns" || true
  fi
  cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Duck</string>
  <key>CFBundleIdentifier</key><string>dev.duck.terminal</string>
  <key>CFBundleExecutable</key><string>Duck</string>
  <key>CFBundleIconFile</key><string>duck</string>
  <key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
EOF
  say "App: $app" "App: $app"
  done_item "~/Applications/Duck.app"
fi

if [ "$boot" = 1 ]; then
  "$bin/duck" install
  done_item "$(say 'abre no login' 'starts on login')"
fi

if [ "$hooks" = 1 ]; then
  # Todo perfil, mesmo sem settings.json (o duck cria o arquivo).
  plist=""
  for d in "${profiles[@]}"; do
    "$bin/duck" install --hooks "$d/settings.json"
    plist="$plist $(home_rel "$d")"
  done
  done_item "$(say "hooks do Claude Code:$plist" "Claude Code hooks:$plist")"
fi
if [ "$status" = 1 ]; then
  # So onde nao ha statusline (a sua fica) ou troca a do proprio Duck.
  for d in "${profiles[@]}"; do "$bin/duck" install --statusline "$d/settings.json"; done
  done_item "$(say 'statusline do Claude Code' 'Claude Code statusline')"
fi

if [ "$prebuilt" = 1 ]; then
  # As extensoes do pacote ja vem compiladas; o Duck procura em ~/.local/share/duck/extensions.
  mkdir -p "$data"
  rm -rf "$data/extensions"
  cp -R "$root/extensions" "$data/extensions"
  say "Extensoes instaladas; ative no Duck: Ctrl+Shift+P > Extensoes." "Extensions installed; enable them in Duck: Ctrl+Shift+P > Extensions."
elif [ "$exts" = 1 ] && [ "$run_wizard" = 1 ] && ! command -v npm >/dev/null; then
  say "Node.js nao encontrado: extensoes nao compiladas (depois: ./install.sh --skip-build --extensions)" \
      "Node.js not found: extensions not built (later: ./install.sh --skip-build --extensions)"
  exts_on=0
elif [ "$exts" = 1 ]; then
  command -v npm >/dev/null || { say "Node.js nao encontrado (precisa do 18+)" "Node.js not found (18+ required)"; exit 1; }
  for e in "$root"/extensions/*/; do
    [ -f "$e/duck-extension.json" ] || continue
    n="$(basename "$e")"
    (cd "$e" && npm install --no-audit --no-fund >/dev/null && npm run build >/dev/null)
    say "Extensao compilada: $n" "Extension built: $n"
  done
  # O Duck procura <prefixo>/share/duck/extensions ao lado do ~/.local/bin.
  mkdir -p "$data"
  ln -sfn "$root/extensions" "$data/extensions"
  say "Ative as extensoes no Duck: Ctrl+Shift+P > Extensoes." "Enable extensions in Duck: Ctrl+Shift+P > Extensions."
  done_item "$(say 'extensoes compiladas' 'extensions built')"
fi

# Assistente: liga o Uso do Claude (le o que a statusline grava).
if [ "$exts_on" = 1 ]; then
  "$bin/duck" config set extensions.claude-usage.enabled true >/dev/null
  done_item "$(say 'extensao Uso do Claude ligada' 'Claude usage extension enabled')"
fi

if [ "${#summary[@]}" -gt 0 ]; then
  echo
  say "Configurado:" "Configured:"
  for l in "${summary[@]}"; do printf '  - %s\n' "$l"; done
fi
# Conferencia final; a atualizacao automatica pula (ninguem esta olhando).
if [ -z "${DUCK_NO_WIZARD:-}" ]; then
  echo
  "$bin/duck" doctor || true
fi

echo
say "Pronto. Abra pelo menu de aplicativos (Duck) ou, num terminal novo, rode: duck" "Done. Open Duck from your app menu or, in a new terminal, run: duck"
say "Confira de novo quando quiser: duck doctor. Passo a passo: docs/PLAYBOOK.pt-BR.md" "Check again any time: duck doctor. Step by step: docs/PLAYBOOK.md"
