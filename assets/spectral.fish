# Completions for spectral(1).
#
# Install to share/fish/vendor_completions.d/spectral.fish.
# `--device` and the on|off toggles complete their values; the device list
# comes from `spectral devices` at completion time, so it tracks PipeWire.

set -l spectral_commands enable disable status devices shutdown

function __spectral_devices
    # Skip the header row; column 1 is the id, columns 3+ are the display
    # name (not the node name the daemon remembers a device by).
    #
    # Bounded because `spectral devices` runs a PipeWire main loop with no
    # deadline of its own: a wedged PipeWire would otherwise hang the shell
    # that pressed Tab, with nothing to do but interrupt it.
    timeout 2 spectral devices 2>/dev/null | tail -n +2 | while read -l line
        set -l fields (string split -n " " -- $line)
        test (count $fields) -ge 3
        and printf '%s\t%s\n' $fields[1] (string join " " $fields[3..])
    end
end

complete -c spectral -f

complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -a enable -d "Create the virtual microphone and start denoising"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -a disable -d "Tear down the virtual microphone"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -a status -d "Show the daemon's state"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -a devices -d "List input devices with their current PipeWire ids"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -a shutdown -d "Stop the daemon"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -s h -l help -d "Show this help"
complete -c spectral -n "not __fish_seen_subcommand_from $spectral_commands" \
    -s V -l version -d "Show the version"

complete -c spectral -n "__fish_seen_subcommand_from enable" -l device -x \
    -a "(__spectral_devices)" -d "Input device to capture"
complete -c spectral -n "__fish_seen_subcommand_from enable" -l threshold -x \
    -d "Voice gate threshold; 0 = denoise only"
complete -c spectral -n "__fish_seen_subcommand_from enable" -l mic-name -x \
    -d "Virtual mic name"
complete -c spectral -n "__fish_seen_subcommand_from enable" -l rumble-filter -x \
    -a "on off" -d "Cut rumble below ~90 Hz before denoising"
complete -c spectral -n "__fish_seen_subcommand_from enable" -l auto-level -x \
    -a "on off" -d "Even out volume; peak limiter as the ceiling"
