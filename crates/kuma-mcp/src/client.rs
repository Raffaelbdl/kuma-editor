//! Thin HTTP client for the daemon API.

use std::time::Duration;

use serde_json::{json, Value};

#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

pub async fn health(base: &str) -> bool {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    http.get(format!("{}/api/health", base.trim_end_matches('/'))).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        // Sampling and renders can take a while; the daemon streams progress over WS but
        // these calls block until done.
        let http = reqwest::Client::builder().timeout(Duration::from_secs(3600)).build().unwrap();
        Client { base, http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn check(r: reqwest::Response) -> anyhow::Result<Value> {
        let status = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = body.get("error").and_then(Value::as_str).unwrap_or("request failed");
            anyhow::bail!("{status}: {msg}{}", body.get("issues").map(|i| format!(" {i}")).unwrap_or_default());
        }
        Ok(body)
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        Self::check(self.http.get(self.url(path)).send().await?).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        Self::check(self.http.post(self.url(path)).json(body).send().await?).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        Self::check(self.http.put(self.url(path)).json(body).send().await?).await
    }

    /// Poll a job until it finishes (or `timeout` elapses).
    pub async fn wait_job(&self, id: u64, timeout: Duration) -> anyhow::Result<Value> {
        let start = std::time::Instant::now();
        loop {
            let j = self.get(&format!("/api/jobs/{id}")).await?;
            let status = &j["status"];
            if status.get("done").is_some() {
                return Ok(json!({ "job": id, "status": "done", "result": status["done"]["result"] }));
            }
            if status.get("failed").is_some() {
                anyhow::bail!("job {id} failed: {}", status["failed"]["error"].as_str().unwrap_or("?"));
            }
            if start.elapsed() > timeout {
                return Ok(json!({ "job": id, "status": "running", "progress": status["running"] }));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
