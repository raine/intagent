use rig_core::OneOrMany;
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::message::Message;
use rig_core::providers::openai::responses_api::{
    Reasoning, ReasoningContext, ReasoningEffort, ReasoningSummaryLevel,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

pub fn reasoning(level: ThinkingLevel) -> serde_json::Value {
    let effort = match level {
        ThinkingLevel::Off => ReasoningEffort::None,
        ThinkingLevel::Minimal => ReasoningEffort::Minimal,
        ThinkingLevel::Low => ReasoningEffort::Low,
        ThinkingLevel::Medium => ReasoningEffort::Medium,
        ThinkingLevel::High => ReasoningEffort::High,
        ThinkingLevel::Xhigh => ReasoningEffort::Xhigh,
        ThinkingLevel::Max => ReasoningEffort::Max,
    };
    let value = Reasoning::new()
        .with_effort(effort)
        .with_summary_level(ReasoningSummaryLevel::Detailed)
        .with_context(ReasoningContext::AllTurns);
    serde_json::to_value(value).expect("Rig reasoning controls serialize")
}

pub fn compatibility_tools() -> Vec<ToolDefinition> {
    ["bash", "read", "write"]
        .into_iter()
        .map(|name| ToolDefinition {
            name: name.to_string(),
            description: format!("Compatibility fixture for the restricted {name} tool"),
            parameters: json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"],
                "additionalProperties": false
            }),
        })
        .collect()
}

pub fn completion_request(
    system_instructions: impl Into<String>,
    user_prompt: impl Into<String>,
    level: ThinkingLevel,
) -> CompletionRequest {
    completion_request_for_history(
        system_instructions,
        vec![Message::user(user_prompt.into())],
        level,
    )
}

pub fn completion_request_for_history(
    system_instructions: impl Into<String>,
    history: Vec<Message>,
    level: ThinkingLevel,
) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: Some(system_instructions.into()),
        chat_history: OneOrMany::many(history).expect("an agent request always has a prompt"),
        documents: Vec::new(),
        tools: compatibility_tools(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: Some(json!({ "reasoning": reasoning(level) })),
        output_schema: None,
        record_telemetry_content: false,
    }
}
