#!/usr/bin/env bash

if ! declare -p COMPOSE_ARGS >/dev/null 2>&1; then
  echo "COMPOSE_ARGS must be defined before sourcing _ha_lib.sh" >&2
  exit 1
fi

compose() {
  docker compose "${COMPOSE_ARGS[@]}" "$@"
}

py() {
  python3 -c "import sys,json; $1" 2>/dev/null || echo ""
}

wait_healthy_compose() {
  local service="$1" name="$2"
  printf "  Waiting for %s..." "$name"
  for _ in $(seq 1 45); do
    if compose exec -T "$service" curl -sf http://localhost:7700/health >/dev/null 2>&1; then
      echo " ready"
      return 0
    fi
    sleep 2
  done
  echo " TIMEOUT"
  return 1
}

cluster_status_compose() {
  local service="$1"
  compose exec -T "$service" \
    curl -sf http://localhost:7700/internal/cluster/status 2>/dev/null || echo "{}"
}

peers_total_compose() {
  local service="$1"
  cluster_status_compose "$service" | py "print(json.load(sys.stdin).get('peers_total',0))"
}

peers_healthy_compose() {
  local service="$1"
  cluster_status_compose "$service" | py "print(json.load(sys.stdin).get('peers_healthy',0))"
}

wait_for_peer_mesh_ready() {
  local max_wait="${1:-45}"
  local elapsed=0

  echo "  Waiting for peer mesh convergence (up to ${max_wait}s)..."
  while [ "$elapsed" -lt "$max_wait" ]; do
    PEERS_NODE_A=$(peers_total_compose "node-a")
    PEERS_NODE_B=$(peers_total_compose "node-b")
    PEERS_NODE_C=$(peers_total_compose "node-c")
    HEALTHY_NODE_A=$(peers_healthy_compose "node-a")
    HEALTHY_NODE_B=$(peers_healthy_compose "node-b")
    HEALTHY_NODE_C=$(peers_healthy_compose "node-c")
    if [ "$PEERS_NODE_A" -ge 2 ] 2>/dev/null \
      && [ "$PEERS_NODE_B" -ge 2 ] 2>/dev/null \
      && [ "$PEERS_NODE_C" -ge 2 ] 2>/dev/null \
      && [ "$HEALTHY_NODE_A" -ge 2 ] 2>/dev/null \
      && [ "$HEALTHY_NODE_B" -ge 2 ] 2>/dev/null \
      && [ "$HEALTHY_NODE_C" -ge 2 ] 2>/dev/null; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  return 1
}

compose_container_id() {
  local service="$1"
  local container_id
  container_id="$(compose ps -q "$service")"
  if [ -z "$container_id" ]; then
    echo "could not resolve container for service $service" >&2
    return 1
  fi
  printf '%s\n' "$container_id"
}

compose_project_name() {
  local service="${1:-node-a}"
  local container_id
  container_id="$(compose_container_id "$service")" || return 1
  docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$container_id"
}

compose_project_network_id() {
  local service="${1:-node-a}"
  local network="${2:-fj-net}"
  local container_id project network_ids
  container_id="$(compose_container_id "$service")" || return 1
  project="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$container_id")"
  if [ -z "$project" ]; then
    echo "container $container_id has no compose project label" >&2
    return 1
  fi
  network_ids="$(docker network ls -q \
    --filter "label=com.docker.compose.project=$project" \
    --filter "label=com.docker.compose.network=$network")"
  if [ "$(printf '%s\n' "$network_ids" | sed '/^$/d' | wc -l | tr -d ' ')" != "1" ]; then
    echo "expected one compose network for project $project/$network, got: $network_ids" >&2
    return 1
  fi
  printf '%s\n' "$network_ids"
}

compose_service_on_project_network() {
  local service="$1"
  local network_id="$2"
  local container_id
  container_id="$(compose_container_id "$service")" || return 1
  docker network inspect -f '{{ range $id, $_ := .Containers }}{{ println $id }}{{ end }}' "$network_id" \
    | grep -Fx "$container_id" >/dev/null
}

disconnect_service_from_project_network() {
  local service="$1"
  local network_id="$2"
  local container_id
  container_id="$(compose_container_id "$service")" || return 1
  docker network disconnect "$network_id" "$container_id"
}

reconnect_service_to_project_network() {
  local service="$1"
  local network_id="$2"
  local container_id
  container_id="$(compose_container_id "$service")" || return 1
  docker network connect --alias "$service" "$network_id" "$container_id"
}
