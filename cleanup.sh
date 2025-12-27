#!/usr/bin/env bash

# cleanup.sh - Cleans up Spectral daemon, GUI, and PipeWire nodes

echo "Stopping Spectral processes..."
pkill -f "spectral-daemon"
pkill -f "spectral"

echo "Removing socket..."
rm -f /tmp/runtime-$USER/spectral-daemon.sock
if [ -n "$XDG_RUNTIME_DIR" ]; then
    rm -f "$XDG_RUNTIME_DIR/spectral-daemon.sock"
fi

echo "Cleaning up PipeWire nodes..."
if command -v pw-cli &> /dev/null; then
    # Find nodes with "Spectral" in their name and destroy them
    # We use a simple awk script to parse 'pw-cli ls Node' output
    pw-cli ls Node | awk '
        /id [0-9]+,/ { 
            current_id = $2; 
            gsub(",", "", current_id); 
        }
        /node.name =/ { 
            if ($0 ~ /Spectral/) {
                print current_id;
            }
        }
    ' | while read id; do
        if [ ! -z "$id" ]; then
            echo "Destroying PipeWire node $id"
            pw-cli destroy "$id"
        fi
    done
else
    echo "pw-cli not found. Skipping PipeWire node cleanup."
fi

echo "Cleanup complete."
