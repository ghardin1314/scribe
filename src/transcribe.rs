use serde::{Deserialize, Serialize};
use std::path::Path;
use std::thread;
use std::time::Duration;

pub struct TranscribeConfig {
    pub api_key: String,
    pub api_url: String,
    pub model: String,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            api_url: "https://api.openai.com/v1/audio/transcriptions".to_string(),
            model: "whisper-1".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Transcript {
    pub text: String,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Serialize)]
pub struct MergedTranscript {
    pub segments: Vec<SpeakerSegment>,
    pub duration: f64,
}

pub fn transcribe(
    path: &str,
    config: &TranscribeConfig,
) -> Result<Transcript, Box<dyn std::error::Error>> {
    let file_path = Path::new(path);
    if !file_path.exists() {
        return Err(format!("file not found: {path}").into());
    }

    let file_bytes = std::fs::read(file_path)?;
    let file_name = file_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let client = reqwest::blocking::Client::new();
    let max_retries = 3;
    let mut attempt = 0;

    loop {
        let part = reqwest::blocking::multipart::Part::bytes(file_bytes.clone())
            .file_name(file_name.clone())
            .mime_str("audio/wav")?;

        let form = reqwest::blocking::multipart::Form::new()
            .part("file", part)
            .text("model", config.model.clone())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word")
            .text("timestamp_granularities[]", "segment");

        let resp = client
            .post(&config.api_url)
            .bearer_auth(&config.api_key)
            .multipart(form)
            .send()?;

        let status = resp.status();

        if status.is_success() {
            let transcript: Transcript = resp.json()?;
            return Ok(transcript);
        }

        let body = resp.text().unwrap_or_default();

        let retryable = status.as_u16() == 429 || status.is_server_error();
        attempt += 1;

        if !retryable || attempt >= max_retries {
            return Err(format!("API error {status}: {body}").into());
        }

        let delay = Duration::from_secs(1 << attempt); // 2s, 4s
        eprintln!("Retrying in {}s (attempt {attempt}/{max_retries})...", delay.as_secs());
        thread::sleep(delay);
    }
}

pub fn merge_transcripts(system: Transcript, mic: Transcript) -> MergedTranscript {
    let duration = system.duration.max(mic.duration);

    let to_speaker_segments = |t: Transcript, speaker: &str| -> Vec<SpeakerSegment> {
        let words = t.words;
        t.segments
            .into_iter()
            .map(|seg| {
                let seg_words: Vec<Word> = words
                    .iter()
                    .filter(|w| w.start >= seg.start && w.end <= seg.end)
                    .cloned()
                    .collect();
                SpeakerSegment {
                    speaker: speaker.to_string(),
                    start: seg.start,
                    end: seg.end,
                    text: seg.text,
                    words: seg_words,
                }
            })
            .collect()
    };

    let mut segments = to_speaker_segments(system, "other");
    segments.extend(to_speaker_segments(mic, "you"));
    segments.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());

    MergedTranscript { segments, duration }
}
