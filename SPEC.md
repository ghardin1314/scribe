# PRD: Local Audio Capture & Transcription CLI

## Overview

Build a macOS command-line tool that captures system audio and microphone input, chunks the audio into segments, sends chunks to a transcription API, and stores transcripts to the local filesystem.

**Target user:** Single developer (me), running locally on macOS 13+.

**Language:** Rust

---

## Goals

1. Capture all system audio output (Zoom, Meet, browser, any app)
2. Simultaneously capture microphone input
3. Chunk audio into configurable segments (default: 30 seconds)
4. Send chunks to a transcription API (Whisper API initially)
5. Save transcripts with timestamps to local files
6. Run as a background CLI process with simple start/stop controls

---

## Technical Requirements

### Platform
- macOS 13.0+ (Ventura or later)
- Apple Silicon and Intel support
- No Xcode IDE required (Xcode Command Line Tools only)

### Dependencies
- Use `screencapturekit` Rust crate for system audio capture
- Use `cpal` or system APIs for microphone capture
- Use `reqwest` for HTTP API calls
- Use `hound` or similar for audio encoding

### Permissions
- Requires "Screen Recording" permission for system audio (ScreenCaptureKit)
- Requires "Microphone" permission for mic input
- On first run, prompt user to grant permissions or display instructions

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

## Features

### 1. Audio Capture

**System Audio:**
- Use ScreenCaptureKit to capture all system audio output
- Do not require user to configure audio routing or install virtual devices
- Handle case where no audio is playing (silence)

**Microphone:**
- Capture from default input device
- Allow specifying device by name via config/flag
- Support disabling mic capture via flag (`--no-mic`)

**Mixing:**
- Mix system audio and mic into single stereo stream (system L, mic R) OR
- Keep as separate channels for better speaker diarization
- Make this configurable (`--mix-mode=stereo|split`)

**Format:**
- Internal format: 16kHz, 16-bit PCM (Whisper-optimal)
- Resample from native rates as needed

### 2. Chunking

- Default chunk duration: 30 seconds
- Configurable via `--chunk-duration=N` (in seconds)
- Overlap option for better transcription continuity: `--overlap=N` (default: 0)
- Emit chunk immediately when duration reached (don't wait for silence)

### 3. Transcription

**API Support:**
- Primary: OpenAI Whisper API (`/v1/audio/transcriptions`)
- API key via environment variable: `OPENAI_API_KEY`
- Configurable endpoint for local Whisper servers: `--api-url=URL`

**Request:**
```
POST /v1/audio/transcriptions
Content-Type: multipart/form-data

file: <audio chunk as WAV or MP3>
model: "whisper-1"
response_format: "verbose_json"  (to get timestamps)
```

**Retry Logic:**
- Retry transient failures (429, 5xx) with exponential backoff
- Max 3 retries per chunk
- Log failures but don't crash; continue capturing

**Concurrency:**
- Process chunks in order but allow N concurrent API calls (default: 2)
- `--concurrency=N` flag

### 4. Storage

**Directory Structure:**
```
~/audio-capture/
├── config.toml           # Optional config file
├── transcripts/
│   └── 2024-01-15/
│       ├── 14-30-00.json      # Start time of chunk
│       ├── 14-30-30.json
│       └── session.jsonl      # Append-only full session log
└── audio/                     # Optional, if --save-audio
    └── 2024-01-15/
        ├── 14-30-00.wav
        └── 14-30-30.wav
```

**Transcript Format (per-chunk JSON):**
```json
{
  "timestamp_start": "2024-01-15T14:30:00Z",
  "timestamp_end": "2024-01-15T14:30:30Z",
  "duration_seconds": 30,
  "text": "Full transcript text here...",
  "segments": [
    {
      "start": 0.0,
      "end": 4.2,
      "text": "Hello everyone, welcome to the meeting."
    }
  ],
  "audio_file": "audio/2024-01-15/14-30-00.wav"  // If saved
}
```

**Session Log (JSONL):**
- Append each chunk result as single line to `session.jsonl`
- Allows streaming reads and easy concatenation

### 5. CLI Interface

**Commands:**
```bash
# Start capturing (foreground)
audio-capture start

# Start capturing (background daemon)
audio-capture start --daemon

# Stop daemon
audio-capture stop

# Show status
audio-capture status

# List recent sessions
audio-capture list

# Concatenate today's transcripts
audio-capture export --date=today --format=markdown
```

**Start Flags:**
```
--chunk-duration=30      Chunk length in seconds
--overlap=0              Overlap between chunks in seconds
--no-mic                 Disable microphone capture
--no-system              Disable system audio capture
--save-audio             Save audio chunks to disk
--api-url=URL            Custom transcription API endpoint
--concurrency=2          Max concurrent API calls
--output-dir=PATH        Override output directory
--mix-mode=stereo|split  Audio channel mixing mode
--device=NAME            Microphone device name
-v, --verbose            Verbose logging
```

### 6. Configuration File

Support optional `~/.audio-capture/config.toml`:
```toml
[capture]
chunk_duration = 30
overlap = 0
save_audio = false
mix_mode = "stereo"

[transcription]
api_url = "https://api.openai.com/v1/audio/transcriptions"
model = "whisper-1"
concurrency = 2

[storage]
output_dir = "~/audio-capture"
```

CLI flags override config file values.

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Permission denied (Screen Recording) | Print instructions to enable, exit with code 1 |
| Permission denied (Microphone) | If `--no-mic`, continue; otherwise print instructions, exit |
| API key missing | Print error, exit with code 1 |
| API call fails (transient) | Retry with backoff, log warning |
| API call fails (permanent) | Log error, save audio chunk if `--save-audio`, continue |
| Disk full | Log error, stop capture gracefully |
| Audio device disconnected | Log warning, attempt to reconnect; if mic, continue with system only |

---

## Out of Scope (v1)

- GUI or menu bar app
- Real-time streaming transcription (we batch in chunks)
- Speaker diarization (may add later)
- Local Whisper inference (just API for now; user can point to local server)
- Cross-platform support (macOS only)
- Audio playback or review
- Automatic meeting detection
- Calendar integration

---

## Success Criteria

1. Can start capture with single command
2. System audio from Zoom/Meet/browser is captured without user configuring audio routing
3. Transcripts appear in filesystem within ~60 seconds of speech
4. Process runs stably for 2+ hour sessions without memory leaks
5. Graceful handling of API failures doesn't lose audio (when `--save-audio` enabled)

---

## Development Notes

### Getting Started
```bash
# Install Xcode CLI tools (required for Apple frameworks)
xcode-select --install

# Create project
cargo new audio-capture
cd audio-capture

# Key dependencies
cargo add screencapturekit
cargo add cpal
cargo add reqwest --features multipart,json
cargo add tokio --features full
cargo add hound
cargo add serde --features derive
cargo add serde_json
cargo add toml
cargo add clap --features derive
cargo add chrono
cargo add dirs
```

### Permission Prompts
On first run using ScreenCaptureKit, macOS will prompt for Screen Recording permission. The binary must be run from Terminal (or the terminal app must have permission). Guide user to:
1. System Settings → Privacy & Security → Screen Recording
2. Add Terminal (or the built binary)
3. Restart the capture process

### Testing
- Test with various audio sources: Zoom, YouTube in browser, Music app
- Test mic capture with headset and built-in mic
- Test long-running sessions (2+ hours)
- Test API failure scenarios with mock server