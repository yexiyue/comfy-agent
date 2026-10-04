//! Public model choices are an explicit subset of the configured provider endpoint.
use crate::{AppState, api::*};
use axum::{Json, extract::State};

#[utoipa::path(get, path = "/api/chat/config", operation_id = "getChatConfig", tag = "chat",
    responses((status = 200, body = ChatConfig)))]
pub async fn config(State(state): State<AppState>) -> Json<ChatConfig> {
    Json((*state.chat_config).clone())
}

impl ChatConfig {
    pub fn new(default_model: &str, models: &str) -> anyhow::Result<Self> {
        let mut ids = vec![default_model.to_owned()];
        for id in models.split(',').map(str::trim).filter(|id| !id.is_empty()) {
            anyhow::ensure!(
                id.split_once("::").map(|p| p.0) == default_model.split_once("::").map(|p| p.0),
                "CHAT_MODELS must use the configured provider"
            );
            if !ids.iter().any(|known| known == id) {
                ids.push(id.into());
            }
        }
        let models = ids
            .into_iter()
            .map(|id| {
                let reasoning_efforts = if id.starts_with("bigmodel::glm-5.3") {
                    vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::High,
                        ReasoningEffort::Max,
                    ]
                } else {
                    vec![]
                };
                ModelOption {
                    id,
                    default_reasoning_effort: reasoning_efforts.first().copied(),
                    reasoning_efforts,
                }
            })
            .collect();
        Ok(Self {
            default_model: default_model.into(),
            models,
        })
    }

    pub fn resolve(
        &self,
        model: Option<String>,
        effort: Option<ReasoningEffort>,
    ) -> Result<(String, Option<String>), runtime::store::StoreError> {
        let id = model.unwrap_or_else(|| self.default_model.clone());
        let model =
            self.models.iter().find(|m| m.id == id).ok_or_else(|| {
                runtime::store::StoreError::Invalid("model is not configured".into())
            })?;
        let effort = effort.or(model.default_reasoning_effort);
        if effort.is_some_and(|e| !model.reasoning_efforts.contains(&e)) {
            return Err(runtime::store::StoreError::Invalid(
                "reasoning effort is not supported for this model".into(),
            ));
        }
        Ok((id, effort.map(|e| e.as_str().into())))
    }
}
