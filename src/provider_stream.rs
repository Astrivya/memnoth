//! Streamed provider transport: SSE framing plus a reducer per wire shape.
//!
//! The non-streaming path ([`crate::provider`]'s `post_json`) reads one whole
//! JSON. This module reads `text/event-stream`: the model answers as the
//! bytes arrive, deltas go to the screen as they land, and the result is the
//! same `Step`-shaped data the turn loop already consumes — text, tool calls,
//! the assistant message exactly as the provider sent it, and the accounting.
//!
//! Two rules keep it honest:
//!
//! * If the gateway never starts a stream (it answers plain JSON, or the
//!   request is refused), this returns an error before any delta is emitted
//!   and the caller falls back to the non-streaming path. Nothing is
//!   double-counted.
//! * Once a stream has started, it never falls back. A truncated stream is an
//!   error, not a shortened answer: finishing a turn with half a reply would
//!   persist an answer the model never finished.

use std::io::{BufRead, BufReader};

use anyhow::Result;
use serde_json::{Value, json};

use crate::provider::Kind;

/// One tool call the stream asked for, rebuilt from its deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// What a finished stream produced: the same shape as one absorbed reply.
#[derive(Debug, Clone)]
pub(crate) struct StreamOutcome {
    pub text: String,
    pub calls: Vec<StreamCall>,
    /// The assistant message exactly as the provider sent it, so the next
    /// tool round can echo it back unchanged.
    pub raw_content: Value,
    /// Reasoning captured for the thinking block, when the model sent any.
    pub reasoning: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One piece of a stream, as it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delta {
    Text(String),
    Reasoning(String),
}

/// An accumulator for one tool call whose arguments arrive in pieces.
#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

impl PartialCall {
    fn finish(self) -> StreamCall {
        let input = if self.arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&self.arguments)
                .unwrap_or_else(|_| json!({ "_raw": self.arguments }))
        };
        StreamCall {
            id: self.id,
            name: self.name,
            input,
        }
    }
}

/// One line of the SSE wire, already split.
fn sse_data(line: &str) -> Option<&str> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    line.strip_prefix("data:").map(|rest| rest.trim_start())
}

/// Why a stream did not produce an answer.
///
/// `Fallback` means the stream never started, so the whole-reply path is
/// the right next move. `Fatal` means the provider answered and said no —
/// retrying it as a whole reply cannot change the answer, and the refusal
/// is the sentence the person needs, so it is the one they get.
#[derive(Debug)]
pub(crate) enum StreamError {
    Fallback(anyhow::Error),
    Fatal(anyhow::Error),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Fallback(error) | StreamError::Fatal(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for StreamError {}

/// Send one streaming request and reduce the reply as it arrives.
///
/// `on_delta` fires for every text or reasoning piece, in order. The caller
/// decides what a piece is worth: the screen coalesces them into one live
/// entry, a headless caller may ignore them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn post_stream(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: &Value,
    kind: Kind,
    on_delta: &mut dyn FnMut(Delta),
) -> Result<StreamOutcome, StreamError> {
    let mut request = client.post(url).json(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().map_err(|error| {
        StreamError::Fallback(anyhow::Error::new(error).context(format!("cannot reach {url}")))
    })?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().unwrap_or_default();
        return Err(StreamError::Fatal(anyhow::anyhow!(
            "{url} answered {status}: {}",
            crate::provider::clip(&text, 600)
        )));
    }

    let reader = BufReader::new(response);
    // The reducers are incremental: every parsed chunk is applied — and its
    // deltas emitted — as it arrives, so the screen really streams. The
    // fallback rule only holds before the first chunk: once one has been
    // applied and shown, a broken connection is an error, never a retry
    // that would answer the same prompt twice.
    let mut openai = OpenAiStream::default();
    let mut anthropic = AnthropicStream::default();
    let mut saw_chunk = false;
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                let error =
                    anyhow::Error::new(error).context(format!("the stream from {url} broke"));
                return Err(if saw_chunk {
                    StreamError::Fatal(error)
                } else {
                    StreamError::Fallback(error)
                });
            }
        };
        let trimmed = line.trim();
        let data = match sse_data(&line) {
            Some(data) => data,
            // A gateway that ignored `stream` and answered the whole reply in
            // one JSON document is still an answer: the reducers read it as a
            // single chunk. The whole-reply path is only for when nothing
            // usable arrived at all.
            None if trimmed.starts_with('{') => trimmed,
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        match serde_json::from_str::<Value>(data) {
            Ok(payload) => {
                saw_chunk = true;
                match kind {
                    Kind::Anthropic => anthropic.apply(&payload, on_delta),
                    Kind::OpenAi | Kind::Memnoth | Kind::Kilo => openai.apply(&payload, on_delta),
                }
            }
            Err(_) if !saw_chunk => {
                // The gateway answered something that is not SSE at all
                // (an HTML error page): let the caller fall back to the
                // whole-reply path rather than inventing a stream.
                return Err(StreamError::Fallback(anyhow::anyhow!(
                    "{url} did not answer with a stream"
                )));
            }
            Err(_) => continue,
        }
    }
    if !saw_chunk {
        return Err(StreamError::Fallback(anyhow::anyhow!(
            "{url} answered without a stream"
        )));
    }

    let outcome: StreamOutcome = match kind {
        Kind::Anthropic => anthropic.finish(),
        Kind::OpenAi | Kind::Memnoth | Kind::Kilo => openai.finish(),
    };
    if outcome.text.trim().is_empty() && outcome.calls.is_empty() {
        return Err(StreamError::Fatal(anyhow::anyhow!(
            "the stream from {url} ended without an answer"
        )));
    }
    Ok(outcome)
}

/// Incremental OpenAI reducer. Each chunk is applied the moment it lands,
/// so its deltas reach the screen before the next chunk is read.
#[derive(Default)]
struct OpenAiStream {
    text: String,
    reasoning: String,
    calls: Vec<PartialCall>,
    input_tokens: u64,
    output_tokens: u64,
    finish_reason: Option<String>,
}

impl OpenAiStream {
    fn apply(&mut self, payload: &Value, on_delta: &mut dyn FnMut(Delta)) {
        let OpenAiStream {
            text,
            reasoning,
            calls,
            input_tokens,
            output_tokens,
            finish_reason,
        } = self;
        if let Some(usage) = payload.get("usage").filter(|usage| !usage.is_null()) {
            *input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(*input_tokens);
            *output_tokens = usage["completion_tokens"]
                .as_u64()
                .unwrap_or(*output_tokens);
        }
        for choice in payload["choices"].as_array().into_iter().flatten() {
            // A gateway that ignored `stream` sent the whole message instead
            // of deltas: read it as the chunk it is.
            let message = &choice["message"];
            if !message.is_null() {
                if let Some(piece) = message["content"].as_str() {
                    text.push_str(piece);
                    on_delta(Delta::Text(piece.to_string()));
                }
                for key in ["reasoning", "reasoning_content"] {
                    if let Some(piece) = message[key].as_str() {
                        reasoning.push_str(piece);
                        on_delta(Delta::Reasoning(piece.to_string()));
                    }
                }
                for chunk in message["tool_calls"].as_array().into_iter().flatten() {
                    let mut call = PartialCall {
                        id: chunk["id"].as_str().unwrap_or_default().to_string(),
                        name: chunk["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        arguments: match &chunk["function"]["arguments"] {
                            Value::String(raw) => raw.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        },
                    };
                    if call.name.is_empty() {
                        continue;
                    }
                    calls.push(std::mem::take(&mut call));
                }
                if let Some(reason) = choice["finish_reason"].as_str() {
                    *finish_reason = Some(reason.to_string());
                }
                continue;
            }
            let delta = &choice["delta"];
            if let Some(piece) = delta["content"].as_str() {
                text.push_str(piece);
                on_delta(Delta::Text(piece.to_string()));
            }
            for key in ["reasoning", "reasoning_content"] {
                if let Some(piece) = delta[key].as_str() {
                    reasoning.push_str(piece);
                    on_delta(Delta::Reasoning(piece.to_string()));
                }
            }
            for chunk in delta["tool_calls"].as_array().into_iter().flatten() {
                let index = chunk["index"].as_u64().unwrap_or(0) as usize;
                while calls.len() <= index {
                    calls.push(PartialCall::default());
                }
                let call = &mut calls[index];
                if let Some(id) = chunk["id"].as_str() {
                    call.id = id.to_string();
                }
                if let Some(name) = chunk["function"]["name"].as_str() {
                    call.name = name.to_string();
                }
                if let Some(arguments) = chunk["function"]["arguments"].as_str() {
                    call.arguments.push_str(arguments);
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                *finish_reason = Some(reason.to_string());
            }
        }
    }

    fn finish(self) -> StreamOutcome {
        let OpenAiStream {
            mut text,
            reasoning,
            calls,
            input_tokens,
            output_tokens,
            finish_reason,
        } = self;
        if text.trim().is_empty() && calls.iter().all(|call| call.name.is_empty()) {
            // Keep the finish reason visible in the error the caller raises.
            if let Some(reason) = finish_reason.as_deref() {
                text.push_str(&format!("(stream ended with {reason})"));
            }
        }
        if finish_reason.as_deref() == Some("length") {
            text.push_str(
                "\n\n[stopped at max_tokens — raise provider.max_tokens for a longer answer]",
            );
        }

        let finished: Vec<StreamCall> = calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(PartialCall::finish)
            .collect();
        let tool_calls: Vec<Value> = finished
            .iter()
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.input.to_string() },
                })
            })
            .collect();
        let mut message = json!({
            "content": if text.is_empty() { Value::Null } else { Value::String(text.clone()) },
            "tool_calls": tool_calls,
        });
        if !reasoning.is_empty() {
            message["reasoning"] = Value::String(reasoning.clone());
        }
        StreamOutcome {
            text: text.trim().to_string(),
            calls: finished,
            raw_content: message,
            reasoning,
            input_tokens,
            output_tokens,
        }
    }
}

/// Feed a whole reply through the incremental reducer: the tests read the
/// same code path the wire uses, one chunk at a time.
#[cfg(test)]
fn reduce_openai(payloads: &[Value], on_delta: &mut dyn FnMut(Delta)) -> StreamOutcome {
    let mut state = OpenAiStream::default();
    for payload in payloads {
        state.apply(payload, on_delta);
    }
    state.finish()
}

/// The Anthropic shape: `message_start` carries input usage, `content_block`
/// events carry the pieces, `message_delta` carries output usage and the stop
/// reason. Thinking blocks keep their signatures, because the provider
/// rejects a tool round whose thinking block was rebuilt.
/// Incremental Anthropic reducer: see `OpenAiStream` for why chunk-at-a-time.
#[derive(Default)]
struct AnthropicStream {
    blocks: Vec<Block>,
    input_tokens: u64,
    output_tokens: u64,
    stop_reason: Option<String>,
}

#[derive(Debug)]
struct Block {
    kind: String,
    text: String,
    reasoning: String,
    signature: String,
    call: Option<PartialCall>,
}

impl AnthropicStream {
    fn apply(&mut self, payload: &Value, on_delta: &mut dyn FnMut(Delta)) {
        let AnthropicStream {
            blocks,
            input_tokens,
            output_tokens,
            stop_reason,
        } = self;
        // A gateway that ignored `stream` sent the whole message: read its
        // blocks as the chunk they are, so one code path serves both.
        if payload["content"].is_array() {
            let usage = &payload["usage"];
            *input_tokens = usage["input_tokens"].as_u64().unwrap_or(*input_tokens);
            *output_tokens = usage["output_tokens"].as_u64().unwrap_or(*output_tokens);
            if let Some(reason) = payload["stop_reason"].as_str() {
                *stop_reason = Some(reason.to_string());
            }
            for content in payload["content"].as_array().into_iter().flatten() {
                let kind = content["type"].as_str().unwrap_or_default().to_string();
                let mut call = None;
                if kind == "tool_use" {
                    let arguments = content["input"].to_string();
                    call = Some(PartialCall {
                        id: content["id"].as_str().unwrap_or_default().to_string(),
                        name: content["name"].as_str().unwrap_or_default().to_string(),
                        arguments,
                    });
                }
                blocks.push(Block {
                    kind,
                    text: content["text"].as_str().unwrap_or_default().to_string(),
                    reasoning: content["thinking"].as_str().unwrap_or_default().to_string(),
                    signature: content["signature"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    call,
                });
            }
            return;
        }
        match payload["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let usage = &payload["message"]["usage"];
                *input_tokens = usage["input_tokens"].as_u64().unwrap_or(*input_tokens);
            }
            "content_block_start" => {
                let content = &payload["content_block"];
                let kind = content["type"].as_str().unwrap_or_default().to_string();
                let mut call = None;
                if kind == "tool_use" {
                    call = Some(PartialCall {
                        id: content["id"].as_str().unwrap_or_default().to_string(),
                        name: content["name"].as_str().unwrap_or_default().to_string(),
                        arguments: String::new(),
                    });
                }
                blocks.push(Block {
                    kind,
                    text: content["text"].as_str().unwrap_or_default().to_string(),
                    reasoning: content["thinking"].as_str().unwrap_or_default().to_string(),
                    signature: content["signature"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    call,
                });
            }
            "content_block_delta" => {
                let index = payload["index"].as_u64().unwrap_or(0) as usize;
                let delta = &payload["delta"];
                let Some(block) = blocks.get_mut(index) else {
                    return;
                };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let piece = delta["text"].as_str().unwrap_or_default();
                        block.text.push_str(piece);
                        on_delta(Delta::Text(piece.to_string()));
                    }
                    "thinking_delta" => {
                        let piece = delta["thinking"].as_str().unwrap_or_default();
                        block.reasoning.push_str(piece);
                        on_delta(Delta::Reasoning(piece.to_string()));
                    }
                    "signature_delta" => {
                        block
                            .signature
                            .push_str(delta["signature"].as_str().unwrap_or_default());
                    }
                    "input_json_delta" => {
                        if let Some(call) = block.call.as_mut() {
                            call.arguments
                                .push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = payload["delta"]["stop_reason"].as_str() {
                    *stop_reason = Some(reason.to_string());
                }
                let usage = &payload["usage"];
                *output_tokens = usage["output_tokens"].as_u64().unwrap_or(*output_tokens);
            }
            _ => {}
        }
    }

    fn finish(self) -> StreamOutcome {
        let AnthropicStream {
            blocks,
            input_tokens,
            output_tokens,
            stop_reason,
        } = self;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut finished = Vec::new();
        let mut content: Vec<Value> = Vec::new();
        for block in blocks {
            match block.kind.as_str() {
                "text" => {
                    if !block.text.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&block.text);
                        content.push(json!({ "type": "text", "text": block.text }));
                    }
                }
                "thinking" => {
                    reasoning.push_str(&block.reasoning);
                    content.push(json!({
                        "type": "thinking",
                        "thinking": block.reasoning,
                        "signature": block.signature,
                    }));
                }
                "tool_use" => {
                    if let Some(call) = block.call {
                        let call = call.finish();
                        content.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.input,
                        }));
                        finished.push(call);
                    }
                }
                _ => {}
            }
        }
        if stop_reason.as_deref() == Some("max_tokens") {
            text.push_str(
                "\n\n[stopped at max_tokens — raise provider.max_tokens for a longer answer]",
            );
        }

        StreamOutcome {
            text: text.trim().to_string(),
            calls: finished,
            raw_content: Value::Array(content),
            reasoning,
            input_tokens,
            output_tokens,
        }
    }
}

/// Feed a whole reply through the incremental reducer: the tests read the
/// same code path the wire uses, one chunk at a time.
#[cfg(test)]
fn reduce_anthropic(payloads: &[Value], on_delta: &mut dyn FnMut(Delta)) -> StreamOutcome {
    let mut state = AnthropicStream::default();
    for payload in payloads {
        state.apply(payload, on_delta);
    }
    state.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai_fixture() -> Vec<Value> {
        vec![
            json!({"choices": [{"delta": {"content": "Hello ", "reasoning": "plan: "}}]}),
            json!({"choices": [{
                "delta": {
                    "content": "world",
                    "reasoning_content": "two",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {"name": "kg_search", "arguments": "{\"q"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]}),
            json!({"choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "function": {"arguments": "uery\": \"x\"}"}
            }]}}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 11, "completion_tokens": 7}}),
        ]
    }

    #[test]
    fn openai_chunks_reassemble_text_reasoning_and_calls() {
        let mut deltas = Vec::new();
        let outcome = reduce_openai(&openai_fixture(), &mut |delta| deltas.push(delta));
        assert_eq!(outcome.text, "Hello world");
        assert_eq!(outcome.reasoning, "plan: two");
        assert_eq!(outcome.input_tokens, 11);
        assert_eq!(outcome.output_tokens, 7);
        assert_eq!(outcome.calls.len(), 1);
        assert_eq!(outcome.calls[0].name, "kg_search");
        assert_eq!(outcome.calls[0].input["query"], "x");
        assert_eq!(
            deltas,
            vec![
                Delta::Text("Hello ".to_string()),
                Delta::Reasoning("plan: ".to_string()),
                Delta::Text("world".to_string()),
                Delta::Reasoning("two".to_string()),
            ]
        );
    }

    #[test]
    fn sse_data_skips_comments_and_blank_lines() {
        assert_eq!(sse_data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data:{\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data(": ping"), None);
        assert_eq!(sse_data(""), None);
        assert_eq!(sse_data("event: message"), None);
    }

    fn anthropic_fixture() -> Vec<Value> {
        serde_json::from_str(
            r#"[
            {"type":"message_start","message":{"usage":{"input_tokens":9}}},
            {"type":"content_block_start","index":0,"content_block":{"type":"thinking"}},
            {"type":"content_block_start","index":1,"content_block":{"type":"text"}},
            {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me "}},
            {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig1"}},
            {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Done."}},
            {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}},
            {"type":"message_stop"}
        ]"#,
        )
        .expect("fixture parses")
    }

    #[test]
    fn anthropic_stream_keeps_thinking_and_its_signature() {
        let mut deltas = Vec::new();
        let outcome = reduce_anthropic(&anthropic_fixture(), &mut |delta| deltas.push(delta));
        assert_eq!(outcome.text, "Done.");
        assert_eq!(outcome.reasoning, "let me ");
        assert_eq!(outcome.input_tokens, 9);
        assert_eq!(outcome.output_tokens, 4);
        assert_eq!(outcome.raw_content[0]["type"], "thinking");
        assert_eq!(outcome.raw_content[0]["signature"], "sig1");
        assert_eq!(
            deltas,
            vec![
                Delta::Reasoning("let me ".to_string()),
                Delta::Text("Done.".to_string()),
            ]
        );
    }
}
