<!-- [scrai:start] -->
## scripts

| File | Summary |
| --- | --- |
| start-stable-server.sh | Start the stable flapjack binary for dashboard development.
This binary is decoupled from ongoing server code changes.
Uses port 7700 by default (override with FLAPJACK_BIND_ADDR).

Usage: ./scripts/start-stable-server.sh
Rebuild: cargo build -p flapjack-server --release && mkdir -p bin && cp ../target/release/flapjack bin/flapjack-stable. |
<!-- [scrai:end] -->
