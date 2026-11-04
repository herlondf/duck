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

  -Boot          open Duck at logon restoring the last checkpoint / abre no logon restaurando o checkpoint
  -ClaudeHooks   register `duck hook` in ~/.claude*/settings.json (backup in settings.json.bak-duck)
  -SkipBuild     reuse target\release / usa o binario ja compilado
  -Extensions    build the bundled extensions (Node 18+); enable them in Ctrl+Shift+P / compila as extensoes; ative no Ctrl+Shift+P
  -All           same as -Boot -ClaudeHooks -Extensions / tudo acima
.EXAMPLE
  pwsh -File .\install.ps1 -All
#>
param([switch] $Boot, [switch] $ClaudeHooks, [switch] $SkipBuild, [switch] $Extensions, [switch] $All)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$pt = (Get-UICulture).Name -like 'pt*'
function Say([string] $ptText, [string] $enText, [string] $color = 'Green') { Write-Host ($pt ? $ptText : $enText) -ForegroundColor $color }
if ($All) { $Boot = $true; $ClaudeHooks = $true; $Extensions = $true }

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
  Copy-Item "$root\target\release\$n.exe" "$bin\$n.exe"
}
Get-ChildItem $bin -Filter '*.old-*.exe' | ForEach-Object { try { Remove-Item $_.FullName -ErrorAction Stop } catch {} }
Copy-Item "$root\assets\duck.ico" "$bin\duck.ico" -Force
$exe = "$bin\duck.exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $bin) {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$bin", 'User')
  Say "PATH do usuario: + $bin (abra um terminal novo)" "User PATH: + $bin (open a new terminal)"
}

& $exe init

$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Duck.lnk'
$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk); $s.TargetPath = "$bin\duckw.exe"; $s.WorkingDirectory = $env:USERPROFILE
$s.IconLocation = "$bin\duck.ico,0"; $s.Description = 'Duck'; $s.Save()
Say "Atalho: $lnk" "Shortcut: $lnk"

if ($Boot) { & $exe install }

Copy-Item "$root\extensions\claude-usage\statusline.ps1" "$bin\duck-statusline.ps1" -Force

if ($ClaudeHooks) {
  Get-ChildItem $env:USERPROFILE -Directory -Force -Filter '.claude*' |
    Where-Object { Test-Path (Join-Path $_.FullName 'settings.json') } |
    ForEach-Object {
      $settings = Join-Path $_.FullName 'settings.json'
      & $exe install --hooks $settings
      # Statusline do Duck (uso de 5h/7d para a extensao Uso do Claude e as
      # Rotinas) so onde ainda nao ha nenhuma: a sua, se existir, fica.
      $j = Get-Content $settings -Raw | ConvertFrom-Json -AsHashtable
      if (-not $j.ContainsKey('statusLine')) {
        $j['statusLine'] = [ordered]@{ type = 'command'; command = "pwsh -NoProfile -File `"$bin\duck-statusline.ps1`""; padding = 0 }
        $j | ConvertTo-Json -Depth 32 | Set-Content $settings -Encoding utf8NoBOM
        Say "Statusline do Duck em $settings" "Duck statusline in $settings"
      }
    }
}

if ($Extensions) {
  if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { throw ($pt ? 'Node.js nao encontrado: winget install OpenJS.NodeJS.LTS' : 'Node.js not found: winget install OpenJS.NodeJS.LTS') }
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
}

# Servidor ja rodando (atualizacao): reinicia sem fechar os shells.
if (Get-Process duckw -ErrorAction SilentlyContinue) {
  # De dentro de um pane do Duck o token do pane nao pode reiniciar o server.
  Remove-Item Env:DUCK_TOKEN, Env:DUCK_PANE, Env:LASSO_TOKEN, Env:LASSO_PANE -ErrorAction SilentlyContinue
  & $exe cli restart-server | Out-Null
  Say 'Server reiniciado; feche e abra a janela do Duck para usar a versao nova.' 'Server restarted; close and reopen the Duck window to use the new version.' Yellow
}

Say "`nPronto. Abra pelo menu Iniciar (Duck) ou rode: duck" "`nDone. Open Duck from the Start menu or run: duck"
Say 'Confira a instalacao com: duck doctor  (config, server, hooks, extensoes e tokens que faltam)' 'Check the install with: duck doctor  (config, server, hooks, extensions and missing tokens)' Cyan
Say 'Passo a passo das extensoes e rotinas: docs/PLAYBOOK.pt-BR.md' 'Step by step for extensions and routines: docs/PLAYBOOK.md' Cyan
