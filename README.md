# scribe

> **Warning: This project is fully vibecoded.** Built entirely through AI-assisted development. There are no tests. It works on my machine. YMMV.

Local audio capture and transcription for macOS. Records system audio + microphone, transcribes with speaker attribution, and writes a clean markdown transcript.

```
scribe
# → captures audio, transcribes locally, writes transcript-2026-02-15.md
```

## Prerequisites

- **macOS 13+** (Ventura or later)
- **Xcode Command Line Tools** — `xcode-select --install`
- **Rust** — `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **whisper.cpp** (for local transcription) — `brew install whisper-cpp`
- **Whisper model file:**
  ```bash
  mkdir -p ~/.cache/whisper
  curl -L -o ~/.cache/whisper/ggml-medium.bin \
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin"
  ```

### macOS Permissions

On first run, macOS will prompt for:
- **Screen Recording** — required for system audio capture (ScreenCaptureKit)
- **Microphone** — required for mic capture

Grant both in System Settings > Privacy & Security.

## Install

```bash
cargo install --path .
```

## Usage

```bash
scribe                              # capture + transcribe, writes transcript-{date}.md
scribe --output=meeting.md          # custom output path
scribe --chunk-duration=15          # shorter chunks (default: 30s)
scribe --no-transcribe              # capture only, no transcription
scribe --save-audio                 # keep WAV files after transcription
scribe --system                     # system audio only
scribe --mic                        # microphone only
```

### Transcription backends

**Local (default):** Automatically starts a local `whisper-server`, transcribes on-device. No API key needed.

```bash
scribe                              # uses whisper-server + medium model
scribe --model=small                # faster, less accurate
scribe --model=large                # slower, more accurate
```

**OpenAI API:** If `OPENAI_API_KEY` is set, uses the OpenAI Whisper API instead of local.

```bash
export OPENAI_API_KEY=sk-...
scribe                              # uses OpenAI API
scribe --api-url=http://localhost:8000/v1/audio/transcriptions  # custom endpoint
```

### Offline transcription

Transcribe existing WAV files without capturing:

```bash
scribe --transcribe=recording.wav
scribe --transcribe-pair=system.wav,mic.wav
```

## Output

The primary output is a markdown file with speaker-attributed segments:

```markdown
# Transcript — 2026-02-15

## 14:30:00 — 14:30:05 (30s)

> **Other** (0s): So the key insight here is that we need to refactor the auth module.

> **You** (3s): Right, and we should probably add integration tests for that.

---
```

- **You** = microphone (your voice)
- **Other** = system audio (meeting participants, videos, etc.)

Intermediate files (per-chunk JSON, session.jsonl) go to `/tmp/scribe/` by default. Override with `--output-dir=PATH`.

## How it works

1. Captures system audio (ScreenCaptureKit) and microphone (CoreAudio) simultaneously
2. Chunks audio into 30s segments, writes split WAV pairs (system + mic)
3. Worker pool transcribes each channel independently via Whisper
4. Merges transcripts with speaker labels, sorted by timestamp
5. Strips acoustic bleed (mic picking up speakers) via word-level dedup
6. Skips silent channels to save processing time
7. Appends each chunk to the markdown transcript incrementally

## All options

```
scribe --help
```
