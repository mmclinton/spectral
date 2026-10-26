#compdef spectral

# Completions for spectral(1).
#
# Install as _spectral on $fpath (share/zsh/site-functions/_spectral), or
# source it after compinit, which registers it for the session.
# `--device` completes to the current input devices, listed with their
# names, and the on|off toggles to their values.

_spectral_devices() {
    # With bashrematch set, =~ fills $BASH_REMATCH and leaves $match empty,
    # and compinit's option reset does not cover it.
    setopt localoptions nobashrematch
    local -a devices match mbegin mend
    local line
    # Bounded because `spectral devices` runs a PipeWire main loop with no
    # deadline of its own: a wedged PipeWire would otherwise hang the shell
    # that pressed Tab, with nothing to do but interrupt it. The header row
    # and the empty-list message are the lines that open with no id.
    #
    # The name is whatever follows the channel count, taken as one piece so
    # the runs of spaces inside it survive.
    for line in ${(f)"$(timeout 2 spectral devices 2>/dev/null)"}; do
        [[ $line =~ '^ *([0-9]+) +[0-9]+ *(.*)$' ]] || continue
        devices+=("${match[1]}:${match[2]}")
    done
    _describe -t devices 'input device' devices
}

_spectral() {
    local curcontext=$curcontext state line
    local -A opt_args
    local -a commands

    _arguments -C \
        '(- :)'{-h,--help}'[Show this help]' \
        '(- :)'{-V,--version}'[Show the version]' \
        '1:command:->command' \
        '*::argument:->argument'

    case $state in
        command)
            commands=(
                "enable:Create the virtual microphone and start denoising"
                "disable:Tear down the virtual microphone"
                "status:Show the daemon's state"
                "devices:List input devices with their current PipeWire ids"
                "shutdown:Stop the daemon"
            )
            _describe -t commands command commands
            ;;
        argument)
            [[ $line[1] == enable ]] || return 1
            _arguments \
                '--device=[Input device to capture]:device:_spectral_devices' \
                '--threshold=[Voice gate threshold; 0 = denoise only]:threshold: ' \
                '--mic-name=[Virtual mic name]:name: ' \
                '--rumble-filter=[Cut rumble below ~90 Hz before denoising]:state:(on off)' \
                '--auto-level=[Even out volume; peak limiter as the ceiling]:state:(on off)'
            ;;
    esac
}

# Autoloaded from $fpath, this file is the body of _spectral and has to
# complete; sourced, it only registers the function.
if [[ $funcstack[1] == _spectral ]]; then
    _spectral "$@"
else
    compdef _spectral spectral
fi
