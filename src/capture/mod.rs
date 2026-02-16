mod mic;
mod system;

pub use mic::MicCapture;
pub use system::SystemCapture;

use std::sync::mpsc;

pub trait Capture {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn rx(&self) -> &mpsc::Receiver<Vec<f32>>;
    fn start(&self) -> Result<(), Box<dyn std::error::Error>>;
    fn stop(&self) -> Result<(), Box<dyn std::error::Error>>;
}
