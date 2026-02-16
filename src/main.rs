mod audio;
mod capture;

use capture::{Capture, MicCapture, SystemCapture};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

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

    let (capture, output_path): (Box<dyn Capture>, &str) = match mode {
        CaptureMode::System => (Box::new(SystemCapture::new()?), "output.wav"),
        CaptureMode::Mic => (Box::new(MicCapture::new()?), "output_mic.wav"),
    };

    let label = match mode {
        CaptureMode::System => "system audio",
        CaptureMode::Mic => "microphone",
    };

    capture.start()?;
    eprintln!("Capturing {label}... Press Ctrl+C to stop.");

    let start = Instant::now();
    let samples = audio::capture_loop(capture.as_ref(), &running);

    eprintln!("Stopping capture...");
    capture.stop()?;

    audio::write_wav(output_path, &samples, capture.sample_rate(), capture.channels())?;

    let elapsed = start.elapsed();
    eprintln!("Done in {elapsed:.1?}");

    Ok(())
}
