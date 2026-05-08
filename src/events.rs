use serde::Deserialize;

/// A single content block within an assistant message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

/// Token usage data from the API response.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    /// Total context tokens (input + cached).
    pub fn total_context_tokens(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

/// The message body inside an `assistant` event.
#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Top-level NDJSON events from `claude -p --output-format stream-json --verbose`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub enum StreamEvent {
    #[serde(rename = "system")]
    System {
        #[serde(default)]
        subtype: String,
        session_id: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: AssistantMessage,
        session_id: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(rename = "result")]
    Result {
        #[serde(default)]
        subtype: String,
        #[serde(default)]
        is_error: bool,
        result: Option<String>,
        session_id: Option<String>,
        duration_ms: Option<u64>,
        total_cost_usd: Option<f64>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_init_event() {
        let json = r#"{"type":"system","subtype":"init","session_id":"abc-123","cwd":"/tmp","tools":["Bash","Read"],"model":"claude-sonnet-4-20250514","permissionMode":"default"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::System { subtype, session_id, .. } => {
                assert_eq!(subtype, "init");
                assert_eq!(session_id.unwrap(), "abc-123");
            }
            _ => panic!("expected System event"),
        }
    }

    #[test]
    fn test_parse_assistant_text() {
        let json = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"hello world"}],"stop_reason":null},"session_id":"abc-123"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { message, session_id, .. } => {
                assert_eq!(session_id.unwrap(), "abc-123");
                let texts: Vec<&str> = message.content.iter()
                    .filter_map(|c| if let ContentBlock::Text { text } = c { Some(text.as_str()) } else { None })
                    .collect();
                assert_eq!(texts, vec!["hello world"]);
            }
            _ => panic!("expected Assistant event"),
        }
    }

    #[test]
    fn test_parse_assistant_tool_use() {
        let json = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"tool_use","id":"tu_1","name":"Bash","input":{"command":"ls"}}],"stop_reason":null},"session_id":"s1"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { message, .. } => {
                let tools: Vec<&str> = message.content.iter()
                    .filter_map(|c| if let ContentBlock::ToolUse { name, .. } = c { Some(name.as_str()) } else { None })
                    .collect();
                assert_eq!(tools, vec!["Bash"]);
            }
            _ => panic!("expected Assistant event"),
        }
    }

    #[test]
    fn test_parse_result_success() {
        let json = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1234,"result":"done","session_id":"abc-123","total_cost_usd":0.05}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Result { is_error, result, session_id, duration_ms, .. } => {
                assert!(!is_error);
                assert_eq!(result.unwrap(), "done");
                assert_eq!(session_id.unwrap(), "abc-123");
                assert_eq!(duration_ms.unwrap(), 1234);
            }
            _ => panic!("expected Result event"),
        }
    }

    #[test]
    fn test_parse_result_error() {
        let json = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in","session_id":"s1","total_cost_usd":0}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Result { is_error, result, .. } => {
                assert!(is_error);
                assert_eq!(result.unwrap(), "Not logged in");
            }
            _ => panic!("expected Result event"),
        }
    }

    #[test]
    fn test_parse_unknown_event_is_other() {
        let json = r#"{"type":"rate_limit_event","rate_limit_info":{}}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::Other));
    }

    #[test]
    fn test_parse_assistant_with_usage() {
        let json = r#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-20250514","role":"assistant","content":[{"type":"text","text":"hi"}],"stop_reason":null,"usage":{"input_tokens":100,"cache_creation_input_tokens":20000,"cache_read_input_tokens":5000,"output_tokens":10}},"session_id":"s1"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { message, .. } => {
                let usage = message.usage.unwrap();
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.cache_creation_input_tokens, 20000);
                assert_eq!(usage.cache_read_input_tokens, 5000);
                assert_eq!(usage.output_tokens, 10);
                assert_eq!(usage.total_context_tokens(), 25100);
            }
            _ => panic!("expected Assistant event"),
        }
    }

    #[test]
    fn test_parse_assistant_without_usage() {
        let json = r#"{"type":"assistant","message":{"id":"msg_1","model":"m","role":"assistant","content":[{"type":"text","text":"hi"}],"stop_reason":null},"session_id":"s1"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        match event {
            StreamEvent::Assistant { message, .. } => {
                assert!(message.usage.is_none());
            }
            _ => panic!("expected Assistant event"),
        }
    }

    #[test]
    fn test_content_block_mixed() {
        let json = r#"{"type":"assistant","message":{"id":"m1","model":"m","role":"assistant","content":[{"type":"text","text":"before"},{"type":"tool_use","id":"t1","name":"Edit","input":{}},{"type":"text","text":"after"}],"stop_reason":null},"session_id":"s"}"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        if let StreamEvent::Assistant { message, .. } = event {
            assert_eq!(message.content.len(), 3);
        } else {
            panic!("expected Assistant");
        }
    }
}
