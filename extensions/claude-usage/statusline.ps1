<#
.SYNOPSIS
  Statusline do Claude Code para o Duck / Claude Code statusline for Duck.
.DESCRIPTION
  O Claude Code manda um JSON no stdin a cada atualizacao da statusline; desde
  a 2.1.80 ele traz `rate_limits` (janelas de 5 horas e 7 dias). Este script
  desenha a linha (modelo, 5h, 7d, custo) e grava um JSON por sessao em
  %USERPROFILE%\AppData\Local\Temp\claude-usage, que a extensao "Uso do Claude"
  e o adiamento das Rotinas (uso_max) leem.

  Claude Code sends a JSON on stdin; since 2.1.80 it has `rate_limits`. This
  script prints the line and saves one JSON per session for the "Claude usage"
  extension and the Routines usage check.

  Ligar / enable (~/.claude*/settings.json; install.ps1 -ClaudeHooks faz isso):
    "statusLine": { "type": "command", "command": "pwsh -NoProfile -File \"<duck>\\bin\\duck-statusline.ps1\"" }
  Ja tem statusline? Chame este script de dentro do seu, passando o mesmo stdin.
#>
$ErrorActionPreference = 'Stop'
$raw = [Console]::In.ReadToEnd()
if (-not $raw) { return }
try { $d = $raw | ConvertFrom-Json } catch { return }

# Janelas de uso: qualquer sub-objeto com used_percentage (os nomes ja mudaram entre versoes).
$windows = @()
if ($d.rate_limits) {
  foreach ($p in $d.rate_limits.PSObject.Properties) {
    if ($p.Value -and ($p.Value.PSObject.Properties.Name -contains 'used_percentage')) {
      $windows += [pscustomobject]@{ Key = $p.Name; Pct = [double]$p.Value.used_percentage; Resets = $p.Value.resets_at }
    }
  }
}

if ($d.session_id) {
  try {
    $dir = Join-Path ([Environment]::GetFolderPath('UserProfile')) 'AppData\Local\Temp\claude-usage'
    New-Item -ItemType Directory -Force $dir | Out-Null
    $f = Join-Path $dir "$($d.session_id).json"
    [ordered]@{
      ts = (Get-Date).ToString('o'); session_id = $d.session_id; model = $d.model.display_name; version = $d.version
      cwd = $d.workspace.current_dir; cost_usd = $d.cost.total_cost_usd; rate_limits = $d.rate_limits; windows = $windows
    } | ConvertTo-Json -Depth 8 | Set-Content "$f.tmp" -Encoding utf8NoBOM
    Move-Item "$f.tmp" $f -Force   # atomico: a extensao nunca le arquivo pela metade
  } catch { }
}

$segs = @()
if ($d.model.display_name) { $segs += $d.model.display_name }
foreach ($w in $windows) {
  $k = switch -Regex ($w.Key) { 'five_hour|5.?h' { '5h' } 'seven_day|7.?d|week' { '7d' } default { $w.Key } }
  $segs += ('{0} {1:N0}%' -f $k, $w.Pct)
}
if ($d.cost.total_cost_usd) { $segs += ('${0:N2}' -f $d.cost.total_cost_usd) }
Write-Output ($segs -join ' · ')
