mod audio;
mod capture;
mod chunker;
mod local;
mod mixer;
mod pipeline;
mod transcribe;

use capture::{Capture, MicCapture, SystemCapture};
use chunker::ChunkConfig;
use mixer::MixMode;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

const TARGET_RATE: u32 = 16000;

enum CaptureMode {
    System,
    Mic,
    Both(MixMode),
}

struct Config {
    mode: CaptureMode,
    chunk_duration: u32,
    overlap: u32,
    output_dir: String,
    output: Option<String>,
    no_transcribe: bool,
    save_audio: bool,
    concurrency: usize,
    local_port: Option<u16>,
}

fn parse_config() -> Config {
    let args: Vec<String> = std::env::args().collect();

    let mode = if args.iter().any(|a| a == "--system") {
        CaptureMode::System
    } else if args.iter().any(|a| a == "--mic") {
        CaptureMode::Mic
    } else {
        let mix_mode = if args.iter().any(|a| a == "--mix-mode=split") {
            MixMode::Split
        } else {
            MixMode::Stereo
        };
        CaptureMode::Both(mix_mode)
    };

    let chunk_duration = args
        .iter()
        .find_map(|a| a.strip_prefix("--chunk-duration="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    let overlap = args
        .iter()
        .find_map(|a| a.strip_prefix("--overlap="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let default_output_dir = std::env::temp_dir()
        .join("scribe")
        .to_string_lossy()
        .to_string();

    let output_dir = args
        .iter()
        .find_map(|a| a.strip_prefix("--output-dir="))
        .unwrap_or(&default_output_dir)
        .to_string();

    // Positional arg (first non-flag) or --output= sets transcript path
    let output = args
        .iter()
        .find_map(|a| a.strip_prefix("--output="))
        .map(|s| s.to_string())
        .or_else(|| {
            args.iter()
                .skip(1)
                .find(|a| !a.starts_with("--"))
                .cloned()
        });

    let no_transcribe = args.iter().any(|a| a == "--no-transcribe");
    let save_audio = args.iter().any(|a| a == "--save-audio");

    let concurrency = args
        .iter()
        .find_map(|a| a.strip_prefix("--concurrency="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    let local_port = args
        .iter()
        .find_map(|a| a.strip_prefix("--local-port="))
        .and_then(|v| v.parse().ok());

    Config { mode, chunk_duration, overlap, output_dir, output, no_transcribe, save_audio, concurrency, local_port }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    eprintln!("scribe — capture system audio + mic, transcribe locally

USAGE:
    scribe [FILE] [OPTIONS]

By default, captures both channels and transcribes using a local
whisper server. Falls back to OpenAI API if OPENAI_API_KEY is set.
Writes transcript to ./transcript-{{date}}.md

OPTIONS:
    FILE                   Transcript output path (positional arg)
    --output=PATH          Same as above, as a flag (default: transcript-{{date}}.md)
    --output-dir=PATH      Intermediate files directory (default: /tmp/scribe)
    --chunk-duration=N     Chunk length in seconds (default: 30)
    --overlap=N            Overlap between chunks in seconds (default: 0)
    --concurrency=N        Transcription worker threads (default: 2)
    --model=NAME           Local whisper model size (default: medium)
    --local-port=N         Local whisper server port (default: 8080)
    --save-audio           Keep WAV files after transcription
    --no-transcribe        Capture only, no transcription
    --system               Capture system audio only
    --mic                  Capture microphone only
    --api-url=URL          Custom transcription API endpoint
    --transcribe=FILE      Transcribe a single WAV file
    --transcribe-pair=S,M  Transcribe a system,mic WAV pair
    -h, --help             Show this help");
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    if let Some(pair) = args.iter().find_map(|a| a.strip_prefix("--transcribe-pair=")) {
        return run_transcribe_pair(pair, &args);
    }

    if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--transcribe=")) {
        return run_transcribe(path, &args);
    }

    let config = parse_config();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Resolve transcription backend: --api-url → local (default) → recording only
    let has_api_url = args.iter().any(|a| a.starts_with("--api-url="));
    let _local_server;
    let live_transcribe_config;

    if config.no_transcribe || !matches!(&config.mode, CaptureMode::Both(_)) {
        _local_server = None;
        live_transcribe_config = None;
    } else if has_api_url {
        // Explicit --api-url: use remote API
        _local_server = None;
        live_transcribe_config = Some(transcribe_config(&args)?);
    } else {
        // Default: local whisper server
        let model = args
            .iter()
            .find_map(|a| a.strip_prefix("--model="))
            .unwrap_or("medium");
        match local::LocalServer::start(model, config.local_port) {
            Ok(server) => {
                let tc = transcribe::TranscribeConfig {
                    api_key: String::new(),
                    api_url: server.api_url(),
                    model: String::new(),
                };
                _local_server = Some(server);
                live_transcribe_config = Some(tc);
            }
            Err(e) => {
                eprintln!("No transcription available — recording only");
                eprintln!("  {e}");
                _local_server = None;
                live_transcribe_config = None;
            }
        }
    };

    let start = Instant::now();

    if config.chunk_duration > 0 {
        let chunk_config = ChunkConfig {
            chunk_duration: config.chunk_duration,
            overlap: config.overlap,
            output_dir: config.output_dir.clone(),
        };

        match config.mode {
            CaptureMode::System => {
                let cap = SystemCapture::new()?;
                cap.start()?;
                eprintln!("Capturing system audio ({}s chunks)... Ctrl+C to stop.", chunk_config.chunk_duration);
                chunker::run_chunked_single(&cap, "system", &chunk_config, &running)?;
                cap.stop()?;
            }
            CaptureMode::Mic => {
                let cap = MicCapture::new()?;
                cap.start()?;
                eprintln!("Capturing microphone ({}s chunks)... Ctrl+C to stop.", chunk_config.chunk_duration);
                chunker::run_chunked_single(&cap, "mic", &chunk_config, &running)?;
                cap.stop()?;
            }
            CaptureMode::Both(ref mix_mode) => {
                let system = SystemCapture::new()?;
                let mic = MicCapture::new()?;
                system.start()?;
                mic.start()?;

                if let Some(tc) = live_transcribe_config {
                    let live_mode = &MixMode::Split;
                    let (tx, rx) = std::sync::mpsc::channel();

                    let (date, _) = chunker::local_timestamp();
                    let transcript_path = match &config.output {
                        Some(p) => PathBuf::from(p),
                        None => PathBuf::from(format!("transcript-{date}.md")),
                    };

                    let pipeline_config = pipeline::PipelineConfig {
                        transcribe: tc,
                        output_dir: config.output_dir.clone(),
                        transcript_path: transcript_path.clone(),
                        concurrency: config.concurrency,
                        save_audio: config.save_audio,
                    };
                    let handles = pipeline::run(rx, pipeline_config);

                    eprintln!("Transcribing to: {}", transcript_path.display());
                    eprintln!("Capturing ({}s chunks, {} workers)... Ctrl+C to stop.",
                        chunk_config.chunk_duration, config.concurrency);
                    chunker::run_chunked_both(&system, &mic, live_mode, &chunk_config, &running, Some(&tx))?;

                    drop(tx);
                    eprintln!("Waiting for transcription workers to finish...");
                    pipeline::shutdown(handles);

                    eprintln!("Transcript: {}", transcript_path.display());
                } else {
                    eprintln!("Capturing system + mic ({}s chunks)... Ctrl+C to stop.", chunk_config.chunk_duration);
                    chunker::run_chunked_both(&system, &mic, mix_mode, &chunk_config, &running, None)?;
                }

                system.stop()?;
                mic.stop()?;
            }
        }
    } else {
        match config.mode {
            CaptureMode::System => {
                run_single(
                    Box::new(SystemCapture::new()?),
                    "system audio",
                    "output.wav",
                    &running,
                )?;
            }
            CaptureMode::Mic => {
                run_single(
                    Box::new(MicCapture::new()?),
                    "microphone",
                    "output_mic.wav",
                    &running,
                )?;
            }
            CaptureMode::Both(mix_mode) => {
                run_both(mix_mode, &running)?;
            }
        }
    }

    let elapsed = start.elapsed();
    eprintln!("Done in {elapsed:.1?}");

    Ok(())
}

fn run_single(
    capture: Box<dyn Capture>,
    label: &str,
    path: &str,
    running: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    capture.start()?;
    eprintln!("Capturing {label}... Press Ctrl+C to stop.");

    let rx = capture.rx();
    let rate = capture.sample_rate();
    let channels = capture.channels();

    let mut samples: Vec<f32> = Vec::new();
    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(chunk) => samples.extend(chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    while let Ok(chunk) = rx.try_recv() {
        samples.extend(chunk);
    }

    eprintln!("Stopping capture...");
    capture.stop()?;

    let mono = mixer::to_mono(&samples, channels);
    let resampled = mixer::resample(&mono, rate, TARGET_RATE);
    let pcm = mixer::f32_to_i16(&resampled);
    audio::write_wav_i16(path, &pcm, TARGET_RATE, 1)?;

    Ok(())
}

fn run_both(
    mix_mode: MixMode,
    running: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    let system = SystemCapture::new()?;
    let mic = MicCapture::new()?;

    let sys_rate = system.sample_rate();
    let sys_ch = system.channels();
    let mic_rate = mic.sample_rate();
    let mic_ch = mic.channels();

    system.start()?;
    mic.start()?;
    eprintln!("Capturing system audio + mic... Press Ctrl+C to stop.");

    // Inline dual capture loop
    let sys_rx = system.rx();
    let mic_rx = mic.rx();
    let mut sys_samples: Vec<f32> = Vec::new();
    let mut mic_samples: Vec<f32> = Vec::new();

    while running.load(Ordering::SeqCst) {
        let mut got_data = false;
        while let Ok(chunk) = sys_rx.try_recv() {
            sys_samples.extend(chunk);
            got_data = true;
        }
        while let Ok(chunk) = mic_rx.try_recv() {
            mic_samples.extend(chunk);
            got_data = true;
        }
        if !got_data {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    while let Ok(chunk) = sys_rx.try_recv() {
        sys_samples.extend(chunk);
    }
    while let Ok(chunk) = mic_rx.try_recv() {
        mic_samples.extend(chunk);
    }

    eprintln!("Stopping capture...");
    system.stop()?;
    mic.stop()?;

    let sys_mono = mixer::to_mono(&sys_samples, sys_ch);
    let mic_mono = mixer::to_mono(&mic_samples, mic_ch);
    let mut sys_resampled = mixer::resample(&sys_mono, sys_rate, TARGET_RATE);
    let mut mic_resampled = mixer::resample(&mic_mono, mic_rate, TARGET_RATE);

    mixer::peak_normalize(&mut sys_resampled, 0.9);
    mixer::peak_normalize(&mut mic_resampled, 0.9);

    match mix_mode {
        MixMode::Stereo => {
            let stereo = mixer::interleave_stereo(&sys_resampled, &mic_resampled);
            let pcm = mixer::f32_to_i16(&stereo);
            audio::write_wav_i16("output.wav", &pcm, TARGET_RATE, 2)?;
        }
        MixMode::Split => {
            let sys_pcm = mixer::f32_to_i16(&sys_resampled);
            let mic_pcm = mixer::f32_to_i16(&mic_resampled);
            audio::write_wav_i16("output_system.wav", &sys_pcm, TARGET_RATE, 1)?;
            audio::write_wav_i16("output_mic.wav", &mic_pcm, TARGET_RATE, 1)?;
        }
    }

    Ok(())
}

fn transcribe_config(args: &[String]) -> Result<transcribe::TranscribeConfig, Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY not set")?;

    let api_url = args
        .iter()
        .find_map(|a| a.strip_prefix("--api-url="))
        .unwrap_or("https://api.openai.com/v1/audio/transcriptions")
        .to_string();

    let model = args
        .iter()
        .find_map(|a| a.strip_prefix("--model="))
        .unwrap_or("whisper-1")
        .to_string();

    Ok(transcribe::TranscribeConfig { api_key, api_url, model })
}

fn run_transcribe(path: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let config = transcribe_config(args)?;
    let result = transcribe::transcribe(path, &config)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_transcribe_pair(pair: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (system_path, mic_path) = pair
        .split_once(',')
        .ok_or("--transcribe-pair expects SYSTEM.wav,MIC.wav")?;

    let config = transcribe_config(args)?;

    eprintln!("Transcribing system audio: {system_path}");
    let system = transcribe::transcribe(system_path, &config)?;

    eprintln!("Transcribing mic audio: {mic_path}");
    let mic = transcribe::transcribe(mic_path, &config)?;

    let merged = transcribe::merge_transcripts(Some(system), Some(mic));
    println!("{}", serde_json::to_string_pretty(&merged)?);
    Ok(())
}
