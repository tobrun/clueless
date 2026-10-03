#!/bin/bash
# Smoke-check an OpenAI-compatible ASR server against a fixture.
# Reads ASR_BASE_URL, ASR_MODEL and optional ASR_API_KEY from the process
# environment, falling back to the repo's .env (or any file in ENV_FILE).
# Exits non-zero on any non-200 or unexpected response.
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="${ROOT}/fixtures/en_question.wav"

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

BASE=$(read_var ASR_BASE_URL) || fail "ASR_BASE_URL is not set and not in $ROOT/.env"
BASE="${BASE%/}"
MODEL=$(read_var ASR_MODEL) || fail "ASR_MODEL is not set and not in $ROOT/.env"
AUTH=()
API_KEY=$(read_var ASR_API_KEY || true)
if [ -n "${API_KEY:-}" ]; then
  AUTH=(-H "Authorization: Bearer ${API_KEY}")
fi

[ -f "$FIXTURE" ] || fail "fixture missing: $FIXTURE (run scripts/make-fixtures.sh)"

MODELS_OUT=$(mktemp -t smoke-asr-models.XXXXXX)
OUT=$(mktemp -t smoke-asr-out.XXXXXX)
trap 'rm -f "$MODELS_OUT" "$OUT"' EXIT

echo "GET ${BASE}/v1/models"
CODE=$(curl -s -o "$MODELS_OUT" -w '%{http_code}' -m 10 \
  "${AUTH[@]}" "${BASE}/v1/models") || fail "models request failed"
[ "$CODE" = "200" ] || fail "models returned HTTP $CODE"
grep -q "$MODEL" "$MODELS_OUT" || fail "model $MODEL not listed"
echo "  ok, $MODEL listed"

echo "POST ${BASE}/v1/audio/transcriptions (${FIXTURE##*/})"
START=$(python3 -c 'import time;print(int(time.time()*1000))')
CODE=$(curl -s -o "$OUT" -w '%{http_code}' -m 60 \
  "${AUTH[@]}" \
  -F "file=@${FIXTURE};type=audio/wav" \
  -F "model=${MODEL}" \
  -F "response_format=json" \
  "${BASE}/v1/audio/transcriptions") || fail "transcription request failed"
END=$(python3 -c 'import time;print(int(time.time()*1000))')
[ "$CODE" = "200" ] || fail "transcription returned HTTP $CODE ($(head -c 200 "$OUT"))"
grep -q '"text"' "$OUT" || fail "no text field in response"
echo "  ok, transcribed in $((END - START)) ms: $(head -c 200 "$OUT")"
echo "smoke-asr: PASS"
