mod audio;
mod capture;
mod mixer;

use capture::{Capture, MicCapture, SystemCapture};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

const TARGET_RATE: u32 = 16000;

enum MixMode {
    Stereo,
    Split,
}

enum CaptureMode {
    System,
    Mic,
    Both(MixMode),
}

fn parse_mode() -> CaptureMode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--system") {
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

    let start = Instant::now();

    match mode {
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

    let samples = audio::capture_loop(capture.as_ref(), running);

    eprintln!("Stopping capture...");
    capture.stop()?;

    let mono = mixer::to_mono(&samples, capture.channels());
    let resampled = mixer::resample(&mono, capture.sample_rate(), TARGET_RATE);
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

    let (sys_samples, mic_samples) = mixer::dual_capture_loop(&system, &mic, running);

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
