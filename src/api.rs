use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

/// Low-level HTTP client for Attacca API.
pub struct Api {
    inner: Client,
    pub key: String,
    pub base: String,
}

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

    /// Build URL from base + path, avoiding double /v1/.
    fn build_url(base: &str, path: &str) -> String {
        let b = base.trim_end_matches('/');
        let p = path.trim_start_matches('/');
        if b.ends_with("/v1") && p.starts_with("v1/") {
            format!("{b}/{}", &p[3..])
        } else {
            format!("{b}/{p}")
        }
    }

    /// Build URL from path.
    pub fn url(&self, path: &str) -> String {
        Self::build_url(&self.base, path)
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    /// GET request. Returns body string or (status, body).
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

    /// POST JSON body. Returns body string or (status, body).
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

    /// Probe various URL patterns to find the correct API endpoint.
    pub async fn diagnose(&self) -> Vec<ProbeResult> {
        let combos = [
            ("https://attacca.cc", "/v1/me"),
            ("https://attacca.cc", "/v1/sessions"),
            ("https://attacca.cc/v1", "/me"),
            ("https://attacca.cc/v1", "/sessions"),
            ("https://attacca.cc/api/v1", "/v1/me"),
            ("https://attacca.cc/api/v1", "/v1/sessions"),
            ("https://attacca.cc/api", "/v1/me"),
            ("https://attacca.cc/api", "/v1/sessions"),
        ];

        let mut out = Vec::new();
        for &(base, path) in &combos {
            let url = Self::build_url(base, path);
            let resp = self.inner.get(&url).headers(self.headers()).send().await;
            let (status, body, ok) = match resp {
                Ok(r) => {
                    let s = r.status().as_u16();
                    let b = r.text().await.unwrap_or_default();
                    (s, b.chars().take(80).collect::<String>(), s == 200)
                }
                Err(e) => (0, format!("{e}"), false),
            };
            out.push(ProbeResult { url, status, preview: body, ok });
        }
        out
    }

    /// Simple health check.
    pub async fn whoami(&self) -> String {
        match self.get("/v1/me").await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    format!("✓ {}", v["display_name"].as_str().unwrap_or("?"))
                } else {
                    format!("✓ (unexpected: {body})")
                }
            }
            Err((s, b)) => format!("✖ HTTP {s}: {}", b.chars().take(60).collect::<String>()),
        }
    }
}

pub struct ProbeResult {
    pub url: String,
    pub status: u16,
    pub preview: String,
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_plain_base() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(api.url("/v1/sessions"), "https://attacca.cc/v1/sessions");
    }

    #[test]
    fn test_url_with_v1_suffix() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/api/v1".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/api/v1/me");
        assert_eq!(api.url("/v1/sessions"), "https://attacca.cc/api/v1/sessions");
    }

    #[test]
    fn test_url_with_v1_prefix() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/v1".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(api.url("/v1/sessions"), "https://attacca.cc/v1/sessions");
    }

    #[test]
    fn test_url_trailing_slash() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/v1/me");
    }

    #[test]
    fn test_build_url_strips_double_v1() {
        assert_eq!(Api::build_url("https://attacca.cc/api/v1", "/v1/me"), "https://attacca.cc/api/v1/me");
    }

    #[test]
    fn test_build_url_no_strip_when_no_v1() {
        assert_eq!(Api::build_url("https://attacca.cc", "/v1/me"), "https://attacca.cc/v1/me");
    }

    #[test]
    fn test_build_url_preserves_me() {
        assert_eq!(Api::build_url("https://attacca.cc/v1", "/me"), "https://attacca.cc/v1/me");
    }
}
