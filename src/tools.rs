use genai::chat::Tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetWeather {
    /// 城市名，例如 Tokyo、上海
    city: String,
    /// 温度单位：C 摄氏度，F 华氏度
    unit: Unit,
}

#[derive(Debug, JsonSchema, Deserialize)]
pub enum Unit {
    C,
    F,
}

impl GetWeather {
    pub fn tool() -> Tool {
        Tool::new("get_weather")
            .with_description("获取指定城市的当前天气。用户询问天气、温度时使用。")
            .with_schema(schemars::schema_for!(GetWeather).to_value())
    }

    pub fn run(&self) -> anyhow::Result<serde_json::Value> {
        Ok(json!({
            "city": self.city,
            "temperature": 22.5,
            "condition": "晴",
            "unit": format!("{:?}", self.unit),
        }))
    }
}
