use crate::capture::Capture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub fn capture_loop(capture: &dyn Capture, running: &AtomicBool) -> Vec<f32> {
    let rx = capture.rx();
    let sample_rate = capture.sample_rate();
    let channels = capture.channels();

    let mut all_samples: Vec<f32> = Vec::new();
    let mut last_report = Instant::now();

    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => {
                all_samples.extend(samples);

                if last_report.elapsed() >= Duration::from_secs(5) {
                    let frames = all_samples.len() / channels as usize;
                    let dur = frames as f64 / sample_rate as f64;
                    eprintln!("  {dur:.1}s captured...");
                    last_report = Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    while let Ok(samples) = rx.try_recv() {
        all_samples.extend(samples);
    }

    all_samples
}

pub fn write_wav(
    path: &str,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    if samples.is_empty() {
        eprintln!("No audio captured.");
        return Ok(());
    }

    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    let frames = samples.len() / channels as usize;
    let duration_secs = frames as f64 / sample_rate as f64;
    eprintln!("Wrote {duration_secs:.1}s of audio to {path}");

    Ok(())
}
