#!/usr/bin/env bash
# Duck installer for Linux and macOS / instalador do Duck para Linux e macOS.
#
#   ./install.sh                 build + install to ~/.local/bin, PATH and shell integration
#                                (offers to install Rust and the build packages when missing)
#   ./install.sh --boot          also open Duck at login, restoring the last checkpoint
#   ./install.sh --claude-hooks  also register `duck hook` in ~/.claude*/settings.json
#   ./install.sh --skip-build    reuse target/release
#   ./install.sh --extensions    build the bundled extensions (Node 18+); enable them in Ctrl+Shift+P
#   ./install.sh --all           same as --boot --claude-hooks --extensions
#   ./install.sh --yes           answer yes to every question (unattended install)
#   ./install.sh --no-shell      leave ~/.bashrc and ~/.zshrc alone
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

boot=0; hooks=0; build=1; exts=0; yes=0; shellrc=1
for a in "$@"; do
  case "$a" in
    --boot) boot=1 ;;
    --claude-hooks) hooks=1 ;;
    --skip-build) build=0 ;;
    --extensions) exts=1 ;;
    --all) boot=1; hooks=1; exts=1 ;;
    --yes|-y) yes=1 ;;
    --no-shell) shellrc=0 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) say "opcao desconhecida: $a" "unknown option: $a"; exit 2 ;;
  esac
done

# Pergunta sim/nao. --yes responde sim; sem terminal (CI) responde nao.
ask() {
  [ "$yes" = 1 ] && return 0
  [ -t 0 ] || return 1
  local r
  if [ "$pt" = 1 ]; then printf '%s [S/n] ' "$1"; else printf '%s [Y/n] ' "$2"; fi
  read -r r || return 1
  case "$r" in ""|[sSyY]*) return 0 ;; *) return 1 ;; esac
}

# Pacote de release: binarios prontos ao lado deste script, sem Cargo.toml.
prebuilt=0; src="$root/target/release"
if [ -x "$root/duck" ] && [ ! -f "$root/Cargo.toml" ]; then prebuilt=1; build=0; src="$root"; fi

os="$(uname -s)"
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
  done
else
  case ":$PATH:" in
    *":$bin:"*) ;;
    *) say "Adicione ao PATH: export PATH=\"$bin:\$PATH\" (no ~/.bashrc ou ~/.zshrc)" \
           "Add to PATH: export PATH=\"$bin:\$PATH\" (in ~/.bashrc or ~/.zshrc)" ;;
  esac
fi

"$bin/duck" init

# Tipo da instalacao para o `duck update`: so a do release se atualiza sozinha.
mkdir -p "$HOME/.local/share/duck"
if [ "$prebuilt" = 1 ]; then echo release; else echo "source $root"; fi > "$HOME/.local/share/duck/install-kind"

if [ "$os" = Linux ]; then
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
fi

[ "$boot" = 1 ] && "$bin/duck" install

if [ "$hooks" = 1 ]; then
  for d in "$HOME"/.claude*; do
    # Todo perfil ~/.claude*, mesmo sem settings.json (o duck cria o arquivo).
    [ -d "$d" ] && "$bin/duck" install --hooks "$d/settings.json"
  done
fi

if [ "$prebuilt" = 1 ]; then
  # As extensoes do pacote ja vem compiladas; o Duck procura em ~/.local/share/duck/extensions.
  mkdir -p "$HOME/.local/share/duck"
  rm -rf "$HOME/.local/share/duck/extensions"
  cp -R "$root/extensions" "$HOME/.local/share/duck/extensions"
  say "Extensoes instaladas; ative no Duck: Ctrl+Shift+P > Extensoes." "Extensions installed; enable them in Duck: Ctrl+Shift+P > Extensions."
elif [ "$exts" = 1 ]; then
  command -v npm >/dev/null || { say "Node.js nao encontrado (precisa do 18+)" "Node.js not found (18+ required)"; exit 1; }
  for e in "$root"/extensions/*/; do
    [ -f "$e/duck-extension.json" ] || continue
    n="$(basename "$e")"
    (cd "$e" && npm install --no-audit --no-fund >/dev/null && npm run build >/dev/null)
    say "Extensao compilada: $n" "Extension built: $n"
  done
  # O Duck procura <prefixo>/share/duck/extensions ao lado do ~/.local/bin.
  mkdir -p "$HOME/.local/share/duck"
  ln -sfn "$root/extensions" "$HOME/.local/share/duck/extensions"
  say "Ative as extensoes no Duck: Ctrl+Shift+P > Extensoes." "Enable extensions in Duck: Ctrl+Shift+P > Extensions."
fi

say "Pronto. Abra pelo menu de aplicativos (Duck) ou, num terminal novo, rode: duck" "Done. Open Duck from your app menu or, in a new terminal, run: duck"
say "Confira com: duck doctor (config, server, hooks, extensoes e tokens que faltam). Passo a passo: docs/PLAYBOOK.pt-BR.md" "Check with: duck doctor (config, server, hooks, extensions and missing tokens). Step by step: docs/PLAYBOOK.md"
