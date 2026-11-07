-- Config do Duck. Copie para ~/.duck.lua (ou rode `duck init`).
-- Salvar o arquivo recarrega tudo sozinho. Ajuste por maquina vai em
-- ~/.duck-local.lua, que roda depois com esta tabela em `config`:
--     config.font.size = 13
local duck = require 'duck'
local A = duck.action

local windows = duck.os == 'windows'

return {
  ------------------------------------------------------------------ visual
  -- A JetBrainsMono Nerd Font vem embutida no Duck. dirs: pastas extras de
  -- fontes, sem instalar. Fonte ausente cai na monoespacada do sistema.
  font = { family = 'JetBrainsMono Nerd Font Mono', size = 11, line_height = 1.2 },
  -- 22 temas embutidos (Ctrl+, -> Galeria de temas). Ou qualquer tema do
  -- Ghostty/WezTerm salvo em ~/.duck/themes/<nome>(.conf|.toml).
  -- Tabela {dark=, light=} segue o modo claro/escuro do Windows.
  theme = 'Catppuccin Mocha',  -- ou { dark = 'Catppuccin Mocha', light = 'Catppuccin Latte' }
  -- colors = { background = '#11111b', working = '#f9e2af', ansi = { ... 16 cores } },
  opacity = 1.0,               -- < 1 deixa ver o fundo
  backdrop = 'none',           -- 'acrylic' | 'mica' | 'none' (Windows 11)
  tab_bar = 'top',             -- 'top' | 'bottom' | 'left' (abas verticais) | 'hidden'
  -- Visual da janela (tudo tambem em Ctrl+, -> Configuracoes).
  ui = {
    titlebar = 'custom',       -- 'custom' (abas + botoes na barra) | 'native' (a do Windows)
    cards = true,              -- panes como cartoes: cantos, sombra e respiro
    radius = 8,                -- raio dos cantos
    glow = true,               -- brilho no pane ativo, na cor da aba
    pane_header = 'multi',     -- cabecalho com pasta/branch/comando: 'multi' | 'always' | 'off'
    animations = true,         -- panes deslizam/crescem, troca de workspace desliza
    cursor_smooth = true,      -- cursor desliza ate a celula nova
    cursor_blink = false,      -- piscar suave (custa redesenho constante)
    autocomplete = 'auto',     -- popup: historico, caminhos, git, npm ('off' desliga)
    syntax = 'auto',           -- realce do comando digitado fora do pwsh ('always' | 'off')
    link_preview = true,       -- Ctrl+clique em arquivo mostra a previa antes do editor
    font = 'Segoe UI Variable Text', -- fonte da interface (abas, menus, cartoes)
    font_weight = 400,         -- peso da fonte do terminal (100-900)
    font_features = {},        -- { '-calt', '-liga' } desliga ligaduras; { 'ss01' } variante
    collapse_lines = 300,      -- saida maior que isso nasce recolhida (0 = nunca)
    token_price = 0,           -- US$ por milhao de tokens: estimativa de custo no painel do agente
    density = 'comfortable',   -- 'compact' (barras e respiros menores; Ctrl+Alt+K alterna)
    sidebar_width = 230,
    -- agent_colors = { trabalho = '#f38ba8', pessoal = '#a6e3a1' },  -- cor do avatar por perfil (~/.claude-<perfil>)
  },
  padding = 6,
  -- background = { image = 'C:/fotos/fundo.jpg', opacity = 0.2 },     -- ou
  -- background = { gradient = { '#1e1e2e', '#2a1f3d' } },
  -- shader = 'C:/caminho/crt.wgsl',  -- WGSL com fn effect(uv, time, size) -> vec4 (ver README)
  visual_bell = true,          -- o pane pisca de leve no lugar do beep
  max_fps = 0,                 -- 0 = vsync; 30 economiza bateria
  gpu_backend = 'dx12',        -- 'dx12' | 'vulkan' | 'gl'
  -- Quick terminal (duck --quick): tecla global desce o terminal no topo.
  quick_terminal = { key = "CTRL+'", height = 0.45 },

  ------------------------------------------------------------------ shells
  -- Sem shell: pwsh no Windows, $SHELL no Linux/macOS.
  shell = windows and { 'pwsh.exe', '-NoLogo' } or nil,
  cwd = duck.home,
  scrollback = 10000,
  scrollback_mb = 64,          -- teto de memoria por pane (alem do de linhas)
  -- pwsh ganha shell integration sozinho (blocos de comando, exit code,
  -- Ctrl+Shift+Up/Down, Ctrl+Shift+O). bash/zsh: veja `duck doctor`.
  shell_integration = true,
  -- Windows: aba nova rele PATH e variaveis do registro (programa instalado com
  -- o Duck aberto ja aparece, sem reiniciar o server). false = env do server.
  refresh_env = true,
  -- Procura versao nova uma vez por dia e avisa (Ctrl+Shift+P > Atualizar o Duck).
  update_check = true,
  notify_command_secs = 30,    -- avisa quando um comando longo termina fora de foco
  -- Dominio = comando que abre o shell. Aparece na paleta ("Novo pane: ...").
  -- Perfil visual: {cmd=..., background='#2b1111', tab_color='#f38ba8'}.
  domains = {
    -- Ubuntu = { 'wsl.exe', '-d', 'Ubuntu', '--cd', '~' },   -- uma distro do WSL
    -- cmd    = { 'cmd.exe' },
    -- SSH com tmux no servidor: a sessao sobrevive a queda da rede e o pane
    -- reconecta sozinho no mesmo lugar.
    -- servidor = { ssh = 'usuario@host', session = 'duck', tab_color = '#f38ba8' },
  },
  -- Cor da aba por regra (casa titulo, pasta ou dominio).
  tab_rules = {
    -- { match = 'prod|producao', color = '#f38ba8' },
  },

  ------------------------------------------------------------------ seguranca
  clipboard_write = 'ask',     -- OSC 52 (app escreve no clipboard): 'ask' | 'allow' | 'deny'
  paste_confirm = true,        -- confirma colar varias linhas ou comando perigoso
  api_scope = 'all',           -- o que um pane pode fazer pela API: 'all' | 'children' | 'read'
  confirm_agent_actions = true,-- agente fechando pane alheio / matando o server pede OK
  audit = true,                -- %LOCALAPPDATA%\duck\audit.log com o que agentes/scripts mandaram
  -- secret_patterns = { ... },-- regex que viram *** no autosave (padrao: tokens, chaves, senha=)
  secret_mask = true,          -- token na tela (ghp_, sk-, AKIA, JWT, PAT do Azure...) vira bolinhas e gera aviso
  -- Guarda de commit/push (duck guard install no repo): barra segredo e estas palavras
  -- (nomes de cliente, de projeto...). Fica aqui, fora do repositorio.
  -- guard = { words = { 'cliente-x', 'projeto-secreto' } },

  ------------------------------------------------------------------ agentes
  -- Processos/titulos reconhecidos como agente (estado working/blocked/idle).
  agents = { 'claude', 'codex', 'cursor-agent', 'opencode', 'gemini', 'aider', 'copilot', 'grok', 'kiro-cli' },
  notify = true,               -- toast do Windows quando um agente para fora de foco
  token_budget = 0,            -- avisa quando os agentes passarem deste total de tokens (0 desliga)
  -- Perfis de agente (Ctrl+Shift+N; tambem usados por "corrigir com agente" e worktree).
  agent_profiles = {
    { name = 'Claude', cmd = 'claude' },
    { name = 'Claude (modo plano)', cmd = 'claude --permission-mode plan' },
    { name = 'Claude (aceita edicoes)', cmd = 'claude --permission-mode acceptEdits' },
    { name = 'Codex', cmd = 'codex' },
  },
  -- Popups: comando num flutuante que some quando o comando sai (paleta "Popup: ...").
  popups = {
    { name = 'lazygit', cmd = 'lazygit' },
    { name = 'git log', cmd = 'git -c color.ui=always log --oneline --graph -40' },
    { name = 'processos', cmd = 'btop' },
  },
  -- Ctrl+clique em arquivo:linha:coluna (erros, stack traces) abre aqui.
  editor = 'code -g {file}:{line}:{col}',
  -- Sugestao do historico enquanto digita: 'auto' (bash/zsh/cmd; o pwsh ja
  -- tem a do PSReadLine) | 'always' | 'off'. Seta para a direita aceita.
  inline_suggestions = 'auto',

  -- Comando externo por evento. Variaveis: DUCK_EVENT, DUCK_PANE, DUCK_STATE,
  -- DUCK_TITLE, DUCK_CWD. Eventos: agent-state-changed, attention,
  -- pane-created, pane-closed, bell, command-finished, approval.
  hooks = {
    -- ['attention'] = 'pwsh -NoProfile -File C:/scripts/avisar.ps1',
  },
  autosave_minutes = 5,        -- salva todos os workspaces (0 desliga)
  -- Opcoes extras ao retomar as sessoes do Claude no restore (vazio = so --resume).
  resume_args = '',

  hyperlinks = {
    -- #123 vira link para a issue do seu repositorio:
    -- { regex = [[\B#(\d+)\b]], format = 'https://github.com/OWNER/REPO/issues/$1' },
  },

  ------------------------------------------------------------------ atalhos
  -- mods: CTRL, SHIFT, ALT, SUPER combinados com '|'. action: duck.action(...)
  -- ou uma funcao(win) que usa win:run(acao, ...) / win:cli{...} / win:toast(...).
  keys = {
    { key = 'p',  mods = 'CTRL|SHIFT', action = A('palette') },
    { key = '\\', mods = 'ALT',        action = A('split', 'right') },
    { key = '-',  mods = 'ALT',        action = A('split', 'down') },
    { key = 'w',  mods = 'ALT',        action = A('close_pane') },
    { key = 'w',  mods = 'CTRL|SHIFT', action = A('close_tab') },
    { key = 'z',  mods = 'ALT',        action = A('zoom') },
    { key = 'r',  mods = 'ALT',        action = A('rename_tab') },
    { key = 'p',  mods = 'ALT',        action = A('key_table', 'resize') },
    { key = 'a',  mods = 'ALT',        action = A('agents') },
    { key = 't',  mods = 'CTRL|SHIFT', action = A('new_tab') },
    { key = 'Tab', mods = 'CTRL',      action = A('tab_relative', 1) },
    { key = 'Tab', mods = 'CTRL|SHIFT', action = A('tab_relative', -1) },
    { key = 's',  mods = 'CTRL|SHIFT', action = A('workspace_switch') },
    { key = 'm',  mods = 'CTRL|ALT',   action = A('move_tab_ws') },
    { key = 'n',  mods = 'CTRL|ALT',   action = A('workspace_new') },
    { key = 's',  mods = 'CTRL|ALT',   action = A('save_state') },
    { key = 'r',  mods = 'CTRL|ALT',   action = A('restore_state') },
    { key = 'd',  mods = 'CTRL|ALT',   action = A('delete_state') },
    { key = 'c',  mods = 'CTRL|SHIFT', action = A('copy') },
    { key = 'v',  mods = 'CTRL|SHIFT', action = A('paste') },
    { key = 'f',  mods = 'CTRL|SHIFT', action = A('search') },
    { key = 'x',  mods = 'CTRL|SHIFT', action = A('copy_mode') },
    { key = 'ArrowLeft',  mods = 'ALT', action = A('focus', 'left') },
    { key = 'ArrowRight', mods = 'ALT', action = A('focus', 'right') },
    { key = 'ArrowUp',    mods = 'ALT', action = A('focus', 'up') },
    { key = 'ArrowDown',  mods = 'ALT', action = A('focus', 'down') },
    { key = 'PageUp',   mods = 'SHIFT', action = A('scroll', 'page_up') },
    { key = 'PageDown', mods = 'SHIFT', action = A('scroll', 'page_down') },
    { key = '=', mods = 'CTRL', action = A('font_size', 1) },
    { key = '-', mods = 'CTRL', action = A('font_size', -1) },
    { key = '0', mods = 'CTRL', action = A('font_size', 0) },
  },

  -- Modos (o "modo redimensionar" do WezTerm). Tecla fora da tabela sai.
  key_tables = {
    resize = {
      { key = 'h', action = A('adjust', 'left', 3) },  { key = 'ArrowLeft',  action = A('adjust', 'left', 3) },
      { key = 'l', action = A('adjust', 'right', 3) }, { key = 'ArrowRight', action = A('adjust', 'right', 3) },
      { key = 'k', action = A('adjust', 'up', 2) },    { key = 'ArrowUp',    action = A('adjust', 'up', 2) },
      { key = 'j', action = A('adjust', 'down', 2) },  { key = 'ArrowDown',  action = A('adjust', 'down', 2) },
      { key = 'Escape', action = A('pop_table') },     { key = 'Enter', action = A('pop_table') },
    },
  },

  ------------------------------------------------------------------ layouts
  -- Abre um workspace pronto pela paleta ("Layout: dev").
  layouts = {
    -- dev = { cwd = '~/projetos/app', tabs = {
    --   { title = 'codigo', layout = { split = 'right', ratio = 0.6,
    --       a = { cmd = 'nvim .' }, b = { split = 'down', a = { cmd = 'claude' }, b = {} } } },
    --   { title = 'logs', color = 'working', layout = { cmd = 'docker compose logs -f' } },
    -- } },
  },

  ------------------------------------------------------------------ workflows
  -- Comandos com parametros na paleta ("Workflow: ..."); {{nome:padrao}}.
  workflows = {
    { name = 'Git: nova branch', command = 'git switch -c {{branch}}' },
    { name = 'Logs de um container', command = 'docker logs -f --tail {{linhas:200}} {{container}}' },
  },

  ------------------------------------------------------------------ paleta
  -- Entradas extras da paleta (Ctrl+Shift+P). As acoes nativas ja estao la.
  palette = {
    -- Exemplo de entrada com escolha:
    -- { label = 'Abrir logs', action = function(win)
    --     win:run('choose', { title = 'Qual log?', choices = {
    --       { label = 'app', action = A('popup', 'tail -f app.log') } } })
    --   end },
    { label = 'Checkpoint: salvar sessoes (todas)', action = A('checkpoint') },
    { label = 'Checkpoint: restaurar sessoes', action = A('checkpoint_restore') },
  },

  ------------------------------------------------------------------ atalhos e menus
  -- Barra de atalhos (embaixo, abaixo dos badges): rotulo + acao; a tecla
  -- mostrada e a que estiver ligada a acao em `keys`. ui = { shortcuts = false } esconde.
  -- shortcuts = {
  --   { label = 'Paleta',       action = A('palette') },
  --   { label = 'Orquestração', action = A('menu', 'orquestracao') },
  --   { label = 'Extensões',    action = A('menu', 'extensoes') },
  --   { label = 'Janela',       action = A('menu', 'janela') },
  --   { label = 'Configuração', action = A('menu', 'config') },
  --   { label = 'Novo agente',  action = A('agent_new') },
  --   { label = 'Avisos',       action = A('inbox') },
  --   { label = 'Todos os atalhos', action = A('menu', 'atalhos') },
  -- },
  -- Menus por assunto (Ctrl+Shift+1..4): a paleta filtrada pelos prefixos do
  -- rotulo (ou da dica) dos comandos, sem acento nem caixa. Mude ou crie:
  -- menus = {
  --   deploy = { title = 'Deploy', match = { 'Azure', 'Git:' } },
  -- },

  ------------------------------------------------------------------ extensoes
  -- Ligue e configure em Ctrl+Shift+2 (ou Ctrl+Shift+P > Extensoes), que grava
  -- em ~/.duck-local.lua, ou aqui. Tokens NUNCA vao neste arquivo:
  --   echo <token> | duck secret set <extensao>/<conta>
  -- `duck doctor` mostra as ligadas que estao sem compilar ou sem token.
  -- Passo a passo de cada uma: docs/PLAYBOOK.pt-BR.md
  -- extensions = {
  --   github = { enabled = true },          -- contas: as do `gh auth login`
  --   git = { enabled = true },             -- repo do pane ativo
  --   azure = { enabled = true, orgs = {    -- token: azure/empresa (PAT: Code, Build e Release Read)
  --     { name = 'empresa', url = 'https://dev.azure.com/empresa', project = 'proj',
  --       releases = {
  --         { auto = true, branches = { 'develop', 'tst', 'master' } },     -- todas com deploy recente (path, match, days)
  --         { name = 'API', definition = 'api-cd', stages = { 'develop=Dev', 'tst=Tst', 'master=Prd|Prod' } },
  --         { name = 'Portal', pipeline = 'portal-ci', branches = { 'develop', 'master' } },
  --       } },
  --   } },
  --   jira = { enabled = true, accounts = {  -- token: jira/trabalho
  --     { name = 'trabalho', url = 'https://jira.empresa.com' },                                  -- Server/DC: PAT
  --     -- { name = 'cloud', url = 'https://x.atlassian.net', type = 'cloud', email = 'eu@x.com' }, -- Cloud: API token
  --   } },
  --   grafana = { enabled = true, accounts = {  -- token: grafana/empresa = 'usuario:senha'
  --     { name = 'empresa', url = 'https://grafana.empresa.com', datasource_uid = 'uid-do-loki' },
  --   }, queries = { 'Erros API={service_name="api"} |= "ERROR"' } },
  --   rollbar = { enabled = true, accounts = { { name = 'app' } } },   -- token: rollbar/app
  --   calendar = { enabled = true, accounts = { { name = 'trabalho' } } }, -- token: calendar/trabalho = URL .ics secreta
  --              -- join_minutes = 10: perto da reuniao, o clique no rodape entra direto
  --   homeassistant = { enabled = true, accounts = { { name = 'casa', url = 'http://homeassistant.local:8123' } } },
  --   containers = { enabled = true, engines = { 'docker' } },   -- ou o nome da distro do WSL com podman
  --   database = { enabled = true, accounts = { { name = 'local', kind = 'postgres', database = 'app', user = 'postgres' } } },
  --   ports = { enabled = true },
  --   notes = { enabled = true },
  --   snippets = { enabled = true },
  --   ['claude-usage'] = { enabled = true },
  --   rotinas = { enabled = true },          -- tarefas em ~/.duck/rotinas/*.md
  -- },

  ------------------------------------------------------------------ barra
  -- Desenha o lado direito da barra. info: workspace, cpu, ram, ram_total,
  -- time, agents = {working=, blocked=, done=, idle=}, title, cwd, mode.
  status = function(info)
    local out = {}
    if info.mode then table.insert(out, { text = ' ' .. info.mode:upper() .. ' ', fg = 'accent' }) end
    -- Agentes e CPU/RAM viraram badges (Ctrl+Shift+P > Extensoes > Agentes/Sistema).
    -- Portas abertas: extensao Portas (Ctrl+Shift+P > Extensoes).
    return out
  end,

  -- Titulo de cada aba. tab: index, title, pane_title, cwd, state, attention,
  -- active, domain, branch (git), todo ("3/7" do plano do agente).
  tab_title = function(tab)
    if tab.title ~= '' then return tab.title end
    local dir = tab.cwd:match('([^/\\]+)[/\\]?$') or tab.cwd
    local name = dir ~= '' and dir or tab.pane_title
    if tab.branch ~= '' and tab.branch ~= 'main' and tab.branch ~= 'master' then
      name = name .. ' ⎇ ' .. tab.branch
    end
    return name
  end,
}
