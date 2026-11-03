# Shell integration do Duck para bash e zsh (WSL, ssh).
# No fim do ~/.bashrc ou ~/.zshrc:
#   [ -f /mnt/c/Users/<voce>/AppData/Local/duck/shell/duck.sh ] && . /mnt/c/Users/<voce>/AppData/Local/duck/shell/duck.sh
# Marca prompt e comandos (OSC 133) e a pasta (OSC 7). Fora do Duck nao faz nada.
[ "$TERM_PROGRAM" = "Duck" ] || return 0 2>/dev/null
[ -n "$__duck_si" ] && return 0
__duck_si=1

if [ -n "$ZSH_VERSION" ]; then
    __duck_precmd() {
        local c=$?
        [ -n "$__duck_ran" ] && printf '\e]133;D;%s\a' "$c"
        __duck_ran=
        printf '\e]7;file://%s%s\a\e]133;A\a' "$HOST" "$PWD"
    }
    __duck_preexec() { __duck_ran=1; printf '\e]133;C\a'; }
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd __duck_precmd
    add-zsh-hook preexec __duck_preexec
else
    __duck_precmd() {
        local c=$?
        [ -n "$__duck_ran" ] && printf '\e]133;D;%s\a' "$c"
        __duck_ran=
        printf '\e]7;file://%s%s\a' "$HOSTNAME" "$PWD"
    }
    __duck_preexec() {
        [ -n "$__duck_ran" ] && return
        case "$BASH_COMMAND" in __duck_*) return ;; esac
        __duck_ran=1
        printf '\e]133;C\a'
    }
    PROMPT_COMMAND="__duck_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    PS1="\[\e]133;A\a\]$PS1\[\e]133;B\a\]"
    trap '__duck_preexec' DEBUG
fi
