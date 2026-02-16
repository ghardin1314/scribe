use screencapturekit::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

struct AudioHandler {
    tx: mpsc::Sender<Vec<f32>>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let Some(audio) = sample.audio_buffer_list() else {
            return;
        };

        let num_buffers = audio.num_buffers();

        if num_buffers == 1 {
            // Interleaved: one buffer, channels already interleaved (L,R,L,R,...)
            if let Some(buf) = audio.get(0) {
                let samples: Vec<f32> = buf
                    .data()
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let _ = self.tx.send(samples);
            }
        } else {
            // Non-interleaved: one buffer per channel, must interleave for WAV
            let channels: Vec<Vec<f32>> = audio
                .iter()
                .map(|buf| {
                    buf.data()
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect()
                })
                .collect();

            if let Some(frame_count) = channels.first().map(|c| c.len()) {
                let mut interleaved = Vec::with_capacity(frame_count * channels.len());
                for i in 0..frame_count {
                    for ch in &channels {
                        interleaved.push(ch.get(i).copied().unwrap_or(0.0));
                    }
                }
                let _ = self.tx.send(interleaved);
            }
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        eprintln!();
        eprintln!("If this is a permission error, enable Screen Recording:");
        eprintln!("  System Settings → Privacy & Security → Screen & System Audio Recording");
        eprintln!("  Add your terminal app, then restart.");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let content = SCShareableContent::get()?;
    let display = content
        .displays()
        .into_iter()
        .next()
        .ok_or("No display found")?;

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build();

    let config = SCStreamConfiguration::new()
        .with_width(2)
        .with_height(2)
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(SAMPLE_RATE as i32)
        .with_channel_count(CHANNELS as i32);

    let (tx, rx) = mpsc::channel();
    let handler = AudioHandler { tx };

    let mut stream = SCStream::new(&filter, &config);
    stream.add_output_handler(handler, SCStreamOutputType::Audio);
    stream.start_capture()?;

    eprintln!("Capturing system audio... Press Ctrl+C to stop.");

    let mut all_samples: Vec<f32> = Vec::new();
    let start = Instant::now();
    let mut last_report = Instant::now();

    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => {
                all_samples.extend(samples);

                if last_report.elapsed() >= Duration::from_secs(5) {
                    let frames = all_samples.len() / CHANNELS as usize;
                    let dur = frames as f64 / SAMPLE_RATE as f64;
                    eprintln!("  {dur:.1}s captured...");
                    last_report = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    eprintln!("Stopping capture...");
    stream.stop_capture()?;

    // Drain any remaining samples in the channel
    while let Ok(samples) = rx.try_recv() {
        all_samples.extend(samples);
    }

    if all_samples.is_empty() {
        eprintln!("No audio captured.");
        return Ok(());
    }

    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let output_path = "output.wav";
    let mut writer = hound::WavWriter::create(output_path, spec)?;
    for &sample in &all_samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    let frames = all_samples.len() / CHANNELS as usize;
    let duration_secs = frames as f64 / SAMPLE_RATE as f64;
    let elapsed = start.elapsed();
    eprintln!("Wrote {duration_secs:.1}s of audio to {output_path} (captured in {elapsed:.1?})");

    Ok(())
}
