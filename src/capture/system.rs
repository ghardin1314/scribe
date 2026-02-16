use super::Capture;
use screencapturekit::prelude::*;
use std::sync::mpsc;

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

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

pub struct SystemCapture {
    stream: SCStream,
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    channels: u16,
}

impl SystemCapture {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
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
}

impl Capture for SystemCapture {
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
        self.stream.start_capture()?;
        Ok(())
    }

    fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.stream.stop_capture()?;
        Ok(())
    }
}
