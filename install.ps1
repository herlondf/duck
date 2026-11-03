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
.EXAMPLE
  pwsh -File .\install.ps1 -Boot -ClaudeHooks
#>
param([switch] $Boot, [switch] $ClaudeHooks, [switch] $SkipBuild)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$pt = (Get-UICulture).Name -like 'pt*'
function Say([string] $ptText, [string] $enText, [string] $color = 'Green') { Write-Host ($pt ? $ptText : $enText) -ForegroundColor $color }

if (-not $SkipBuild) {
  $cargo = (Get-Command cargo -ErrorAction SilentlyContinue)?.Source ?? "$env:USERPROFILE\.cargo\bin\cargo.exe"
  if (-not (Test-Path $cargo)) { throw ($pt ? 'Rust nao encontrado: winget install Rustlang.Rustup' : 'Rust not found: winget install Rustlang.Rustup') }
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

if ($ClaudeHooks) {
  Get-ChildItem $env:USERPROFILE -Directory -Force -Filter '.claude*' |
    Where-Object { Test-Path (Join-Path $_.FullName 'settings.json') } |
    ForEach-Object { & $exe install --hooks (Join-Path $_.FullName 'settings.json') }
}

# Servidor ja rodando (atualizacao): reinicia sem fechar os shells.
if (Get-Process duckw -ErrorAction SilentlyContinue) {
  # De dentro de um pane do Duck o token do pane nao pode reiniciar o server.
  Remove-Item Env:DUCK_TOKEN, Env:DUCK_PANE, Env:LASSO_TOKEN, Env:LASSO_PANE -ErrorAction SilentlyContinue
  & $exe cli restart-server | Out-Null
  Say 'Server reiniciado; feche e abra a janela do Duck para usar a versao nova.' 'Server restarted; close and reopen the Duck window to use the new version.' Yellow
}

Say "`nPronto. Abra pelo menu Iniciar (Duck) ou rode: duck" "`nDone. Open Duck from the Start menu or run: duck"
