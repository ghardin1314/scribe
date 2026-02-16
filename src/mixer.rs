use rubato::{FftFixedIn, Resampler};

pub enum MixMode {
    Stereo,
    Split,
}

pub fn to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

pub fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let mut resampler = FftFixedIn::<f32>::new(
        from_rate as usize,
        to_rate as usize,
        1024,
        2,
        1,
    )
    .expect("failed to create resampler");

    let chunk_size = resampler.input_frames_next();
    let mut output = Vec::new();
    let mut pos = 0;

    while pos + chunk_size <= samples.len() {
        let chunk = &samples[pos..pos + chunk_size];
        let result = resampler.process(&[chunk], None).expect("resample failed");
        output.extend_from_slice(&result[0]);
        pos += chunk_size;
    }

    // Handle remainder — zero-pad to chunk_size, trim output proportionally
    if pos < samples.len() {
        let remaining = samples.len() - pos;
        let mut last_chunk = vec![0.0f32; chunk_size];
        last_chunk[..remaining].copy_from_slice(&samples[pos..]);
        let result = resampler
            .process(&[&last_chunk], None)
            .expect("resample failed");
        let expected = (remaining as f64 * to_rate as f64 / from_rate as f64).ceil() as usize;
        let take = expected.min(result[0].len());
        output.extend_from_slice(&result[0][..take]);
    }

    output
}

/// Scale samples so peak amplitude reaches `target` (0.0–1.0).
/// Returns unchanged if silent.
pub fn peak_normalize(samples: &mut [f32], target: f32) {
    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak > 0.0 {
        let gain = target / peak;
        for s in samples.iter_mut() {
            *s *= gain;
        }
    }
}

pub fn interleave_stereo(system: &[f32], mic: &[f32]) -> Vec<f32> {
    let len = system.len().max(mic.len());
    let mut out = Vec::with_capacity(len * 2);
    for i in 0..len {
        out.push(system.get(i).copied().unwrap_or(0.0));
        out.push(mic.get(i).copied().unwrap_or(0.0));
    }
    out
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect()
}

