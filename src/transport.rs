//! Pure HTTP transport layer for the Attacca API.
//!
//! Handles connection, authentication, URL building, and raw
//! GET/POST requests. No domain logic — that lives in [`api`](crate::api).

use reqwest::Client;
use std::time::Duration;

/// HTTP transport for the Attacca API.
///
/// Wraps a `reqwest::Client` with bearer auth and a configurable base URL.
#[derive(Clone)]
pub struct Transport {
    inner: Client,
    pub key: String,
    pub base: String,
}

pub type HttpResult = Result<String, (u16, String)>;

impl Transport {
    /// Build a transport from environment variables.
    ///
    /// | Variable | Default |
    /// |---|---|
    /// | `ATTACCA_API_KEY` | (empty — all requests will auth-fail) |
    /// | `ATTACCA_API_URL` | `https://attacca.cc/api/v1` |
    pub fn from_env() -> Self {
        let key = std::env::var("ATTACCA_API_KEY").unwrap_or_default();
        let base = std::env::var("ATTACCA_API_URL")
            .unwrap_or_else(|_| "https://attacca.cc/api/v1".to_string());
        let inner = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("attacca-cli/0.2")
            .build()
            .expect("reqwest Client::build");
        Self { inner, key, base }
    }

    /// Build a full URL from a path, handling `/v1` prefix deduplication.
    ///
    /// ```ignore
    /// # base = "https://attacca.cc/api/v1"
    /// url("me")         → "https://attacca.cc/api/v1/me"
    /// url("/v1/me")     → "https://attacca.cc/api/v1/me"  (v1 deduplicated)
    /// ```
    pub fn url(&self, path: &str) -> String {
        let b = self.base.trim_end_matches('/');
        let p = path.trim_start_matches('/');
        if (b.ends_with("/v1") || b.ends_with("/api/v1")) && p.starts_with("v1/") {
            format!("{b}/{}", &p[3..])
        } else {
            format!("{b}/{p}")
        }
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.key).parse().unwrap(),
        );
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    /// Raw GET — returns response body as string on success (2xx),
    /// or `(status_code, body)` on error.
    pub async fn get(&self, path: &str) -> HttpResult {
        let url = self.url(path);
        let resp = self.inner.get(&url).headers(self.headers()).send().await;
        Self::into_result(resp).await
    }

    /// Raw POST with JSON body — returns response body on success
    /// (2xx **or** 202), or `(status_code, body)` on error.
    pub async fn post(&self, path: &str, json: &serde_json::Value) -> HttpResult {
        let url = self.url(path);
        let resp = self
            .inner
            .post(&url)
            .headers(self.headers())
            .json(json)
            .send()
            .await;
        Self::into_result(resp).await
    }

    async fn into_result(
        resp: Result<reqwest::Response, reqwest::Error>,
    ) -> HttpResult {
        match resp {
            Ok(r) => {
                let s = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                if s < 300 || s == 202 {
                    Ok(body)
                } else {
                    Err((s, body))
                }
            }
            Err(e) => Err((0, format!("{e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_api_v1_base() {
        let t = Transport {
            inner: Client::builder().build().unwrap(),
            key: String::new(),
            base: "https://attacca.cc/api/v1".into(),
        };
        assert_eq!(t.url("me"), "https://attacca.cc/api/v1/me");
        assert_eq!(t.url("sessions"), "https://attacca.cc/api/v1/sessions");
    }

    #[test]
    fn test_url_plain_base() {
        let t = Transport {
            inner: Client::builder().build().unwrap(),
            key: String::new(),
            base: "https://attacca.cc".into(),
        };
        assert_eq!(t.url("v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(t.url("v1/sessions"), "https://attacca.cc/v1/sessions");
    }

    #[test]
    fn test_url_strips_double_v1() {
        let t = Transport {
            inner: Client::builder().build().unwrap(),
            key: String::new(),
            base: "https://attacca.cc/api/v1".into(),
        };
        assert_eq!(t.url("/v1/me"), "https://attacca.cc/api/v1/me");
        assert_eq!(t.url("/v1/sessions"), "https://attacca.cc/api/v1/sessions");
    }

    #[test]
    fn test_url_strips_v1_short() {
        let t = Transport {
            inner: Client::builder().build().unwrap(),
            key: String::new(),
            base: "https://attacca.cc/v1".into(),
        };
        assert_eq!(t.url("/v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(t.url("/v1/sessions"), "https://attacca.cc/v1/sessions");
    }
}
