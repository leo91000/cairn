//! JSON-RPC frames written by the stdio sessions, the MCP client and the MCP server.
use serde::Serialize;
use serde_json::Value;

pub const VERSION: &str = "2.0";
pub const METHOD_NOT_FOUND: i64 = -32601;

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Message {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Result {
        id: Value,
        result: Value,
    },
    Error {
        id: Value,
        error: ErrorObject,
    },
}

/// A message with its optional protocol marker. Codex's app-server omits `jsonrpc`.
#[derive(Debug, Serialize)]
pub struct Frame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jsonrpc: Option<&'static str>,
    #[serde(flatten)]
    pub message: Message,
}

impl Frame {
    pub fn new(message: Message, jsonrpc: bool) -> Self {
        Self {
            jsonrpc: jsonrpc.then_some(VERSION),
            message,
        }
    }

    pub fn strict(message: Message) -> Self {
        Self::new(message, true)
    }

    pub fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Self {
        let error = ErrorObject {
            code,
            message: message.to_owned(),
            data,
        };
        Self::strict(Message::Error { id, error })
    }
}
