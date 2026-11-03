# Shell integration do Duck para PowerShell (injetada sozinha no pwsh).
# Marca o prompt e cada comando (OSC 133) e a pasta atual (OSC 7). E com isso
# que o Duck desenha os blocos de comando (exit code, duracao), pula entre
# prompts, copia a saida do ultimo comando e avisa quando um comando longo acaba.
if ($env:TERM_PROGRAM -ne 'Duck' -or $global:__duck_si) { return }
$global:__duck_si = $true
$global:__duck_ran = $false
$global:__duck_prompt = $function:prompt

function global:prompt {
    $ok = $?
    $code = $global:LASTEXITCODE
    $e = [char]27; $bel = [char]7
    $out = ''
    if ($global:__duck_ran) {
        $exit = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
        $out += "$e]133;D;$exit$bel"
    }
    $global:__duck_ran = $false
    $loc = $executionContext.SessionState.Path.CurrentLocation
    if ($loc.Provider.Name -eq 'FileSystem') {
        $out += "$e]7;file://$env:COMPUTERNAME/$($loc.ProviderPath -replace '\\', '/')$bel"
    }
    $p = & $global:__duck_prompt
    "$out$e]133;A$bel$p$e]133;B$bel"
}

# Inicio do comando (133;C): o Enter do PSReadLine marca antes de aceitar a linha.
if (Get-Module PSReadLine) {
    Set-PSReadLineKeyHandler -Key Enter -BriefDescription DuckAcceptLine -ScriptBlock {
        $global:__duck_ran = $true
        [Console]::Write("$([char]27)]133;C$([char]7)")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }
}
