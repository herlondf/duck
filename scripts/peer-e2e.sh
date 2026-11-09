#!/usr/bin/env bash
# Teste de ponta a ponta dos computadores pareados (painel Computadores,
# duck peer/msg/send-file/relay) com varios servers isolados nesta maquina:
# cada um com HOME e USER proprios (dados, config, cofre e socket separados)
# e escuta em 127.0.0.x. Nao mexe no Duck do usuario.
#
#   cargo build && scripts/peer-e2e.sh          (BIN=... para outro binario)
#
# Cenarios: convite; mensagem com aprovacao; agentes de la; arquivo;
# diagnostico; pedido com aprovacao; o aceite que nao volta (o outro so
# escuta noutro IP: o pareamento fecha pela consulta); conversa encerrada;
# relay (dois computadores sem escuta direta, so pelo relay).
set -uo pipefail
BIN=${BIN:-$PWD/target/debug/duck}
T=$(mktemp -d /tmp/duck-e2e.XXXXXX)
FAIL=0
PIDS=()

say() { printf '\n== %s\n' "$*"; }
ok() { printf '  ok   %s\n' "$*"; }
bad() { printf '  FAIL %s\n' "$*"; FAIL=1; }
# run <nome> <args...>: o duck daquele "computador".
run() { local n=$1; shift; env -u DUCK_TOKEN -u DUCK_PANE -u DUCK_BIN -u LASSO_TOKEN -u LASSO_PANE \
        HOME="$T/$n" USER="e2e$n" XDG_CONFIG_HOME="$T/$n/.config" "$BIN" "$@"; }
# mk <nome> <listen> [relay] [segredo]
mk() {
  mkdir -p "$T/$1/Downloads"
  printf "config.peer = { listen = '%s', name = '%s', relay = '%s', relay_secret = '%s' }\n" "$2" "$1" "${3:-}" "${4:-}" > "$T/$1/.duck-local.lua"
  run "$1" cli list >/dev/null || bad "server $1 nao subiu"
}
json() { python3 -c "import json,sys; v=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
# approve <nome> <tipo>: aprova o primeiro pedido desse tipo (espera ate 10 s).
approve() {
  local id=""
  for _ in $(seq 50); do
    id=$(run "$1" cli peers-state 2>/dev/null | json "next((a['id'] for a in v['asks'] if a['kind']=='$2'), '')")
    [ -n "$id" ] && break; sleep 0.2
  done
  [ -n "$id" ] || { bad "$1: nenhum pedido '$2' para aprovar"; return 1; }
  run "$1" cli approve --id "$id" --ok true >/dev/null
}
# approve_msg <nome>: mensagem de agente (aparece nas aprovacoes do frame).
approve_msg() {
  local id=""
  for _ in $(seq 50); do
    id=$(run "$1" cli frame --client e2e 2>/dev/null | json "next((a['id'] for a in v.get('approvals',[]) if a['kind']=='peer'), '')")
    [ -n "$id" ] && break; sleep 0.2
  done
  [ -n "$id" ] || { bad "$1: mensagem nao chegou para aprovar"; return 1; }
  run "$1" cli approve --id "$id" --ok true >/dev/null
}
agent_pane() { # cria um pane com "agente" parado para receber mensagens
  local p; p=$(run "$1" cli spawn --cmd "cat" | json "v['pane']")
  run "$1" cli set-state --pane "$p" --state idle --agent claude >/dev/null
  echo "$p"
}
cleanup() {
  for n in a b c d e f; do [ -d "$T/$n" ] && run "$n" cli kill-server >/dev/null 2>&1; done
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  rm -rf "$T"
}
trap cleanup EXIT

[ -x "$BIN" ] || { echo "sem binario: $BIN (cargo build)"; exit 2; }
echo "binario: $BIN · pasta: $T"

say "convite: b gera, a usa"
mk a 127.0.0.1:17101; mk b 127.0.0.1:17102
INV=$(run b peer invite 2>/dev/null)
[[ $INV == duck:b@* ]] && ok "convite $INV" || bad "convite: $INV"
out=$(run a peer join "$INV" 2>&1); [[ $out == *"conectado com b"* ]] && ok "$out" || bad "join: $out"
out=$(run a peer list); [[ $out == *"online  b"*"cifrado"* ]] && ok "a ve b online e cifrado" || bad "a list: $out"
out=$(run b peer list); [[ $out == *"online  a"*"cifrado"* ]] && ok "b ve a online e cifrado" || bad "b list: $out"

say "mensagem com aprovacao, agentes de la, resposta"
PB=$(agent_pane b)
out=$(run a peer sessions b); [[ $out == *"$PB"*"idle"* ]] && ok "a lista os agentes de b" || bad "sessions: $out"
out=$(run a msg b "oi do a" 2>&1); [[ $out == *"espera o usuario"* ]] && ok "$out" || bad "msg: $out"
approve_msg b && sleep 0.5
out=$(run b cli read --pane "$PB" | json "v.get('text','')"); [[ $out == *"oi do a"* ]] && ok "chegou no agente de b" || bad "nao chegou: $out"
CONV=$(printf '%s' "$out" | grep -o 'conversa [0-9a-f]*' | head -1 | cut -d' ' -f2)
PA=$(agent_pane a)
out=$(run b msg a --conv "$CONV" "resposta do b" 2>&1); [[ $out == *"conversa $CONV"* ]] && ok "b responde na mesma conversa" || bad "resposta: $out"
approve_msg a && sleep 0.5
out=$(run a cli read --pane "$PA" | json "v.get('text','')"); [[ $out == *"resposta do b"* ]] && ok "resposta chegou em a" || bad "resposta nao chegou: $out"
out=$(run a peer convs); [[ $out == *"$CONV"*"com b"* ]] && ok "conversa listada" || bad "convs: $out"
run b peer close "$CONV" >/dev/null
out=$(run a msg b --conv "$CONV" "mais uma" 2>&1); [[ $out == *"encerrada"* ]] && ok "conversa encerrada recusa" || bad "conversa encerrada aceitou: $out"

say "arquivo"
head -c 300000 /dev/urandom > "$T/teste.bin"
out=$(run a send-file b "$T/teste.bin" 2>&1); [[ $out == *"chegou"* ]] && ok "$out" || bad "send-file: $out"
approve b file && sleep 0.5
f=$(ls "$T"/b/Downloads/Duck/teste.bin 2>/dev/null)
if [ -n "$f" ] && cmp -s "$f" "$T/teste.bin"; then ok "arquivo salvo e igual"; else bad "arquivo nao chegou ou diferente"; fi

say "diagnostico"
out=$(run a peer doctor b); [[ $out == *"Volta: b alcança"* ]] && ok "doctor ve a volta" || bad "doctor: $out"

say "pedido com aprovacao (sem convite)"
mk c 127.0.0.1:17103
out=$(run c peer join "$INV" 2>&1); [[ $out == *"invalido"* ]] && ok "convite usado nao vale para outro" || bad "convite reusado: $out"
( run c peer connect 127.0.0.1:17101 > "$T/c.out" 2>&1 ) & PIDS+=($!)
approve a pair; wait "${PIDS[-1]}"
out=$(cat "$T/c.out"); [[ $out == *"Numero de confirmacao"*"aceitou"* ]] && ok "c pediu, a aprovou" || bad "connect: $out"
out=$(run a peer list); [[ $out == *"online  c"* ]] && ok "a guardou c" || bad "a sem c: $out"

say "aceite sem caminho de volta (o bug da VM): d escuta so em 127.0.0.4"
mk d 127.0.0.4:17104
( run d peer connect 127.0.0.1:17101 > "$T/d.out" 2>&1 ) & PIDS+=($!)
approve a pair; wait "${PIDS[-1]}"
out=$(cat "$T/d.out"); [[ $out == *"aceitou"* ]] && ok "d fechou o pareamento pela consulta" || bad "d: $out"
sleep 1
out=$(run a cli inbox | json "' | '.join(i['title'] for i in v['items'])"); [[ $out == *"sem caminho de volta"* ]] && ok "a avisa que nao alcanca d" || bad "aviso: $out"
out=$(run d peer doctor a); [[ $out == *"NÃO alcança"* ]] && ok "doctor de d acusa que a nao volta" || bad "doctor d: $out"

say "relay: e e f sem escuta direta"
"$BIN" relay --listen 127.0.0.1:17900 --secret s3 > "$T/relay.out" 2>&1 & PIDS+=($!)
sleep 0.5
mk e "" 127.0.0.1:17900 s3; mk f "" 127.0.0.1:17900 s3
sleep 1.5
INV=$(run e peer invite 2>/dev/null)
[[ $INV == *"relay:127.0.0.1:17900"* ]] && ok "convite pelo relay" || bad "convite e: $INV"
out=$(run f peer join "$INV" 2>&1); [[ $out == *"conectado com e"* ]] && ok "$out" || bad "join pelo relay: $out"
PE=$(agent_pane e)
out=$(run f msg e "pelo relay" 2>&1); [[ $out == *"espera o usuario"* ]] && ok "mensagem pelo relay" || bad "msg relay: $out"
approve_msg e && sleep 0.5
out=$(run e cli read --pane "$PE" | json "v.get('text','')"); [[ $out == *"pelo relay"* ]] && ok "chegou em e pelo relay" || bad "relay nao entregou: $out"
PF=$(agent_pane f)
out=$(run e msg f "volta pelo relay" 2>&1); [[ $out == *"espera o usuario"* ]] && ok "e responde pelo relay" || bad "volta relay: $out"

echo
if [ $FAIL = 0 ]; then echo "TUDO OK"; else echo "FALHOU"; fi
exit $FAIL
