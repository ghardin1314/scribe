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

Wire everything together: chunker produces split WAV pairs → worker pool transcribes each channel → merge with speaker labels → filesystem.

**Implemented:**
- `--live` flag: forces split mode, creates mpsc channel between chunker and worker pool
- Worker pool (`--concurrency=N`, default 2) shares `Arc<Mutex<Receiver<ChunkPair>>>`
- Per-channel silence detection (RMS < -40 dBFS) — skips API call for silent channels
- Acoustic bleed dedup: strips runs of 3+ consecutive mic words matching system words at similar timestamps
- Per-chunk output: JSON + session.jsonl + session.md (markdown transcript)
- `--save-audio` preserves WAVs; default deletes after successful transcription
- Graceful shutdown: drop sender → workers drain queue → join all threads
- Failed chunks keep WAVs for `--transcribe-pair` retry

**Progress:** `[x] Complete`

---

### Phase 7: Default UX

> **Risk: Low** | **Effort: Low** | **Standalone: Yes**

Make live transcription the default experience. Running `scribe` with `OPENAI_API_KEY` set should just work — producing a clean markdown transcript in the current directory while intermediates go to a temp dir.

**Implementation:**
1. Auto-live: when `OPENAI_API_KEY` is set and mode is Both, enable live pipeline automatically
2. Default `output_dir` → `/tmp/scribe/` (intermediate WAVs, JSON, JSONL)
3. Markdown transcript → `./transcript-{date}.md` in current directory
4. `--output=PATH` to override markdown output location
5. Remove `--live` flag (now default behavior)
6. Add `--no-transcribe` to opt out of live transcription
7. Print transcript path on exit

**Verification:**
- [ ] `OPENAI_API_KEY=sk-... scribe` — captures, transcribes, writes `transcript-2026-02-15.md`
- [ ] Intermediates land in `/tmp/scribe/`, not current directory
- [ ] `scribe --no-transcribe` — capture only, WAVs in temp dir
- [ ] `scribe --output=meeting.md` — transcript written to custom path
- [ ] `scribe --save-audio --output-dir=./saved` — intermediates go to `./saved/`
- [ ] No API key set — capture only with message about setting key
- [ ] `--system` or `--mic` only — capture only, no pipeline

**Progress:** `[x] Complete`

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
| 6 | Pipeline Integration | `[x] Complete` |
| 7 | Default UX | `[x] Complete` |
| 8 | Daemon Mode + Signals | `[ ] Not started` |
