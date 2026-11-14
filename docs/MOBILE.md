# Duck no celular: contrato entre o Duck e o app

O app (`herlondf/duck-app`, Flutter) é um **controle remoto dos agentes**: vê,
aprova, manda recado, inicia e interrompe agentes e, com permissão, digita
no shell. Não há backend: o "servidor" é o próprio Duck no computador. Este
documento é o contrato entre os dois lados; mudou aqui, muda nos dois.

## 1. Conexão

- O celular é um **pareado do tipo `mobile`** (chave própria, como um
  computador), com o mesmo canal de `secure.rs`: `MAGIC` + Noise
  `Noise_XX_25519_ChaChaPoly_BLAKE2s`, quadros `u16` BE, mensagem da
  aplicação = JSON com o tamanho total (`u32`) no primeiro quadro.
- Caminhos, na ordem: endereços diretos (rede local, Tailscale) e depois o
  relay (`{"dial": chave, "secret"}`, ver `relay.rs`). O app guarda a lista e
  coloca na frente o que funcionou.
- O Duck identifica o celular pela chave pública do handshake. Chave
  desconhecida e sem convite válido: o Duck fecha a conexão.

## 2. Pareamento por QR

- No Duck: **Ctrl+Shift+4 > Celulares > Parear celular** (e `duck mobile pair`
  na CLI, que imprime o QR no terminal). O QR é
  `duck://pair/<base64url(JSON)>`, com o JSON:
  `{"v":1,"name":"hd-fedora","key":"<hex 32B>","addrs":["192.168.0.10:7878"],
  "relay":"relay.exemplo.com:7879","relay_secret":"...","invite":"<16 hex>","exp":<unix>}`.
  O convite vale 10 minutos e serve uma vez só.
- O app conecta, faz o handshake e manda
  `{"op":"pair","kind":"mobile","name":"<aparelho>","invite":"..."}`.
- O Duck aceita e responde `{"ok":true,"scopes":[...],"confirm":"123-456-789"}`.
  O `confirm` sai do hash do handshake, como em `secure.rs`. O app mostra o
  número e o Duck mostra um aviso com o mesmo número.
- Permissões iniciais: `view`, `approve`, `message`. As outras se ligam no
  painel Celulares do Duck.
- No painel, cada celular aparece com nome, última conexão, permissões
  (marcar e desmarcar) e **Revogar** (apaga a chave na hora).

## 3. Sessão

Depois do handshake, mensagens JSON nos dois sentidos:

- Pedido: `{"id":7,"op":"call","cmd":"agents","args":{...}}`.
  - Sucesso: `{"id":7,"ok":true,"data":{...}}`.
  - Erro: `{"id":7,"ok":false,"error":{"code":"forbidden|not_found|invalid|unavailable","message":"..."}}`.
- Assinar eventos: `{"id":8,"op":"subscribe","events":["agents","approvals","inbox","screen:<pane>"]}`.
  Depois disso o Duck manda `{"ev":"<nome>","data":{...}}` sempre que algo
  mudar. `screen:<pane>` manda a tela daquele pane, no máximo 4 por segundo.
- Ping: `{"op":"ping"}` → `{"op":"pong"}`. O Duck fecha a conexão depois de
  5 minutos sem nada; o app reconecta quando volta para a frente.
- Todo `call` vai para o `audit.log` com `caller = "mobile:<nome>"`.

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

## 5. Push (o relay acorda o app)

- O app manda o token do FCM: `{"op":"call","cmd":"push-token","args":{"token":"..."}}`
  (★, permissão `view`). O Duck guarda o token junto do celular.
- Nos eventos `approval` (Duck ou agente pedindo), `done` (agente terminou),
  `loop` (loop falhou ou passou) e `command` (comando longo terminou), o Duck
  manda ao relay uma linha de controle:
  `{"push":["<token>"],"secret":"<relay_secret>","data":{"duck":"hd-fedora","kind":"approval","pane":12,"approval":34,"n":2}}`.
- O relay manda isso pelo FCM HTTP v1 como mensagem de dados (`priority:
  high`). A conta de serviço fica no relay: `--fcm-credentials arquivo.json`
  ou `DUCK_RELAY_FCM_CREDENTIALS`. Ele limita 30 pushes por minuto por
  segredo.
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
