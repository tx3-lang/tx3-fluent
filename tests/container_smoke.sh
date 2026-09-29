#!/usr/bin/env bash
# Container smoke test: builds the image, serves the fixture registrations over
# HTTP in `token` mode, and checks that `initialize` and `tools/list` answer
# with the expected tools. Needs docker, curl and jq.
#
#   tests/container_smoke.sh               build tx3-fluent:smoke, then test it
#   IMAGE=ghcr.io/… SKIP_BUILD=1 tests/container_smoke.sh
#                                          test an existing image
#   KEEP=1 tests/container_smoke.sh        leave the container for inspection
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
IMAGE=${IMAGE:-tx3-fluent:smoke}
PORT=${PORT:-18080}
TOKEN=smoke-token
NAME=fluent-smoke-$$
PROTOCOL_VERSION=2025-06-18
EXPECTED='fluent_get_skill
fluent_inspect_address
strike_staking_mainnet_add_stake
strike_staking_mainnet_stake
strike_staking_mainnet_withdraw_stake
transfer_preprod_transfer'

DATA=$(mktemp -d)
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    echo "--- container logs ---" >&2
    docker logs "$NAME" >&2 2>&1 || true
  fi
  [ -n "${KEEP:-}" ] || docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$DATA"
  exit "$status"
}
trap cleanup EXIT

if [ -z "${SKIP_BUILD:-}" ]; then
  docker build -t "$IMAGE" "$ROOT"
fi

# The container runs as uid 1000, so the mounted directory must be readable
# by anyone. No TRP endpoint is contacted: nothing is prepared.
cp -R "$ROOT/crates/fluent-core/tests/fixtures/registrations/valid" "$DATA/registrations"
cat >"$DATA/fluent.toml" <<'TOML'
[server]
listen = "0.0.0.0:8080"

[registrations]
dir = "/data/registrations"

[networks.mainnet]
trp_url = "http://127.0.0.1:1"

[networks.preprod]
trp_url = "http://127.0.0.1:1"

[auth]
mode = "token"
token_env = "FLUENT_API_TOKEN"
TOML
chmod -R a+rX "$DATA"

docker run -d --name "$NAME" -p "127.0.0.1:$PORT:8080" \
  -e FLUENT_API_TOKEN="$TOKEN" -v "$DATA:/data" "$IMAGE" >/dev/null

URL="http://127.0.0.1:$PORT"
for _ in $(seq 1 60); do
  if curl -fsS "$URL/healthz" >/dev/null 2>&1; then
    break
  fi
  if [ "$(docker inspect -f '{{.State.Running}}' "$NAME")" != true ]; then
    echo "the container exited" >&2
    exit 1
  fi
  sleep 1
done
curl -fsS "$URL/healthz" | jq -e '.status == "ok"' >/dev/null

# The JSON-RPC reply in a JSON or SSE body.
reply() {
  awk '/^data:/ { sub(/^data: */, ""); print; next } /^\{/ { print }' | jq -c 'select(type == "object" and has("id"))'
}

HEADERS=$(mktemp)
post() {
  curl -fsS -D "$HEADERS" "$URL/mcp" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    ${SESSION:+-H "Mcp-Session-Id: $SESSION"} \
    ${SESSION:+-H "Mcp-Protocol-Version: $PROTOCOL_VERSION"} \
    -d "$1"
}

# An unauthenticated request is refused.
status=$(curl -s -o /dev/null -w '%{http_code}' "$URL/mcp" -H "Content-Type: application/json" -d '{}')
[ "$status" = 401 ] || { echo "unauthenticated /mcp answered $status, want 401" >&2; exit 1; }

SESSION=
init=$(post '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"'"$PROTOCOL_VERSION"'","capabilities":{},"clientInfo":{"name":"container-smoke","version":"0"}}}' | reply)
SESSION=$(tr -d '\r' <"$HEADERS" | sed -n 's/^[Mm]cp-[Ss]ession-[Ii]d: *//p')
[ -n "$SESSION" ] || { echo "initialize returned no session id" >&2; exit 1; }
echo "$init" | jq -e '.result.serverInfo.name == "tx3-fluent"' >/dev/null

post '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null

tools=$(post '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | reply | jq -r '.result.tools[].name' | sort)
rm -f "$HEADERS"
if [ "$tools" != "$EXPECTED" ]; then
  echo "tools/list returned:" >&2
  echo "$tools" >&2
  echo "want:" >&2
  echo "$EXPECTED" >&2
  exit 1
fi

echo "container smoke passed: $(echo "$init" | jq -r '.result.serverInfo | "\(.name) \(.version)"'), $(echo "$tools" | wc -l | tr -d ' ') tools"
