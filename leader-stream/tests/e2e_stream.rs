mod support;

use futures_util::StreamExt;
use reqwest::Client;
use tokio::time::timeout;
use std::time::Duration;

#[tokio::test]
async fn sse_stream_opens() {
    let server = support::TestServer::spawn().await;
    let client = Client::new();

    let response = client
        .get(format!("{}/api/leader-stream", server.base_url()))
        .send()
        .await
        .expect("leader-stream request");
    assert!(response.status().is_success());
    assert!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/event-stream"))
            .unwrap_or(false)
    );

    let mut stream = response.bytes_stream();
    let chunk = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("sse read timeout")
        .expect("sse chunk missing")
        .expect("sse chunk error");
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("stream-open"));
}
