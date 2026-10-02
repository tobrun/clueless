#!/bin/bash
# Smoke-check the ASR server against a fixture. Usage: scripts/smoke-asr.sh HOST [PORT]
# Exits non-zero on any non-200 or unexpected response.
set -u

HOST="${1:-localhost}"
PORT="${2:-8097}"
BASE="http://${HOST}:${PORT}"
MODEL="${ASR_MODEL:-istupakov/parakeet-tdt-0.6b-v3-onnx}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="${ROOT}/fixtures/en_question.wav"

fail() { echo "FAIL: $1" >&2; exit 1; }

[ -f "$FIXTURE" ] || fail "fixture missing: $FIXTURE (run scripts/make-fixtures.sh)"

echo "GET ${BASE}/v1/models"
CODE=$(curl -s -o /tmp/smoke-asr-models.json -w '%{http_code}' -m 10 "${BASE}/v1/models") || fail "models request failed"
[ "$CODE" = "200" ] || fail "models returned HTTP $CODE"
grep -q "$MODEL" /tmp/smoke-asr-models.json || fail "model $MODEL not listed"
echo "  ok, $MODEL listed"

echo "POST ${BASE}/v1/audio/transcriptions (${FIXTURE##*/})"
START=$(python3 -c 'import time;print(int(time.time()*1000))')
CODE=$(curl -s -o /tmp/smoke-asr-out.json -w '%{http_code}' -m 60 \
  -F "file=@${FIXTURE};type=audio/wav" \
  -F "model=${MODEL}" \
  -F "response_format=json" \
  "${BASE}/v1/audio/transcriptions") || fail "transcription request failed"
END=$(python3 -c 'import time;print(int(time.time()*1000))')
[ "$CODE" = "200" ] || fail "transcription returned HTTP $CODE ($(head -c 200 /tmp/smoke-asr-out.json))"
grep -q '"text"' /tmp/smoke-asr-out.json || fail "no text field in response"
echo "  ok, transcribed in $((END - START)) ms: $(head -c 200 /tmp/smoke-asr-out.json)"
echo "smoke-asr: PASS"
