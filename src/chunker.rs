use crate::audio;
use crate::capture::Capture;
use crate::mixer::{self, MixMode};
use crate::pipeline::ChunkPair;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const TARGET_RATE: u32 = 16000;

pub struct ChunkConfig {
    pub chunk_duration: u32,
    pub overlap: u32,
    pub output_dir: String,
}

/// Returns (date, time) e.g. ("2026-02-15", "14-30-05")
pub(crate) fn local_timestamp() -> (String, String) {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let date = format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        );
        let time = format!("{:02}-{:02}-{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
        (date, time)
    }
}

fn chunk_dir(output_dir: &str, date: &str) -> PathBuf {
    let dir = PathBuf::from(output_dir).join("audio").join(date);
    std::fs::create_dir_all(&dir).expect("failed to create chunk output dir");
    dir
}

fn process_source(buf: &[f32], rate: u32, channels: u16) -> Vec<f32> {
    let mono = mixer::to_mono(buf, channels);
    let resampled = mixer::resample(&mono, rate, TARGET_RATE);
    let mut normalized = resampled;
    mixer::peak_normalize(&mut normalized, 0.9);
    normalized
}

fn flush_chunk_both(
    sys_buf: &[f32],
    mic_buf: &[f32],
    sys_rate: u32,
    sys_ch: u16,
    mic_rate: u32,
    mic_ch: u16,
    mix_mode: &MixMode,
    dir: &PathBuf,
    chunk_tx: Option<&Sender<ChunkPair>>,
) -> Result<(), Box<dyn std::error::Error>> {
    if sys_buf.is_empty() && mic_buf.is_empty() {
        return Ok(());
    }

    let sys_processed = process_source(sys_buf, sys_rate, sys_ch);
    let mic_processed = process_source(mic_buf, mic_rate, mic_ch);

    let (date, time) = local_timestamp();

    match mix_mode {
        MixMode::Stereo => {
            let stereo = mixer::interleave_stereo(&sys_processed, &mic_processed);
            let pcm = mixer::f32_to_i16(&stereo);
            let path = dir.join(format!("{time}.wav"));
            audio::write_wav_i16(path.to_str().unwrap(), &pcm, TARGET_RATE, 2)?;
        }
        MixMode::Split => {
            let sys_pcm = mixer::f32_to_i16(&sys_processed);
            let mic_pcm = mixer::f32_to_i16(&mic_processed);
            let sys_path = dir.join(format!("{time}_system.wav"));
            let mic_path = dir.join(format!("{time}_mic.wav"));
            audio::write_wav_i16(sys_path.to_str().unwrap(), &sys_pcm, TARGET_RATE, 1)?;
            audio::write_wav_i16(mic_path.to_str().unwrap(), &mic_pcm, TARGET_RATE, 1)?;

            if let Some(tx) = chunk_tx {
                let _ = tx.send(ChunkPair {
                    timestamp: time,
                    date,
                    system_path: sys_path,
                    mic_path: mic_path,
                });
            }
        }
    }

    Ok(())
}

fn flush_chunk_single(
    buf: &[f32],
    rate: u32,
    channels: u16,
    dir: &PathBuf,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if buf.is_empty() {
        return Ok(());
    }

    let processed = process_source(buf, rate, channels);
    let pcm = mixer::f32_to_i16(&processed);

    let (_, time) = local_timestamp();
    let filename = if label.is_empty() {
        format!("{time}.wav")
    } else {
        format!("{time}_{label}.wav")
    };
    let path = dir.join(filename);
    audio::write_wav_i16(path.to_str().unwrap(), &pcm, TARGET_RATE, 1)?;

    Ok(())
}

pub fn run_chunked_both(
    system: &dyn Capture,
    mic: &dyn Capture,
    mix_mode: &MixMode,
    config: &ChunkConfig,
    running: &AtomicBool,
    chunk_tx: Option<&Sender<ChunkPair>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let sys_rx = system.rx();
    let mic_rx = mic.rx();
    let sys_rate = system.sample_rate();
    let sys_ch = system.channels();
    let mic_rate = mic.sample_rate();
    let mic_ch = mic.channels();

    let overlap = config.overlap.min(config.chunk_duration.saturating_sub(1));
    let sys_chunk_samples = (config.chunk_duration as usize) * (sys_rate as usize) * (sys_ch as usize);
    let mic_chunk_samples = (config.chunk_duration as usize) * (mic_rate as usize) * (mic_ch as usize);
    let sys_overlap_samples = (overlap as usize) * (sys_rate as usize) * (sys_ch as usize);
    let mic_overlap_samples = (overlap as usize) * (mic_rate as usize) * (mic_ch as usize);

    let (date, _) = local_timestamp();
    let dir = chunk_dir(&config.output_dir, &date);

    let mut sys_buf: Vec<f32> = Vec::new();
    let mut mic_buf: Vec<f32> = Vec::new();
    let mut chunk_start = Instant::now();
    let mut last_report = Instant::now();
    let mut chunk_count: u32 = 0;

    while running.load(Ordering::SeqCst) {
        let mut got_data = false;

        while let Ok(chunk) = sys_rx.try_recv() {
            sys_buf.extend(chunk);
            got_data = true;
        }
        while let Ok(chunk) = mic_rx.try_recv() {
            mic_buf.extend(chunk);
            got_data = true;
        }

        if !got_data {
            std::thread::sleep(Duration::from_millis(2));
        }

        // Check if chunk is ready (use sample count as primary, time as fallback)
        let chunk_ready = sys_buf.len() >= sys_chunk_samples || mic_buf.len() >= mic_chunk_samples;

        if chunk_ready {
            flush_chunk_both(
                &sys_buf, &mic_buf,
                sys_rate, sys_ch, mic_rate, mic_ch,
                mix_mode, &dir, chunk_tx,
            )?;
            chunk_count += 1;

            // Retain overlap
            let sys_drain = sys_buf.len().saturating_sub(sys_overlap_samples);
            sys_buf.drain(..sys_drain);
            let mic_drain = mic_buf.len().saturating_sub(mic_overlap_samples);
            mic_buf.drain(..mic_drain);

            chunk_start = Instant::now();
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let chunk_elapsed = chunk_start.elapsed().as_secs_f32();
            eprintln!("  chunks: {chunk_count}, current chunk: {chunk_elapsed:.1}s");
            last_report = Instant::now();
        }
    }

    // Final drain from channels
    while let Ok(chunk) = sys_rx.try_recv() {
        sys_buf.extend(chunk);
    }
    while let Ok(chunk) = mic_rx.try_recv() {
        mic_buf.extend(chunk);
    }

    // Flush final partial chunk
    flush_chunk_both(
        &sys_buf, &mic_buf,
        sys_rate, sys_ch, mic_rate, mic_ch,
        mix_mode, &dir, chunk_tx,
    )?;
    if !sys_buf.is_empty() || !mic_buf.is_empty() {
        chunk_count += 1;
    }

    eprintln!("Total chunks: {chunk_count}");
    Ok(())
}

pub fn run_chunked_single(
    capture: &dyn Capture,
    label: &str,
    config: &ChunkConfig,
    running: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    let rx = capture.rx();
    let rate = capture.sample_rate();
    let channels = capture.channels();

    let overlap = config.overlap.min(config.chunk_duration.saturating_sub(1));
    let chunk_samples = (config.chunk_duration as usize) * (rate as usize) * (channels as usize);
    let overlap_samples = (overlap as usize) * (rate as usize) * (channels as usize);

    let (date, _) = local_timestamp();
    let dir = chunk_dir(&config.output_dir, &date);

    let mut buf: Vec<f32> = Vec::new();
    let mut chunk_start = Instant::now();
    let mut last_report = Instant::now();
    let mut chunk_count: u32 = 0;

    while running.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(samples) => buf.extend(samples),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if buf.len() >= chunk_samples {
            flush_chunk_single(&buf, rate, channels, &dir, label)?;
            chunk_count += 1;

            let drain = buf.len().saturating_sub(overlap_samples);
            buf.drain(..drain);

            chunk_start = Instant::now();
        }

        if last_report.elapsed() >= Duration::from_secs(5) {
            let chunk_elapsed = chunk_start.elapsed().as_secs_f32();
            eprintln!("  chunks: {chunk_count}, current chunk: {chunk_elapsed:.1}s");
            last_report = Instant::now();
        }
    }

    // Final drain
    while let Ok(samples) = rx.try_recv() {
        buf.extend(samples);
    }

    // Flush final partial chunk
    if !buf.is_empty() {
        flush_chunk_single(&buf, rate, channels, &dir, label)?;
        chunk_count += 1;
    }

    eprintln!("Total chunks: {chunk_count}");
    Ok(())
}
