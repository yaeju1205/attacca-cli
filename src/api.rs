use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub struct Api {
    inner: Client,
    pub key: String,
    pub base: String,
}

pub type ApiResult = Result<String, (u16, String)>;

impl Api {
    pub fn from_env() -> Self {
        let key = std::env::var("ATTACCA_API_KEY").unwrap_or_default();
        let base = std::env::var("ATTACCA_API_URL")
            .unwrap_or_else(|_| "https://attacca.cc/api/v1".to_string());
        let inner = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("attacca-cli/0.2")
            .build()
            .unwrap_or_default();
        Self { inner, key, base }
    }

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
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

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

    pub async fn whoami(&self) -> String {
        match self.get("me").await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    format!("✓ {}", v["display_name"].as_str().unwrap_or("?"))
                } else {
                    "✓ ok".to_string()
                }
            }
            Err((s, b)) => format!("✖ HTTP {s}: {}", b.chars().take(60).collect::<String>()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_api_v1_base() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/api/v1".into() };
        assert_eq!(api.url("me"), "https://attacca.cc/api/v1/me");
        assert_eq!(api.url("sessions"), "https://attacca.cc/api/v1/sessions");
    }

    #[test]
    fn test_url_plain_base() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc".into() };
        assert_eq!(api.url("v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(api.url("v1/sessions"), "https://attacca.cc/v1/sessions");
    }

    #[test]
    fn test_url_strips_double_v1() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/api/v1".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/api/v1/me");
        assert_eq!(api.url("/v1/sessions"), "https://attacca.cc/api/v1/sessions");
    }

    #[test]
    fn test_url_strips_v1_short() {
        let api = Api { inner: Client::builder().build().unwrap_or_default(), key: String::new(), base: "https://attacca.cc/v1".into() };
        assert_eq!(api.url("/v1/me"), "https://attacca.cc/v1/me");
        assert_eq!(api.url("/v1/sessions"), "https://attacca.cc/v1/sessions");
    }
}
