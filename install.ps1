<#
.SYNOPSIS
  Builds and installs Duck for the current user (Windows).
  Compila e instala o Duck para o usuario atual (Windows).
.DESCRIPTION
  Without parameters: builds (release), copies the binaries to .\bin, adds .\bin
  to the user PATH, creates ~/.duck.lua (if missing) and a Start menu shortcut.
  Nothing outside the user profile is changed.
  Sem parametros: compila, copia os binarios para .\bin, poe .\bin no PATH do
  usuario, cria ~/.duck.lua (se nao existir) e um atalho no menu Iniciar.

  First install (no -Boot/-ClaudeHooks/-Extensions/-All): a checklist of what to
  set up, with sensible defaults checked; numbers toggle, Enter confirms. Without
  a console to ask (or with -Yes) the defaults apply. At the end: duck doctor.
  Primeira instalacao: um checklist do que configurar; numeros ligam/desligam,
  Enter confirma. Sem console (ou com -Yes) vale o padrao.

  -Boot          open Duck at logon restoring the last checkpoint / abre no logon restaurando o checkpoint
  -ClaudeHooks   register `duck hook` and the Duck statusline in ~/.claude*/settings.json (backup in settings.json.bak-duck)
  -SkipBuild     reuse target\release / usa o binario ja compilado
  -Extensions    build the bundled extensions (Node 18+); enable them in Ctrl+Shift+P / compila as extensoes; ative no Ctrl+Shift+P
  -All           same as -Boot -ClaudeHooks -Extensions / tudo acima
  -Yes           no questions: the checklist defaults / sem perguntas: o padrao do checklist
  -Wizard        show the checklist again (not only on the first install) / mostra o checklist de novo
  -NoWizard      never show it ($env:DUCK_NO_WIZARD = 1 does the same) / nunca mostra
  From a release package (duck-windows-x86_64.zip) the binaries are already built:
  nothing is compiled and the bundled extensions are used as they are.
  Num pacote de release os binarios ja vem prontos: nada e compilado.
.EXAMPLE
  pwsh -File .\install.ps1 -All
#>
param([switch] $Boot, [switch] $ClaudeHooks, [switch] $SkipBuild, [switch] $Extensions, [switch] $All,
      [switch] $Yes, [switch] $Wizard, [switch] $NoWizard)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$pt = (Get-UICulture).Name -like 'pt*'
function Say([string] $ptText, [string] $enText, [string] $color = 'Green') { Write-Host ($pt ? $ptText : $enText) -ForegroundColor $color }
function T([string] $ptText, [string] $enText) { $pt ? $ptText : $enText }
$explicit = $Boot -or $ClaudeHooks -or $Extensions -or $All
if ($All) { $Boot = $true; $ClaudeHooks = $true; $Extensions = $true }

# Pacote de release: binarios prontos ao lado deste script, sem Cargo.toml.
$prebuilt = (Test-Path "$root\duck.exe") -and -not (Test-Path "$root\Cargo.toml")
$src = $prebuilt ? $root : "$root\target\release"
$buildExts = $Extensions -and -not $prebuilt
if ($prebuilt) { $SkipBuild = $true }

$data = Join-Path $env:LOCALAPPDATA 'duck'
$localLua = Join-Path $env:USERPROFILE '.duck-local.lua'
function Home-Rel([string] $p) { $p.StartsWith($env:USERPROFILE) ? ('~' + $p.Substring($env:USERPROFILE.Length)) : $p }
function Set-To-False([string] $key) { (Test-Path $localLua) -and (Select-String -Path $localLua -Pattern "^config\.$key = false" -Quiet) }
function Was-Set([string] $key) { (Test-Path $localLua) -and (Select-String -Path $localLua -Pattern "^config\.$key = " -Quiet) }

# Perfis do Claude Code: ~/.claude e ~/.claude-*. Sem nenhum, ~/.claude (o
# `duck install --hooks` cria): vale quando o Claude Code for instalado.
$profiles = @(Get-ChildItem $env:USERPROFILE -Directory -Force -ErrorAction SilentlyContinue |
  Where-Object { $_.Name -eq '.claude' -or $_.Name -like '.claude-*' } | Sort-Object Name | ForEach-Object { $_.FullName })
$noProfile = $profiles.Count -eq 0
if ($noProfile) { $profiles = @(Join-Path $env:USERPROFILE '.claude') }

# O que configurar. Sem o assistente: o de sempre (atalho, ~/.duck.lua) mais as opcoes.
$Status = $ClaudeHooks; $Init = $true; $Menu = $true; $ShellInt = $true; $AutoUpd = $true; $ExtOn = $false
$updKey = $prebuilt ? 'update_auto' : 'update_check'

# Assistente da primeira instalacao. As opcoes (-All, -Boot...) pulam;
# reinstalar ou atualizar so mostra com -Wizard (-Yes: aplica o padrao).
$first = -not (Test-Path (Join-Path $data 'install-kind'))
$runWizard = (-not $explicit) -and (-not $NoWizard) -and (-not $env:DUCK_NO_WIZARD) -and ($Wizard -or $Yes -or $first)
if ($runWizard) {
  $plist = ($profiles | ForEach-Object { Home-Rel $_ }) -join ' '
  if ($noProfile) { $plist += (T ' (nenhum perfil ainda: cria)' ' (no profile yet: creates it)') }
  $node = (Get-Command node -ErrorAction SilentlyContinue) ? '' : (T ', nao encontrado' ', not found')
  $luaNote = (Test-Path (Join-Path $env:USERPROFILE '.duck.lua')) ? (T ' (ja existe: fica como esta)' ' (exists: kept as is)') : ''
  if ($prebuilt) {
    $updLabel = T 'atualizacoes automaticas (baixa, confere e instala; reinicia o server quando nenhum agente trabalha)' 'automatic updates (download, verify, install; restart the server when no agent is working)'
    $extLabel = T "extensoes: ligar o Uso do Claude (precisa do Node 18+$node; as outras em Ctrl+Shift+P)" "extensions: enable Claude usage (needs Node 18+$node; the rest in Ctrl+Shift+P)"
  } else {
    $updLabel = T 'aviso de versao nova (pelo codigo nao se atualiza sozinho)' 'new version notice (a source install does not update itself)'
    $extLabel = T "extensoes: compilar e ligar o Uso do Claude (precisa do Node 18+$node)" "extensions: build them and enable Claude usage (needs Node 18+$node)"
  }
  $labels = @(
    (T "hooks do Claude Code em: $plist" "Claude Code hooks in: $plist"),
    (T 'statusline do Claude Code (uso de 5h/7d para o Uso do Claude e as Rotinas; a sua fica)' 'Claude Code statusline (5h/7d usage for Claude usage and Routines; yours is kept)'),
    (T 'shell integration do pwsh (blocos de comando, exit code; o Duck carrega sozinho)' 'pwsh shell integration (command blocks, exit codes; Duck loads it by itself)'),
    "~/.duck.lua$luaNote",
    (T 'atalho no menu Iniciar' 'Start menu shortcut'),
    (T 'abrir no logon, restaurando a ultima sessao' 'start on logon, restoring the last session'),
    $updLabel,
    $extLabel
  )
  $on = @($true, $true, $true, $true, $true, $true, $true, $false)
  # Desligado antes (-Wizard de novo): comeca desmarcado.
  if (Set-To-False $updKey) { $on[6] = $false }
  if (Set-To-False 'shell_integration') { $on[2] = $false }
  $interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
  if ($Yes -or -not $interactive) {
    Say 'Sem console para perguntar (ou -Yes): usando o padrao.' 'No console to ask (or -Yes): using the defaults.' Yellow
  } else {
    while ($true) {
      Write-Host ''
      Say 'Duck: o que configurar?' 'Duck: what to set up?' Cyan
      for ($i = 0; $i -lt $labels.Count; $i++) {
        $mark = $on[$i] ? 'x' : ' '
        Write-Host ('  ' + ($i + 1) + ' [' + $mark + '] ' + $labels[$i])
      }
      $line = Read-Host (T 'Numeros ligam/desligam (ex.: 6 8), Enter confirma' 'Numbers toggle (e.g. 6 8), Enter confirms')
      if ([string]::IsNullOrWhiteSpace($line)) { break }
      foreach ($tok in ($line -split '[\s,;]+')) {
        $n = 0
        if ([int]::TryParse($tok, [ref] $n) -and $n -ge 1 -and $n -le $labels.Count) { $on[$n - 1] = -not $on[$n - 1] }
      }
    }
  }
  $ClaudeHooks = $on[0]; $Status = $on[1]; $ShellInt = $on[2]; $Init = $on[3]; $Menu = $on[4]; $Boot = $on[5]; $AutoUpd = $on[6]; $ExtOn = $on[7]
  if (-not $prebuilt) { $buildExts = $ExtOn }
}
$summary = [System.Collections.Generic.List[string]]::new()

if (-not $SkipBuild) {
  $cargo = (Get-Command cargo -ErrorAction SilentlyContinue)?.Source ?? "$env:USERPROFILE\.cargo\bin\cargo.exe"
  if (-not (Test-Path $cargo)) { throw ($pt ? 'Rust nao encontrado: winget install Rustlang.Rustup' : 'Rust not found: winget install Rustlang.Rustup') }
  # O Rust no Windows usa o linker do Visual Studio (MSVC Build Tools).
  $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
  $vc = (Test-Path $vswhere) -and (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
  if (-not $vc) {
    $cmd = 'winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
    throw ($pt ? "MSVC Build Tools nao encontrado (o Rust precisa do linker). Instale com:`n  $cmd" : "MSVC Build Tools not found (Rust needs its linker). Install with:`n  $cmd")
  }
  Say 'Compilando (release, ~5 min na primeira vez)...' 'Building (release, ~5 min the first time)...' Cyan
  & $cargo build --release --manifest-path "$root\Cargo.toml"
  if ($LASTEXITCODE) { throw 'cargo build failed' }
}

# Um exe em uso nao pode ser sobrescrito, mas pode ser renomeado: os shells
# abertos (ptyhosts) continuam vivos durante a atualizacao.
$bin = Join-Path $root 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
$stamp = Get-Date -Format 'yyyyMMddHHmmss'
foreach ($n in 'duck', 'duckw') {
  if (Test-Path "$bin\$n.exe") { Rename-Item "$bin\$n.exe" "$n.old-$stamp.exe" }
  Copy-Item "$src\$n.exe" "$bin\$n.exe"
}
Get-ChildItem $bin -Filter '*.old-*.exe' | ForEach-Object { try { Remove-Item $_.FullName -ErrorAction Stop } catch {} }
Copy-Item "$root\assets\duck.ico" "$bin\duck.ico" -Force
$exe = "$bin\duck.exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $bin) {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$bin", 'User')
  Say "PATH do usuario: + $bin (abra um terminal novo)" "User PATH: + $bin (open a new terminal)"
}
$summary.Add((T "PATH do usuario: $bin" "user PATH: $bin"))

if ($Init) { & $exe init; $summary.Add('~/.duck.lua') }

# Tipo da instalacao para o `duck update`: so a do release se atualiza sozinha.
New-Item -ItemType Directory -Force $data | Out-Null
Set-Content (Join-Path $data 'install-kind') ($prebuilt ? 'release' : "source $root") -Encoding utf8NoBOM

# Opcoes do assistente que viram config (~/.duck-local.lua): desmarcado grava
# false; marcado so desfaz um false gravado antes (o padrao e ligado).
if ($runWizard) {
  foreach ($kv in @(@($updKey, $AutoUpd), @('shell_integration', $ShellInt))) {
    if (-not $kv[1]) { & $exe config set $kv[0] false | Out-Null }
    elseif (Was-Set $kv[0]) { & $exe config set $kv[0] true | Out-Null }
  }
  if ($ShellInt) { $summary.Add((T 'shell integration do pwsh' 'pwsh shell integration')) }
  if ($prebuilt) { $summary.Add(($AutoUpd ? (T 'atualizacoes automaticas' 'automatic updates') : (T 'atualizacoes: so avisa (update_auto = false)' 'updates: notify only (update_auto = false)'))) }
  elseif ($AutoUpd) { $summary.Add((T 'aviso de versao nova' 'new version notice')) }
}

if ($Menu) {
  $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Duck.lnk'
  $sh = New-Object -ComObject WScript.Shell
  $s = $sh.CreateShortcut($lnk); $s.TargetPath = "$bin\duckw.exe"; $s.WorkingDirectory = $env:USERPROFILE
  $s.IconLocation = "$bin\duck.ico,0"; $s.Description = 'Duck'; $s.Save()
  Say "Atalho: $lnk" "Shortcut: $lnk"
  $summary.Add((T 'atalho no menu Iniciar' 'Start menu shortcut'))
}

if ($Boot) { & $exe install; $summary.Add((T 'abre no logon' 'starts on logon')) }

Copy-Item "$root\extensions\claude-usage\statusline.ps1" "$bin\duck-statusline.ps1" -Force

# Todo perfil, mesmo sem settings.json ainda (perfil novo): o duck cria o arquivo.
if ($ClaudeHooks) {
  foreach ($p in $profiles) { & $exe install --hooks (Join-Path $p 'settings.json') }
  $summary.Add((T 'hooks do Claude Code: ' 'Claude Code hooks: ') + (($profiles | ForEach-Object { Home-Rel $_ }) -join ' '))
}
# Statusline do Duck (uso de 5h/7d para a extensao Uso do Claude e as
# Rotinas) so onde ainda nao ha nenhuma: a sua, se existir, fica.
if ($Status) {
  foreach ($p in $profiles) { & $exe install --statusline (Join-Path $p 'settings.json') }
  $summary.Add((T 'statusline do Claude Code' 'Claude Code statusline'))
}

if ($buildExts -and -not (Get-Command npm -ErrorAction SilentlyContinue)) {
  if ($runWizard) {
    Say 'Node.js nao encontrado: extensoes nao compiladas (winget install OpenJS.NodeJS.LTS; depois install.ps1 -SkipBuild -Extensions)' 'Node.js not found: extensions not built (winget install OpenJS.NodeJS.LTS; then install.ps1 -SkipBuild -Extensions)' Yellow
    $buildExts = $false; $ExtOn = $false
  } else {
    throw ($pt ? 'Node.js nao encontrado: winget install OpenJS.NodeJS.LTS' : 'Node.js not found: winget install OpenJS.NodeJS.LTS')
  }
}
if ($buildExts) {
  foreach ($e in Get-ChildItem "$root\extensions" -Directory | Where-Object { Test-Path "$($_.FullName)\duck-extension.json" }) {
    Push-Location $e.FullName
    npm install --no-audit --no-fund | Out-Null
    npm run build | Out-Null
    $failed = $LASTEXITCODE
    Pop-Location
    if ($failed) { throw "extension $($e.Name): build failed" }
    Say "Extensao compilada: $($e.Name)" "Extension built: $($e.Name)"
  }
  # Ficam na instalacao (para todos os usuarios); cada um liga as suas no menu.
  Say 'Ative as extensoes no Duck: Ctrl+Shift+P > Extensoes.' 'Enable extensions in Duck: Ctrl+Shift+P > Extensions.' Yellow
  $summary.Add((T 'extensoes compiladas' 'extensions built'))
}
# Assistente: liga o Uso do Claude (le o que a statusline grava).
if ($ExtOn) {
  & $exe config set extensions.claude-usage.enabled true | Out-Null
  $summary.Add((T 'extensao Uso do Claude ligada' 'Claude usage extension enabled'))
}

# Servidor ja rodando (atualizacao): reinicia sem fechar os shells.
if (Get-Process duckw -ErrorAction SilentlyContinue) {
  # De dentro de um pane do Duck o token do pane nao pode reiniciar o server.
  Remove-Item Env:DUCK_TOKEN, Env:DUCK_PANE, Env:LASSO_TOKEN, Env:LASSO_PANE -ErrorAction SilentlyContinue
  & $exe cli restart-server | Out-Null
  Say 'Server reiniciado; feche e abra a janela do Duck para usar a versao nova.' 'Server restarted; close and reopen the Duck window to use the new version.' Yellow
}

Write-Host ''
Say 'Configurado:' 'Configured:' Cyan
foreach ($l in $summary) { Write-Host "  - $l" }
if (-not $env:DUCK_NO_WIZARD) {
  Write-Host ''
  & $exe doctor
}

Say "`nPronto. Abra pelo menu Iniciar (Duck) ou rode: duck" "`nDone. Open Duck from the Start menu or run: duck"
Say 'Confira de novo quando quiser: duck doctor  (config, server, hooks, extensoes e tokens que faltam)' 'Check again any time: duck doctor  (config, server, hooks, extensions and missing tokens)' Cyan
Say 'Passo a passo das extensoes e rotinas: docs/PLAYBOOK.pt-BR.md' 'Step by step for extensions and routines: docs/PLAYBOOK.md' Cyan
exit 0
