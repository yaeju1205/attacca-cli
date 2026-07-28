use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

/// Low-level HTTP client for Attacca API.
pub struct Api {
    inner: Client,
    pub key: String,
    pub base: String,
}

/// Result of an API call: (status_code, body_string)
pub type ApiResult = Result<String, (u16, String)>;

impl Api {
    pub fn from_env() -> Self {
        let key = std::env::var("ATTACCA_API_KEY").unwrap_or_default();
        let base = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("attacca-cli/0.2")
            .build()
            .unwrap_or_default();
        Self { inner, key, base }
    }

    pub fn url(&self, path: &str) -> String {
        let b = self.base.trim_end_matches('/');
        let p = path.trim_start_matches('/');
        format!("{b}/{p}")
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    /// GET request. Returns (body) or (status, body).
    pub async fn get(&self, path: &str) -> ApiResult {
        let url = self.url(path);
        let resp = self.inner.get(&url).headers(self.headers()).send().await;
        match resp {
            Ok(r) => {
                let s = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                if s < 300 { Ok(body) } else { Err((s, body)) }
            }
            Err(e) => Err((0, format!("{e}"))),
        }
    }

    /// POST JSON body. Returns (body) or (status, body).
    pub async fn post(&self, path: &str, json: &Value) -> ApiResult {
        let url = self.url(path);
        let resp = self.inner.post(&url).headers(self.headers()).json(json).send().await;
        match resp {
            Ok(r) => {
                let s = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                if s < 300 || s == 202 { Ok(body) } else { Err((s, body)) }
            }
            Err(e) => Err((0, format!("{e}"))),
        }
    }

    /// Probe multiple URL patterns to find the right base URL.
    /// Returns (base_url, identity_string) on success.
    pub async fn diagnose(&self) -> Vec<ProbeResult> {
        let bases = [
            self.base.as_str(),
            "https://attacca.cc",
            "https://attacca.cc/api/v1",
        ];
        let paths = ["/v1/me", "/v1/sessions"];

        let mut out = Vec::new();
        for base in &bases {
            for path in &paths {
                let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
                let resp = self.inner.get(&url).headers(self.headers()).send().await;
                let (status, body, note) = match resp {
                    Ok(r) => {
                        let s = r.status().as_u16();
                        let b = r.text().await.unwrap_or_default();
                        let note = if s == 200 { "✓" } else { "" };
                        (s, b.chars().take(80).collect::<String>(), note)
                    }
                    Err(e) => (0, format!("{e}"), ""),
                };
                out.push(ProbeResult {
                    url,
                    status,
                    preview: body,
                    ok: note == "✓",
                });
            }
        }
        out
    }

    /// Simple health check. Returns display name if successful.
    pub async fn whoami(&self) -> String {
        match self.get("/v1/me").await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    let name = v["display_name"].as_str().unwrap_or("?");
                    format!("✓ {name}")
                } else {
                    format!("✓ (unexpected response)")
                }
            }
            Err((s, b)) => {
                let preview = b.chars().take(60).collect::<String>();
                format!("✖ HTTP {s}: {preview}")
            }
        }
    }
}

pub struct ProbeResult {
    pub url: String,
    pub status: u16,
    pub preview: String,
    pub ok: bool,
}
