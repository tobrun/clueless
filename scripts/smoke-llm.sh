#!/bin/bash
# Smoke-check the LLM server. Usage: scripts/smoke-llm.sh HOST [PORT]
# Exits non-zero on any non-200 or unexpected response.
set -u

HOST="${1:-localhost}"
PORT="${2:-8000}"
BASE="http://${HOST}:${PORT}"
MODEL="${LLM_MODEL:-your-model-id}"

fail() { echo "FAIL: $1" >&2; exit 1; }

echo "GET ${BASE}/v1/models"
CODE=$(curl -s -o /tmp/smoke-llm-models.json -w '%{http_code}' -m 10 "${BASE}/v1/models") || fail "models request failed"
[ "$CODE" = "200" ] || fail "models returned HTTP $CODE"
grep -q "$MODEL" /tmp/smoke-llm-models.json || fail "model $MODEL not listed"
echo "  ok, $MODEL listed"

echo "POST ${BASE}/v1/chat/completions (stream, thinking off)"
CODE=$(curl -s -N -o /tmp/smoke-llm-chat.txt -w '%{http_code}' -m 60 \
  -H 'Content-Type: application/json' \
  -d "{\"model\":\"${MODEL}\",\"stream\":true,\"max_tokens\":32,\"temperature\":0.4,\
\"chat_template_kwargs\":{\"enable_thinking\":false},\
\"messages\":[{\"role\":\"user\",\"content\":\"Say hello in five words.\"}]}" \
  "${BASE}/v1/chat/completions") || fail "chat request failed"
[ "$CODE" = "200" ] || fail "chat returned HTTP $CODE"
grep -q 'data:' /tmp/smoke-llm-chat.txt || fail "chat returned no SSE events"
grep -q '\[DONE\]' /tmp/smoke-llm-chat.txt || fail "chat stream never sent [DONE]"
grep -q 'reasoning' /tmp/smoke-llm-chat.txt && fail "reasoning arrived despite enable_thinking=false"
grep -q '<think>' /tmp/smoke-llm-chat.txt && fail "content contains <think>"
echo "  ok, stream delivered"
echo "smoke-llm: PASS"
