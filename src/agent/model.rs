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

pub fn triage_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "bash".into(),
            description: "Run one executable-allowlisted command or pipeline without a shell. Pass untrusted or multiline input through stdin.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "minLength": 1, "maxLength": 32768 },
                    "cwd": { "type": "string", "description": "Absolute working directory. Use the matched project's canonical path for repository-scoped commands." },
                    "stdin": { "type": "string", "maxLength": 262144 }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "read".into(),
            description: "Read line-numbered UTF-8 text beneath approved project and skill roots.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "offset": { "type": "integer", "minimum": 1, "maximum": 1000000 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 2000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "write".into(),
            description: "Replace the complete project registry with a valid YAML list of canonical repository paths.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "content": { "type": "string", "maxLength": 65536 }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
    ]
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

pub fn summary_completion_request_for_history(
    system_instructions: impl Into<String>,
    history: Vec<Message>,
    level: ThinkingLevel,
) -> CompletionRequest {
    request_for_history(system_instructions, history, level, Vec::new())
}

pub fn triage_completion_request_for_history(
    system_instructions: impl Into<String>,
    history: Vec<Message>,
    level: ThinkingLevel,
) -> CompletionRequest {
    request_for_history(system_instructions, history, level, triage_tools())
}

pub fn completion_request_for_history(
    system_instructions: impl Into<String>,
    history: Vec<Message>,
    level: ThinkingLevel,
) -> CompletionRequest {
    request_for_history(system_instructions, history, level, compatibility_tools())
}

fn request_for_history(
    system_instructions: impl Into<String>,
    history: Vec<Message>,
    level: ThinkingLevel,
    tools: Vec<ToolDefinition>,
) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: Some(system_instructions.into()),
        chat_history: OneOrMany::many(history).expect("an agent request always has a prompt"),
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params: Some(json!({ "reasoning": reasoning(level) })),
        output_schema: None,
        record_telemetry_content: false,
    }
}
