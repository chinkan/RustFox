use serde::Serialize;
use tracing::warn;

use crate::config::LangSmithConfig;

pub struct LangSmithClient {
    inner: Option<LangSmithInner>,
}

struct LangSmithInner {
    client: reqwest::Client,
    api_key: String,
    project: String,
    base_url: String,
}

#[derive(Debug, Clone)]
pub struct RunParams {
    pub id: String,
    pub name: String,
    pub run_type: RunType,
    pub parent_run_id: Option<String>,
    pub inputs: serde_json::Value,
    pub session_name: String,
    pub start_time: String,
}

#[derive(Debug, Clone)]
pub struct EndRunParams {
    pub id: String,
    pub outputs: Option<serde_json::Value>,
    pub error: Option<String>,
    pub end_time: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunType {
    Chain,
    Llm,
    Tool,
}

impl LangSmithClient {
    pub fn new(config: Option<&LangSmithConfig>) -> Self {
        let inner = config.map(|cfg| LangSmithInner {
            client: reqwest::Client::new(),
            api_key: cfg.api_key.clone(),
            project: cfg.project.clone(),
            base_url: cfg.base_url.clone(),
        });
        Self { inner }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    /// Fire-and-forget: POST /runs to start a run.
    pub fn start_run(&self, params: RunParams) {
        let Some(inner) = &self.inner else { return };
        let client = inner.client.clone();
        let api_key = inner.api_key.clone();
        let url = format!("{}/runs", inner.base_url);

        tokio::spawn(async move {
            let mut body = serde_json::json!({
                "id": params.id,
                "name": params.name,
                "run_type": params.run_type,
                "inputs": params.inputs,
                "start_time": params.start_time,
                "session_name": params.session_name,
            });
            if let Some(parent) = params.parent_run_id {
                body["parent_run_id"] = serde_json::Value::String(parent);
            }

            match client
                .post(&url)
                .header("x-api-key", &api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!(
                        "LangSmith POST /runs {} failed: {} — {}",
                        params.name,
                        status,
                        &text[..text.len().min(200)]
                    );
                }
                Err(e) => warn!("LangSmith POST /runs {}: {}", params.name, e),
                _ => {}
            }
        });
    }

    /// Fire-and-forget: PATCH /runs/{id} to finish a run.
    pub fn end_run(&self, params: EndRunParams) {
        let Some(inner) = &self.inner else { return };
        let client = inner.client.clone();
        let api_key = inner.api_key.clone();
        let url = format!("{}/runs/{}", inner.base_url, params.id);

        tokio::spawn(async move {
            let mut body = serde_json::json!({
                "end_time": params.end_time,
            });
            if let Some(outputs) = params.outputs {
                body["outputs"] = outputs;
            }
            if let Some(error) = params.error {
                body["error"] = serde_json::Value::String(error);
            }

            match client
                .patch(&url)
                .header("x-api-key", &api_key)
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    warn!(
                        "LangSmith PATCH /runs/{} failed: {} — {}",
                        params.id,
                        status,
                        &text[..text.len().min(200)]
                    );
                }
                Err(e) => warn!("LangSmith PATCH /runs/{}: {}", params.id, e),
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_when_no_config() {
        let client = LangSmithClient::new(None);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_enabled_when_config_present() {
        let cfg = LangSmithConfig {
            api_key: "ls__test".to_string(),
            project: "test".to_string(),
            base_url: "https://api.smith.langchain.com".to_string(),
        };
        let client = LangSmithClient::new(Some(&cfg));
        assert!(client.is_enabled());
    }

    #[test]
    fn test_run_type_serializes_lowercase() {
        let json = serde_json::to_string(&RunType::Llm).unwrap();
        assert_eq!(json, r#""llm""#);
        let json = serde_json::to_string(&RunType::Chain).unwrap();
        assert_eq!(json, r#""chain""#);
        let json = serde_json::to_string(&RunType::Tool).unwrap();
        assert_eq!(json, r#""tool""#);
    }
}
