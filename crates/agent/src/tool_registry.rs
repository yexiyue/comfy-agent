use std::collections::HashMap;

use anyhow::{Context, bail};
use genai::chat::{Tool, ToolCall};
use serde_json::Value;

pub use tools::AgentTool;

#[derive(Default)]
pub struct ToolRegistry {
    definitions: Vec<Tool>,
    implementations: HashMap<&'static str, Box<dyn AgentTool>>,
}

impl ToolRegistry {
    pub fn register<T: AgentTool + 'static>(&mut self, tool: T) -> anyhow::Result<()> {
        let name = tool.name();
        if name.trim().is_empty() {
            bail!("工具名称不能为空");
        }
        if self.implementations.contains_key(name) {
            bail!("工具名称重复：{name}");
        }

        let definition = Tool::new(name)
            .with_description(tool.description())
            .with_schema(tool.schema());
        self.definitions.push(definition);
        self.implementations.insert(name, Box::new(tool));

        Ok(())
    }

    pub fn definitions(&self) -> Vec<Tool> {
        self.definitions.clone()
    }

    pub async fn execute(&self, call: &ToolCall) -> anyhow::Result<Value> {
        let tool = self
            .implementations
            .get(call.fn_name.as_str())
            .with_context(|| format!("未知工具：{}", call.fn_name))?;

        tool.execute(call.fn_arguments.clone()).await
    }
}
