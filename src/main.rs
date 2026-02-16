use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use screencapturekit::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

// --- Mode selection ---

enum CaptureMode {
    System,
    Mic,
}

fn parse_mode() -> CaptureMode {
    if std::env::args().any(|a| a == "--mic") {
        CaptureMode::Mic
    } else {
        CaptureMode::System
    }
}

// --- System audio capture (ScreenCaptureKit) ---

struct SystemAudioHandler {
    tx: mpsc::Sender<Vec<f32>>,
}

impl SCStreamOutputTrait for SystemAudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let Some(audio) = sample.audio_buffer_list() else {
            return;
        };

        let num_buffers = audio.num_buffers();

        if num_buffers == 1 {
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

struct SystemCapture {
    stream: SCStream,
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
}

impl SystemCapture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let content = SCShareableContent::get().map_err(|e| {
            format!(
                "{e}\n\nEnable Screen Recording:\n  \
                 System Settings → Privacy & Security → Screen & System Audio Recording"
            )
        })?;
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
        let handler = SystemAudioHandler { tx };

        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(handler, SCStreamOutputType::Audio);

        Ok(Self {
            stream,
            rx,
            sample_rate: SAMPLE_RATE,
            channels: CHANNELS,
        })
    }

    fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.start_capture()?;
        Ok(())
    }

    fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.stop_capture()?;
        Ok(())
    }
}

// --- Microphone capture (cpal) ---

struct MicCapture {
    stream: cpal::Stream,
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
}

impl MicCapture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or("No input device found. Check Microphone permission:\n  \
                     System Settings → Privacy & Security → Microphone")?;

        let device_name = device.description().map(|d| d.to_string()).unwrap_or_else(|_| "unknown".into());
        eprintln!("Using input device: {device_name}");

        let supported = device.default_input_config()?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();

        eprintln!("  Format: {sample_rate}Hz, {channels}ch, {:?}", supported.sample_format());

        let (tx, rx) = mpsc::channel();

        let err_fn = |err: cpal::StreamError| {
            eprintln!("Mic stream error: {err}");
        };

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &supported.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(data.to_vec());
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => {
                let tx = tx.clone();
                device.build_input_stream(
                    &supported.into(),
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let floats: Vec<f32> =
                            data.iter().map(|&s| s as f32 / 32768.0).collect();
                        let _ = tx.send(floats);
                    },
                    err_fn,
                    None,
                )?
            }
            format => return Err(format!("Unsupported sample format: {format:?}").into()),
        };

        Ok(Self {
            stream,
            rx,
            sample_rate,
            channels,
        })
    }

    fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.play()?;
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.pause().ok(); // pause may not be supported on all backends
        Ok(())
    }
}

// --- Common capture loop + WAV writing ---

fn capture_loop(
    rx: &mpsc::Receiver<Vec<f32>>,
    running: &AtomicBool,
    sample_rate: u32,
    channels: u16,
) -> Vec<f32> {
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
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Drain remaining
    while let Ok(samples) = rx.try_recv() {
        all_samples.extend(samples);
    }

    all_samples
}

fn write_wav(
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

// --- Main ---

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mode = parse_mode();

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    let start = Instant::now();

    match mode {
        CaptureMode::System => {
            let capture = SystemCapture::new()?;
            capture.start()?;
            eprintln!("Capturing system audio... Press Ctrl+C to stop.");

            let samples =
                capture_loop(&capture.rx, &running, capture.sample_rate, capture.channels);

            eprintln!("Stopping capture...");
            capture.stop()?;

            write_wav("output.wav", &samples, capture.sample_rate, capture.channels)?;
        }
        CaptureMode::Mic => {
            let capture = MicCapture::new()?;
            capture.start()?;
            eprintln!("Capturing microphone... Press Ctrl+C to stop.");

            let samples =
                capture_loop(&capture.rx, &running, capture.sample_rate, capture.channels);

            eprintln!("Stopping capture...");
            capture.stop()?;

            write_wav("output_mic.wav", &samples, capture.sample_rate, capture.channels)?;
        }
    }

    let elapsed = start.elapsed();
    eprintln!("Done in {elapsed:.1?}");

    Ok(())
}
