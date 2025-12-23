use std::process::{Child, Command, Stdio};
use std::time::Duration;

use portpicker::pick_unused_port;
use reqwest::Client;
use tokio::time::sleep;

pub struct TestServer {
    child: Child,
    base_url: String,
}

impl TestServer {
    pub async fn spawn() -> Self {
        let port = pick_unused_port().expect("free port");
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("leader-stream"));
        cmd.env("PORT", port.to_string())
            .env("DISABLE_BACKGROUND_TASKS", "1")
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().expect("spawn leader-stream");
        let base_url = format!("http://127.0.0.1:{}", port);
        wait_for_ready(&base_url).await;

        Self { child, base_url }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn wait_for_ready(base_url: &str) {
    let client = Client::new();
    let health_url = format!("{}/health", base_url);
    for _ in 0..50 {
        if let Ok(response) = client.get(&health_url).send().await {
            if response.status().is_success() {
                return;
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("server did not become ready at {}", health_url);
}
