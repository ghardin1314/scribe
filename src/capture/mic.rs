use super::Capture;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc;

pub struct MicCapture {
    stream: cpal::Stream,
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
}

impl MicCapture {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or("No input device found. Check Microphone permission:\n  \
                     System Settings → Privacy & Security → Microphone")?;

        let device_name = device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_else(|_| "unknown".into());
        eprintln!("Using input device: {device_name}");

        let supported = device.default_input_config()?;
        let sample_rate = supported.sample_rate();
        let channels = supported.channels();

        eprintln!(
            "  Format: {sample_rate}Hz, {channels}ch, {:?}",
            supported.sample_format()
        );

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
}

impl Capture for MicCapture {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn rx(&self) -> &mpsc::Receiver<Vec<f32>> {
        &self.rx
    }

    fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.play()?;
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.pause().ok();
        Ok(())
    }
}
