#!/bin/sh
# Instala ou atualiza o Duck a partir do ultimo release, sem compilar nada.
# Installs or updates Duck from the latest release, nothing to build.
#
#   curl -fsSL https://raw.githubusercontent.com/herlondf/duck/main/get.sh | sh
#   curl -fsSL .../get.sh | sh -s -- --boot --claude-hooks     (opcoes do install.sh)
#   DUCK_VERSION=v0.1.0 sh get.sh                              (uma versao especifica)
set -eu

repo="${DUCK_REPO:-herlondf/duck}"
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) name=duck-linux-x86_64 ;;
  *)
    echo "Ainda nao ha pacote pronto para $(uname -s) $(uname -m). Compile do codigo: https://github.com/$repo#install" >&2
    echo "No prebuilt package for $(uname -s) $(uname -m) yet. Build from source: https://github.com/$repo#install" >&2
    exit 1 ;;
esac
if [ -n "${DUCK_VERSION:-}" ]; then base="https://github.com/$repo/releases/download/$DUCK_VERSION"
else base="https://github.com/$repo/releases/latest/download"; fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
echo "Baixando / downloading $base/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
(cd "$tmp" && grep "  $name.tar.gz\$" SHA256SUMS | sha256sum -c --quiet -) || {
  echo "Checksum nao confere / checksum mismatch: $name.tar.gz" >&2; exit 1; }
tar xzf "$tmp/$name.tar.gz" -C "$tmp"
bash "$tmp/$name/install.sh" "$@"
