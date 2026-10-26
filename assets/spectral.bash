# Completions for spectral(1).
#
# Install to share/bash-completion/completions/spectral.bash.
# `--device` completes to the ids of the current input devices, and the
# on|off toggles to their values. The names stay out: bash shows a candidate
# only as the text it would insert, so a name would have to ride inside the
# candidate, and menu-complete would then type it onto the command line.

_spectral_devices() {
    # Bounded because `spectral devices` runs a PipeWire main loop with no
    # deadline of its own: a wedged PipeWire would otherwise hang the shell
    # that pressed Tab, with nothing to do but interrupt it. The header row
    # and the empty-list message are the lines that open with no id.
    timeout 2 spectral devices 2>/dev/null | while read -r id channels _; do
        [[ $id =~ ^[0-9]+$ && $channels =~ ^[0-9]+$ ]] && printf '%s\n' "$id"
    done
}

_spectral() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD - 1]}
    local commands='enable disable status devices shutdown'
    local flags='--device --threshold --mic-name --rumble-filter --auto-level'

    # `=` is a word break, so `--device=4` reaches here as three words and
    # the flag taking the value sits one or two back from the cursor.
    if [[ $cur == = ]]; then
        cur=
    elif [[ $prev == = ]]; then
        prev=${COMP_WORDS[COMP_CWORD - 2]}
    fi

    if ((COMP_CWORD == 1)); then
        mapfile -t COMPREPLY < <(compgen -W "$commands -h --help -V --version" -- "$cur")
        return
    fi
    [[ ${COMP_WORDS[1]} == enable ]] || return

    case $prev in
        --device)
            mapfile -t COMPREPLY < <(compgen -W "$(_spectral_devices)" -- "$cur")
            return
            ;;
        --rumble-filter | --auto-level)
            mapfile -t COMPREPLY < <(compgen -W 'on off' -- "$cur")
            return
            ;;
        --threshold | --mic-name)
            return
            ;;
    esac

    # The parser rejects a flag given twice, so one already on the line is
    # not offered again.
    local i used=' ' flag unused=()
    for ((i = 2; i < COMP_CWORD; i++)); do
        used+="${COMP_WORDS[i]} "
    done
    for flag in $flags; do
        [[ $used == *" $flag "* ]] || unused+=("$flag")
    done
    mapfile -t COMPREPLY < <(compgen -W "${unused[*]}" -- "$cur")
}

complete -F _spectral spectral
