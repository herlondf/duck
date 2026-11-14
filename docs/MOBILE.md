# Duck no celular: contrato entre o Duck e o app

O app (`herlondf/duck-app`, Flutter) é um **controle remoto dos agentes**: vê,
aprova, manda recado, inicia e interrompe agentes e, com permissão, digita
no shell. Não há backend: o "servidor" é o próprio Duck no computador. Este
documento é o contrato entre os dois lados; mudou aqui, muda nos dois.

As seções 1 a 6 são o contrato; a 7 traz o formato exato de cada evento e de
cada comando, com exemplo. No Duck: `src/server/mobile.rs` (sessão, push),
`src/server/cmds.rs` (comandos ★), `src/mobile.rs` (permissões, QR, guarda),
`src/fcm.rs` e `src/relay.rs` (push).

## 1. Conexão

- O celular é um **pareado do tipo `mobile`** (chave própria, como um
  computador), com o mesmo canal de `secure.rs`: `MAGIC` + Noise
  `Noise_XX_25519_ChaChaPoly_BLAKE2s`, quadros `u16` BE, mensagem da
  aplicação = JSON com o tamanho total (`u32`) no primeiro quadro.
- Caminhos, na ordem: endereços diretos (rede local, Tailscale) e depois o
  relay (`{"dial": chave, "secret"}`, ver `relay.rs`). O app guarda a lista e
  coloca na frente o que funcionou.
- O Duck identifica o celular pela chave pública do handshake. Chave
  desconhecida e sem convite válido: o Duck responde um erro `forbidden`
  (`"celular nao pareado com este Duck: leia o QR de novo"`) e fecha a conexão.
- Toda mensagem do app tem `op`; é isso que separa o celular de outro Duck na
  mesma porta.

## 2. Pareamento por QR

- No Duck: **Ctrl+Shift+4 > Celulares > Parear celular** (e `duck mobile pair`
  na CLI, que imprime o QR no terminal). O QR é
  `duck://pair/<base64url(JSON)>` (base64url **sem** `=` no fim), com o JSON:
  `{"v":1,"name":"hd-fedora","key":"<hex 32B>","addrs":["192.168.0.10:7878"],
  "relay":"relay.exemplo.com:7879","relay_secret":"...","invite":"<16 hex>","exp":<unix>}`.
  O convite vale 10 minutos e serve uma vez só.
  - `addrs`: endereços diretos (`ip:porta`, sem o prefixo `relay:`); pode vir
    vazio quando o Duck só usa relay.
  - `relay` só vem com relay configurado; `relay_secret` só quando o relay tem
    senha. Para discar pelo relay o app usa a chave inteira (`key`).
- O app conecta, faz o handshake e manda
  `{"op":"pair","kind":"mobile","name":"<aparelho>","invite":"..."}` (pode ter `id`).
- O Duck aceita e responde `{"ok":true,"scopes":[...],"confirm":"123-456-789"}`.
  O `confirm` sai do hash do handshake, como em `secure.rs`. O app mostra o
  número e o Duck mostra um aviso com o mesmo número.
  - A resposta traz também `"name"` (o nome que o Duck deu ao aparelho: o
    pedido limpo, minúsculo, e `-2`, `-3`... se já existe) e `"duck"` (o nome
    do computador). Exemplo:
    `{"ok":true,"scopes":["view","approve","message"],"confirm":"482-019-377","name":"pixel8","duck":"hd-fedora"}`.
  - Convite errado, usado ou vencido:
    `{"ok":false,"error":{"code":"forbidden","message":"convite invalido, ja usado ou vencido: gere outro QR no Duck"}}`, e fecha.
  - **A mesma conexão continua como sessão** (seção 3): não precisa reconectar.
  - O mesmo celular (mesma chave) pareando de novo mantém nome e permissões.
- Permissões iniciais: `view`, `approve`, `message`. As outras se ligam no
  painel Celulares do Duck (ou `duck mobile scopes <nome> +control -keyboard`).
- No painel, cada celular aparece com nome, última conexão, permissões
  (marcar e desmarcar) e **Revogar** (apaga a chave na hora).
- Guarda no Duck: nomes e última conexão em `mobile.json` (pasta de dados);
  chave, permissões e token do FCM no cofre (`mobile/<nome>`, como a chave de
  `peer/static`).

## 3. Sessão

Depois do handshake, mensagens JSON nos dois sentidos:

- Pedido: `{"id":7,"op":"call","cmd":"agents","args":{...}}`.
  - Sucesso: `{"id":7,"ok":true,"data":{...}}`.
  - Erro: `{"id":7,"ok":false,"error":{"code":"forbidden|not_found|invalid|unavailable","message":"..."}}`.
  - `id`: qualquer JSON (número ou texto); volta igual. As respostas podem
    chegar **fora de ordem** (cada `call` roda em paralelo, até 4 por sessão;
    o 5º recebe `unavailable` "pedidos demais ao mesmo tempo").
  - `forbidden`: comando fora da tabela da seção 4 ou sem a permissão;
    `not_found`: pane/pedido/sessão que não existe **ou pane privado**;
    `invalid`: argumento faltando/errado; `unavailable`: o recurso não está
    disponível agora (Tradutor, extensão Rotinas desligada, pasta inacessível).
- Assinar eventos: `{"id":8,"op":"subscribe","events":["agents","approvals","inbox","screen:<pane>"]}`.
  Depois disso o Duck manda `{"ev":"<nome>","data":{...}}` sempre que algo
  mudar. `screen:<pane>` manda a tela daquele pane, no máximo 4 por segundo.
  - Cada `subscribe` **troca** o conjunto inteiro (mande a lista toda; `[]`
    cancela tudo). Precisa da permissão `view`.
  - Resposta: `{"id":8,"ok":true,"data":{"events":["agents","screen:12"],"rejected":["screen:99"]}}`
    (`rejected`: nome desconhecido, pane que não existe ou privado).
  - Logo depois da resposta vem o estado atual de cada evento novo; depois,
    só quando muda. `agents`, `approvals` e `inbox`: no máximo 1 por segundo.
  - O pane de `screen:<pane>` fechou (ou ficou privado): vem
    `{"ev":"screen:12","data":{"pane":12,"closed":true}}` e a assinatura sai.
  - Celular revogado com a sessão aberta: vem `{"ev":"revoked","data":{}}` e
    o Duck fecha a conexão.
- Ping: `{"op":"ping"}` → `{"op":"pong"}` (com `id`, se o ping tiver). O Duck
  fecha a conexão depois de 5 minutos sem nada **vindo do app** (eventos do
  Duck não contam); o app reconecta quando volta para a frente.
- Todo `call` vai para o `audit.log` com `caller = "mobile:<nome>"` (leituras
  inclusive; o conteúdo de `file-put` e o token não entram, só o tamanho).
- `op` desconhecida: erro `invalid`. `{"op":"pair",...}` numa sessão já
  pareada responde `{"ok":true,"data":{"scopes":[...]}}`.

## 4. Permissões e comandos

O `cmd` é o nome do comando do server, quando já existe. Os novos estão
marcados com ★. Pane privado nunca aparece e nunca é lido. Texto da tela
sai com os segredos mascarados.

| Permissão | Comandos |
|---|---|
| `view` | `agents` (todos os workspaces, com computador, conta, estado, passo, contexto, fila), `list`, `read` (tela + histórico recente), `last-output`, `blocks`, `block-output`, `why`, `queue-ls`, `inbox`, `inbox-read`, `loops`, `changes` (arquivos mexidos + diff), ★`timeline` (linha do tempo da sessão do agente), ★`usage` (uso do Claude por conta), ★`system` (CPU, RAM, versão), ★`routines` (lista + último resultado), ★`recent-dirs` (pastas recentes/zoxide), ★`file-get` (baixar arquivo de um pane, até 20 MB) |
| `approve` | `approve` (pedidos do Duck), ★`agent-answer` `{pane, yes}` (só com o agente em `blocked`: manda `1` ou Esc) |
| `message` | `queue`, `queue-rm`, `queue-up`, `queue-now`, ★`translate` `{text, pane}` (o motor do Tradutor, pela CLI do agente do pane) |
| `control` | `spawn` (agente numa pasta, com tarefa inicial, worktree opcional), ★`interrupt` `{pane, key: "esc"\|"ctrl-c"}`, ★`resume` (sessões do Claude para retomar), `loop-start`, `loop-stop`, ★`routine-run`, ★`file-put` (arquivo/foto na pasta do pane, até 20 MB, nome saneado), `close` |
| `keyboard` | `send`, `paste` em qualquer pane não privado (shell incluído) |

Regras do lado do Duck:

- Comando de pane (`read`, `last-output`, `blocks`, `block-output`, `why`,
  `queue-ls`, `changes`, `timeline`, `file-get`, `agent-answer`, `queue*`,
  `translate`, `interrupt`, `loop-*`, `file-put`, `close`, `send`, `paste`):
  `args.pane` é **obrigatório** (nada de "o pane ativo da janela").
- Segredos: todo texto da resposta e dos eventos passa pelos padrões de
  segredo da config (`secret_patterns`) e pelos da tela (os mesmos que a
  janela esconde) e vira `***`. Só o `data` de `file-get` (o arquivo) não.
- `push-token` (★, `view`) é da sessão: guarda o token no celular que chamou.
- `args.client` é ignorado.

## 5. Push (o relay acorda o app)

- O app manda o token do FCM: `{"op":"call","cmd":"push-token","args":{"token":"..."}}`
  (★, permissão `view`). O Duck guarda o token junto do celular. Token vazio
  (`""`) desliga o push daquele celular.
- Nos eventos `approval` (Duck ou agente pedindo), `done` (agente terminou),
  `loop` (loop falhou ou passou) e `command` (comando longo terminou), o Duck
  manda ao relay uma linha de controle:
  `{"push":["<token>"],"secret":"<relay_secret>","data":{"duck":"hd-fedora","kind":"approval","pane":12,"approval":34,"n":2}}`.
  - `approval` só vem em pedido do Duck (o id para `approve`); agente em
    `blocked` manda `kind:"approval"` sem `approval` (responder com
    `agent-answer`). `pane` é 0 em pedido sem pane (pareamento de computador).
  - `n`: pedidos do Duck esperando + agentes em `blocked` (sem os privados).
  - `loop` traz também `"ok": true|false` (passou ou desistiu).
  - Eventos de pane privado não geram push. Sem relay configurado, nada sai.
  - Vai para todos os celulares com token, uma linha por evento.
- O relay manda isso pelo FCM HTTP v1 como mensagem de dados (`priority:
  high`). A conta de serviço fica no relay: `--fcm-credentials arquivo.json`
  ou `DUCK_RELAY_FCM_CREDENTIALS`. Ele limita 30 pushes por minuto por
  segredo.
  - No FCM todo valor de `data` vira **texto**: o app recebe
    `{"duck":"hd-fedora","kind":"approval","pane":"12","approval":"34","n":"2"}`.
  - Corpo enviado: `{"message":{"token":"...","data":{...},"android":{"priority":"high"},
    "apns":{"headers":{"apns-push-type":"background","apns-priority":"5"},"payload":{"aps":{"content-available":1}}}}}`.
  - O relay responde ao Duck `{"ok":true,"sent":1,"errors":[]}`; passou do
    limite: `{"ok":false,"error":"limite de 30 pushes por minuto"}`; sem FCM:
    `{"ok":false,"error":"este relay nao tem push (falta --fcm-credentials)"}`.
- **Sem conteúdo no push:** nada de título de aba, comando ou texto. O app
  monta uma notificação local genérica ("hd-fedora: um agente pede
  aprovação"), com **Aprovar/Negar**. As duas pedem biometria e conectam
  para buscar o pedido de verdade antes de decidir.

## 6. Segurança

- Biometria ou PIN para abrir o app e para `approve`, `control` e `keyboard`.
- A chave privada do celular fica no secure storage (Keystore).
- Revogar no Duck derruba as sessões abertas daquele celular.
- O relay não lê nada: o Noise vai de ponta a ponta. O segredo do relay só
  autoriza discar e mandar push.
- Um celular lento não segura o Duck: a escrita para ele tem limite de 20 s
  (passou, a sessão cai) e nunca acontece com o estado do server travado.

## 7. Formatos

Os exemplos mostram o `data` da resposta (`{"id":..,"ok":true,"data": <isto>}`)
ou do evento (`{"ev":..,"data": <isto>}`). Campos podem ganhar irmãos novos:
o app ignora o que não conhece.

### Eventos

`agents` (o mesmo item do comando `agents`, sem `why`):

```json
{"agents": [{
  "pane": 12, "workspace": "default", "tab": "duck", "tab_index": 2,
  "computer": "hd-fedora", "profile": "pessoal", "config_dir": null,
  "name": "claude", "state": "blocked", "since": 1794659000, "source": "hook",
  "step": "Rodar os testes", "msg": "Bash(cargo test)", "prompt": "arrume o build",
  "todo": [2, 5], "tokens": 81234, "session_tokens": 1203000,
  "session": "5f0c…", "cwd": "/home/hd/duck", "queue": ["depois rode o clippy"], "attention": true
}], "now": 1794659100}
```

- `state`: `working`, `blocked` (pedindo você), `done` (terminou, ninguém viu),
  `idle`. `since`/`now`: unix (s). `profile` = a conta do Claude ("pessoal" é o
  `~/.claude`; `totvs` é `~/.claude-totvs`). `todo`: `[feitos, total]` ou `null`.
  `tokens`: contexto da última resposta; `session_tokens`: a sessão toda.
  `step`, `msg`, `prompt`, `tokens`... podem ser `null`.

`approvals` (pedidos do Duck esperando o usuário; `approve {id, ok}` decide):

```json
{"approvals": [{"id": 34, "kind": "action", "text": "O pane 7 quer fechar o pane 9 (build).", "pane": 7}]}
```

`kind`: `action` (agente pedindo ação destrutiva), `clipboard`, `peer`
(mensagem de outro computador), `pair` (outro computador quer conectar),
`file` (arquivo chegando).

`inbox` (caixa de avisos, mais novo primeiro; `read` = maior `seq` já lido):

```json
{"items": [{"seq": 51, "at": 1794659050, "kind": "agent", "pane": 12, "state": "done",
  "title": "Agente terminou", "body": "default · duck · ✳ Build", "color": "done"}], "read": 48}
```

`screen:<pane>` (tela visível, texto puro; `cursor` = `[linha, coluna]`):

```json
{"pane": 12, "title": "✳ Build", "cols": 120, "rows": 3, "cursor": [2, 2], "state": "working",
 "lines": ["$ cargo test", "test result: ok. 115 passed", "$"]}
```

### Comandos existentes (resumo do `data`)

- `list`: `{"workspaces":[{"name","active_tab","tabs":[{"id","title","active_pane","color","panes":[{"id","title","cwd","domain","agent","attention","parent","floating","private","idle_secs","running","loop"}]}]}]}` (sem panes privados; aba só com privados some).
- `read` `{pane, lines?, all?}` → `{"pane":12,"text":"..."}`. `last-output` `{pane}` → `{"pane","text","exit","ms"}`.
- `blocks` `{pane}` → `{"pane","history","offset","blocks":[{"cmd","exit","ms","prompt","input","out_start","out_end"}]}`; `block-output` `{pane, abs}` (o `prompt` do bloco) → `{"pane","cmd","text","exit","ms"}`.
- `why` `{pane}` → `{"pane","state","lines":["claude blocked ha 2min (fonte: hook)", ...], ...}`.
- `queue` `{pane, text}` → `{"delivered":true}` ou `{"queued":2}`; `queue-ls` → `{"pane","queue":[...]}`; `queue-rm|queue-up|queue-now` `{pane, index}` (0 = o primeiro) → `{"pane","queue","delivered"}`.
- `inbox-read` → `{}`. `loops` → `{"loops":[{"pane","check","max","iter","phase","started","last_exit","last_out"}]}`.
- `changes` `{pane}` → `{"cwd","files":3,"add":40,"del":2,"list":[["M","src/a.rs",30,1], ...]}` (status do git, arquivo, +, -).
  Com `{pane, file}` (um arquivo da lista): `{"pane","cwd","file","diff":"diff --git a/src/a.rs ...","truncated":false}`
  (diff contra o HEAD, até 200 KB; arquivo novo vem inteiro com `+`).
- `approve` `{id, ok}` → `{}`. `close` `{pane}` → `{}`. `send` `{pane, text, enter?}` / `paste` `{pane, text}` → `{}`.
- `loop-start` `{pane, check, max?, task?}` → `{"pane","max"}`; `loop-stop` `{pane}` → `{}`.
- `spawn`: o do server (`{where:"tab", cwd, title, workspace?, argv?}`) mais o
  atalho do celular `{cwd, agent?, task?, worktree?}`:
  `{"cwd":"/home/hd/duck","agent":"claude","task":"arrume o build","worktree":"fix-build"}`
  → abre uma aba (sem roubar o foco da janela) e digita `claude 'arrume o build'`
  quando o shell sobe. `agent` é só o nome do programa (`claude`, `codex`,
  `gemini`). `worktree`: cria `<repo>-<branch>` com a branch nova
  (`git worktree add -b`) e abre lá. Resposta: `{"pane": 31}`.

### Comandos novos (★)

`timeline` `{pane, limit?=200}`: transcript da sessão do agente do pane.

```json
{"pane": 12, "session": "5f0c…", "total": 418, "events": [
  {"ts": "2026-11-14T13:02:11.120Z", "kind": "prompt", "text": "arrume o build"},
  {"ts": "2026-11-14T13:02:15.004Z", "kind": "edit", "text": "Edit src/a.rs", "file": "src/a.rs"},
  {"ts": "2026-11-14T13:02:20.500Z", "kind": "bash", "text": "Bash: cargo test"},
  {"ts": "2026-11-14T13:02:40.000Z", "kind": "error", "text": "error[E0308]: mismatched types"}]}
```

`kind`: `prompt`, `edit`, `read`, `bash`, `task`, `tool`, `error`. `ts` em UTC.
Pane sem agente ou sem sessão conhecida (hooks não instalados): `invalid`;
transcript não achado: `not_found`.

`usage` `{}`: uso do Claude por conta (o que a statusline do Duck grava).

```json
{"accounts": [{"profile": "pessoal", "config_dir": "/home/hd/.claude", "model": "Opus", "updated": 1794659000,
  "session": "5f0c…", "cost_usd": 3.12,
  "windows": [{"key": "5h", "pct": 42.0, "resets": 1794670000}, {"key": "7d", "pct": 18.5, "resets": 1795000000}]}]}
```

`resets` vem como o Claude Code manda (unix em s). Lista vazia: statusline
do Duck não instalada ou sem uso nos últimos 8 dias.

`system` `{}` (bytes e %; `uptime` em s):

```json
{"name": "hd-fedora", "version": "0.5.0", "os": "linux", "panes": 9, "agents": 3, "working": 1,
 "cpu": 12.5, "cores": 16, "ram_used": 8123456789, "ram_total": 33554432000, "swap_used": 0, "swap_total": 8589934592, "uptime": 86400}
```

`routines` `{}` (extensão Rotinas; `running`: a extensão está ligada):

```json
{"running": true, "routines": [{"id": "noticias", "title": "Notícias de IA", "schedule": "0 9 * * 1-5",
  "enabled": true, "paused": false,
  "last": {"at": 1794650000, "ok": true, "secs": 41, "text": "1. ...", "error": null}}]}
```

`last` é `null` se nunca rodou; `text` vem cortado em 2000 caracteres.

`routine-run` `{id}` (`control`) → `{"id": "noticias", "queued": true}`.
Extensão desligada: `unavailable`.

`recent-dirs` `{limit?=40}`: zoxide primeiro, depois as pastas do histórico.

```json
{"dirs": [{"path": "/home/hd/duck", "name": "duck", "source": "zoxide"}, {"path": "/tmp/x", "name": "x", "source": "history"}]}
```

`file-get` `{pane, path}`: arquivo dentro da pasta do pane (`path` relativo a
ela ou absoluto lá dentro; fora dela é `invalid`), até 20 MB.

```json
{"pane": 12, "name": "shot.png", "path": "/home/hd/duck/shot.png", "size": 48213, "data": "<base64 padrão>"}
```

`file-put` `{pane, name, data}` (`control`; `data` em base64 padrão, até
20 MB): grava na pasta do pane com o nome saneado (sem pasta, sem `:*?"<>|`,
sem ponto no começo) e sem sobrescrever (`foto (2).jpg`).

```json
{"pane": 12, "name": "foto (2).jpg", "path": "/home/hd/duck/foto (2).jpg", "size": 120345}
```

`agent-answer` `{pane, yes}` (`approve`) → `{"pane": 12, "sent": "1"}` (ou
`"esc"`). Agente fora de `blocked`: `invalid` "o agente nao esta esperando
resposta (estado working)".

`interrupt` `{pane, key: "esc"|"ctrl-c"}` (`control`; padrão `esc`) → `{"pane": 12}`.

`translate` `{pane, text}` (`message`): pt-BR → en pelo motor do Tradutor
(a CLI do agente do pane, com a conta dele; até 60 s). Não envia: para mandar,
`queue` com o `translation`.

```json
{"pane": 12, "translation": "fix the build and run the tests", "engine": "claude", "model": "haiku", "from": "pt-BR", "to": "en",
 "notes": [{"en": "fix", "pt": "arrume", "why": "verbo curto usado em pedidos técnicos"}]}
```

`resume` (`control`): sem `session`, lista as sessões do Claude (mais nova
primeiro, `limit?=30`); com `{session, workspace?}`, abre uma aba com
`claude --resume <session>` na pasta e conta da sessão.

```json
{"sessions": [{"session": "5f0c…", "profile": "pessoal", "cwd": "/home/hd/duck", "title": "arrume o build", "updated": 1794659000}]}
```

```json
{"pane": 33}
```

`push-token` `{token}` (`view`) → `{"push": true}` (`false` com token vazio).

### Comandos do dono (janela e CLI, nunca do celular nem de um pane)

`mobile-pair` → `{"url": "duck://pair/…", "payload": {…o JSON do QR…}, "minutes": 10, "exp": 1794659600}`;
`mobile-list` → `{"devices": [{"name", "scopes", "last_seen", "paired_at", "push", "key" (16 hex), "online"}]}`;
`mobile-revoke {name}` → `{"name", "sessions_closed"}`;
`mobile-scopes {name, changes: ["+control", "-keyboard"]}` (ou `scopes: [...]`, a lista inteira) → `{"name", "scopes"}`.
