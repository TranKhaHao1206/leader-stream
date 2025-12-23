mod support;

use reqwest::Client;

#[tokio::test]
async fn http_endpoints_smoke() {
    let server = support::TestServer::spawn().await;
    let client = Client::new();

    let health = client
        .get(format!("{}/health", server.base_url()))
        .send()
        .await
        .expect("health request");
    assert!(health.status().is_success());
    let body = health.text().await.expect("health body");
    assert_eq!(body, "ok");

    let docs = client
        .get(format!("{}/docs", server.base_url()))
        .send()
        .await
        .expect("docs request");
    assert!(docs.status().is_success());
    let body = docs.text().await.expect("docs body");
    assert!(body.contains("/api/next-leaders"));
}
