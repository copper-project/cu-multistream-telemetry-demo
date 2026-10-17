set positional-arguments

# Prepare the Delta environment and run the robot (default).
default *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -r /data/dev/env.sh ]]; then
        source /data/dev/env.sh
    fi
    # Runtime-only limits; no persistent sysctl configuration or robot log files.
    sysctl -w net.core.wmem_max=67108864 net.core.rmem_max=16777216 >/dev/null
    exec cargo +stable run -r --locked -- "$@"

# Run the desktop telemetry station.
telemetry *args:
    cargo run -r -p telemetry-station -- "$@"

fmt:
    cargo fmt --all
    taplo fmt Cargo.toml telemetry-station/Cargo.toml
    fmtron copperconfig.ron
