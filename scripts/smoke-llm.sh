#!/bin/bash
# Smoke-check an OpenAI-compatible LLM server.
# Reads LLM_BASE_URL, LLM_MODEL and optional LLM_API_KEY from the process
# environment, falling back to the repo's .env (or any file in ENV_FILE).
# Exits non-zero on any non-200 or unexpected response.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

read_var() { # process env first, then the env file
  local name="$1"
  if [ -n "${!name:-}" ]; then
    printf '%s' "${!name}"
    return 0
  fi
  local file="${ENV_FILE:-$ROOT/.env}"
  [ -f "$file" ] || return 1
  sed -n "s/^[[:space:]]*${name}[[:space:]]*=[[:space:]]*//p" "$file" | tail -n 1 \
    | sed -e 's/^["'\'']//' -e 's/["'\'']$//' -e 's/[[:space:]]*#.*$//'
}

fail() { echo "FAIL: $1" >&2; exit 1; }

BASE=$(read_var LLM_BASE_URL) || fail "LLM_BASE_URL is not set and not in $ROOT/.env"
BASE="${BASE%/}"
MODEL=$(read_var LLM_MODEL) || fail "LLM_MODEL is not set and not in $ROOT/.env"
AUTH=()
API_KEY=$(read_var LLM_API_KEY || true)
if [ -n "${API_KEY:-}" ]; then
  AUTH=(-H "Authorization: Bearer ${API_KEY}")
fi

OUT=$(mktemp -t smoke-llm.XXXXXX)
trap 'rm -f "$OUT"' EXIT

echo "GET ${BASE}/v1/models"
CODE=$(curl -s -o "$OUT" -w '%{http_code}' -m 10 "${AUTH[@]}" "${BASE}/v1/models") \
  || fail "models request failed"
[ "$CODE" = "200" ] || fail "models returned HTTP $CODE"
grep -q "$MODEL" "$OUT" || fail "model $MODEL not listed"
echo "  ok, $MODEL listed"

echo "POST ${BASE}/v1/chat/completions (stream, thinking off)"
CODE=$(curl -s -N -o "$OUT" -w '%{http_code}' -m 60 \
  "${AUTH[@]}" \
  -H 'Content-Type: application/json' \
  -d "{\"model\":\"${MODEL}\",\"stream\":true,\"max_tokens\":32,\"temperature\":0.4,\
\"chat_template_kwargs\":{\"enable_thinking\":false},\
\"messages\":[{\"role\":\"user\",\"content\":\"Say hello in five words.\"}]}" \
  "${BASE}/v1/chat/completions") || fail "chat request failed"
[ "$CODE" = "200" ] || fail "chat returned HTTP $CODE"
grep -q 'data:' "$OUT" || fail "chat returned no SSE events"
grep -q '\[DONE\]' "$OUT" || fail "chat stream never sent [DONE]"
grep -q 'reasoning' "$OUT" && fail "reasoning arrived despite enable_thinking=false"
grep -q '<think>' "$OUT" && fail "content contains <think>"
echo "  ok, stream delivered"
echo "smoke-llm: PASS"
