#!/usr/bin/env bash
# Duck installer for Linux and macOS / instalador do Duck para Linux e macOS.
#
#   ./install.sh                 build + install to ~/.local/bin
#   ./install.sh --boot          also open Duck at login, restoring the last checkpoint
#   ./install.sh --claude-hooks  also register `duck hook` in ~/.claude*/settings.json
#   ./install.sh --skip-build    reuse target/release
#   ./install.sh --extensions    build the bundled extensions (Node 18+); enable them in Ctrl+Shift+P
#
# Nothing outside your home directory is changed. / Nada fora do seu home e alterado.
set -euo pipefail

root="$(cd "$(dirname "$0")" && pwd)"
case "${LANG:-}${LC_ALL:-}" in pt*) pt=1 ;; *) pt=0 ;; esac
say() { if [ "$pt" = 1 ]; then printf '%s\n' "$1"; else printf '%s\n' "$2"; fi; }

boot=0; hooks=0; build=1; exts=0
for a in "$@"; do
  case "$a" in
    --boot) boot=1 ;;
    --claude-hooks) hooks=1 ;;
    --skip-build) build=0 ;;
    --extensions) exts=1 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) say "opcao desconhecida: $a" "unknown option: $a"; exit 2 ;;
  esac
done

os="$(uname -s)"
if [ "$build" = 1 ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    say "Rust nao encontrado. Instale com: curl https://sh.rustup.rs -sSf | sh" \
        "Rust not found. Install it with: curl https://sh.rustup.rs -sSf | sh"
    exit 1
  fi
  if [ "$os" = Linux ]; then
    # Bibliotecas de janela/teclado que o winit e o wgpu carregam.
    say "Linux: precisa de gcc, pkg-config e libs de X11/Wayland (ex.: sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev)" \
        "Linux: needs gcc, pkg-config and X11/Wayland libs (e.g. sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev)"
  fi
  say "Compilando (release, ~5 min na primeira vez)..." "Building (release, ~5 min the first time)..."
  cargo build --release --manifest-path "$root/Cargo.toml"
fi

bin="$HOME/.local/bin"
mkdir -p "$bin"
install -m 755 "$root/target/release/duck" "$bin/duck"
install -m 755 "$root/target/release/duckw" "$bin/duckw"

case ":$PATH:" in
  *":$bin:"*) ;;
  *) say "Adicione ao PATH: export PATH=\"$bin:\$PATH\" (no ~/.bashrc ou ~/.zshrc)" \
         "Add to PATH: export PATH=\"$bin:\$PATH\" (in ~/.bashrc or ~/.zshrc)" ;;
esac

"$bin/duck" init

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
    [ -f "$d/settings.json" ] && "$bin/duck" install --hooks "$d/settings.json"
  done
fi

if [ "$exts" = 1 ]; then
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

say "Pronto. Abra pelo menu de aplicativos (Duck) ou rode: duck" "Done. Open Duck from your app menu or run: duck"
