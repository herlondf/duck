# Duck

Terminal nativo para Windows (Rust + GPU) com **mux persistente** (o "tmux"
embutido), **orquestração de agentes de IA** e config em **Lua**. Junta o
WezTerm (janela GPU, config Lua, mux) com o herdr (estado dos agentes, API para
um agente controlar os outros), mais o que a [pesquisa](PESQUISA.md) trouxe de
Warp, Ghostty, kitty, Zellij, Wave e Kiro.

- Fechar a janela não mata nada, e **nem um crash do server mata**: cada
  shell vive num processo próprio (`ptyhost`) e o server novo reconecta.
- O terminal sabe quais panes têm um agente (Claude, Codex, Kiro…) e em que
  estado estão: `working`, `blocked`, `done` ou `idle`. Você pode aprovar ou negar o pedido do agente sem
  trocar de aba.
- Cada comando vira um **bloco** com exit code e duração. Dá para pular entre
  prompts e copiar só a saída do último comando.
- Agentes controlam o terminal pela CLI ou por **MCP**.

Roadmap e status: [FUNCIONALIDADES.md](FUNCIONALIDADES.md).

## Instalar

```powershell
pwsh -File D:\Tools\Duck\install.ps1                 # compila, PATH, ~/.duck.lua, atalho
pwsh -File D:\Tools\Duck\install.ps1 -Boot           # + abre no logon restaurando o checkpoint
pwsh -File D:\Tools\Duck\install.ps1 -ClaudeHooks    # + estado dos agentes por hook do Claude Code
duck doctor                                          # confere config, server, hooks, WSL
```

São dois executáveis com o mesmo código, como `wezterm.exe` / `wezterm-gui.exe`:

- **`duck.exe` (console):** CLI e subcomandos. Sem argumentos, abre a
  janela e devolve o terminal na hora.
- **`duckw.exe` (sem console):** a janela, usada pelos atalhos e pelo boot,
  e os processos de fundo.

Para compilar, precisa do Rust (`winget install Rustlang.Rustup`) e do MSVC Build Tools.

## Atalhos (padrão, todos configuráveis)

| Atalho | Ação |
|---|---|
| `Ctrl+Shift+P` | Paleta de comandos (busca fuzzy, mostra o atalho de cada ação) |
| `Ctrl+,` | Configurações (tema, fonte, opacidade, segurança…) |
| `Alt+\` / `Alt+-` | Dividir à direita / abaixo |
| `Alt+W` / `Alt+Z` | Fechar pane / zoom do pane |
| `Ctrl+Shift+W` | Fechar a aba (todos os panes dela) |
| `Alt+F` | Pane flutuante (abre/esconde; continua rodando escondido) |
| `Alt+←↑→↓` | Focar pane vizinho |
| `Alt+P` | Modo redimensionar (`h/j/k/l` ou setas, `Esc` sai) |
| `Alt+R` | Renomear aba |
| `Alt+A` | Painel de agentes (`Enter` foca, `Ctrl+Y` aprova, `Ctrl+N` nega) |
| `Ctrl+Shift+H` | Sessões do Claude para retomar (todos os perfis) |
| `Ctrl+Shift+I` / `Ctrl+Shift+Y` | Painel do agente / linha do tempo da sessão |
| `Ctrl+Shift+Enter` | Editor de comando (várias linhas, Ctrl+Z) |
| `Ctrl+Shift+Z` | Modo foco |
| `Ctrl+Shift+Home` | Tela inicial (projetos, sessões, ações) |
| `Ctrl+Alt+B` | Abas na lateral / recolher a lateral |
| `Alt+↑↓` / `Alt+Enter` | Autocompletar: escolher / aceitar |
| `Ctrl+Shift+A` | Agent Manager: todos os agentes em grade, ao vivo |
| `Ctrl+Shift+N` | Novo agente (perfis; opção "em worktree nova") |
| `Ctrl+Shift+E` | Corrigir com agente o último comando que falhou (ou clique na faixa vermelha) |
| `Ctrl+Shift+M` | Recado para o agente, entregue quando ele parar |
| `Ctrl+Shift+K` | Bifurcar a sessão do agente num pane novo |
| `Ctrl+Shift+D` | Revisar o diff linha a linha e mandar os comentários ao agente |
| `Ctrl+Shift+R` | Histórico de comandos de todos os panes (desta pasta primeiro) |
| `→` no fim da linha | Aceita a sugestão inline (texto fantasma do histórico) |
| `Ctrl+Shift+↑` / `↓` | Prompt anterior / próximo (blocos de comando) |
| `Ctrl+Shift+O` | Copiar a saída do último comando |
| `Ctrl+Shift+Espaço` | Hints: rótulos em URL/caminho/hash/IP (minúscula copia, maiúscula abre) |
| `Ctrl+Shift+B` | Broadcast: digitar em todos os panes da aba |
| `Ctrl+Shift+L` | Pane privado (fora do autosave e da leitura por agentes) |
| `Ctrl+Shift+T`, `Ctrl+Tab` | Nova aba, próxima aba |
| `Ctrl+Shift+S` | Trocar de workspace (inclui as pastas do zoxide) |
| `Ctrl+Alt+N` | Criar/ir para workspace |
| `Ctrl+Alt+S` / `R` / `D` | Salvar / restaurar / apagar estado de workspace |
| `Ctrl+Shift+F` | Buscar no scrollback (`/regex/` também) |
| `Ctrl+Shift+G` | Filtrar o scrollback: lista só as linhas que casam |
| `Ctrl+Shift+X` | Modo cópia (vim: `hjkl`, `v` marca, `y` copia) |
| `Ctrl+Shift+C` / `V` | Copiar / colar (seleção com mouse já copia) |
| `Ctrl+clique` | Abrir link; em `arquivo:linha:coluna` abre no VS Code (`editor` na config) |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Fonte maior / menor / padrão |

**Quick terminal:** `duck --quick` (ou `install.ps1 -Boot` com
`duck install --quick`) deixa um terminal escondido no topo da tela.
`Ctrl+'` desce e sobe esse terminal, que some ao perder o foco.

## Visual (rodada de UI/UX)

- **Barra de titulo propria:** abas em pilula, chip do workspace, inicio,
  painel do agente e os botoes da janela na mesma faixa. Arraste a area
  vazia para mover; duplo clique maximiza; as bordas redimensionam.
- **Abas:** icone do processo (pwsh, WSL, SSH) ou avatar do perfil do agente;
  ponto pulsando enquanto o agente trabalha, ambar quando pede aprovacao,
  ✓ quando termina; barra de progresso (plano do agente ou OSC 9;4); ✕ no
  hover e clique do meio fecham; arrastar reordena; soltar fora da janela
  abre a aba numa janela nova; parar o mouse em cima mostra a previa.
- **Abas verticais:** `tab_bar = 'left'` (ou `Ctrl+Alt+B`), agrupadas por
  workspace; o mesmo atalho recolhe para so icones.
- **Panes como cartoes:** cantos arredondados, sombra, respiro e brilho no
  ativo; o divisor engrossa no hover e arrasta com o mouse; abrir, fechar e
  zoom animam; cabecalho com pasta, branch, comando rodando e tempo.
- **Blocos de comando:** cada comando vira um cartao (verde/vermelho), com selo
  de exit e duracao. No hover: copiar a saida, re-executar, recolher, mandar
  ao agente e fixar no topo do pane. Saida com mais de 300 linhas nasce
  recolhida.
- **Minimapa:** a barra de rolagem mostra cada comando (verde ok, vermelho
  falhou) e as ocorrencias da busca; clique pula para o ponto.
- **Agentes:** painel lateral (`Ctrl+Shift+I`) com plano, tokens e custo;
  linha do tempo da sessao (`Ctrl+Shift+Y`); cartao com Aprovar/Negar/Abrir
  quando um agente fora de vista pede algo; toast do Windows com os mesmos
  botoes (protocolo `duck://`).
- **Digitacao:** editor de comando (`Ctrl+Shift+Enter`: varias linhas,
  selecao, Ctrl+Z/Y); popup de autocompletar (`Alt+↑↓` escolhe, `Alt+Enter`
  aceita); realce do comando; cursor que desliza.
- **Temas:** galeria com previa ao vivo; tema gerado do papel de parede;
  sliders de espacamento, tamanho, peso, opacidade e raio; ligaduras e
  variantes da fonte; shaders prontos (CRT, brilho, grao); icones Nerd Font
  embutidos.
- **Navegacao:** paleta unica (comandos, abas de todos os workspaces, sessoes,
  historico e arquivos do repo) com atalhos em teclas e dica de atalho depois
  do 3o uso; tela inicial (`Ctrl+Shift+Home`); modo foco (`Ctrl+Shift+Z`);
  troca de workspace com transicao; busca com contador (`3/27`) e marcas no
  minimapa.
- **Atualizar sem perder sessoes:** `duck cli restart-server` (ou a paleta)
  grava o layout e reinicia o server; os shells continuam. Nao use
  `kill-server` para isso: ele fecha todos os shells.
- **Automacao:** `duck cli gui-action --name home` dispara qualquer acao da
  janela (`--args '["right"]'` para argumentos).

## Fluxos de agente

- **Corrigir com agente:** comando que falhou → faixa vermelha → clique (ou
  `Ctrl+Shift+E`). O comando, o exit e a saída vão para o agente da aba; se
  não houver um, abre um ao lado. Entra na fila: um agente ocupado recebe
  quando parar.
- **Agente em worktree:** `Ctrl+Shift+N` → "em worktree nova". O Duck cria
  `<repo>-<branch>` com a branch nova e abre o agente lá numa aba `⎇ branch`.
  Ao terminar, use a paleta → "Git: finalizar worktree" para ver o diff,
  mesclar na branch base ou remover a worktree.
- **Agent Manager (`Ctrl+Shift+A`):** grade com a tela de cada agente
  atualizando sozinha. Setas navegam, Enter foca, Ctrl+Y aprova, Ctrl+N nega
  e M deixa recado.
- **Progresso do plano:** o hook lê `TaskCreate`/`TaskUpdate` (ou
  `TodoWrite`) e mostra `3/7` na aba, no painel e na grade.
- **Revisão (`Ctrl+Shift+D`):** linhas alteradas do `git diff`. Enter comenta
  a linha, e "Enviar" manda todos os comentários juntos ao agente.
- **Colar print:** Ctrl+Shift+V com imagem no clipboard salva um PNG e cola o
  caminho; no WSL, o caminho vira `/mnt/c/...`.
- **Orçamento:** `token_budget` avisa (toast e barra) quando a soma de tokens
  das sessões passar do limite.

## Histórico, gravação e outros

- **Histórico:** todo comando de todos os panes (pwsh e bash/zsh do WSL) fica
  em `%LOCALAPPDATA%\duck\history.jsonl`, com pasta, exit, duração e pane;
  segredos viram `***`. `Ctrl+Shift+R` busca, e a seta → aceita a sugestão
  enquanto você digita.
- **Sticky scroll:** rolando a saída de um comando, o comando fica preso no
  topo do pane.
- **Portas:** `localhost:5173` na saída aparece na barra (`⇄ 5173`) e na
  paleta ("Abrir http://localhost:5173").
- **Gravação:** a paleta → "Gravar sessão" grava o pane em asciinema `.cast`
  (`%LOCALAPPDATA%\duck\recordings`). `duck play <arquivo> --speed 2`
  reproduz.
- **Popups:** `popups` na config (lazygit, git log, btop…) abrem num
  flutuante que some quando o comando sai.
- **SSH resiliente:** domínio `{ ssh = 'usuario@host', session = 'duck' }`
  roda `tmux new -A` no servidor. Se a rede cair, o pane reconecta sozinho no
  mesmo lugar e a sessão remota continua.

## Segurança

| Proteção | O que faz |
|---|---|
| OSC 52 | App (ou arquivo exibido com `cat`) escrevendo no clipboard **pede confirmação**; "Negar" vem primeiro. Ler o clipboard é sempre negado. |
| Colar com segurança | Texto com várias linhas ou comando perigoso (`sudo`, `rm -rf`, `curl … \| sh`, `iwr … \| iex`…) pede confirmação; caracteres de controle são removidos. |
| Texto do app | Título, notificações e caminhos sem caracteres de controle. |
| Links OSC 8 | Se o texto exibido não é o destino, o Duck mostra o destino real antes de abrir. |
| Escopo da API | Cada pane tem um token próprio. `api_scope = 'children'` limita um agente aos panes que ele criou; `'read'`, a só ler. |
| Ações de agente | Fechar pane alheio, digitar num agente trabalhando ou matar o server pedem OK na janela. |
| Auditoria | `%LOCALAPPDATA%\duck\audit.log`: tudo que agentes e scripts mandaram (a digitação da janela fica de fora). |
| Segredos | Tokens, chaves e `senha=` viram `***` no autosave/checkpoint; pane privado não grava texto. |
| Pipe | Primeira linha da conexão é um token que só o dono lê; outro usuário da máquina não manda texto para os seus shells. |

## Agentes

```powershell
duck cli list                                       # workspaces, abas, panes, agentes (JSON)
duck cli spawn --where right --cmd "claude"         # subagente num pane ao lado -> {"pane": 7}
duck cli send --pane 7 --text "rode os testes" --enter
duck cli wait --pane 7 --state blocked,done --timeout 900
duck cli read --pane 7 --lines 40                   # texto da tela (ou --all)
duck cli last-output --pane 7                       # saída + exit do último comando
duck cli last-failure --pane 7                      # último comando que falhou: texto, exit, saída
duck cli blocks --pane 7                            # todos os comandos do pane (texto, exit, duração)
duck cli history --cwd D:/projetos/app --limit 20    # histórico (de todos os panes)
duck cli queue --pane 7 --text "agora rode os testes"   # recado: entregue quando o agente parar
duck fleet --agents 3 --cmd claude tarefas.txt      # fila: N agentes, uma tarefa por linha
duck run --cmd "npm test" --timeout 600             # sem janela (CI): imprime a saída, devolve o exit
duck mcp                                            # servidor MCP (stdio): duck_list/spawn/send/read/wait/last_output
```

Para usar por MCP no Claude Code: `claude mcp add duck -- D:\Tools\Duck\bin\duck.exe mcp`.

**Estado do agente:**

1. **Hook** (o mais preciso): `duck install --hooks <settings.json>`.
   - `UserPromptSubmit` e `PreToolUse` → `working`;
   - `Notification` → `blocked`;
   - `Stop` → `done`.

   O hook também guarda o `session_id` (para o checkpoint) e os tokens de
   contexto da última resposta, que aparecem no painel.
2. **Automático** (qualquer agente, inclusive no WSL): processo do pane ou
   spinner no título. O estado vem da tela: prompt de permissão → `blocked`,
   saída recente → `working`.

## Persistência e estabilidade

- Cada pane tem um `duckw ptyhost` dono do ConPTY. O server pode cair:
  - os shells continuam rodando;
  - o server novo reconecta e remonta abas e splits a partir de `live.json`;
  - o host devolve os últimos 256 KB de saída para refazer a tela.
- O autosave grava, a cada 5 min, todos os workspaces e as sessões de agente
  (checkpoint). Depois de um reboot, a janela oferece restaurar, e o
  restaurar roda `claude --resume <sessão>` com o perfil certo, inclusive no WSL.
- Panic numa conexão ou num pane fica registrado em
  `%LOCALAPPDATA%\duck\server.log` e não derruba o resto.
- Se a GPU for perdida (driver reiniciou), a janela se recria.
- O scrollback tem teto em memória por pane (`scrollback_mb`).
- Um pane despejando saída sem parar não trava a janela: o intervalo entre
  frames se adapta.

## Personalização (`~/.duck.lua`)

`duck init` cria o arquivo a partir de [`duck.example.lua`](duck.example.lua),
a referência comentada. Salvar recarrega na hora. Ajustes da máquina e a tela
de configurações (`Ctrl+,`) gravam em `~/.duck-local.lua`.

- **Temas:** 5 embutidos, ou qualquer tema do Ghostty/WezTerm em
  `~/.duck/themes/<nome>`. Com `theme = { dark = …, light = … }`, segue o
  claro/escuro do Windows.
- **Fundo:** cor, Acrylic/Mica, opacidade, gradiente (`background.gradient`)
  ou imagem (`background.image`).
- **Shader de pós-processamento:** `shader = 'arquivo.wgsl'`, com
  `fn effect(uv, time, size) -> vec4<f32>`, que pode ler a tela com
  `scene(uv)`. Exemplo de scanlines CRT:
  ```wgsl
  fn effect(uv: vec2<f32>, time: f32, size: vec2<f32>) -> vec4<f32> {
      let c = scene(uv);
      return vec4(c.rgb * (0.9 + 0.1 * sin(uv.y * size.y * 3.14159)), c.a);
  }
  ```
- **Perfis:** domínios com fundo e cor de aba próprios (produção em
  vermelho). Também há cor de aba por regra (`tab_rules`) e manual (paleta).
- **Barra:** a barra e os títulos são funções Lua (`status`, `tab_title`).
- **Atalhos e modos:** `keys` e `key_tables`; a ação pode ser nativa ou uma
  função Lua.
- **Layouts:** layouts declarativos (`layouts`) abrem um workspace pronto.
  Também há workflows com parâmetros (`workflows`, `{{nome:padrão}}`).
- **Eventos:**
  - handlers Lua em `on = { ['attention'] = function(ev, win) … end }`;
  - comandos externos em `hooks = { ['command-finished'] = '…' }`.
- **Plugins Lua:** `require 'nome'` acha `~/.duck/plugins/nome.lua`.
  Instale com `duck plugin add <url-git>`.
- **Plugins WebAssembly** (`~/.duck/plugins/*.wasm` ou `.wat`), em qualquer
  linguagem que compile para wasm32:
  - isolados, sem disco, rede ou processos;
  - com limite de instruções por evento.

  ABI:

  | Lado | Função |
  |---|---|
  | exporta | `memory` |
  | exporta | `duck_alloc(len) -> ptr` |
  | exporta | `duck_on_event(ptr, len)`, que recebe o evento em JSON |
  | importa | `duck.request(ptr, len)`: `{"cmd": …}` vai ao server, `{"action": …, "args": […]}` roda uma ação da janela |
  | importa | `duck.log(ptr, len)` |

  Exemplo mínimo: o teste `plugin_requests` em `src/plugins.rs`.

## Arquitetura

```
duckw (janela) ─┐                         ┌─ duckw ptyhost ── ConPTY ── pwsh
duck cli / mcp ─┼─ named pipe + token ─ duckw server ─┼─ duckw ptyhost ── ConPTY ── wsl/claude
plugins WASM ────┘   (frames por diferença) │ parser VT, layout, agentes,   └─ … (sobrevivem ao server)
                                            │ autosave, live.json, hooks, auditoria
```

| Peça | Escolha |
|---|---|
| Janela / input | winit |
| Render | wgpu (DX12) + glyphon; retângulos, imagem e pós-processamento por pipelines próprios |
| Parser VT | alacritty_terminal (com synchronized output) |
| PTY | portable-pty (ConPTY), um processo host por pane |
| Config / plugins | mlua (Lua 5.4) · wasmi (WebAssembly) |
| IPC | local socket (named pipe) + JSON por linha |

Código:

| Arquivo | Conteúdo |
|---|---|
| `src/server.rs` | mux |
| `src/ptyhost.rs` | dono do shell |
| `src/gui.rs` | janela |
| `src/render.rs` | GPU |
| `src/config.rs` | Lua |
| `src/plugins.rs` | WASM |
| `src/cli.rs` | CLI, hook, `doctor`, `run`, `fleet`, `mcp`, `view` |
| `src/proto.rs` | transporte |
| `src/shell/` | shell integration |

Testes: `cargo test` (23).
