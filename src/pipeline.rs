use crate::transcribe::{self, SpeakerSegment, TranscribeConfig};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

pub struct ChunkPair {
    pub timestamp: String,
    pub date: String,
    pub system_path: PathBuf,
    pub mic_path: PathBuf,
}

pub struct PipelineConfig {
    pub transcribe: TranscribeConfig,
    pub output_dir: String,
    pub transcript_path: PathBuf,
    pub concurrency: usize,
    pub save_audio: bool,
}

#[derive(Serialize)]
struct AudioFiles {
    system: String,
    mic: String,
}

#[derive(Serialize)]
struct ChunkResult {
    timestamp_start: String,
    timestamp_end: String,
    duration_seconds: f64,
    segments: Vec<SpeakerSegment>,
    audio_files: AudioFiles,
}

pub fn run(rx: Receiver<ChunkPair>, config: PipelineConfig) -> Vec<JoinHandle<()>> {
    let rx = Arc::new(Mutex::new(rx));
    let config = Arc::new(config);
    let mut handles = Vec::with_capacity(config.concurrency);

    for i in 0..config.concurrency {
        let rx = Arc::clone(&rx);
        let config = Arc::clone(&config);
        handles.push(thread::spawn(move || worker(i, rx, config)));
    }

    handles
}

fn worker(id: usize, rx: Arc<Mutex<Receiver<ChunkPair>>>, config: Arc<PipelineConfig>) {
    loop {
        let pair = {
            let lock = rx.lock().unwrap();
            lock.recv()
        };

        let pair = match pair {
            Ok(p) => p,
            Err(_) => break, // channel closed
        };

        eprintln!("[worker {id}] transcribing chunk {}", pair.timestamp);

        if let Err(e) = process_chunk(&pair, &config) {
            eprintln!("[worker {id}] error processing {}: {e}", pair.timestamp);
            // keep WAVs for --transcribe-pair retry
            continue;
        }

        if !config.save_audio {
            let _ = fs::remove_file(&pair.system_path);
            let _ = fs::remove_file(&pair.mic_path);
        }
    }
}

/// RMS silence threshold — below this, skip transcription for a channel.
/// -40 dBFS ≈ 0.01 RMS, a reasonable floor for "no real audio."
const SILENCE_RMS_THRESHOLD: f64 = 0.01;

fn is_silent(path: &PathBuf) -> bool {
    let reader = match hound::WavReader::open(path) {
        Ok(r) => r,
        Err(_) => return false, // can't read → not silent, let transcribe handle the error
    };

    let mut sum_sq: f64 = 0.0;
    let mut count: u64 = 0;
    for sample in reader.into_samples::<i16>() {
        if let Ok(s) = sample {
            let f = s as f64 / i16::MAX as f64;
            sum_sq += f * f;
            count += 1;
        }
    }

    if count == 0 {
        return true;
    }

    let rms = (sum_sq / count as f64).sqrt();
    rms < SILENCE_RMS_THRESHOLD
}

fn process_chunk(
    pair: &ChunkPair,
    config: &PipelineConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let sys_path_str = pair.system_path.to_str().unwrap();
    let mic_path_str = pair.mic_path.to_str().unwrap();

    let sys_silent = is_silent(&pair.system_path);
    let mic_silent = is_silent(&pair.mic_path);

    if sys_silent && mic_silent {
        eprintln!("  both channels silent, skipping");
        return Ok(());
    }

    let system = if sys_silent {
        eprintln!("  system channel silent, skipping");
        None
    } else {
        Some(transcribe::transcribe(sys_path_str, &config.transcribe)?)
    };

    let mic = if mic_silent {
        eprintln!("  mic channel silent, skipping");
        None
    } else {
        Some(transcribe::transcribe(mic_path_str, &config.transcribe)?)
    };

    let merged = transcribe::merge_transcripts(system, mic);

    let (_, end_time) = crate::chunker::local_timestamp();

    let result = ChunkResult {
        timestamp_start: pair.timestamp.clone(),
        timestamp_end: end_time,
        duration_seconds: merged.duration,
        segments: merged.segments,
        audio_files: AudioFiles {
            system: pair.system_path.to_string_lossy().to_string(),
            mic: pair.mic_path.to_string_lossy().to_string(),
        },
    };

    // Write individual chunk JSON
    let transcript_dir = PathBuf::from(&config.output_dir)
        .join("transcripts")
        .join(&pair.date);
    fs::create_dir_all(&transcript_dir)?;

    let json_path = transcript_dir.join(format!("{}.json", pair.timestamp));
    let json = serde_json::to_string_pretty(&result)?;
    fs::write(&json_path, &json)?;

    // Append to session.jsonl
    let jsonl_path = transcript_dir.join("session.jsonl");
    let line = serde_json::to_string(&result)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl_path)?;
    writeln!(file, "{line}")?;

    // Append to transcript markdown
    append_markdown(&config.transcript_path, &result)?;

    eprintln!("  wrote {}", json_path.display());
    Ok(())
}

fn format_timestamp(ts: &str) -> String {
    ts.replace('-', ":")
}

fn format_time(seconds: f64) -> String {
    let m = (seconds / 60.0) as u32;
    let s = (seconds % 60.0) as u32;
    if m > 0 {
        format!("{m}:{s:02}")
    } else {
        format!("{s}s")
    }
}

fn append_markdown(path: &PathBuf, result: &ChunkResult) -> Result<(), Box<dyn std::error::Error>> {
    let is_new = !path.exists() || fs::metadata(path).map_or(true, |m| m.len() == 0);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    if is_new {
        let (date_str, _) = crate::chunker::local_timestamp();
        writeln!(file, "# Transcript — {date_str}\n")?;
    }

    let start = format_timestamp(&result.timestamp_start);
    let end = format_timestamp(&result.timestamp_end);
    let dur = format_time(result.duration_seconds);
    writeln!(file, "## {start} — {end} ({dur})\n")?;

    // Merge consecutive same-speaker segments
    let mut merged: Vec<(&str, f64, String)> = Vec::new();
    for seg in &result.segments {
        if let Some(last) = merged.last_mut() {
            if last.0 == seg.speaker {
                last.2.push_str(&seg.text);
                continue;
            }
        }
        merged.push((&seg.speaker, seg.start, seg.text.clone()));
    }

    for (speaker, start, text) in &merged {
        let label = match speaker.as_ref() {
            "you" => "You",
            "other" => "Other",
            s => s,
        };
        let ts = format_time(*start);
        writeln!(file, "> **{label}** ({ts}): {}\n", text.trim())?;
    }

    writeln!(file, "---\n")?;
    Ok(())
}

pub fn shutdown(handles: Vec<JoinHandle<()>>) {
    for h in handles {
        let _ = h.join();
    }
}
