use anyhow::{Context, bail};
use genai::{
    Client,
    resolver::{Endpoint, ServiceTargetResolver},
};

const CODING_ENDPOINT: &str = "https://open.bigmodel.cn/api/coding/paas/v4/";

pub struct AgentConfig {
    pub model: String,
    pub max_steps: usize,
    endpoint: Option<String>,
}

impl AgentConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(
            std::env::var("MODEL").ok(),
            std::env::var("API_BASE_URL").ok(),
            std::env::var("AGENT_MAX_STEPS").ok(),
        )
    }

    fn from_values(
        model: Option<String>,
        endpoint: Option<String>,
        max_steps: Option<String>,
    ) -> anyhow::Result<Self> {
        let model = model.unwrap_or_else(|| "bigmodel::glm-4.6".into());
        if model.trim().is_empty() {
            bail!("MODEL must not be empty");
        }
        let max_steps = max_steps
            .as_deref()
            .unwrap_or("6")
            .parse::<usize>()
            .context("AGENT_MAX_STEPS must be a positive integer")?;
        if max_steps == 0 {
            bail!("AGENT_MAX_STEPS must be greater than 0");
        }
        let endpoint = match endpoint {
            Some(url) if url.trim().is_empty() => None,
            Some(url) => {
                let url = url.trim();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    bail!("API_BASE_URL must use http:// or https://");
                }
                Some(format!("{}/", url.trim_end_matches('/')))
            }
            None if model.starts_with("bigmodel::") => Some(CODING_ENDPOINT.to_owned()),
            None => None,
        };
        Ok(Self {
            model,
            max_steps,
            endpoint,
        })
    }

    pub fn build_client(&self) -> anyhow::Result<Client> {
        let mut builder = Client::builder();
        if let Some(endpoint) = &self.endpoint {
            let endpoint = endpoint.clone();
            let resolver =
                ServiceTargetResolver::from_resolver_fn(move |mut target: genai::ServiceTarget| {
                    target.endpoint = Endpoint::from_owned(endpoint.clone());
                    Ok(target)
                });
            builder = builder.with_service_target_resolver(resolver);
        }
        Ok(builder.build()?)
    }
}

#[cfg(test)]
mod tests {
    use super::AgentConfig as Config;
    use super::*;

    #[test]
    fn defaults_preserve_coding_plan_and_other_adapters() {
        let default = Config::from_values(None, None, None).unwrap();
        assert_eq!(default.endpoint.as_deref(), Some(CODING_ENDPOINT));
        assert_eq!(default.max_steps, 6);
        assert!(
            Config::from_values(Some("openai::gpt-4.1".into()), None, None)
                .unwrap()
                .endpoint
                .is_none()
        );
        assert!(
            Config::from_values(None, Some(String::new()), None)
                .unwrap()
                .endpoint
                .is_none()
        );
    }

    #[test]
    fn normalizes_endpoint_and_rejects_invalid_settings() {
        let config = Config::from_values(
            None,
            Some("http://localhost:8000/v1".into()),
            Some("3".into()),
        )
        .unwrap();
        assert_eq!(
            config.endpoint.as_deref(),
            Some("http://localhost:8000/v1/")
        );
        assert_eq!(config.max_steps, 3);
        for invalid in ["0", "-1", "oops"] {
            assert!(Config::from_values(None, None, Some(invalid.into())).is_err());
        }
        assert!(Config::from_values(Some(" ".into()), None, None).is_err());
        assert!(Config::from_values(None, Some("file:///tmp".into()), None).is_err());
    }
}
