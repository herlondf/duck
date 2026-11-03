# Duck — o que outros terminais têm e pode nos agregar

Levantamento de 01/11/2026. Fontes: Kiro CLI, Warp, Ghostty, Wave, kitty,
Zellij, WezTerm, Windows Terminal, iTerm2, Alacritty, além de advisories de
segurança. Cada item traz a origem, o esforço (P = horas, M = 1–2 dias,
G = semana+) e se já temos.

Prioridade: **1** = fazer já · **2** = próximo ciclo · **3** = quando sobrar.

> **Status (02/11/2026):** implementado, com as exceções marcadas e
> justificadas na Fase 7 do [FUNCIONALIDADES.md](FUNCIONALIDADES.md):
> - protocolo binário e redesenho parcial na GPU, porque as medições mostraram
>   que não eram o gargalo;
> - widgets web e sessões do WSL no dashboard.
>
> A coluna "Hoje no Duck" abaixo é o retrato de **antes** do ciclo.

---

## 1. Segurança (o mais urgente)

| Pri | Funcionalidade | De onde | Esforço | Hoje no Duck |
|---|---|---|---|---|
| **1** | **OSC 52 negado por padrão**: um app ou arquivo malicioso escreve no clipboard e o próximo Ctrl+V roda o comando dele. Pedir confirmação ou permitir só por domínio/pane | Warp (CVE corrigido), par-term | P | **Vulnerável**: aceita e copia direto |
| **1** | **Colar com segurança**: avisar ao colar texto com várias linhas ou com `sudo`/`rm -rf`/`curl \| sh`; tirar os caracteres de controle (C0/C1/ESC) do que é colado | Windows Terminal, iTerm2, kitty | P | Só bracketed paste |
| **1** | **Título e textos vindos do app sem caracteres de controle**: título de aba, toast e painel de agentes usam texto do app | advisories broot/pickem | P | Não filtra |
| 2 | **Permissão por comando na API**: o `duck cli` hoje pode tudo. Escopos, como o kitty: um pane só lê, ou só manda texto para os filhos dele | kitty `allow_remote_control` por janela | M | Token único, tudo ou nada |
| 2 | **Confirmar ações destrutivas vindas de agente**: `kill-server`, `close` de pane que não é seu, `send` para pane com agente ativo | herdr / Warp Agent Mode (aprovação passo a passo) | M | Não confirma |
| 2 | **Log de auditoria da API**: quem (pane de origem) mandou o quê para quem | Warp Drive / Oz | P | Não tem |
| 2 | **OSC 8 (hyperlink do app) mostrando o destino real** antes de abrir, contra link que diz uma coisa e abre outra | iTerm2, WezTerm | P | Não suporta OSC 8 |
| 3 | **Modo "input seguro"** ao digitar senha (prompt `sudo`/`ssh`): nada de log nem de autosave do texto | iTerm2 Secure Keyboard Entry | M | O autosave grava as últimas 200 linhas |
| 3 | **Estado salvo sem segredos**: filtrar tokens/senhas (regex) do texto gravado no checkpoint | — | P | Grava cru em `%LOCALAPPDATA%\duck\state` |

## 2. Estabilidade

| Pri | Funcionalidade | De onde | Esforço | Hoje |
|---|---|---|---|---|
| **1** | **Shells sobrevivem a crash do server**: separar o dono dos PTYs (processo mínimo) da lógica, ou reiniciar o server reaproveitando os PTYs | tmux, Zellij | G | Server morre e leva os shells |
| **1** | **Panic não derruba o mux**: `catch_unwind` por conexão e por pane; um pane com bug não mata os outros | — | P | Um panic numa thread pode envenenar o lock |
| **1** | **Ressurreição automática**: ao subir o server depois de crash ou boot, oferecer o último autosave | Zellij 0.39 session resurrection | P | Só manual (`--restore`) |
| 2 | **Watchdog da GUI**: frame travado ou GPU perdida (driver reiniciou) → recriar o device sozinho | Ghostty, WezTerm | M | Trata só `Lost`/`Outdated` da superfície |
| 2 | **Limite de memória por pane**: app que despeja GBs não estoura a RAM (scrollback com teto em bytes, não só em linhas) | Alacritty, kitty | P | Teto só em linhas |
| 2 | **Backpressure**: com um pane cuspindo saída sem parar, a GUI e os outros panes continuam fluidos | Ghostty | M | Frame inteiro a cada 8 ms |
| 2 | **Log do server em arquivo** com rotação, e `duck cli doctor` (versão, GPU, fontes, WSL, hooks) | WezTerm `ls-fonts`, Ghostty `+show-config` | P | Sem log |
| 3 | **Versão do protocolo** no handshake: GUI nova com server velho dá erro claro em vez de travar | WezTerm mux | P | Não versiona |

## 3. Performance

| Pri | Funcionalidade | De onde | Esforço | Hoje |
|---|---|---|---|---|
| **1** | **Frame por diferença**: mandar só as linhas que mudaram (hoje vai a tela inteira em JSON) | WezTerm mux, Zellij | M | Tela inteira |
| **1** | **Synchronized output** (DEC 2026): o app avisa "começo/fim de atualização" e o terminal não desenha no meio. O Kiro e o Claude Code usam e recomendam terminais com isso | Kiro docs, Ghostty, kitty | P | Não suporta, e o Claude Code pisca |
| 2 | **Protocolo binário** (bincode/MessagePack) em vez de JSON para os frames | — | M | JSON |
| 2 | **Redesenho parcial**: só repintar o pane que mudou, sem reconstruir todas as camadas | Alacritty damage tracking | M | Redesenha tudo |
| 2 | **Atlas de glifos menor / backend Vulkan**: baixar os 166 MB da GUI | — | M | DX12 |
| 2 | **FPS máximo configurável** e economia na bateria (`max_fps` em bateria) | WezTerm | P | Vsync |
| 3 | **Startup < 100 ms**: medir e cortar (fontes carregadas sob demanda) | Ghostty, Alacritty | M | Não medido |

## 4. UI/UX

| Pri | Funcionalidade | De onde | Esforço | Hoje |
|---|---|---|---|---|
| **1** | **Blocos de comando**: cada comando com a saída, o exit code, a duração e a hora; pular entre blocos, copiar só a saída do último comando, marcar em vermelho o que falhou. Depende de shell integration (OSC 133) | Warp, Ghostty, iTerm2, WezTerm | M | Não tem |
| **1** | **Shell integration automática** para pwsh e bash/zsh (OSC 133 + OSC 7): cwd confiável, "abrir pane na mesma pasta", prompt marcado | Ghostty (injeta sozinho), WezTerm | M | Lê o OSC 7 se o shell mandar |
| **1** | **Quick terminal** (Quake): uma tecla global desce o terminal do topo da tela, por cima de tudo | Ghostty, Windows Terminal | M | Não tem |
| **1** | **Painéis flutuantes**: pane por cima dos outros, que liga/desliga e continua rodando escondido (bom para um agente "de plantão") | Zellij, Wave | M | Não tem |
| 2 | **Layouts declarativos** em Lua: um arquivo descreve abas, splits e comandos ("projeto X = editor + 2 agentes + logs") e abre tudo de uma vez | Zellij (KDL), Wave | M | Só restaurar estado salvo |
| 2 | **Hints / seleção por teclado**: tecla mostra rótulos em URLs, caminhos e hashes na tela, e digitar o rótulo copia ou abre | kitty `hints`, WezTerm quick select | M | Só Ctrl+clique |
| 2 | **Broadcast**: digitar em vários panes ao mesmo tempo (vários servidores, vários agentes) | kitty, iTerm2 | P | Não tem |
| 2 | **Notificação de comando longo terminado** (> N s, fora de foco), não só de agente | iTerm2, Ghostty, Warp | P | Só agentes |
| 2 | **Workflows salvos**: comandos com parâmetros (`deploy {{ambiente}}`) na paleta, compartilháveis com o time | Warp Drive | M | Entradas Lua na paleta |
| 2 | **Busca regex + filtrar a tela** (mostrar só as linhas que casam) | WezTerm, iTerm2 | P | Busca simples |
| 2 | **Widgets nos blocos**: pane que mostra arquivo, imagem, markdown ou web, não só terminal | Wave | G | Não tem |
| 3 | **Barra de rolagem com marcas** (onde estão os prompts, os erros e os resultados da busca) | iTerm2, Windows Terminal | P | Não tem |
| 3 | **Cursor e aba animados, visual bell** (piscar suave em vez de beep) | WezTerm, Ghostty | P | Não tem |
| 3 | **Tela de configuração** gerada da config, para quem não quer editar Lua | Windows Terminal | G | Só Lua |

## 5. Agentes (para ir além do herdr)

| Pri | Funcionalidade | De onde | Esforço | Hoje |
|---|---|---|---|---|
| **1** | **Dashboard de sessões**: todas as sessões de agente (vivas e antigas) por pasta, para retomar com um Enter | Kiro CLI 2.22 (session dashboard), herdr | M | Painel só das vivas |
| **1** | **Agente pergunta, Duck mostra a aprovação**: quando o agente está `blocked` num pedido de permissão, aprovar ou negar pelo painel sem trocar de aba | Warp Agent Mode | M | Só avisa |
| 2 | **Custo e contexto por agente** na barra (tokens, % de contexto, se o agente expõe) | Kiro (painel de uso) | M | Não tem |
| 2 | **Fila de tarefas**: dar 5 tarefas, o Duck distribui entre N panes de agente e avisa quando cada uma acaba | herdr-fleet, Warp Oz | G | `wait` manual |
| 2 | **Diff do que o agente mudou** desde o último prompt (git), num painel ao lado | Warp, Wave | M | Não tem |
| 3 | **Modo headless**: `duck run --agent claude "tarefa"` sem janela, para CI | Kiro CLI headless | M | Só com GUI |
| 3 | **MCP server do Duck**: o agente controla o terminal por MCP, além da CLI | — | M | Só CLI |

## 6. Personalização

| Pri | Funcionalidade | De onde | Esforço | Hoje |
|---|---|---|---|---|
| **1** | **Tema que segue o Windows** (claro/escuro automático) | Ghostty, WezTerm | P | Fixo |
| 2 | **Importar temas** do formato iTerm2/WezTerm/Ghostty (há centenas prontos) | Ghostty (300+ temas) | P | 5 embutidos |
| 2 | **Imagem/gradiente de fundo** com opacidade | WezTerm, Windows Terminal | P | Só cor + Acrylic |
| 2 | **Perfis**: shell + tema + fonte + cwd por perfil (perfil "produção" com fundo vermelho) | Windows Terminal, iTerm2 | P | Domínios sem visual próprio |
| 2 | **Cor por aba/pane** (manual ou por regra: ssh em prod → vermelho) | iTerm2, Windows Terminal | P | Não tem |
| 3 | **Plugins WASM** (qualquer linguagem, isolados), além de Lua | Zellij | G | Lua |
| 3 | **Shaders de pós-processamento** (CRT, brilho), opcionais | Ghostty, Windows Terminal | M | Não tem |

---

## Recomendação: próximo ciclo (≈ 1 semana)

Ordem pensando em risco primeiro, depois no ganho do dia a dia:

1. **Segurança P** (meio dia): OSC 52 negado por padrão, colar com segurança, texto do app filtrado.
2. **Estabilidade P** (meio dia): `catch_unwind`, log em arquivo, `duck cli doctor`, oferecer o autosave após crash.
3. **Synchronized output** (meio dia): tira o pisca-pisca do Claude Code.
4. **Shell integration + blocos de comando** (2 dias): é a funcionalidade que mais muda o uso.
5. **Frame por diferença** (1 dia): menos CPU com muitos panes.
6. **Quick terminal + painéis flutuantes** (1–2 dias).

## Fontes

- [Kiro CLI 2.0](https://kiro.dev/blog/cli-2-0/) · [experimentos do Kiro CLI](https://kiro.dev/docs/cli/experimental/) · [releases de set/2026](https://www.havoptic.com/releases/kiro/2026/9)
- [Warp Agent Mode](https://www.warp.dev/blog/agent-mode) · [guia Warp 2026](https://www.deployhq.com/guides/warp)
- [Ghostty 1.2](https://alternativeto.net/news/2025/9/ghostty-1-2-adds-command-palette-new-app-icons-quick-terminal-size-and-ssh-improvements) · [Ghostty no LWN](https://lwn.net/Articles/1004377)
- [Wave Terminal](https://www.waveterm.dev/) · [widgets do Wave](https://docs.waveterm.dev/customwidgets)
- [kitty remote control](https://sw.kovidgoyal.net/kitty/remote-control/)
- [Zellij 0.39: session resurrection e plugins](https://zellij.dev/news/session-resurrection-ui-components)
- [OSC 52 no Warp](https://www.superagent.sh/blog/when-terminal-output-owns-your-clipboard-osc-52-in-warp) · [injeção de escape no pickem](https://securelayer7.net/lab/pickem-terminal-escape-sequence-injection-unsanitized-labels) · [broot](https://www.vulncheck.com/advisories/broot-terminal-escape-sequence-injection-via-unsanitized-file-and-directory-names-in-the-tree-view) · [par-term SECURITY.md](https://docs.rs/crate/par-term/0.40.0/source/SECURITY.md) · [colar com segurança (LWN)](https://lwn.net/Articles/752137/)

---

# Rodada 2 (02/11/2026): VS Code, Google Antigravity e CLIs

> **Status:** todos implementados — ver a Fase 8 do
> [FUNCIONALIDADES.md](FUNCIONALIDADES.md), que separa o que já foi conferido
> na janela do que ainda falta conferir.

Fontes novas: o terminal integrado do VS Code (shell integration,
decorations, sticky scroll, sugestões), o Google Antigravity (Agent Manager,
Artifacts), CLIs de agente (Codex, Gemini, Kiro) e ferramentas de shell
(atuin, fzf, lazygit). Também apps de agentes paralelos (Conductor, Crystal).

## Agentes (o que mais rende)

| Pri | Funcionalidade | De onde | Esforço |
|---|---|---|---|
| **1** | **"Corrigir com agente" no bloco que falhou**: um clique manda o comando, a saída e o exit para um pane de agente (ou abre um) | Quick Fix do VS Code, Warp | P |
| **1** | **Agente em git worktree**: "novo agente" cria uma worktree + branch, abre o agente nela e mostra a branch na aba; ao terminar, oferece diff e merge | Conductor, Crystal, Antigravity (workspaces paralelos) | M |
| **1** | **Agent Manager em grade**: tela cheia com todos os agentes lado a lado (miniatura viva de cada pane, estado, pergunta pendente) | Antigravity Manager Surface, Warp vertical tabs | M |
| 2 | **Progresso do plano do agente**: lê os TODOs do transcript (TodoWrite) e mostra "3/7 tarefas" no painel e na aba | Antigravity Artifacts | P |
| 2 | **Recado na fila**: deixar uma mensagem para o agente que é entregue quando ele parar (sem interromper) | Antigravity (feedback sem parar a execução) | P |
| 2 | **Colar imagem para o agente**: Ctrl+V com print no clipboard salva PNG e cola o caminho | Claude Code / Codex aceitam caminhos de imagem | P |
| 2 | **Bifurcar sessão**: duplicar o agente atual num pane novo (`claude --resume <id> --fork-session`) | Claude Code | P |
| 3 | **Perfis de lançamento**: "agente planejador" (modo plano) e "agente rápido", com modelo e permissões | Antigravity Planning/Fast | P |
| 3 | **Revisão de código no terminal**: diff navegável, comentar uma linha e mandar o comentário ao agente | Warp code review | G |

## UX de terminal

| Pri | Funcionalidade | De onde | Esforço |
|---|---|---|---|
| **1** | **Links `arquivo:linha:coluna`** (erros de compilador, stack traces) abrem no VS Code (`code -g`) | VS Code terminal links | P |
| **1** | **Histórico estilo atuin**: todo comando (OSC 133) gravado com pasta, exit, duração e pane, incluindo o WSL; Ctrl+R fuzzy filtrando por pasta e por sucesso | atuin | M |
| 2 | **Sticky scroll**: o prompt do comando cuja saída você está lendo fica preso no topo do pane | VS Code | P |
| 2 | **Porta detectada**: `localhost:3000` na saída vira link e entra na barra ("servidor rodando: 3000") | VS Code ports, Wave | P |
| 2 | **Sugestões inline** (texto fantasma) do histórico da pasta enquanto você digita | VS Code terminal suggest, fish, Fig | M |
| 3 | **Gravar sessão** (asciinema `.cast`) e reproduzir; base para o vídeo narrado | asciinema, VHS | M |
| 3 | **Popups prontos** em flutuante: lazygit, htop, logs do docker, com um atalho cada | tmux popups, Zellij | P |

## Infra

| Pri | Funcionalidade | De onde | Esforço |
|---|---|---|---|
| 2 | **Domínio SSH com mux remoto**: a sessão no servidor sobrevive à queda da rede, e o Duck reanexa | WezTerm ssh domain, mosh, exe.dev + tmux | G |
| 3 | **Orçamento de tokens**: alerta quando a soma de contexto/custo dos agentes passa de um limite | Kiro (painel de uso) | P |

## Recomendação

Ordem por ganho no dia a dia: "Corrigir com agente" → links `arquivo:linha` →
agente em worktree → histórico atuin → Agent Manager em grade. Os cinco
cabem em uns 3 dias; os itens P da tabela, em mais 1 dia.

Fontes: [Antigravity (Google)](https://developers.googleblog.com/build-with-google-antigravity-our-new-agentic-development-platform/) ·
[guia do Antigravity](https://blog.openreplay.com/google-antigravity-ide-guide/) ·
[shell integration do VS Code](https://code.visualstudio.com/docs/terminal/shell-integration) ·
[recursos do terminal do VS Code](https://www.hongkiat.com/blog/optimize-visual-studio-code-terminal/) ·
[Warp + Gemini CLI](https://marketing.warp.dev/agents/gemini-cli) ·
[Codex CLI + tmux](https://codex.danielvaughan.com/2026/04/27/codex-cli-terminal-native-workflow-neovim-tmux-multiplexer-integration/)
