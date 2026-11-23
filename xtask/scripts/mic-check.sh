# Runs as the desktop user under `bash -euo pipefail -c`, inside session.sh,
# with the command that runs Spectral as its arguments (`spectral`, or
# `flatpak run <app id>`). Starts PipeWire and WirePlumber with a test tone
# as the only input device, enables Spectral on it, and passes once the
# virtual mic is running, is linked at both ends and is the default source.

spectral=("$@")
tone=xtask-tone
mic=xtask-mic
logs=$HOME/logs

# A silent null source has no output ports, so nothing could link to it;
# audiotestsrc does, but the default configuration does not load it.
conf=$HOME/.config/pipewire/pipewire.conf.d
mkdir -p "$conf" "$logs"
cat >"$conf/xtask-tone.conf" <<EOF
context.spa-libs = {
  audiotestsrc = audiotestsrc/libspa-audiotestsrc
}
context.objects = [
  { factory = adapter
    args = {
      factory.name     = audiotestsrc
      node.name        = $tone
      node.description = $tone
      media.class      = Audio/Source
      object.linger    = true
    }
  }
]
EOF

pipewire >"$logs/pipewire.log" 2>&1 &
wireplumber >"$logs/wireplumber.log" 2>&1 &

# fail <reason...>: ends the check, printing what it saw with the reasons
# last, since a failed step's report keeps only the end of the output.
fail() {
  local log
  for log in "$logs"/*.log; do
    echo "--- ${log##*/}"
    tail -n 10 "$log"
  done
  echo "--- status"
  "${spectral[@]}" status || true
  echo "--- links"
  pw-link -lI || true
  printf 'mic check: %s\n' "$@"
  exit 1
}

# wait_for <seconds> <what> <command...>: polls until the command succeeds.
wait_for() {
  local seconds=$1 what=$2
  shift 2
  local deadline=$((SECONDS + seconds))
  until "$@" >/dev/null 2>&1; do
    if ((SECONDS >= deadline)); then
      fail "no $what after ${seconds}s"
    fi
    sleep 0.2
  done
}

# WirePlumber publishes the default metadata, and configures the tone's
# ports, once it manages the graph.
session_manager_is_up() {
  pw-cli ls Metadata | grep -q 'metadata.name = "default"'
}
tone_has_ports() {
  pw-link -o | grep -q "^$tone:"
}

wait_for 10 "PipeWire socket" test -S "$XDG_RUNTIME_DIR/pipewire-0"
wait_for 20 "session manager" session_manager_is_up
wait_for 10 "ports on $tone" tone_has_ports

id=$("${spectral[@]}" devices | awk -v name="$tone" '$3 == name { print $1 }')
if [ -z "$id" ]; then
  fail "\`devices\` does not list $tone"
fi
if ! "${spectral[@]}" enable --device "$id" --mic-name "$mic"; then
  fail "\`enable --device $id\` failed"
fi

mic_is_running() {
  local info
  info=$(pw-cli info "$mic" 2>/dev/null) || return 1
  grep -q 'media.class = "Audio/Source/Virtual"' <<<"$info" &&
    grep -q 'state: "running"' <<<"$info"
}

# active_link <from-node> <to-node>: an active link from a port of one node
# to a port of the other; an empty name matches any node.
active_link() {
  local id
  for id in $(pw-link -lI | awk -v from="$1" -v to="$2" '
      $2 !~ /^\|/ { port = $2; next }
      $2 == "|->" && (from == "" || index(port, from ":") == 1) \
                  && (to == "" || index($4, to ":") == 1) { print $1 }'); do
    pw-cli info "$id" | grep -q 'state: "active"' && return 0
  done
  return 1
}

mic_is_default() {
  pw-metadata 0 default.audio.source | grep -q "\"name\":\"$mic\""
}

deadline=$((SECONDS + 20))
while true; do
  failed=()
  mic_is_running || failed+=("$mic is not a running Audio/Source/Virtual node")
  active_link "$tone" "" || failed+=("nothing reads $tone through an active link")
  active_link "" "$mic" || failed+=("nothing feeds $mic through an active link")
  mic_is_default || failed+=("$mic is not the default source")
  if ((${#failed[@]} == 0)); then
    echo "mic check: $mic is running, linked at both ends and the default source"
    exit 0
  fi
  if ((SECONDS >= deadline)); then
    fail "${failed[@]}"
  fi
  sleep 0.5
done
