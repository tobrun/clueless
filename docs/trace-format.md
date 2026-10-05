# Trace format

A trace is the set of files one meeting or one re-run leaves on disk, written so that a later build can read it back, replay its audio and compare its output with a newer run.
The owning code is `crates/trace/`; the recording switches are `[trace]` in [configuration.md](configuration.md) and the command line modes are listed there too.

## Layout

The data directory (`--data-dir PATH`, default `~/.clueless`) holds one directory per session, named by its UTC start time; a run of a session lives under that session.

```
~/.clueless/                         0700
  sessions/
    2026-10-05T14-03-22Z/            one meeting
      manifest.json
      events.jsonl
      audio/me.wav                   only with [trace] audio = true
      audio/them.wav
      runs/
        2026-10-06T09-00-00Z/        one re-run
          manifest.json
          events.jsonl
          notes.txt                  the recorded notes, given to the re-run
```

Directories are created with mode 0700 and files with 0600.
A re-run trace has the same shape as its session (`manifest.json` plus `events.jsonl`) and copies the session's notes file to `notes.txt` for the tools that read a run directory standalone.
No file under the data directory contains an API key: manifest server URLs are redacted and keys are never part of the format.
While a meeting is recording, the app holds an advisory lock on its `events.jsonl`; `--delete` refuses a locked (live) session.

## manifest.json

Written once when the trace opens, so tooling can interpret the files without the app.

| Field | Meaning |
| --- | --- |
| `schema` | format version, `1` for traces this build writes |
| `started_at_ms` | wall-clock Unix milliseconds at meeting start |
| `origin` | `live` (microphone and system audio), `replay_wav` (a WAV replay) or `rerun` (a session re-run) |
| `speed` | playback speed the trace was produced at, `1` for live meetings |
| `app_version` | the app version, e.g. `0.1.0` |
| `git_commit` | short git commit, `-dirty` when the tree had changes, `unknown` when git could not answer |
| `audio` | whether the `audio/` WAVs were written |
| `session` | the settings in effect: speakers, start profile, the LLM server (redacted URL, model, `max_tokens`, `temperature`), the speech server (redacted URL, model, optional language), the voice detector thresholds, the engine timings in ms and the compression threshold in tokens |

## events.jsonl

One JSON object per line, one record per line, in the order the engine produced them.
The envelope is `{"seq": 12, "at_ms": 3481, "kind": "...", ...}`: `seq` is a per-file counter starting at 1, `at_ms` is a monotonic clock in milliseconds at the moment of the record, and `kind` picks the record body, whose fields follow in the same object.
`seq` has no holes unless a `records_lost` record says otherwise.
A reader sorts by `seq`; unknown kinds read as an `unknown` record, and a last line that does not parse (a kill mid-write) is ignored, which is what makes `--sessions` show `cut off`.

Meeting time of a record is `(at_ms - at_ms of clock_started) * manifest.speed`: the time into the meeting the record stands for, undoing a replay speedup.

The kinds, their fields and who writes them:

| Kind | Fields | Written by |
| ---- | ------ | ---------- |
| `command` | `command`, `profile` for SetProfile | engine loop, every command received while a session is open, also ignored ones |
| `notes` | `text` | `start_meeting`, when a notes file was read |
| `clock_started` | none | `start_meeting`, when the meeting clock is created |
| `meeting_state` | `state` | wrapped UI sink |
| `profile` | `profile` | wrapped UI sink |
| `status` | `source`, `level`, `text` | wrapped UI sink |
| `transcript_interim` | `speaker`, `utterance`, `text` | wrapped UI sink |
| `transcript_final` | `speaker`, `utterance`, `t0_ms`, `t1_ms`, `text` | wrapped UI sink |
| `transcript_dropped` | `speaker`, `utterance` | wrapped UI sink |
| `suggestion_start` | `suggestion` | wrapped UI sink |
| `suggestion_delta` | `suggestion`, `text` | wrapped UI sink |
| `suggestion_end` | `suggestion`, `end`, `message` | wrapped UI sink |
| `clear_suggestion` | none | wrapped UI sink |
| `sources_drained` | none | wrapped UI sink |
| `segment` | `speaker`, `utterance`, `segment_kind`, `t0_ms`, `t1_ms`, `samples`, `overlaps_prev` | stream thread, for every segment handed on |
| `asr_call` | `speaker`, `utterance`, `segment_kind`, `started_at_ms`, `duration_ms`, `outcome` (`text`, `no_speech`, `error`, `cancelled`), `raw_text`, `error` | speech workers |
| `echo_check` | `utterance`, `held_ms`, `echo` | final worker, Me only |
| `utterance_dropped` | `speaker`, `utterance`, `reason` | final worker and stream thread |
| `piece_done` | `speaker`, `chars` (absent when the piece was dropped) | engine loop |
| `policy` | `outcome`, `profile`, `suggestion` when fired | engine loop |
| `llm_request` | `call`, `purpose`, `suggestion`, `origin`, `profile`, `body` | engine loop and compress task |
| `llm_delta` | `call`, `channel` (`content`, `reasoning`), `text` | suggestion task |
| `llm_end` | `call`, `outcome` (`done`, `cancelled`, `error`), `error`, `finish_reason`, `usage`, `raw_text`, `shown_text`, `passed`, `first_content_ms`, `first_reasoning_ms` | suggestion task and compress task |
| `summary_applied` | `call`, `replaced` | compress task |
| `audio_anchor` | `speaker`, `t_ms`, `sample_index` | audio thread, at the first frame and whenever the distance between time and place changes |
| `records_lost` | `records`, `audio_frames` | record thread |
| `end` | `reason` | `stop_meeting` and the failed-start path |
| `unknown` | none | reader only, for a kind it does not know |

A meeting that ends normally always has an `end` record last: the engine closes the trace before it reports `Idle`, which is also what makes the files complete when the app quits.
The text trace is always written while `[trace] enabled` holds; with `enabled = false` none of these files appear.

## Audio files

`audio/<speaker>.wav` is 16 kHz mono 16-bit PCM, laid out on the meeting timeline: the sample at index `i` holds the audio of meeting time `i / 16` ms.
Silence and stretches where the source delivered nothing appear as zeros.
Every place where the simple rule above does not hold is listed in an `audio_anchor` record (`speaker`, `t_ms`, `sample_index`): the anchor says that from that sample on, meeting time restarts at `t_ms`.
A tool that cuts an utterance out of a WAV walks the anchors of that speaker to find the right byte offset.
The inter-speaker offset after a backwards jump is not corrected.
After a kill a WAV is valid up to its last flush, which is why a killed session can lose its tail.

## Schema version

`schema` in `manifest.json` is the format version; this build writes `1` and reads `1`.
A manifest with a higher number is refused with `<path>: trace schema 2 is newer than this build reads (1)` on stderr and exit 2, rather than misread.
Within a version, readers ignore record fields they do not know and map unknown kinds to `unknown`, so additive changes stay readable.
