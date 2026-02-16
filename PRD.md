# PRD: Scribe — Local Audio Capture & Transcription CLI

## Overview

macOS CLI tool that captures system audio + microphone, chunks audio into segments, sends chunks to a transcription API, and stores transcripts locally.

**Target:** Single developer, macOS 13+, Apple Silicon + Intel
**Language:** Rust

---

## Architecture

```
┌─────────────────┐     ┌─────────────────┐
│ System Audio    │────►│                 │
│ (ScreenCaptureKit)    │   Audio Mixer   │
└─────────────────┘     │   & Buffer      │
                        │                 │
┌─────────────────┐     │                 │
│ Microphone      │────►│                 │
│ (cpal/CoreAudio)│     └────────┬────────┘
└─────────────────┘              │
                                 ▼
                        ┌─────────────────┐
                        │   Chunker       │
                        │   (30s default) │
                        └────────┬────────┘
                                 │
                                 ▼
                        ┌─────────────────┐
                        │  Async Queue    │
                        └────────┬────────┘
                                 │
                    ┌────────────┴────────────┐
                    ▼                         ▼
           ┌─────────────────┐      ┌─────────────────┐
           │ Transcription   │      │ Local Audio     │
           │ API Call        │      │ Storage (opt)   │
           └────────┬────────┘      └─────────────────┘
                    │
                    ▼
           ┌─────────────────┐
           │ Filesystem      │
           │ (transcripts/)  │
           └─────────────────┘
```

---

## Implementation Phases

### Phase 1: System Audio Capture

> **Risk: High** | **Effort: Medium** | **Standalone: Yes**

Highest-risk piece — ScreenCaptureKit has macOS permission requirements, the Rust crate may have quirks, and if this doesn't work nothing else matters.

**Implementation:**
1. Add deps: `screencapturekit`, `hound`
2. Initialize ScreenCaptureKit stream for system audio output
3. Collect raw PCM samples into a buffer
4. On stop (Ctrl+C), write buffer to a WAV file (native sample rate, f32/i16)
5. Handle permission denied — print instructions pointing to System Settings → Privacy & Security → Screen Recording

**Verification:**
- [x] Play a YouTube video, run binary, stop after 10s
- [x] Open output WAV in QuickTime/Audacity — audio is audible and correct
- [x] Run without Screen Recording permission — get clear error message
- [x] Run with no audio playing — produces silent WAV without crashing

**Progress:** `[x] Complete`

---

### Phase 2: Microphone Capture

> **Risk: Low** | **Effort: Low** | **Standalone: Yes**

Isolate mic issues (device selection, sample rates) from system audio.

**Implementation:**
1. Add dep: `cpal`
2. Enumerate input devices, select default (or by name)
3. Open input stream, collect PCM samples into buffer
4. On stop, write buffer to WAV
5. Handle permission denied — print Microphone permission instructions

**Verification:**
- [x] Speak into mic, stop, verify WAV has your voice
- [ ] Specify non-existent device name — get clear error
- [x] Run without Microphone permission — get clear error message
- [ ] Test with both built-in mic and headset

**Progress:** `[x] Complete`

---

### Phase 3: Mixing + Resampling

> **Risk: Medium** | **Effort: Medium** | **Standalone: Yes**

Validates that both capture streams run concurrently without glitches or drift.

**Implementation:**
1. Add dep: `rubato` (or manual resampling)
2. Run system audio + mic capture on separate threads
3. Resample both streams to 16kHz, 16-bit PCM (Whisper-optimal)
4. Mix into single stereo buffer (system=L, mic=R) for `stereo` mode
5. Support `split` mode — keep as separate channels/files
6. Accept `--mix-mode=stereo|split` flag

**Verification:**
- [ ] Play audio + speak simultaneously, verify output WAV has system on left and mic on right
- [ ] Run for 60s — no audible glitches, drift, or gaps
- [ ] Verify output is 16kHz/16-bit via `ffprobe` or Audacity
- [ ] Test `split` mode — produces two separate files

**Progress:** `[x] Complete`

---

### Phase 4: Chunking

> **Risk: Low** | **Effort: Low** | **Standalone: Yes**

Time-based buffer rotation — no new external deps.

**Implementation:**
1. Add time tracking to the audio buffer
2. When chunk duration reached, finalize current WAV, start new buffer
3. Support `--chunk-duration=N` (seconds, default 30)
4. Support `--overlap=N` (seconds, default 0) — keep last N seconds in next buffer
5. Write chunks to `<output-dir>/audio/<date>/HH-MM-SS.wav`

**Verification:**
- [ ] Run for 90s with `--chunk-duration=30` — get 3 WAV files
- [ ] Each file is ~30s duration
- [ ] File names reflect correct timestamps
- [ ] Test `--overlap=5` — each chunk starts 5s before previous ended
- [ ] Test `--chunk-duration=10` — get 9 files in 90s

**Progress:** `[x] Complete`

---

### Phase 5: Transcription API Client

> **Risk: Low** | **Effort: Low** | **Standalone: Yes** | **Parallelizable with Phases 1-4**

Can be developed and tested entirely with pre-recorded WAV files — no audio capture needed.

**Implementation:**
1. Add deps: `reqwest` (multipart, json), `tokio`, `serde`, `serde_json`
2. Build multipart POST to `/v1/audio/transcriptions`
   - file: WAV chunk
   - model: `whisper-1`
   - response_format: `verbose_json`
3. Parse response into typed struct with segments
4. Retry logic: exponential backoff on 429/5xx, max 3 retries
5. Read `OPENAI_API_KEY` from env
6. Support `--api-url=URL` for custom endpoints

**Verification:**
- [ ] Send a WAV from Phase 4 to Whisper API — get valid transcript back
- [ ] Parse `verbose_json` response — segments have start/end/text
- [ ] Missing API key — clear error, exit code 1
- [ ] Bad API key — clear error after retry exhaustion
- [ ] Test with local Whisper server via `--api-url`

**Progress:** `[x] Complete`

---

### Phase 5b: Speaker-Attributed Transcription

> **Risk: Low** | **Effort: Low** | **Standalone: Yes**

Dual-channel transcription — transcribe system and mic WAVs independently, merge by timestamp. Speaker identity comes free from capture source (no ML diarization needed).

**Implementation:**
1. Add `timestamp_granularities[]=word` and `=segment` to API request
2. Add `Word { word, start, end }` struct, `words` field on `Transcript`
3. Add `SpeakerSegment` and `MergedTranscript` types
4. `merge_transcripts(system, mic)` — labels system `"other"`, mic `"you"`, attaches words to parent segments by time range, sorts by start time
5. Add `--transcribe-pair=SYSTEM.wav,MIC.wav` CLI flag
6. Overlapping speech interleaves naturally — no dedup needed

**Verification:**
- [x] `--transcribe-pair=system.wav,mic.wav` — merged JSON with speaker labels and interleaved segments
- [x] `--transcribe=chunk.wav` — still works, now includes `words` array
- [x] Word timestamps present when API supports it, empty array when not

**Progress:** `[x] Complete`

---

### Phase 6: Pipeline Integration

> **Risk: Medium** | **Effort: Medium** | **Standalone: Yes**

Wire everything together: chunks flow into async queue → transcription workers → filesystem.

**Implementation:**
1. Add deps: `chrono`, `dirs`
2. Create async channel between chunker and transcription workers
3. Spawn N workers (default 2, `--concurrency=N`)
4. Workers pull chunks, call API, write results to filesystem
5. Per-chunk JSON:
   ```json
   {
     "timestamp_start": "2024-01-15T14:30:00Z",
     "timestamp_end": "2024-01-15T14:30:30Z",
     "duration_seconds": 30,
     "text": "...",
     "segments": [{"start": 0.0, "end": 4.2, "text": "..."}],
     "audio_file": "audio/2024-01-15/14-30-00.wav"
   }
   ```
6. Append each result as line to `session.jsonl`
7. Directory structure: `~/audio-capture/transcripts/<date>/`
8. Support `--save-audio` to persist WAV chunks, otherwise delete after transcription
9. On API failure with `--save-audio`, keep the WAV for manual retry

**Verification:**
- [ ] Run full capture for 2 min — `transcripts/<date>/` has per-chunk JSON files
- [ ] `session.jsonl` has one line per chunk, valid JSON per line
- [ ] Timestamps in JSON match wall clock
- [ ] `--save-audio` — WAV files persist in `audio/<date>/`
- [ ] Without `--save-audio` — no WAV files remain
- [ ] Kill API mid-run — capture continues, failed chunks logged

**Progress:** `[ ] Not started`

---

### Phase 7: CLI + Config

> **Risk: Low** | **Effort: Low** | **Standalone: Yes**

**Implementation:**
1. Add deps: `clap` (derive), `toml`
2. Subcommands: `start`, `stop`, `status`, `list`, `export`
3. `start` flags:
   ```
   --chunk-duration=30      --overlap=0
   --no-mic                 --no-system
   --save-audio             --api-url=URL
   --concurrency=2          --output-dir=PATH
   --mix-mode=stereo|split  --device=NAME
   -v, --verbose
   ```
4. Load `~/.audio-capture/config.toml` if present
5. Merge: CLI flags > config file > defaults
6. `list` — show recent sessions (date, duration, chunk count)
7. `export --date=today --format=markdown` — concatenate transcripts

**Verification:**
- [ ] `scribe start --chunk-duration=15 --no-mic --save-audio -v` — all flags respected
- [ ] Config file sets `chunk_duration = 45` — used when no flag passed
- [ ] CLI flag overrides config file value
- [ ] `scribe list` — shows sessions
- [ ] `scribe export --date=today --format=markdown` — readable output
- [ ] `scribe start` with no config and no flags — sensible defaults

**Progress:** `[ ] Not started`

---

### Phase 8: Daemon Mode + Signal Handling

> **Risk: Medium** | **Effort: Medium** | **Standalone: Yes**

**Implementation:**
1. `start --daemon` — fork/daemonize, write PID to `~/.audio-capture/scribe.pid`
2. `stop` — read PID file, send SIGTERM
3. `status` — check if PID is alive
4. Signal handler (SIGTERM, SIGINT):
   - Stop audio capture
   - Flush current partial chunk (write WAV + transcribe)
   - Wait for in-flight transcription requests to complete (with timeout)
   - Clean up PID file
5. Handle stale PID files (process died without cleanup)

**Verification:**
- [ ] `scribe start --daemon` — process backgrounds, PID file created
- [ ] `scribe status` — reports running
- [ ] `scribe stop` — process terminates, PID file removed
- [ ] Ctrl+C in foreground mode — graceful shutdown, no half-written files
- [ ] Kill -9 then `scribe status` — detects stale PID, reports not running
- [ ] Shutdown flushes partial chunk — no audio data lost

**Progress:** `[ ] Not started`

---

## Phase Dependency Graph

```
Phase 1 (System Audio) ──┐
                          ├──► Phase 3 (Mixing) ──► Phase 4 (Chunking) ──┐
Phase 2 (Mic Capture) ───┘                                               │
                                                                          ├──► Phase 6 (Pipeline)
Phase 5 (API Client) ────────────────────────────────────────────────────┘        │
                                                                                   ▼
                                                                          Phase 7 (CLI + Config)
                                                                                   │
                                                                                   ▼
                                                                          Phase 8 (Daemon)
```

Phase 5 can be built in parallel with Phases 1-4.

---

## Progress Tracker

| Phase | Description | Status |
|-------|-------------|--------|
| 1 | System Audio Capture | `[x] Complete` |
| 2 | Microphone Capture | `[x] Complete` |
| 3 | Mixing + Resampling | `[x] Complete` |
| 4 | Chunking | `[x] Complete` |
| 5 | Transcription API Client | `[x] Complete` |
| 5b | Speaker-Attributed Transcription | `[x] Complete` |
| 6 | Pipeline Integration | `[ ] Not started` |
| 7 | CLI + Config | `[ ] Not started` |
| 8 | Daemon Mode + Signals | `[ ] Not started` |
