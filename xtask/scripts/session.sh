# Runs as root under `bash -euo pipefail -c`, with the command to run as its
# arguments: stands in for a desktop login, so the command runs as uid 1000
# with a system bus, a session bus and a runtime directory of its own.
#
# The system bus is for Flatpak, whose parental-controls check fails without
# one; WirePlumber 0.4 and `flatpak run` need the session bus.

# A committed image keeps the pid file of a bus that ran during the install.
mkdir -p /run/dbus
rm -f /run/dbus/pid
dbus-daemon --system --fork

# dbus-run-session and flatpak look the user up; not every image has one.
getent passwd 1000 >/dev/null || useradd --uid 1000 --user-group xtask

export HOME=/tmp/xtask-home
# Where a desktop puts it, and where Flatpak looks for the host's PipeWire
# socket whatever XDG_RUNTIME_DIR says.
export XDG_RUNTIME_DIR=/run/user/1000
install -d -m 700 -o 1000 -g 1000 "$HOME"
# A launch mounts the directory it shares with the display server here.
[ -d "$XDG_RUNTIME_DIR" ] || install -d -m 700 -o 1000 -g 1000 "$XDG_RUNTIME_DIR"

exec setpriv --reuid=1000 --regid=1000 --init-groups -- dbus-run-session -- "$@"
