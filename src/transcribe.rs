use serde::{Deserialize, Serialize};
use std::path::Path;
use std::thread;
use std::time::Duration;

#[derive(Clone)]
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
    #[serde(default)]
    pub words: Vec<Word>,
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
            let mut transcript: Transcript = resp.json()?;
            // whisper.cpp nests words inside segments; OpenAI uses top-level words.
            // Normalize: if top-level words is empty, flatten from segments.
            if transcript.words.is_empty() {
                transcript.words = transcript
                    .segments
                    .iter()
                    .flat_map(|s| s.words.iter().cloned())
                    .collect();
            }
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

fn normalize_word(w: &str) -> String {
    w.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Remove mic words that are acoustic bleed from system audio.
/// Finds runs of 3+ consecutive mic words matching system words at similar
/// timestamps and strips them, preserving genuine user speech.
fn dedup_bleed(system: &Transcript, mic: &mut Transcript) {
    const TIME_TOLERANCE: f64 = 1.0;
    const MIN_RUN: usize = 3;

    if system.words.is_empty() || mic.words.is_empty() {
        return;
    }

    // Check each mic word against system words
    let matches: Vec<bool> = mic.words.iter().map(|mw| {
        let norm = normalize_word(&mw.word);
        if norm.is_empty() {
            return false;
        }
        system.words.iter().any(|sw| {
            normalize_word(&sw.word) == norm
                && (mw.start - sw.start).abs() < TIME_TOLERANCE
        })
    }).collect();

    // Mark runs of MIN_RUN+ consecutive matches for removal
    let mut to_remove = vec![false; mic.words.len()];
    let mut i = 0;
    while i < matches.len() {
        if matches[i] {
            let run_start = i;
            while i < matches.len() && matches[i] {
                i += 1;
            }
            if i - run_start >= MIN_RUN {
                for j in run_start..i {
                    to_remove[j] = true;
                }
            }
        } else {
            i += 1;
        }
    }

    // Filter top-level words, collecting removed timestamps
    let mut removed_times: Vec<(f64, f64)> = Vec::new();
    let mut idx = 0;
    mic.words.retain(|w| {
        let keep = !to_remove[idx];
        if !keep {
            removed_times.push((w.start, w.end));
        }
        idx += 1;
        keep
    });

    // Rebuild segments: trim text covered by removed words
    for seg in &mut mic.segments {
        // Check how much of this segment's time range was removed
        let bleed_coverage: f64 = removed_times.iter()
            .filter(|(s, e)| *s >= seg.start && *e <= seg.end)
            .map(|(s, e)| e - s)
            .sum();
        let seg_duration = seg.end - seg.start;

        if seg_duration > 0.0 && bleed_coverage / seg_duration > 0.8 {
            // Mostly bleed — drop entire segment
            seg.text.clear();
        } else {
            // Rebuild text from remaining words in this segment's range
            let remaining: Vec<&str> = mic.words.iter()
                .filter(|w| w.start >= seg.start && w.end <= seg.end)
                .map(|w| w.word.trim())
                .collect();
            if remaining.is_empty() {
                seg.text.clear();
            } else {
                seg.text = remaining.join(" ");
            }
        }
    }

    mic.segments.retain(|seg| !seg.text.is_empty());
}

pub fn merge_transcripts(system: Option<Transcript>, mic: Option<Transcript>) -> MergedTranscript {
    let sys_dur = system.as_ref().map_or(0.0, |t| t.duration);
    let mic_dur = mic.as_ref().map_or(0.0, |t| t.duration);
    let duration = sys_dur.max(mic_dur);

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

    // Dedup bleed from mic before merging
    let mut mic = mic;
    if let (Some(sys), Some(m)) = (&system, &mut mic) {
        dedup_bleed(sys, m);
    }

    let mut segments = Vec::new();
    if let Some(sys) = system {
        segments.extend(to_speaker_segments(sys, "other"));
    }
    if let Some(mic) = mic {
        segments.extend(to_speaker_segments(mic, "you"));
    }
    segments.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());

    MergedTranscript { segments, duration }
}
