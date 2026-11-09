<#
  Instala ou atualiza o Duck a partir do ultimo release, sem compilar nada.
  Installs or updates Duck from the latest release, nothing to build.

    irm https://raw.githubusercontent.com/herlondf/duck/main/get.ps1 | iex
    & ([scriptblock]::Create((irm .../get.ps1))) -Boot -ClaudeHooks    (opcoes do install.ps1)
    $env:DUCK_VERSION = 'v0.1.0'; irm .../get.ps1 | iex               (uma versao especifica)

  Fica em %LOCALAPPDATA%\Duck (binarios em %LOCALAPPDATA%\Duck\bin, no PATH do usuario).
  Roda no Windows PowerShell 5.1 ou no PowerShell 7; o instalador precisa do 7 (pwsh).
#>
param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $Rest)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = if ($env:DUCK_REPO) { $env:DUCK_REPO } else { 'herlondf/duck' }
$base = if ($env:DUCK_VERSION) { "https://github.com/$repo/releases/download/$($env:DUCK_VERSION)" } else { "https://github.com/$repo/releases/latest/download" }
$name = 'duck-windows-x86_64.zip'

$pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
if (-not $pwsh) { throw 'PowerShell 7 nao encontrado / not found: winget install Microsoft.PowerShell (depois rode de novo / then run again)' }

$tmp = Join-Path $env:TEMP "duck-get-$PID"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
  Write-Host "Baixando / downloading $base/$name"
  Invoke-WebRequest "$base/$name" -OutFile "$tmp\$name" -UseBasicParsing
  Invoke-WebRequest "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS" -UseBasicParsing
  $line = Get-Content "$tmp\SHA256SUMS" | Where-Object { $_ -match "\s\*?$([regex]::Escape($name))$" } | Select-Object -First 1
  $want = if ($line) { ($line -split '\s+')[0] } else { '' }
  $got = (Get-FileHash "$tmp\$name" -Algorithm SHA256).Hash
  if (-not $want -or $want -ne $got) { throw "Checksum nao confere / checksum mismatch: $name" }

  $dest = Join-Path $env:LOCALAPPDATA 'Duck'
  # Extensoes antigas saem (arquivos apagados numa versao nova nao ficam para tras).
  if (Test-Path "$dest\extensions") { Remove-Item "$dest\extensions" -Recurse -Force -ErrorAction SilentlyContinue }
  Expand-Archive "$tmp\$name" -DestinationPath $dest -Force
  & $pwsh.Source -NoProfile -File "$dest\install.ps1" @Rest
  if ($LASTEXITCODE) { throw "install.ps1 falhou / failed ($LASTEXITCODE)" }
} finally {
  Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
