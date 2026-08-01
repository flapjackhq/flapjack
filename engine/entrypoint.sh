#!/bin/sh
# entrypoint.sh — Docker entrypoint for Flapjack server.
# Writes node.json from env vars when FLAPJACK_NODE_ID and FLAPJACK_PEERS are set
# (used for replication setup). Otherwise just runs the binary directly.
#
# FLAPJACK_PEERS format: "node-id=http://host:port,node-id2=http://host2:port"

set -e

DATA_DIR="${FLAPJACK_DATA_DIR:-/data}"

json_escape() {
  printf '%s' "$1" | awk '
    BEGIN { ORS="" }
    {
      if (NR > 1) {
        printf "\\n"
      }
      gsub(/\\/,"\\\\")
      gsub(/"/,"\\\"")
      gsub(/\t/,"\\t")
      gsub(/\r/,"\\r")
      printf "%s", $0
    }
  '
}

refuse_unwritable_data_dir() {
  runtime_uid="$(id -u)"
  runtime_gid="$(id -g)"
  echo "[entrypoint] Data directory $DATA_DIR is not writable by ${runtime_uid}:${runtime_gid}. Run this host-side recovery command: docker run --rm -v <volume>:/data alpine chown -R ${runtime_uid}:${runtime_gid} /data" >&2
  exit 1
}

mkdir -p "$DATA_DIR" || refuse_unwritable_data_dir
[ -w "$DATA_DIR" ] || refuse_unwritable_data_dir

if [ -n "$FLAPJACK_NODE_ID" ] && [ -n "$FLAPJACK_PEERS" ]; then
  # Build peers JSON array
  PEERS="["
  FIRST=true
  OLD_IFS=$IFS
  IFS=','
  for peer in $FLAPJACK_PEERS; do
    PEER_ID="${peer%%=*}"
    PEER_ADDR="${peer#*=}"
    if [ "$FIRST" = true ]; then
      FIRST=false
    else
      PEERS="$PEERS,"
    fi
    ESCAPED_PEER_ID="$(json_escape "$PEER_ID")"
    ESCAPED_PEER_ADDR="$(json_escape "$PEER_ADDR")"
    PEERS="$PEERS{\"node_id\":\"$ESCAPED_PEER_ID\",\"addr\":\"$ESCAPED_PEER_ADDR\"}"
  done
  IFS=$OLD_IFS
  PEERS="$PEERS]"

  BIND="${FLAPJACK_BIND_ADDR:-0.0.0.0:7700}"
  ESCAPED_NODE_ID="$(json_escape "$FLAPJACK_NODE_ID")"
  ESCAPED_BIND="$(json_escape "$BIND")"

  cat > "$DATA_DIR/node.json" <<EOF
{"node_id":"$ESCAPED_NODE_ID","bind_addr":"$ESCAPED_BIND","peers":$PEERS}
EOF

  echo "[entrypoint] Wrote $DATA_DIR/node.json for replication startup"
fi

[ "${1#-}" != "$1" ] && set -- flapjack "$@"
exec "$@"
