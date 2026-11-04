# Playbook: instalar e configurar o Duck

Passo a passo para deixar o Duck pronto: instalação, conferência, extensões
(uma receita para cada) e rotinas. [English version](PLAYBOOK.md).

- [1. Instalação](#1-instalação)
- [2. Primeiro uso](#2-primeiro-uso)
- [3. Extensões: o básico](#3-extensões-o-básico)
- [4. Receitas por extensão](#4-receitas-por-extensão)
- [5. Rotinas](#5-rotinas)
- [6. Problemas comuns](#6-problemas-comuns)

---

## 1. Instalação

### Windows: caminho rápido

Num PowerShell 7 (`pwsh`):

```powershell
# Pré-requisitos (pule o que já tiver)
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install OpenJS.NodeJS.LTS       # extensões
winget install Git.Git
winget install Microsoft.PowerShell    # se ainda não tiver o pwsh 7

# Duck
git clone https://github.com/herlondf/duck.git
cd duck
pwsh -File .\install.ps1 -All
```

Para os comandos novos valerem, abra um terminal novo depois do `winget`.

O `-All` faz tudo de uma vez:

| Opção | O que faz |
|---|---|
| (sempre) | Compila. Põe `duck`/`duckw` em `.\bin` e no PATH. Cria o `~/.duck.lua`. Cria o atalho no menu Iniciar. |
| `-Boot` | Abre o Duck no logon, restaurando o último checkpoint. |
| `-ClaudeHooks` | Registra os hooks do Duck em cada `~/.claude*/settings.json`, para ele saber o estado exato dos agentes. Também liga o statusline do Duck onde ainda não há nenhum: é ele que alimenta o "Uso do Claude" e o limite das Rotinas. |
| `-Extensions` | Compila as extensões que vêm junto. Elas ficam desligadas até você ligar. |
| `-SkipBuild` | Reaproveita o binário já compilado (útil para só refazer extensões ou hooks). |

A primeira compilação leva uns 5 minutos. O instalador avisa, com o comando
para instalar, se faltar o Rust, o MSVC Build Tools ou o Node.

### Linux e macOS

```bash
curl https://sh.rustup.rs -sSf | sh
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev   # Debian/Ubuntu
git clone https://github.com/herlondf/duck.git && cd duck
./install.sh --all
```

O Windows é o sistema de uso diário. O Linux compila e passa nos testes,
mas foi pouco usado; o macOS ainda não foi testado.

### Conferir

```powershell
duck doctor
```

Cada linha começa com `ok` ou `!!`:

```
ok config C:\Users\voce\.duck.lua
ok server 0.1.0 (pid 1234), 3 panes, 3 hosts vivos
ok hooks do Duck em ~/.claude/settings.json
ok WSL: Ubuntu
extensoes: 4 de 16 ligadas (Ctrl+Shift+P > Extensoes liga, configura e pede os tokens)
ok node v22.11.0
ok  github
!!  jira: falta o token jira/trabalho (echo <token> | duck secret set jira/trabalho)
```

Toda linha `!!` já diz o que fazer.

### Shell integration no WSL

Para o WSL ter blocos de comando (exit code, duração, minimapa), ponha no
`~/.bashrc` da distro a linha que o `duck doctor` mostra, parecida com esta:

```bash
. /mnt/c/Users/<voce>/AppData/Local/duck/shell/duck.sh
```

No pwsh do Windows ela já vem ligada.

### Atualizar

```powershell
cd duck
git pull
pwsh -File .\install.ps1 -Extensions
```

O instalador reinicia o server sem fechar os shells. Depois é só fechar e
abrir a janela do Duck.

---

## 2. Primeiro uso

A janela tem três faixas:

- **Em cima:** início, workspace, abas e o sino da caixa de entrada (ao lado
  do minimizar, com o número de avisos não lidos; fica vermelho quando um
  agente espera por você).
- **Rodapé:** um badge por extensão (Sistema, Agentes, Azure, Jira…). Clique
  abre o painel; arrastar reordena.
- **Barra de atalhos:** os atalhos principais com as teclas; clicar também
  funciona.

Não precisa decorar atalho. Os menus por assunto juntam tudo:

| Atalho | Abre |
|---|---|
| `Ctrl+Shift+P` | Paleta: todos os comandos, abas, sessões, histórico e arquivos |
| `Ctrl+Shift+1` | Orquestração: agentes, loop, mapa, caixa de entrada, worktree |
| `Ctrl+Shift+2` | Extensões: painéis e comandos das extensões |
| `Ctrl+Shift+3` | Janela: abas, panes e workspaces |
| `Ctrl+Shift+4` | Configuração |
| `Ctrl+Shift+/` | Todos os atalhos ligados |
| `Ctrl+,` | Configurações (tema, fonte, opacidade…) |

Os avisos (agente pedindo aprovação, extensão, loop) aparecem no canto da
tela, com botões, e ficam guardados na caixa de entrada (`Ctrl+Shift+J`).

### Personalizar a barra e os menus

No `~/.duck.lua` (o [duck.example.lua](../duck.example.lua) traz tudo comentado):

```lua
shortcuts = {
  { label = 'Paleta', action = A('palette') },
  { label = 'Orquestração', action = A('menu', 'orquestracao') },
  { label = 'Deploy', action = A('menu', 'deploy') },
},
menus = {
  -- prefixos do rótulo (ou da dica) dos comandos da paleta; sem acento nem caixa
  deploy = { title = 'Deploy', match = { 'Azure', 'Git:' } },
},
keys = {
  { key = '5', mods = 'CTRL|SHIFT', action = A('menu', 'deploy') },
},
ui = { footer = true, shortcuts = true },   -- false esconde o rodapé / a barra de atalhos
```

Salvar o arquivo recarrega na hora.

---

## 3. Extensões: o básico

### Ligar

As extensões que vêm junto começam **desligadas**: desligada, uma extensão não
gasta memória. Há dois jeitos de ligar:

1. **Pela janela:** `Ctrl+Shift+2` (ou `Ctrl+Shift+P` → "Extensões"). Escolha
   a extensão, ligue, preencha as configurações e adicione as contas. O token
   é pedido ali mesmo, mascarado. Isso grava no `~/.duck-local.lua`.
2. **Pelo arquivo:** em `~/.duck.lua`, dentro de `extensions = { ... }` (veja
   as receitas abaixo).

Algumas extensões (Azure, Jira, GitHub) só se configuram pelo arquivo. A tela
de Extensões liga e desliga todas.

### Tokens

Token **nunca** vai no arquivo de config. Grave pelo menu de Extensões ou pela
linha de comando:

```powershell
echo <token> | duck secret set jira/trabalho     # grava
duck secret has jira/trabalho                    # confere (sim/nao)
duck secret delete jira/trabalho                 # apaga
```

O nome é sempre `<extensao>/<conta>`, e `<conta>` é o `name` da conta na
config. No Windows os tokens ficam no Gerenciador de Credenciais (`duck:<nome>`);
no Linux e no macOS, em `~/.duck/secrets`.

Arquivo JSON inteiro (conta de serviço do Google):

```powershell
Get-Content conta.json -Raw | duck secret set googleplay/empresa
```

### Onde ficam as coisas

| Arquivo | Para quê |
|---|---|
| `~/.duck.lua` | Sua config (versionável) |
| `~/.duck-local.lua` | O que a tela de configurações e de extensões grava (por máquina) |
| `~/.duck/rotinas/*.md` | Tarefas das Rotinas |
| `~/.duck/queries/<conta>.sql` | Consultas salvas do Banco de dados |
| `~/.duck/snippets.md` | Snippets |
| `~/.duck/notes/` | Notas por projeto |
| `~/.duck/extensions/<nome>` | Suas próprias extensões ([docs/EXTENSIONS.md](EXTENSIONS.md)) |

### Conferir

O `duck doctor` mostra as extensões ligadas que não estão compiladas ou que
estão sem token. Dentro do Duck, um erro de configuração aparece no topo do
painel da extensão.

---

## 4. Receitas por extensão

Cada receita diz para que serve a extensão, o que ela precisa, o token e a
config. Nos exemplos, troque os nomes de conta e as URLs pelos seus.

### GitHub

- **Serve para:** notificações, revisões pedidas, issues e PRs, repositórios,
  em várias contas.
- **Precisa:** o [GitHub CLI](https://cli.github.com) logado. Não usa token
  próprio; lê as contas do `gh`.
  ```powershell
  winget install GitHub.cli
  gh auth login            # repita para cada conta
  ```
- **Config:**
  ```lua
  github = { enabled = true },                                  -- todas as contas do gh
  -- github = { enabled = true, accounts = { 'meu-login' }, poll_seconds = 60 },
  ```

### Azure DevOps

- **Serve para:** PRs para revisar, seus PRs, builds e **o que está publicado
  em cada ambiente** (releases), em uma ou várias organizações.
- **Token:** um PAT, criado em `https://dev.azure.com/<org>/_usersSettings/tokens`
  (ou User settings → Personal access tokens) com **Code (Read)**,
  **Build (Read)** e **Release (Read)**:
  ```powershell
  echo <pat> | duck secret set azure/empresa
  ```
- **Config** (só pelo arquivo):
  ```lua
  azure = { enabled = true, orgs = {
    { name = 'empresa', url = 'https://dev.azure.com/empresa', project = 'proj',
      releases = {
        -- Todos os projetos de uma vez: cada release clássica com deploy nos
        -- últimos 30 dias vira uma linha "develop ✓ #12 · tst ✓ #13 · master ✗ #14".
        { auto = true, branches = { 'develop', 'tst', 'master' } },
        -- Um projeto só: o último deploy de cada ambiente.
        -- 'rotulo=Ambiente' troca o nome mostrado; 'Prd|Prod' aceita qualquer um.
        { name = 'API', definition = 'api-cd', stages = { 'develop=Dev', 'tst=Tst', 'master=Prd|Prod' } },
        -- Pipeline YAML ou CI: o último build de cada branch.
        { name = 'Portal', pipeline = 'portal-ci', branches = { 'develop', 'tst', 'master' } },
      } },
  } },
  ```
  - **Modo automático (`auto = true`):** não precisa listar projeto nenhum.
    O ambiente de cada branch sai do filtro de branch do gatilho do próprio
    ambiente (o Dev que publica `develop`); sem filtro, pelo nome (develop →
    Dev, tst → Tst/QA/Hml, master → Prd/Prod). Filtros: `path = 'backend'`
    (pasta das releases), `match = 'api'` e `exclude = 'legado'`
    (expressões no nome), `days = 30` (janela) e `name = 'Releases'` (título
    da seção). Pode ter mais de um, cada um com seus filtros e nome.
  - Projeto com deploy que falhou ou esperando aprovação fica no topo. A
    linha abre o detalhe com os 5 últimos deploys de cada ambiente.
  - `notify = false` num item de `releases` desliga os avisos só dele.
  - Org antiga (`https://empresa.visualstudio.com`) também funciona.
  - Cada release aceita `project = '...'` quando estiver em outro projeto.
  - `release_notify = false` na org desliga o aviso de deploy terminado.
  - `poll_seconds` (padrão 120) muda o intervalo.
- **Descobrir os nomes:** paleta → "Azure: definicoes de release (para
  configurar)". Lista as definições e os ambientes de cada uma, com um trecho
  pronto para copiar. Um ambiente digitado errado aparece no painel com os
  nomes que existem.
- **No painel:** no modo automático, cada linha abre o detalhe do projeto;
  nos itens configurados um a um, cada linha abre a release no navegador. "Historico" mostra os
  15 últimos deploys do ambiente. O badge do rodapé mostra quantos deploys
  falharam.

### Jira

- **Serve para:** suas tarefas (com prazo), issues que você acompanha, avisos
  de atribuição, comentário e mudança de status. Funciona com Server/DC e
  Cloud.
- **Token:**
  - Server/DC: Perfil → Personal Access Tokens.
  - Cloud: <https://id.atlassian.com/manage-profile/security/api-tokens>.
  ```powershell
  echo <token> | duck secret set jira/trabalho
  ```
- **Config** (só pelo arquivo):
  ```lua
  jira = { enabled = true, due_days = 3, accounts = {
    { name = 'trabalho', url = 'https://jira.empresa.com' },                                   -- Server/DC
    { name = 'cloud', url = 'https://empresa.atlassian.net', type = 'cloud', email = 'eu@empresa.com' },
  } },
  ```

### Grafana (Loki)

- **Serve para:** consultas LogQL com a contagem na janela, as últimas linhas
  e um aviso quando aparece ocorrência nova.
- **Token:** `usuario:senha` do Grafana (Basic auth):
  ```powershell
  echo 'usuario:senha' | duck secret set grafana/empresa
  ```
- **UID do Loki:** Grafana → Connections → Data sources → Loki. O UID está na
  URL (`/datasources/edit/<uid>`).
- **Config:**
  ```lua
  grafana = { enabled = true, window_hours = 24, poll_seconds = 300,
    accounts = { { name = 'empresa', url = 'https://grafana.empresa.com', datasource_uid = '<uid>' } },
    queries = { 'Erros API={service_name="api"} |= "ERROR"' } },     -- Nome=LogQL
  ```

### Rollbar

- **Serve para:** erros ativos e avisos, com resolver e silenciar.
- **Token:** em Project → Settings → Project Access Tokens, um token com
  `read`. Para resolver e silenciar, precisa também de `write`.
  ```powershell
  echo <token> | duck secret set rollbar/app
  ```
- **Config:**
  ```lua
  rollbar = { enabled = true, environments = { 'production' },
    accounts = { { name = 'app', account_slug = 'empresa', project_slug = 'app' } } },   -- slugs só para os links
  ```

### Agenda (Google / Outlook)

- **Serve para:** próximos compromissos, lembrete antes de cada um e botão
  "Entrar".
- **Token:** a **URL secreta iCal** da agenda.
  - Google Agenda: Configurações da agenda → "Endereço secreto no formato iCal".
  - Outlook: Configurações → Calendário → Agendas compartilhadas → Publicar
    agenda → link ICS.
  ```powershell
  echo 'https://calendar.google.com/calendar/ical/.../basic.ics' | duck secret set calendar/trabalho
  ```
- **Config:**
  ```lua
  calendar = { enabled = true, remind_minutes = 5, days = 7, accounts = { { name = 'trabalho' } } },
  ```

### Home Assistant

- **Serve para:** luzes, interruptores, cortinas, clima e fechaduras, mais
  avisos de sensores.
- **Token:** HA → Perfil → Segurança → "Tokens de acesso de longa duração":
  ```powershell
  echo <token> | duck secret set homeassistant/casa
  ```
- **Config:**
  ```lua
  homeassistant = { enabled = true,
    accounts = { { name = 'casa', url = 'http://homeassistant.local:8123' } },
    favorites = { 'light.sala', 'switch.cafeteira' },          -- vazio = tudo, por tipo
    alerts = { 'binary_sensor.porta_entrada' } },              -- avisa quando liga
  ```

### Google Play

- **Serve para:** faixas e releases dos apps e as avaliações, com resposta às
  avaliações.
- **Token:** o JSON de uma conta de serviço do Google Cloud com acesso ao Play
  Console (Play Console → Usuários e permissões → convidar o e-mail da conta
  de serviço).
  ```powershell
  Get-Content conta-servico.json -Raw | duck secret set googleplay/empresa
  ```
- **Config:**
  ```lua
  googleplay = { enabled = true, packages = { 'com.empresa.app' }, accounts = { { name = 'empresa' } } },
  ```

### Banco de dados (Postgres e Redis)

- **Serve para:** rodar consultas salvas, com o resultado em tabela. Só
  leitura, a não ser que `allow_write = true`.
- **Token:** a senha do banco (pode ficar vazia):
  ```powershell
  echo <senha> | duck secret set database/local
  ```
- **Config:**
  ```lua
  database = { enabled = true, accounts = {
    { name = 'local', kind = 'postgres', host = 'localhost', port = 5432, database = 'app', user = 'postgres' },
    { name = 'cache', kind = 'redis', host = 'localhost' },
  } },
  ```
- **Consultas:** em `~/.duck/queries/<conta>.sql`, cada bloco começa com
  `-- name: Título`. Na primeira vez o arquivo é criado com um exemplo. No
  Redis é um comando por linha.

### Containers

- **Serve para:** containers do Docker e do Podman (também dentro do WSL), com
  logs, shell, iniciar, parar e reiniciar. Avisa quando um container cai.
- **Precisa:** `docker` e/ou `podman` (no Windows ou numa distro do WSL).
- **Config:**
  ```lua
  containers = { enabled = true, engines = { 'docker', 'Ubuntu' } },   -- nome da distro = podman dentro dela
  ```
  O padrão `auto` sobe todas as distros do WSL para procurar o podman. Diga
  quais usar.

### Uso do Claude

- **Serve para:** limites de 5 horas e de 7 dias, custo do dia e sessões
  recentes, por perfil do Claude Code (`~/.claude`, `~/.claude-trabalho`…).
- **Precisa:** do statusline do Duck. O `install.ps1 -ClaudeHooks` liga sozinho
  onde não há statusline. Se você já tem um, chame
  `<duck>\bin\duck-statusline.ps1` de dentro dele, passando o mesmo stdin, ou
  grave você mesmo o JSON (os campos estão no cabeçalho do script).
- **Config:** `['claude-usage'] = { enabled = true, warn_percent = 80 }`.

### Git, Portas, Notas, Snippets

Não precisam de token. Basta ligar:

```lua
git = { enabled = true },        -- branch, mudanças, worktrees e stashes do pane ativo (Windows e WSL)
ports = { enabled = true, ignore = { 'svchost' } },   -- portas TCP abertas e o processo
notes = { enabled = true },      -- notas e tarefas por projeto em ~/.duck/notes
snippets = { enabled = true },   -- trechos em ~/.duck/snippets.md e histórico do que foi copiado
```

Para gravar o histórico de cópia em disco, use `persist = true` no Snippets.
Cópias que parecem senha ou token nunca entram no histórico.

---

## 5. Rotinas

As Rotinas mandam um pedido ao Claude Code num horário marcado e trazem a
resposta como aviso: notícias da manhã, resumo das issues novas, conferir um
deploy. Usam o login do seu plano (sem API key) e rodam no server do Duck,
então funcionam com a janela fechada.

### Ligar

1. Tenha o [Claude Code](https://claude.com/claude-code) instalado e logado
   (`claude` no terminal).
2. Ligue a extensão: `Ctrl+Shift+2` → Rotinas, ou
   `rotinas = { enabled = true }`.
3. Na primeira vez ela cria `~/.duck/rotinas/exemplo.md`, desligado.

### Criar uma rotina

Use a paleta → "Rotinas: nova rotina" (cria `rotina-N.md` com um modelo
comentado), ou crie você mesmo um arquivo `~/.duck/rotinas/<id>.md`:

```markdown
---
title: Issues novas do projeto
cron: 0 9 * * 1-5
profile: trabalho
cwd: ~/projetos/api
tools: Bash(gh issue list:*), Read
quiet: NADA
---
Liste as issues abertas nas últimas 24 horas no repositório deste diretório
(gh issue list). Para cada uma, uma linha com número, título e se parece bug.
Se não houver nenhuma, responda só NADA.
```

Salvar já agenda. O painel Rotinas mostra o próximo horário, a última
execução e os botões **Rodar agora**, Editar, Pausar e Ver.

### Campos

| Campo | Padrão | O que faz |
|---|---|---|
| `title` | nome do arquivo | Nome mostrado. |
| `cron` | — | Horário em 5 campos: `min hora dia mês dia-da-semana`. Aceita `*`, `1-5`, `1,15`, `*/10`. Domingo é 0 ou 7. |
| `every` | — | No lugar do `cron`: `30m`, `2h`, `1d`. O mínimo é `5m`. |
| `profile` | `~/.claude` | Perfil do Claude: `trabalho` = `~/.claude-trabalho`, ou um caminho. |
| `cwd` | `~/.duck/rotinas` | Pasta em que o Claude roda (precisa existir). |
| `tools` | nenhuma | Ferramentas permitidas: `WebSearch, WebFetch, Read, Bash(gh issue list:*)`. |
| `mode` | — | `acceptEdits` (pode editar arquivos) ou `plan`. |
| `model` | — | Modelo (ex.: `sonnet`). |
| `max_turns` | — | Limite de voltas do agente. |
| `quiet` | — | Se a resposta começar com este texto (ex.: `NADA`), não avisa. |
| `notify` | `true` | Mostra o aviso com a resposta. |
| `catchup` | `false` | Roda o horário perdido (Duck fechado, PC dormindo). Sem isso, roda só se o atraso for de até 10 minutos. |
| `enabled` | `true` | `false` desliga sem apagar. |
| `uso_max` | `85` | Adia se o limite de 5h do perfil estiver em X% ou mais. `0` desliga. |
| `ceder` | `true` | Adia enquanto um agente seu trabalha no mesmo repositório do `cwd`. |
| `falar` | — | Nome da Echo que lê a resposta (via Home Assistant). |
| `celular` | `false` | Push no celular (via Home Assistant). |
| `falar_noite` | `false` | Fala também entre 22h e 7h. Sem isso, à noite vai só o push. |
| `falar_max` | `250` | Máximo de caracteres falados (até 600). |

Para valores sim/não: `false`, `nao`, `não`, `no`, `0` e `off` desligam;
qualquer outro valor liga.

**Adiamento.** Com `uso_max` ou `ceder`, uma rotina que encontra o limite alto
ou um agente seu no mesmo repositório espera, e o painel mostra "adiada: …".
Se ela ficar adiada até o horário seguinte, aquela execução é pulada. O
`uso_max` depende do statusline do Duck (veja "Uso do Claude").

**Histórico.** Cada execução fica em `~/.duck/rotinas/historico/<id>/`. O botão
"Continuar no Claude" abre a sessão daquela execução para você seguir a
conversa.

### Resposta na Alexa ou no celular (opcional)

As Rotinas mandam a resposta a uma automação do Home Assistant por webhook.

1. **Grave o id do webhook** no Duck. Invente um id longo e aleatório; ele
   funciona como senha.
   ```powershell
   echo <id-do-webhook> | duck secret set rotinas/ha-webhook
   ```
2. **Diga onde está o Home Assistant**, em ordem de tentativa (a rede local
   primeiro, depois um túnel):
   ```lua
   rotinas = { enabled = true, ha_urls = { 'http://192.168.0.10:8123', 'https://casa.exemplo.com' } },
   ```
3. **Crie a automação no HA** (Configurações → Automações → YAML). O Duck manda
   `{ mensagem, canal, echo, titulo }`; o `canal` vale `echo`, `celular` ou
   `ambos`. Exemplo com a [Alexa Media Player](https://github.com/alandtse/alexa_media_player)
   e o app do Home Assistant no celular (ajuste os nomes):
   ```yaml
   alias: Duck - respostas das rotinas
   triggers:
     - trigger: webhook
       webhook_id: "<id-do-webhook>"
       allowed_methods: [POST]
       local_only: false
   actions:
     - if: "{{ trigger.json.canal in ['echo', 'ambos'] }}"
       then:
         - action: notify.alexa_media
           data:
             target: "media_player.{{ trigger.json.echo }}"
             message: "{{ trigger.json.mensagem }}"
             data: { type: announce }
     - if: "{{ trigger.json.canal in ['celular', 'ambos'] }}"
       then:
         - action: notify.mobile_app_meu_celular
           data:
             title: "{{ trigger.json.titulo }}"
             message: "{{ trigger.json.mensagem }}"
   ```
4. **Na rotina**, use `falar: escritorio` (o nome do `media_player` da Echo,
   sem o prefixo) e/ou `celular: true`. O botão **Testar fala/push** do painel
   manda um teste na hora, mesmo à noite.

---

## 6. Problemas comuns

| Sintoma | O que fazer |
|---|---|
| `install.ps1` reclama do MSVC ou do `link.exe` | Rode o `winget install Microsoft.VisualStudio.2022.BuildTools ...` que ele mostra e abra um terminal novo. |
| `duck` não é reconhecido | Abra um terminal novo (o PATH muda só nos novos). |
| Extensão ligada sem dados | `duck doctor`. Se faltar token: `echo <token> \| duck secret set <ext>/<conta>`. Se não estiver compilada: `install.ps1 -SkipBuild -Extensions`. |
| Azure: "PAT vencido ou sem permissao" | Gere outro PAT com Code, Build e Release (Read) e grave por cima com `duck secret set`. |
| Azure: ambiente "não existe" | O painel lista os ambientes que existem. Copie o nome exato, ou use "Azure: definicoes de release". |
| GitHub: "Nenhuma conta" | `gh auth login`. |
| Uso do Claude: "Pasta de uso não encontrada" | Falta o statusline do Duck: `install.ps1 -SkipBuild -ClaudeHooks`. Depois abra uma sessão do Claude. |
| Rotina com "erro no arquivo" | Falta o cabeçalho `---`, falta `cron:`/`every:`, o corpo está vazio, ou o `cwd` não existe. |
| Rotina não roda | Veja se está pausada ou com `enabled: false`, ou "adiada" no painel. "Rodar agora" testa na hora. |
| Avisos de agente não aparecem | Confira `notify = true` no `~/.duck.lua` (é o padrão). O aviso só sai se você não estiver olhando para aquele pane. Em tela cheia, ele vai para a central do Windows. |
| Agente sem estado nas abas | `install.ps1 -SkipBuild -ClaudeHooks`, depois reabra o Claude. |
| Atualizei e nada mudou | Feche e abra a janela do Duck (o server já foi reiniciado). |

Ainda com problema? O log do server fica em
`%LOCALAPPDATA%\duck\server.log` e o das extensões em
`%LOCALAPPDATA%\duck\ext-host.log`.
