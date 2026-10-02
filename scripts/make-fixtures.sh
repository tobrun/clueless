#!/bin/bash
# Generates the fixture WAV files under fixtures/ (16 kHz mono 16-bit PCM)
# with the macOS say command, plus the expected texts under fixtures/expected.
# Fails with the voice name when a voice is missing.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ROOT}/fixtures"
EXP="${OUT}/expected"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

EN_VOICE="Daniel"
NL_VOICE="Ellen"
FR_VOICE="Flo (French (France))"
ME_VOICE="Daniel"
THEM_VOICE="Flo (English (UK))"

fail() { echo "FAIL: $1" >&2; exit 1; }

check_voice() {
  local v="$1"
  if ! say -v "$v" -o "$TMP/voice-probe.aiff" "." 2>/dev/null; then
    fail "voice not available: $v"
  fi
}

synth() { # voice text out.wav
  local voice="$1" text="$2" out="$3"
  say -v "$voice" -r 170 -o "$TMP/say.aiff" "$text"
  afconvert -f WAVE -d LEI16@16000 -c 1 "$TMP/say.aiff" "$out" 2>/dev/null
  [ -s "$out" ] || fail "synthesis produced no audio for voice $voice"
}

dur() { # wav -> seconds as a float
  python3 -c "import wave,sys; w=wave.open(sys.argv[1]); print(w.getnframes()/w.getframerate())" "$1"
}

mkdir -p "$OUT" "$EXP"
for v in "$EN_VOICE" "$NL_VOICE" "$FR_VOICE" "$ME_VOICE" "$THEM_VOICE"; do check_voice "$v"; done

# --- one-line questions ------------------------------------------------------
EN_TEXT="Can we ship the new release on friday if all the automated tests and the manual checks pass?"
NL_TEXT="Kunnen we de presentatie morgenochtend bespreken"
FR_TEXT="Pouvez vous envoyer le rapport avant la fin de la journee"

synth "$EN_VOICE" "$EN_TEXT" "$OUT/en_question.wav"
synth "$NL_VOICE" "$NL_TEXT" "$OUT/nl_question.wav"
synth "$FR_VOICE" "$FR_TEXT" "$OUT/fr_question.wav"
printf '%s\n' "$EN_TEXT" > "$EXP/en_question.txt"
printf '%s\n' "$NL_TEXT" > "$EXP/nl_question.txt"
printf '%s\n' "$FR_TEXT" > "$EXP/fr_question.txt"

# --- conversation: two time-aligned 60 s tracks ------------------------------
# Slot starts in seconds; a track is silent while the other speaks.
ME_LINES=(
  "Hi everyone, thanks for joining. What is the status of the release?"
  "Sounds good. Who is writing the release notes today?"
  "I can review the pull request this afternoon."
  "Great. Let us meet again on monday. Bye everyone."
)
THEM_LINES=(
  "Hello. The release is almost ready, only two tests are failing."
  "The tests passed overnight. We can ship on friday."
  "I will write the notes. Sarah will review the final change."
  "Perfect. Have a nice weekend. See you monday."
)

# synthesize every conversation line first
i=0
for line in "${ME_LINES[@]}"; do synth "$ME_VOICE" "$line" "$TMP/daniel$i.wav"; i=$((i+1)); done
i=0
for line in "${THEM_LINES[@]}"; do synth "$THEM_VOICE" "$line" "$TMP/flo_uk$i.wav"; i=$((i+1)); done

python3 - "$TMP" "$OUT" "$EXP" <<'PYEOF'
import wave, sys, os
tmp, out, exp = sys.argv[1:4]
RATE = 16000
TOTAL = 60 * RATE

def read_wav(path):
    with wave.open(path) as w:
        assert w.getnchannels() == 1 and w.getframerate() == RATE and w.getsampwidth() == 2, path
        import struct
        n = w.getnframes()
        return list(struct.unpack(f"<{n}h", w.readframes(n)))

me_lines = [
    "Hi everyone, thanks for joining. What is the status of the release?",
    "Sounds good. Who is writing the release notes today?",
    "I can review the pull request this afternoon.",
    "Great. Let us meet again on monday. Bye everyone.",
]
them_lines = [
    "Hello. The release is almost ready, only two tests are failing.",
    "The tests passed overnight. We can ship on friday.",
    "I will write the notes. Sarah will review the final change.",
    "Perfect. Have a nice weekend. See you monday.",
]
me_slots = [0.5, 16.5, 33.5, 50.5]
them_slots = [8.0, 25.0, 42.0, 55.0]

me_track = [0] * TOTAL
them_track = [0] * TOTAL
for lines, slots, track, voice in ((me_lines, me_slots, me_track, "daniel"),
                                   (them_lines, them_slots, them_track, "flo_uk")):
    for i, (text, slot) in enumerate(zip(lines, slots)):
        p = os.path.join(tmp, f"{voice}{i}.wav")
        samples = read_wav(p)
        start = int(slot * RATE)
        if start + len(samples) >= TOTAL:
            sys.exit(f"line too long to fit 60 s: {text!r}")
        for j, s in enumerate(samples):
            track[start + j] = s

def write_wav(path, track):
    import struct
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(struct.pack(f"<{len(track)}h", *track))

write_wav(os.path.join(out, "conv_me.wav"), me_track)
write_wav(os.path.join(out, "conv_them.wav"), them_track)

# echo_me: the Them track at half volume, nothing else
echo_track = [int(s // 2) for s in them_track]
write_wav(os.path.join(out, "echo_me.wav"), echo_track)

# expected conversation lines in commit order (by slot time)
with open(os.path.join(exp, "conv.txt"), "w") as f:
    events = [(s, "Me", t) for s, t in zip(me_slots, me_lines)] + \
             [(s, "Them", t) for s, t in zip(them_slots, them_lines)]
    for _, speaker, text in sorted(events):
        f.write(f"{speaker}: {text}\n")
PYEOF

# --- monologue ~40 s ---------------------------------------------------------
MONO_TEXT="Good morning. Today I want to talk about our roadmap for the fourth quarter. \
First, we finished the meeting transcript feature last week. \
Second, the mobile app is behind schedule by two weeks. \
Third, we are hiring two more engineers for the platform team. \
The budget is approved and the interviews start next week. \
We also plan to move the build system to a new server. \
That migration should take about three days. \
Thank you for listening, I am happy to take any questions."
synth "$ME_VOICE" "$MONO_TEXT" "$TMP/monologue_raw.wav"
MONO_DUR=$(dur "$TMP/monologue_raw.wav")
python3 - "$TMP/monologue_raw.wav" "$OUT/monologue_40s.wav" "$MONO_DUR" <<'PYEOF'
import wave, sys, struct
src, dst, raw = sys.argv[1], sys.argv[2], float(sys.argv[3])
with wave.open(src) as w:
    frames = w.readframes(w.getnframes())
    params = w.getparams()
target = int(round(max(raw, 40.0) * 16000))
have = len(frames) // 2
pad = (target - have) * 2
if pad > 0:
    frames += b"\x00" * pad
with wave.open(dst, "wb") as w:
    w.setparams(params)
    w.writeframes(frames[: target * 2])
print(f"monologue: {raw:.1f} s speech, {max(raw, 40.0):.1f} s file")
PYEOF
printf '%s\n' "$MONO_TEXT" > "$EXP/monologue.txt"

# --- silence_5s ---------------------------------------------------------------
python3 - "$OUT/silence_5s.wav" <<'PYEOF'
import wave, sys
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(1)
    w.setsampwidth(2)
    w.setframerate(16000)
    w.writeframes(b"\x00" * (5 * 16000 * 2))
PYEOF

# --- README with lengths -------------------------------------------------------
{
  echo "# Test fixtures"
  echo
  echo "All files are 16 kHz mono 16-bit PCM WAV, generated by scripts/make-fixtures.sh."
  echo "Expected texts are under fixtures/expected/."
  echo
  echo "| File | Seconds | Content |"
  echo "| ---- | ------- | ------- |"
  echo "| en_question.wav | $(dur "$OUT/en_question.wav" | cut -d. -f1) | English question (Daniel): $EN_TEXT |"
  echo "| nl_question.wav | $(dur "$OUT/nl_question.wav" | cut -d. -f1) | Dutch question (Ellen): $NL_TEXT |"
  echo "| fr_question.wav | $(dur "$OUT/fr_question.wav" | cut -d. -f1) | French question (Flo fr_FR): $FR_TEXT |"
  echo "| conv_me.wav | 60 | Four Me lines (Daniel) at 0.5, 16.5, 33.5, 50.5 s; silent while Them speaks; expected/conv.txt |"
  echo "| conv_them.wav | 60 | Four Them lines (Flo en_GB) at 8, 25, 42, 55 s; silent while Me speaks; expected/conv.txt |"
  echo "| echo_me.wav | 60 | The conv_them track at half volume (simulates Them heard through speakers into the mic) |"
  echo "| monologue_40s.wav | $(dur "$OUT/monologue_40s.wav" | cut -d. -f1) | One continuous English monologue (Daniel); expected/monologue.txt |"
  echo "| silence_5s.wav | 5 | Digital silence |"
} > "$OUT/README.md"

echo "fixtures written to $OUT"
for f in "$OUT"/*.wav; do
  [ -f "$f" ] && echo "  $(basename "$f"): $(dur "$f") s"
done
