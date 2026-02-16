pub fn write_wav_i16(
    path: &str,
    samples: &[i16],
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
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
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
