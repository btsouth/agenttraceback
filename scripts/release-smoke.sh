#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/agenttraceback-release-smoke.XXXXXX")"
trap 'rm -rf "$TMP_ROOT"' EXIT

export AGENTTRACEBACK_DATA_DIR="$TMP_ROOT/data"
export AGENTTRACEBACK_CONFIG_DIR="$TMP_ROOT/config"
export XDG_RUNTIME_DIR="$TMP_ROOT/runtime"
mkdir -p "$XDG_RUNTIME_DIR"

cargo build --quiet -p agenttraceback-cli -p agenttraceback-daemon
CLI="$ROOT/target/debug/agenttraceback"

"$CLI" daemon start --json > "$TMP_ROOT/start.json"
"$CLI" demo install --json > "$TMP_ROOT/demo.json"
"$CLI" sessions --json > "$TMP_ROOT/sessions.json"
SESSION_ID="$(jq -r '.[0].id' "$TMP_ROOT/sessions.json")"
"$CLI" search 'path:src/auth' --json > "$TMP_ROOT/search.json"
"$CLI" verify --all --json > "$TMP_ROOT/verify.json"
"$CLI" export "$SESSION_ID" --format markdown --json > "$TMP_ROOT/export.json"
"$CLI" export "$SESSION_ID" --format json --full --json > "$TMP_ROOT/full-export.json"
"$CLI" daemon backup --output "$TMP_ROOT/backup.db" --json > "$TMP_ROOT/backup.json"
"$CLI" demo remove --json > "$TMP_ROOT/remove.json"
"$CLI" daemon stop --json > "$TMP_ROOT/stop.json"
"$CLI" daemon restore "$TMP_ROOT/backup.db" --confirm --json > "$TMP_ROOT/restore.json"
"$CLI" daemon start --json > "$TMP_ROOT/start-after-restore.json"
"$CLI" status --json > "$TMP_ROOT/status-after-restore.json"
"$CLI" sessions --json > "$TMP_ROOT/sessions-after-restore.json"
"$CLI" daemon stop --json > "$TMP_ROOT/final-stop.json"

jq -e '.installed == true' "$TMP_ROOT/demo.json" >/dev/null
jq -e 'length == 1' "$TMP_ROOT/sessions.json" >/dev/null
jq -e '.items | any(.targetDisplay == "src/auth.ts")' "$TMP_ROOT/search.json" >/dev/null
jq -e '.valid == true' "$TMP_ROOT/verify.json" >/dev/null
EXPORT_PATH="$(jq -r '.path' "$TMP_ROOT/export.json")"
FULL_EXPORT_PATH="$(jq -r '.path' "$TMP_ROOT/full-export.json")"
test -f "$EXPORT_PATH"
test -f "$FULL_EXPORT_PATH"
jq -e '.redacted == false' "$TMP_ROOT/full-export.json" >/dev/null
jq -e '.fullContent | length > 0' "$FULL_EXPORT_PATH" >/dev/null
test -f "$TMP_ROOT/backup.db"
jq -e '.installed == false' "$TMP_ROOT/remove.json" >/dev/null
jq -e '.reachable == true' "$TMP_ROOT/status-after-restore.json" >/dev/null
jq -e 'length == 1 and .[0].eventCount == 5' "$TMP_ROOT/sessions-after-restore.json" >/dev/null

printf 'release smoke: daemon, demo, search, verify, redacted/full export, backup, and restore passed\n'
