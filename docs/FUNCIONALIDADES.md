# Duck — funcionalidades e roadmap

Emulador de terminal nativo (Rust + wgpu) com multiplexador persistente e
orquestração de agentes embutidos. Junta o WezTerm (janela GPU, config Lua,
mux) com o herdr (estado dos agentes, API de socket para agentes se
controlarem). Uso e referência: [README.md](README.md).

Metas que guiam toda decisão:

1. **Leve**: idle perto de 0% de CPU (só redesenha quando muda), um código só (dois exe finos).
2. **Personalizável**: tudo que o WezTerm configura e mais. A barra, os títulos,
   a paleta e os eventos também são scriptáveis em Lua.
3. **Agente é cidadão de primeira classe**: o terminal sabe quem é agente, em
   que estado está e expõe isso por API.

Legenda: `[x]` pronto e testado · `[~]` pronto, teste parcial · `[ ]` a fazer.

## Medições (02/11/2026, release, esta máquina, RTX 3050, 3 panes abertos)

| | Duck | WezTerm nightly |
|---|---|---|
| Throughput (`bench-render.ps1`, 100 mil linhas) | **12.485 linhas/s** | 7.455 linhas/s |
| RAM da janela (working set) | **165 MB** | 189 MB |
| RAM do server / de cada host de pane | 17 MB / ~15 MB | — |
| CPU parado, janela | **0 ms** em 5 s | — |
| CPU parado, server | 16 ms em 5 s (varredura de agentes) | — |
| Janela visível depois do clique | **31–76 ms** | — |
| Shells sobrevivem a crash do server | **sim** (testado com kill -9) | não |

---

## Fase 0 — Terminal

- [x] Janela (winit) + render por GPU (wgpu DX12 + glyphon)
- [x] PTY no Windows (ConPTY), shell padrão `pwsh`, env da sessão do Claude limpo
- [x] Parser VT (alacritty_terminal): 16/256/truecolor, negrito, itálico,
      sublinhado, tachado, dim, inverso, tela alternativa
- [x] Fundo por célula, cursor de verdade (bloco/barra/sublinhado; vazado sem foco)
- [x] Teclado: texto, AltGr do ABNT2, Ctrl/Alt+tecla, setas com modificador,
      F1–F12, modo de cursor do app, Shift+Enter para nova linha no Claude Code
- [x] Respostas a consultas do app (cor de fundo OSC 11, tamanho, DA)
- [x] Resize de pane → resize do PTY
- [x] Scrollback com roda do mouse (vira seta na tela alternativa)
- [x] Seleção com mouse (copia ao soltar), duplo clique em palavra, colar com
      botão direito ou Ctrl+Shift+V (bracketed paste)
- [x] Mouse para o app (SGR: clique, arraste, roda); Shift força a seleção
- [x] Arrastar arquivo cola o caminho; OSC 52 (app copia para o clipboard)

## Fase 1 — Multiplexador persistente

- [x] `duck server` dono dos shells; fechar a janela não mata nada
- [x] GUI anexa e desanexa; reconecta sozinha se o server reiniciar
- [x] Abas, splits (direita/abaixo/esquerda/cima), fechar pane, zoom
- [x] Workspaces com troca fuzzy; as pastas do zoxide criam o workspace já na pasta
- [x] Modo redimensionar (Alt+P: h/j/k/l)
- [x] Foco por direção (Alt+setas), mover aba, renomear aba e workspace
- [x] Domínios: local e WSL (duas distros), testados de ponta a
      ponta (`DUCK_PANE`/`TERM_PROGRAM` chegam ao Linux via WSLENV); ssh e cmd por config.
- [x] Env limpo ao criar pane (sem `CLAUDECODE`, `CLAUDE_CODE_*` e o
      `NO_COLOR` injetado pela sessão do Claude)
- [x] Pipe autenticado por token (outro usuário local não manda texto para os seus shells)

## Fase 2 — Config e personalização

- [x] Config em Lua (`~/.duck.lua`) com hot reload ao salvar
- [x] Override por máquina (`~/.duck-local.lua`)
- [x] 5 temas embutidos + cores próprias; fontes com pastas extras e fallback
- [x] Opacidade + Acrylic/Mica (swapchain DirectComposition), barra de título escura
- [x] Atalhos e key tables (modos) em Lua; a ação pode ser nativa ou função
- [x] Barra de status por função Lua, com CPU/RAM/hora/agentes sem script externo
- [x] Título das abas por função Lua
- [x] Paleta em pt-BR, estendível em Lua, que mostra o atalho de cada ação (cheat sheet)
- [x] Hyperlinks por regex (`#2605` → issue) + URLs, com Ctrl+clique
- [x] Barra em cima, embaixo ou escondida; cores por estado do agente

## Fase 3 — Agentes (o que vem do herdr)

- [x] Detecção por processo (claude, codex, cursor-agent, opencode, gemini…)
      e pelo título (spinner/✳ do Claude Code, que também cobre o WSL)
- [x] Estado por pane: `working` / `blocked` / `done` / `idle`, pelo hook do
      Claude Code (`duck hook`) ou pela tela como fallback
- [x] Atenção: ponto colorido e sublinhado na aba, borda do pane, toast do
      Windows e taskbar piscando (não notifica o pane que você está vendo)
- [x] Painel de agentes (Alt+A): estado, idade, workspace/aba, Enter foca
- [x] **API + CLI** (`duck cli`): spawn, send, read, wait, list, frame, set-state…
- [x] Orquestração: um agente abre subagentes em panes, manda tarefas e
      espera com `wait --state blocked,done`

## Fase 4 — Sessões e checkpoint

- [x] Salvar workspace (abas, splits, pastas, domínio, 200 linhas de texto)
- [x] Autosave a cada 5 min (configurável)
- [x] Restaurar e apagar estado salvo (seletor fuzzy)
- [x] Checkpoint: todos os workspaces + sessões de agente; restaurar roda
      `claude --resume <id>` com o `CLAUDE_CONFIG_DIR` certo (pwsh ou bash/WSL)
- [~] Restaurar no boot: `duck install` / `install.ps1 -Boot` cria o atalho na
      pasta Inicializar com `duckw --restore`. Pronto, mas não ativado nesta
      máquina (mexe fora do projeto: rode o `install.ps1 -Boot`).

## Fase 5 — Hooks e plugins

- [x] Eventos para Lua (`on = {...}`): `pane-created`, `pane-closed`,
      `agent-state-changed`, `attention`, `bell`
- [x] Hooks externos por evento (`hooks = {...}`), com variáveis `DUCK_*`
- [x] Plugins Lua (`require`) em `~/.duck/plugins`; `duck plugin add|list|update`
- [x] Ações prontas na paleta: relogin do Claude (Windows/WSL), checkpoint,
      domínios. O `/parear` continua como skill do Claude Code.

## Fase 6 — Polimento

- [x] Ligaduras (cada trecho é moldado inteiro: `->` vira seta), emoji colorido, Nerd Font
- [x] Busca no scrollback com destaque (Ctrl+Shift+F, Enter = próxima)
- [x] Modo cópia estilo vim (Ctrl+Shift+X)
- [x] Benchmark contra o WezTerm (tabela acima)
- [x] Instalador (`install.ps1`): build, PATH, config, atalho, boot e hooks opcionais
- [ ] Imagens no terminal (sixel/kitty): o parser (alacritty_terminal) não suporta
- [ ] IME (entrada de idiomas asiáticos); as teclas mortas do ABNT2 funcionam

## Fase 7 — Ciclo da pesquisa ([PESQUISA.md](PESQUISA.md))

Segurança
- [x] OSC 52 pede confirmação ("Negar" primeiro); leitura do clipboard sempre negada
- [x] Colar com segurança: confirma várias linhas / comando perigoso; tira caracteres de controle
- [x] Texto vindo do app (título, notificação, caminho) sem caracteres de controle
- [x] Escopo da API por pane (token próprio): `all` / `children` / `read`
- [x] Ações destrutivas de agente pedem OK na janela
- [x] Log de auditoria (`audit.log`)
- [x] Links OSC 8 com o destino real visível antes de abrir
- [x] Pane privado (fora do autosave e da leitura por agentes) e filtro de segredos no disco

Estabilidade
- [x] **Shells sobrevivem ao crash do server** (`ptyhost` por pane + `live.json`), testado com kill -9
- [x] Panic isolado por conexão/pane, com log em `server.log`
- [x] Restaurar o último checkpoint oferecido ao subir sem shells vivos
- [~] Janela se recria se a GPU for perdida (código pronto; TDR real não provocado)
- [x] Teto de memória do scrollback por pane; intervalo de frames adaptativo
- [x] `duck doctor`; aviso de versão diferente entre janela e server

Performance
- [x] Frame por diferença (só as linhas que mudaram) — throughput subiu para 12,5 mil linhas/s
- [x] Synchronized output (DEC 2026): o Claude Code não pisca
- [x] FPS máximo configurável; backend da GPU configurável
- [x] Abertura da janela em 31–76 ms

UI/UX
- [x] Shell integration (OSC 133/7): automática no pwsh; bash/zsh com um `source` (testado no WSL)
- [x] Blocos de comando: faixa verde/vermelha, exit e duração, Ctrl+Shift+↑/↓, copiar a última saída
- [x] Barra de rolagem com marcas de prompt e de erro
- [x] Quick terminal (`duck --quick`, tecla global)
- [x] Painéis flutuantes (Alt+F)
- [x] Layouts declarativos e workflows com parâmetros
- [x] Hints por teclado, broadcast, filtro por regex, busca `/regex/`
- [x] Aviso de comando longo terminado fora de foco
- [x] Visual bell
- [x] Tela de configurações (Ctrl+,) gravando em `~/.duck-local.lua`

Agentes
- [x] Dashboard de sessões do Claude (todos os perfis do Windows), Enter retoma
- [x] Aprovar/negar o pedido do agente pelo painel (Ctrl+Y / Ctrl+N), com a pergunta visível
- [x] Tokens de contexto por agente no painel (lidos do transcript pelo hook)
- [x] `duck fleet`: fila de tarefas distribuída entre N agentes
- [x] Diff do repo do agente num flutuante
- [x] `duck run` (sem janela, para CI) e `duck mcp` (servidor MCP)

Personalização
- [x] Tema segue claro/escuro do Windows; temas do Ghostty/WezTerm importáveis
- [x] Fundo com gradiente ou imagem; shader de pós-processamento (WGSL)
- [x] Perfis por domínio (fundo e cor de aba), cor de aba manual e por regra
- [x] Plugins WebAssembly isolados, com limite de instruções

O que ficou de fora, de propósito:
- **Protocolo binário:** depois do frame por diferença, o JSON ficou pequeno
  e o gargalo não está aí (medido).
- **Redesenho parcial na GPU:** a janela parada gasta 0 ms; o cache de texto
  já evita remoldar o que não mudou.
- **Widgets web dentro do pane (Wave):** o `duck view` mostra arquivo e
  markdown num flutuante; um navegador embutido custaria um WebView inteiro.
- **Sessões de agente do WSL no dashboard:** só os perfis do Windows por
  enquanto.

## Fase 8 — Rodada 2 da pesquisa (VS Code, Antigravity, CLIs)

Validado pela API/CLI, por testes unitários e na janela com input real.
O único `[~]` é o domínio SSH, que não teve um servidor para o teste real.

Agentes
- [x] Corrigir com agente: `last-failure` traz comando, exit e saída; entra na fila do agente (testado com agente parado)
- [x] Clique na faixa vermelha do bloco abre "Corrigir com agente" (conferido: o agente recebeu comando, exit e saída)
- [x] Agente em git worktree (`git worktree add -b`, aba `⎇ branch`) e "finalizar worktree" (diff, mesclar, remover)
- [x] Agent Manager em grade (`Ctrl+Shift+A`), com aprovar/negar/recado (conferido na janela)
- [x] Progresso do plano (TaskCreate/TaskUpdate/TodoWrite) na aba e no painel — teste com transcript real e unitário
- [x] Recado na fila (`queue`), entregue com bracketed paste quando o agente para
- [x] Colar print como arquivo PNG (caminho Windows ou `/mnt/c` no WSL) — conferido
- [x] Bifurcar sessão (`--fork-session`) num pane novo
- [x] Perfis de agente (`agent_profiles`), usados também por corrigir e worktree
- [x] Revisão do diff com comentários por linha enviados ao agente — conferido de ponta a ponta
- [x] Orçamento de tokens: evento `budget` + toast (testado com limite baixo)

UX de terminal
- [x] Links `arquivo:linha:coluna` abrem no editor (`editor` na config); caminhos do WSL convertidos
- [x] Histórico estilo atuin (`history.jsonl`, `duck cli history`), com o texto exato do comando (OSC 133;B)
- [x] Marcas de prompt resolvidas depois do repaint do ConPTY (antes ficavam algumas linhas atrasadas em saídas grandes)
- [x] Marcas reancoradas pelo texto do comando depois de um resize
- [x] ConPTY criado direto pela API do Windows, sem `PSEUDOCONSOLE_RESIZE_QUIRK`: depois de um split, o comando digitado aparecia uma linha abaixo do prompt (a quebra de linha do alacritty difere da do conhost). Efeito colateral conhecido: o repaint do ConPTY pode repetir 1–2 linhas no scrollback depois de um resize.
- [x] Sticky scroll (comando preso no topo ao rolar a saída)
- [x] Portas detectadas na saída: barra e paleta
- [x] Sugestão inline do histórico (texto fantasma; → aceita) — `auto`: só bash/zsh/cmd, o pwsh já tem a do PSReadLine
- [x] Gravação asciinema (`record`) e `duck play`
- [x] Popups configuráveis (flutuante que troca o anterior)

Infra
- [~] Domínio SSH com tmux remoto e reconexão automática no mesmo lugar do layout (config testada; sem servidor SSH para teste real)
- [x] API nova: `last-failure`, `blocks`, `history`, `queue`, `record`, `spawn --type`

## Fase 9 — UI/UX própria (40 itens)

Conferido na janela (instância de teste isolada, prints) salvo onde indicado.

Janela e moldura
- [x] 1 Barra de título própria (abas, botões, arrastar, duplo clique, bordas redimensionam)
- [x] 2 Abas verticais na lateral, agrupadas por workspace, com ícone; recolhível
- [~] 3 Mica/Acrylic nas Configurações (sem print: o efeito só aparece na composição do DWM)
- [x] 4 Panes como cartões (cantos, sombra, respiro)
- [x] 5 Brilho no pane ativo na cor da aba; inativos apagados

Abas
- [x] 6 Estado animado (pulso, âmbar, ✓)
- [x] 7 Barra de progresso (plano do agente ou OSC 9;4)
- [~] 8 Prévia do pane ao parar o mouse na aba (código pronto; o teste sintético de mouse não segura o hover)
- [x] 9 Arrastar reordena; soltar fora abre janela nova (`move-tab --workspace`)
- [x] 10 ✕ no hover e clique do meio fecham (`close-tab`)

Panes e layout
- [x] 11 Divisor arrastável, engrossa no hover
- [x] 12 Animação de abrir/fechar/zoom (140 ms)
- [x] 13 Cabeçalho do pane: pasta, branch, comando rodando, tempo, agente
- [x] 14 Modo foco
- [x] 15 Minimapa com comandos verde/vermelho e busca; clique pula

Blocos de comando
- [x] 16 Cartão por comando
- [x] 17 Botões: copiar saída, re-executar, recolher, mandar ao agente
- [x] 18 Saída longa recolhida ("N linhas recolhidas")
- [x] 19 Selo de exit/duração
- [x] 20 Fixar bloco no topo do pane

Agentes
- [x] 21 Avatar e cor por perfil (aba, cabeçalho, painel)
- [x] 22 Painel lateral: plano, tokens, custo (`token_price`)
- [x] 23 Cartão de aprovação com botões; diálogos (OSC 52, colar perigoso) com botões
- [x] 24 Linha do tempo da sessão (pedidos, ferramentas, arquivos, erros)
- [~] 25 Toast do Windows com Aprovar/Negar/Abrir (`duck://`; registro feito ao abrir a janela; não cliquei num toast real)

Entrada e edição
- [x] 26 Editor de comando (várias linhas, seleção, Ctrl+Z/Y, clique posiciona)
- [x] 27 Autocompletar em popup (histórico, caminhos, git, npm)
- [x] 28 Cursor suave (piscar suave opcional)
- [x] 29 Realce do comando digitado (fora do pwsh, que já tem o PSReadLine)
- [x] 30 Prévia de arquivo no Ctrl+clique

Visual e tipografia
- [x] 31 Galeria de temas com prévia ao vivo + tema do papel de parede
- [x] 32 Ligaduras e variantes (`ui.font_features`)
- [x] 33 Nerd Font embutida no binário
- [x] 34 Sliders ao vivo (espaçamento, tamanho, peso, opacidade, raio)
- [x] 35 Shaders prontos: CRT, brilho, grão

Navegação e paleta
- [x] 36 Paleta única: comandos, abas, sessões, histórico, arquivos
- [x] 37 Atalhos como teclas + dica depois do 3º uso
- [x] 38 Tela inicial do workspace
- [x] 39 Troca de workspace desliza
- [x] 40 Busca com contador e marcas no minimapa

Extras desta rodada: `duck cli restart-server` (atualiza sem perder shells),
`duck cli gui-action`, `duck url` (botões dos toasts).

## Fase 10 — Orquestração e desempenho (04/11/2026)

Conferido numa instância isolada (servidor e janela de teste, prints) salvo onde indicado.

Orquestração
- [x] Loop com verificador: o agente trabalha, o Duck roda a verificação quando ele para, a falha volta como recado, até passar ou acabar as voltas (`loop-start`, `loops`, `loop-stop`; aba `⟳ 2/5`)
- [x] Mapa dos agentes (`Ctrl+Shift+U`): agentes, panes filhos e subagentes do Claude Code em árvore, ao vivo
- [x] Caixa de entrada (`Ctrl+Shift+J`, sino na barra): agentes esperando e os últimos 60 avisos, inclusive com a janela fechada
- [x] Worktree com porta própria (`DUCK_PORT`) e `.duck-worktree.json` (copiar, env, setup, teardown) — teste unitário; não rodei num repo real
- [x] Rotinas adiam quando o limite de 5h do perfil passou de `uso_max` (85%) ou quando um agente seu trabalha no mesmo repositório (`ceder`) — compilado; o adiamento não foi provocado de verdade
- [x] `duck.agents()` no SDK (permissão `agents`)
- [~] Sessões do Claude no WSL no painel de sessões (só distros rodando; cache de 60 s) — a pasta foi lida; não retomei uma sessão por ali

Desempenho
- [x] Varredura de processos dos agentes pulada sem saída nova nem pane novo (uma por minuto no máximo)
- [x] Janela minimizada solta textos, atlas de glifos e alvo do shader e devolve a memória: working set de 169 MB para 14 MB. A memória reservada pelo driver não muda
- [x] Minimizar não encolhe mais os terminais para 2x2 (o Windows manda 160x28; antes tudo requebrava)

## Próximos passos sugeridos

1. Rodar `install.ps1 -ClaudeHooks -Boot` e usar no dia a dia junto com o WezTerm.
2. Pôr o `source .../duck.sh` no `~/.bashrc` das distros WSL (`duck doctor` mostra o caminho).
3. RAM da janela: quase tudo é DX12 + atlas de glifos (`gpu_backend = 'vulkan'` para comparar).
4. Listar sessões do WSL no dashboard e abrir o mux de outro usuário via domínio ssh.
