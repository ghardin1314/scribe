mod audio;
mod capture;
mod chunker;
mod mixer;
mod pipeline;
mod transcribe;

use capture::{Capture, MicCapture, SystemCapture};
use chunker::ChunkConfig;
use mixer::MixMode;
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
    live: bool,
    save_audio: bool,
    concurrency: usize,
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

    let output_dir = args
        .iter()
        .find_map(|a| a.strip_prefix("--output-dir="))
        .unwrap_or("output")
        .to_string();

    let live = args.iter().any(|a| a == "--live");
    let save_audio = args.iter().any(|a| a == "--save-audio");

    let concurrency = args
        .iter()
        .find_map(|a| a.strip_prefix("--concurrency="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);

    Config { mode, chunk_duration, overlap, output_dir, live, save_audio, concurrency }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if let Some(pair) = args.iter().find_map(|a| a.strip_prefix("--transcribe-pair=")) {
        return run_transcribe_pair(pair, &args);
    }

    if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--transcribe=")) {
        return run_transcribe(path, &args);
    }

    let config = parse_config();

    if config.live {
        match &config.mode {
            CaptureMode::System | CaptureMode::Mic => {
                return Err("--live requires both system + mic (don't use --system or --mic)".into());
            }
            _ => {}
        }
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Validate API key early in live mode
    let live_transcribe_config = if config.live {
        Some(transcribe_config(&args)?)
    } else {
        None
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
                    // Live mode: split + transcription pipeline
                    let live_mode = &MixMode::Split;
                    let (tx, rx) = std::sync::mpsc::channel();
                    let pipeline_config = pipeline::PipelineConfig {
                        transcribe: tc,
                        output_dir: config.output_dir.clone(),
                        concurrency: config.concurrency,
                        save_audio: config.save_audio,
                    };
                    let handles = pipeline::run(rx, pipeline_config);

                    eprintln!("Live capture + transcription ({}s chunks, {} workers)... Ctrl+C to stop.",
                        chunk_config.chunk_duration, config.concurrency);
                    chunker::run_chunked_both(&system, &mic, live_mode, &chunk_config, &running, Some(&tx))?;

                    drop(tx);
                    eprintln!("Waiting for transcription workers to finish...");
                    pipeline::shutdown(handles);
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
