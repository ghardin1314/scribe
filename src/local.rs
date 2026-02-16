use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 8080;

pub struct LocalServer {
    child: Child,
    pub port: u16,
}

fn find_binary() -> Option<String> {
    for name in &["whisper-cpp-server", "whisper-server"] {
        if Command::new(name)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return Some(name.to_string());
        }
    }
    None
}

fn find_model(model: &str) -> Option<PathBuf> {
    let filename = format!("ggml-{model}.bin");
    let home = std::env::var("HOME").ok()?;

    let candidates = [
        format!("{home}/.cache/whisper/{filename}"),
        format!("{home}/.local/share/scribe/models/{filename}"),
        format!("{home}/models/{filename}"),
    ];

    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

impl LocalServer {
    pub fn start(model: &str, port: Option<u16>) -> Result<Self, Box<dyn std::error::Error>> {
        let binary = find_binary()
            .ok_or("whisper-server not found in PATH.\n  Install: brew install whisper-cpp")?;

        let model_path = find_model(model).ok_or_else(|| {
            format!(
                "Model ggml-{model}.bin not found.\n  \
                 Download: whisper-cpp-download-ggml-model {model}"
            )
        })?;

        let port = port.unwrap_or(DEFAULT_PORT);

        eprintln!(
            "Starting local whisper server (model: {}, port: {port})...",
            model_path.display()
        );

        let child = Command::new(&binary)
            .arg("-m")
            .arg(&model_path)
            .arg("--port")
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start {binary}: {e}"))?;

        let server = LocalServer { child, port };
        server.wait_ready()?;

        Ok(server)
    }

    fn wait_ready(&self) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("http://localhost:{}/health", self.port);
        let client = reqwest::blocking::Client::new();
        let start = Instant::now();
        let timeout = Duration::from_secs(30);

        loop {
            if start.elapsed() > timeout {
                return Err("Whisper server failed to start within 30s".into());
            }

            match client.get(&url).timeout(Duration::from_secs(1)).send() {
                Ok(resp) if resp.status().is_success() => {
                    eprintln!("Local whisper server ready");
                    return Ok(());
                }
                _ => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    pub fn api_url(&self) -> String {
        format!("http://localhost:{}/inference", self.port)
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        eprintln!("Stopping local whisper server...");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
