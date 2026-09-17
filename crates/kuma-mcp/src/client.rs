//! Thin HTTP client for the daemon API.

use std::time::Duration;

use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

/// Probe a daemon. Returns its health document when one answers. Never goes through a
/// proxy: the daemon is always on loopback and `http_proxy` in the environment would
/// otherwise swallow the request.
pub async fn health(base: &str) -> Option<Value> {
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().ok()?;
    let r = http.get(format!("{}/api/health", base.trim_end_matches('/'))).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json::<Value>().await.ok()
}

impl Client {
    pub fn new(base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        // Sampling and renders can take a while; the daemon streams progress over WS but
        // these calls block until done.
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3600)).build().unwrap();
        Client { base, http }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Errors keep whatever the daemon said: its JSON `error` (plus validation `issues`),
    /// or the plain-text body axum emits for malformed requests.
    async fn check(r: reqwest::Response) -> anyhow::Result<Value> {
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let msg = body.get("error").and_then(Value::as_str).unwrap_or(if text.is_empty() { "request failed" } else { text.trim() });
            anyhow::bail!("{status}: {msg}{}", body.get("issues").map(|i| format!(" {i}")).unwrap_or_default());
        }
        Ok(body)
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        Self::check(self.http.get(self.url(path)).send().await?).await
    }

    pub async fn get_query(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Value> {
        Self::check(self.http.get(self.url(path)).query(query).send().await?).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        Self::check(self.http.post(self.url(path)).json(body).send().await?).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        Self::check(self.http.put(self.url(path)).json(body).send().await?).await
    }

    /// Poll a job until it finishes (or `timeout` elapses, returning it still running).
    pub async fn wait_job(&self, id: u64, timeout: Duration) -> anyhow::Result<Value> {
        let start = std::time::Instant::now();
        loop {
            let j = self.get(&format!("/api/jobs/{id}")).await?;
            if j["done"].as_bool() == Some(true) {
                if j["ok"].as_bool() == Some(true) {
                    return Ok(j);
                }
                anyhow::bail!("job {id} failed: {}", j["message"].as_str().unwrap_or("?"));
            }
            if start.elapsed() > timeout {
                return Ok(j);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}
