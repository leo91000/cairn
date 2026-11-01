//! The model-facing budget is separate from activity previews and wire guards.
//!
//! This module only formats completed results. It never invokes an operation,
//! retains its payload, follows a reference, or changes authorization.
use crate::{
    error::{Error, Result},
    validation::encoded_size,
};
use serde_json::{Value, json};

pub(super) const LIMIT: usize = 1024 * 1024;

const NOTICE: &str = "Tool result truncated to the 1 MiB encoded response budget. The original tool outcome is preserved. Omitted data was not retained; do not repeat a side-effecting operation to recover it.";

pub(super) const POLICY: &str = " Cairn limits each complete encoded tool result to 1 MiB. Large completed results are visibly truncated without repeating the operation; the original success/error outcome is preserved and omitted data is not retained.";

#[cfg(test)]
thread_local! {
    static MEASURED_ENCODED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn size(value: &Value) -> usize {
    let bytes = encoded_size(value).expect("JSON values serialize into a byte counter");
    #[cfg(test)]
    MEASURED_ENCODED_BYTES.with(|measured| measured.set(measured.get() + bytes));
    bytes
}

fn marker() -> Value {
    json!({
        "truncated": true,
        "retained": false,
    })
}

/// A worker can discard an oversized completed value before sending its frame.
/// Only that value's encoded size is known, not the final duplicated MCP size.
pub(super) fn omitted_value(value_bytes: usize) -> Value {
    let mut structured = marker();
    structured["originalValueBytes"] = value_bytes.into();
    json!({
        "content": [{ "type": "text", "text": NOTICE }],
        "structuredContent": structured,
    })
}

/// Count the complete result after protocol adaptation, including escaping,
/// duplicated representations and metadata. Below-budget authorized data is
/// returned unchanged, including credentials delivered by granted tools.
pub(super) fn finish(mut result: Value) -> Result<Value> {
    if !result.is_object() {
        return Err(Error::bad_gateway(
            "MCP response result must be an object. Tool outcome is unknown; do not repeat the operation automatically.",
        ));
    }
    let original_bytes = size(&result);
    if original_bytes <= LIMIT {
        return Ok(result);
    }

    let content = match result
        .as_object_mut()
        .and_then(|object| object.remove("content"))
    {
        Some(Value::Array(content)) => content,
        _ => Vec::new(),
    };
    let mut structured = marker();
    structured["originalResultBytes"] = original_bytes.into();
    result["structuredContent"] = structured;
    result["content"] = json!([{ "type": "text", "text": NOTICE }]);

    prune_metadata(&mut result)?;

    let mut result_bytes = size(&result);
    for mut block in content {
        // The notice is always the first block, so each additional complete
        // block contributes exactly its encoded bytes plus one array comma.
        let Some(available_block_bytes) = LIMIT.checked_sub(result_bytes + 1) else {
            break;
        };
        let block_bytes = size(&block);
        if block_bytes <= available_block_bytes {
            result["content"].as_array_mut().unwrap().push(block);
            result_bytes += block_bytes + 1;
            continue;
        }

        if block["type"] == "text"
            && let Some(Value::String(text)) =
                block.as_object_mut().and_then(|block| block.remove("text"))
        {
            block["text"] = "".into();
            if size(&block) > available_block_bytes {
                break;
            }

            // Search UTF-8 boundaries against the remaining encoded block
            // budget. Escaping can cost six bytes per source byte. Earlier
            // retained blocks are never serialized again during the search.
            let mut low = 0;
            let mut high = text.len();
            while low < high {
                let mut end = low + (high - low).div_ceil(2);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end == low {
                    break;
                }
                block["text"] = text[..end].into();
                if size(&block) <= available_block_bytes {
                    low = end;
                } else {
                    high = end - 1;
                }
            }
            block["text"] = text[..low].into();
            result_bytes += size(&block) + 1;
            result["content"].as_array_mut().unwrap().push(block);
        }
        // Binary/media/resource blocks are retained whole or omitted. A text
        // prefix is explicitly only a preview, never a sliced JSON document.
        break;
    }
    let final_bytes = size(&result);
    debug_assert_eq!(result_bytes, final_bytes);
    debug_assert!(final_bytes <= LIMIT);
    Ok(result)
}

/// Omit whole optional fields, largest first, reserving complete auth
/// metadata. Count each field once; many small extensions must not cause a
/// quadratic series of whole-result serializations.
fn prune_metadata(result: &mut Value) -> Result<()> {
    if size(result) <= LIMIT {
        return Ok(());
    }
    result["structuredContent"]["metadataOmitted"] = true.into();
    let mut bytes = size(result);
    let mut candidates = Vec::new();
    for (key, value) in result.as_object().unwrap() {
        if matches!(
            key.as_str(),
            "content" | "structuredContent" | "isError" | "resultType"
        ) {
            continue;
        }
        if key == "_meta"
            && let Some(meta) = value.as_object()
        {
            for (key, value) in meta {
                let field_bytes = size(&json!(key)) + 1 + size(value);
                // A recognized challenge is considered only after every
                // optional sibling. It can be omitted only if it cannot fit
                // alongside the bounded mandatory result envelope by itself.
                let optional = key != "mcp/www_authenticate";
                candidates.push((optional, field_bytes, Some(key.clone()), "_meta".to_owned()));
            }
            continue;
        }
        let field_bytes = size(&json!(key)) + 1 + size(value);
        candidates.push((true, field_bytes, None, key.clone()));
    }
    candidates.sort_unstable();
    while bytes > LIMIT {
        let Some((_, field_bytes, meta_key, key)) = candidates.pop() else {
            return Err(Error::bad_gateway(
                "Mandatory MCP result metadata exceeds the response budget. Tool outcome is unknown; do not repeat the operation automatically.",
            ));
        };
        let object = if meta_key.is_some() {
            result["_meta"].as_object_mut().unwrap()
        } else {
            result.as_object_mut().unwrap()
        };
        let comma_bytes = usize::from(object.len() > 1);
        object.remove(meta_key.as_deref().unwrap_or(&key));
        bytes -= field_bytes + comma_bytes;
    }
    debug_assert_eq!(bytes, size(result));
    Ok(())
}

fn truncated_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "truncated": { "const": true },
            "retained": { "const": false },
            "originalResultBytes": { "type": "integer", "minimum": 0 },
            "originalValueBytes": { "type": "integer", "minimum": 0 },
            "metadataOmitted": { "type": "boolean" },
        },
        "required": ["truncated", "retained"],
        "additionalProperties": false,
    })
}

fn invalid_catalog() -> Error {
    Error::bad_gateway("MCP server returned an invalid tool catalog.")
}

/// Validate each advertisement descriptor before transforming its fields.
/// Only this response copy changes; upstream discovery/storage and grant
/// filtering retain their existing contracts.
pub(super) fn advertise(listing: Value, gateway: bool) -> Result<Value> {
    let Value::Object(mut listing) = listing else {
        return Err(invalid_catalog());
    };
    let Some(Value::Array(tools)) = listing.remove("tools") else {
        return Err(invalid_catalog());
    };
    let tools = tools
        .into_iter()
        .map(|tool| describe(tool, gateway))
        .collect::<Result<Vec<_>>>()?;
    listing.insert("tools".to_owned(), Value::Array(tools));
    Ok(Value::Object(listing))
}

fn describe(tool: Value, gateway: bool) -> Result<Value> {
    let Value::Object(mut tool) = tool else {
        return Err(invalid_catalog());
    };
    let valid_name = tool
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.is_empty());
    let valid_input = tool.get("inputSchema").is_some_and(Value::is_object);
    let valid_description = tool.get("description").is_none_or(Value::is_string);
    if !valid_name || !valid_input || !valid_description {
        return Err(invalid_catalog());
    }

    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut description = format!("{description}{POLICY}");
    if let Some(schema) = tool.remove("outputSchema") {
        if gateway {
            // External schemas stay opaque: no dialect/ref relocation,
            // compilation or network resolution. Withdraw only this copy.
            description.push_str(" The gateway omits the upstream outputSchema because a truncated response has a different shape.");
        } else {
            // Only the verified source-owned draft07 profile is transformed.
            // Definitions keep their root location and local refs keep meaning.
            let Value::Object(mut schema) = schema else {
                return Err(invalid_catalog());
            };
            let mut advertised = serde_json::Map::new();
            advertised.insert("type".to_owned(), "object".into());
            for key in ["$schema", "definitions"] {
                if let Some(value) = schema.remove(key) {
                    advertised.insert(key.to_owned(), value);
                }
            }
            advertised.insert("anyOf".to_owned(), json!([schema, truncated_schema()]));
            tool.insert("outputSchema".to_owned(), Value::Object(advertised));
        }
    }
    tool.insert("description".to_owned(), description.into());
    Ok(Value::Object(tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounded(result: Value) -> Value {
        let result = finish(result).unwrap();
        assert!(size(&result) <= LIMIT);
        serde_json::from_slice(&serde_json::to_vec(&result).unwrap()).unwrap()
    }

    #[test]
    fn complete_authorized_values_and_exact_limit_are_unchanged() {
        let value = json!({
            "content": [{ "type": "text", "text": "granted-credential-fixture" }],
            "structuredContent": { "token": "granted-credential-fixture" },
            "_meta": { "producer": "fixture" },
        });
        assert_eq!(bounded(value.clone()), value);

        let mut exact = json!({ "content": [{ "type": "text", "text": "" }] });
        let overhead = size(&exact);
        exact["content"][0]["text"] = "a".repeat(LIMIT - overhead).into();
        assert_eq!(size(&exact), LIMIT);
        assert_eq!(bounded(exact.clone()), exact);
        exact["content"][0]["text"] = "a".repeat(LIMIT - overhead + 1).into();
        assert_eq!(bounded(exact)["structuredContent"]["truncated"], true);
    }

    #[test]
    fn escaped_unicode_error_and_complete_auth_metadata_survive() {
        let text = "\u{0}\"\\é🦀".repeat(LIMIT / 8);
        let challenge = "Bearer error=\"insufficient_scope\", scope=\"read\"";
        let original = json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": { "large": text },
            "isError": true,
            "resultType": "complete",
            "status": "failed",
            "producerCallId": "fixture-call",
            "_meta": { "mcp/www_authenticate": challenge },
        });
        let original_size = size(&original);
        let result = bounded(original);
        assert_eq!(result["isError"], true);
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["status"], "failed");
        assert_eq!(result["producerCallId"], "fixture-call");
        assert_eq!(result["_meta"]["mcp/www_authenticate"], challenge);
        assert_eq!(
            result["structuredContent"]["originalResultBytes"],
            original_size
        );
        assert_eq!(result["structuredContent"]["retained"], false);
        let preview = result["content"][1]["text"].as_str().unwrap();
        assert!(text.starts_with(preview));
        assert!(!preview.is_empty());
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("truncated")
        );
    }

    #[test]
    fn oversized_optional_metadata_is_omitted_whole_without_losing_small_challenge() {
        let result = bounded(json!({
            "content": [{ "type": "text", "text": "completed" }],
            "isError": true,
            "resultType": "complete",
            "extension": "x".repeat(LIMIT + 1),
            "_meta": {
                "mcp/www_authenticate": "Bearer scope=\"read\"",
                "opaque": "y".repeat(LIMIT + 1),
            },
        }));
        assert!(result.get("extension").is_none());
        assert!(result["_meta"].get("opaque").is_none());
        assert_eq!(
            result["_meta"]["mcp/www_authenticate"],
            "Bearer scope=\"read\""
        );
        assert_eq!(result["structuredContent"]["metadataOmitted"], true);
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn many_tiny_optional_metadata_fields_cannot_evict_a_small_auth_challenge() {
        let challenge = json!(["Bearer error=\"insufficient_scope\", scope=\"read\""]);
        let mut meta = serde_json::Map::new();
        meta.insert("mcp/www_authenticate".to_owned(), challenge.clone());
        for index in 0..90_000 {
            meta.insert(format!("x{index:06}"), json!(0));
        }
        let original = json!({
            "content": [{ "type": "text", "text": "denied" }],
            "isError": true,
            "_meta": meta,
        });
        let wire = json!({ "jsonrpc": "2.0", "id": 1, "result": original });
        let wire_bytes = serde_json::to_vec(&wire).unwrap().len();
        assert!(wire_bytes > LIMIT);
        assert!(wire_bytes < 2_000_000);

        MEASURED_ENCODED_BYTES.with(|measured| measured.set(0));
        let result = finish(original).unwrap();
        let measured = MEASURED_ENCODED_BYTES.with(std::cell::Cell::get);
        assert!(
            measured < 8 * wire_bytes,
            "Metadata sizing revisited {measured} bytes for a {wire_bytes}-byte fixture"
        );
        assert!(serde_json::to_vec(&result).unwrap().len() <= LIMIT);
        assert_eq!(result["_meta"]["mcp/www_authenticate"], challenge);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["metadataOmitted"], true);
        assert_eq!(result["structuredContent"]["retained"], false);
    }

    #[test]
    fn auth_challenge_that_cannot_fit_itself_is_omitted_whole() {
        let result = bounded(json!({
            "content": [{ "type": "text", "text": "denied" }],
            "isError": true,
            "_meta": { "mcp/www_authenticate": "x".repeat(LIMIT + 1) },
        }));
        assert!(result["_meta"].get("mcp/www_authenticate").is_none());
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["metadataOmitted"], true);
    }

    #[test]
    fn protocol_adaptation_rejects_non_object_results_before_indexing_in_all_versions() {
        for modern in [false, true] {
            for malformed in [
                json!(42),
                json!([]),
                json!("x".repeat(LIMIT + 1)),
                json!(["x".repeat(LIMIT + 1)]),
            ] {
                let error =
                    super::super::finish_result(malformed, "tools/call", modern, true).unwrap_err();
                assert_eq!(error.status, 502);
                assert_eq!(
                    error.message,
                    "MCP response result must be an object. Tool outcome is unknown; do not repeat the operation automatically."
                );
            }
        }
    }

    #[test]
    fn advertisement_rejects_malformed_descriptors_and_mixed_catalogs_in_all_versions() {
        let valid = json!({
            "name": "fixture",
            "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" },
        });
        for malformed in [
            json!(42),
            json!([]),
            json!(null),
            json!(true),
            json!("fixture"),
            json!({}),
            json!({ "name": 42, "inputSchema": {} }),
            json!({ "name": "", "inputSchema": {} }),
            json!({ "name": "fixture", "inputSchema": [] }),
            json!({ "name": "fixture", "inputSchema": {}, "description": 42 }),
        ] {
            for modern in [false, true] {
                for gateway in [false, true] {
                    for tools in [
                        json!([malformed]),
                        json!([valid, malformed]),
                        json!([malformed, valid]),
                    ] {
                        let error = super::super::finish_result(
                            json!({ "tools": tools }),
                            "tools/list",
                            modern,
                            gateway,
                        )
                        .unwrap_err();
                        assert_eq!(error.status, 502);
                        assert_eq!(
                            error.message,
                            "MCP server returned an invalid tool catalog."
                        );
                    }
                }
            }
        }
        for malformed in [
            json!(null),
            json!(42),
            json!([]),
            json!({}),
            json!({ "tools": null }),
            json!({ "tools": 42 }),
            json!({ "tools": {} }),
        ] {
            assert_eq!(
                advertise(malformed, true).unwrap_err().message,
                "MCP server returned an invalid tool catalog."
            );
        }
        let invalid_owned_schema =
            json!({ "tools": [{ "name": "fixture", "inputSchema": {}, "outputSchema": [] }] });
        assert!(advertise(invalid_owned_schema, false).is_err());
    }

    #[test]
    fn advertisement_preserves_valid_input_permissions_and_extensions() {
        let canonical = json!({
            "name": "fixture",
            "description": "Original description.",
            "inputSchema": { "type": "object", "$ref": "https://invalid.example/do-not-fetch" },
            "outputSchema": { "$ref": "#", "$defs": {} },
            "annotations": { "readOnlyHint": false },
            "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": ["read"] }] },
            "extension": [42, null],
        });
        let result = advertise(
            json!({ "tools": [canonical], "nextCursor": "cursor" }),
            true,
        )
        .unwrap();
        let advertised = &result["tools"][0];
        for key in ["name", "inputSchema", "annotations", "_meta", "extension"] {
            assert_eq!(advertised[key], canonical[key]);
        }
        assert_eq!(result["nextCursor"], "cursor");
        assert!(advertised.get("outputSchema").is_none());
        assert!(
            advertised["description"]
                .as_str()
                .unwrap()
                .starts_with("Original description.")
        );
        assert!(canonical.get("outputSchema").is_some());
    }

    #[test]
    fn complete_media_is_kept_or_omitted_never_sliced() {
        let small = json!({ "type": "image", "mimeType": "image/png", "data": "aGVsbG8=" });
        let large =
            json!({ "type": "audio", "mimeType": "audio/wav", "data": "AAAA".repeat(LIMIT / 4) });
        let result = bounded(json!({
            "content": [small, large],
            "structuredContent": { "large": "x".repeat(LIMIT) },
        }));
        assert_eq!(result["content"][1], small);
        assert_eq!(result["content"].as_array().unwrap().len(), 2);
        assert_eq!(result["structuredContent"]["retained"], false);
    }

    #[test]
    fn many_small_blocks_have_linear_encoded_sizing_work() {
        let block = json!({ "type": "text", "text": "" });
        let original = json!({
            "content": vec![block.clone(); 60_000],
            "structuredContent": { "value": "completed" },
        });
        let wire = json!({ "jsonrpc": "2.0", "id": 1, "result": original });
        let wire_bytes = serde_json::to_vec(&wire).unwrap().len();
        assert!(wire_bytes > LIMIT);
        assert!(
            wire_bytes < 2_000_000,
            "The fixture must fit the existing stdio wire guard"
        );

        // Count actual bytes traversed by the production sizing function,
        // without a wall-time assertion or a production telemetry path. The
        // previous growing-envelope loop scans tens of GB for this valid input.
        MEASURED_ENCODED_BYTES.with(|measured| measured.set(0));
        let result = finish(original).unwrap();
        let measured = MEASURED_ENCODED_BYTES.with(std::cell::Cell::get);
        assert!(
            measured < 4 * wire_bytes,
            "Encoded sizing revisited {measured} bytes for a {wire_bytes}-byte wire fixture"
        );
        assert!(serde_json::to_vec(&result).unwrap().len() <= LIMIT);
        assert_eq!(result["structuredContent"]["truncated"], true);
        assert_eq!(result["structuredContent"]["retained"], false);
        let content = result["content"].as_array().unwrap();
        assert!(content.len() > 1);
        for kept in &content[1..] {
            assert_eq!(kept, &block);
        }
    }

    #[test]
    fn opaque_mandatory_oversize_is_a_fixed_unknown_outcome_protocol_failure() {
        let error = finish(json!({
            "content": [],
            "resultType": "x".repeat(LIMIT + 1),
        }))
        .unwrap_err();
        assert!(error.message.contains("outcome is unknown"));
        assert!(error.message.len() < 256);
    }

    fn assert_owned_schema_profile(value: &Value) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    assert!(!matches!(
                        key.as_str(),
                        "$id" | "$anchor" | "$dynamicRef" | "$recursiveRef" | "$defs"
                    ));
                    if key == "$ref" {
                        assert!(value.as_str().unwrap().starts_with("#/definitions/"));
                    }
                    if key == "$schema" {
                        assert_eq!(value, "http://json-schema.org/draft-07/schema#");
                    }
                    assert_owned_schema_profile(value);
                }
            }
            Value::Array(array) => {
                for value in array {
                    assert_owned_schema_profile(value);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn all_owned_output_schema_unions_preserve_local_refs_and_accept_truncation() {
        let mut tools = super::super::catalog();
        tools.extend([
            crate::chats::questions::ask_tool(),
            crate::chats::questions::list_tool(),
            crate::chats::questions::cancel_tool(),
            crate::waits::tool(&[], false),
        ]);
        tools.extend(
            crate::delegation::tools()
                .into_iter()
                .filter(|tool| tool.get("outputSchema").is_some()),
        );
        for original in tools {
            let schema = &original["outputSchema"];
            // The two hand-authored schemas have no dialect/ref machinery.
            if original["name"] != "search" && original["name"] != "execute" {
                assert_owned_schema_profile(schema);
            }
            let advertised = describe(original.clone(), false).unwrap();
            let validator = jsonschema::validator_for(&advertised["outputSchema"]).unwrap();
            assert!(validator.is_valid(&marker()));
            let complete = match original["name"].as_str().unwrap() {
                "search" | "execute" => json!({ "result": null }),
                "ask_questions" => json!({ "questionId": "fixture", "status": "pending" }),
                "pending_questions" => json!({ "questions": [] }),
                "withdraw_question" => json!({
                    "id": "fixture",
                    "chatId": "chat",
                    "runId": "run",
                    "blocking": false,
                    "source": "cairn",
                    "status": "cancelled",
                    "fields": [],
                }),
                "wait_for" => json!({ "deferred": true }),
                "task_status" => json!({ "tasks": [] }),
                "delegate_task" | "task_cancel" => json!({
                    "chatId": "chat",
                    "title": "fixture",
                    "agentId": "agent",
                    "agentName": "fixture",
                    "projectId": null,
                    "provider": "codex",
                    "model": null,
                    "status": "idle",
                    "lifecycle": "active",
                    "finished": false,
                    "paused": false,
                    "error": null,
                    "pendingQuestions": 0,
                    "outcome": null,
                    "runId": null,
                    "finishedAt": null,
                }),
                _ => unreachable!("Add complete result coverage for new owned output schemas"),
            };
            assert!(
                jsonschema::is_valid(schema, &complete),
                "{}",
                original["name"]
            );
            assert!(validator.is_valid(&complete), "{}", original["name"]);
            let mut body = advertised["outputSchema"]["anyOf"][0].clone();
            if let Some(definitions) = advertised["outputSchema"].get("definitions") {
                body["definitions"] = definitions.clone();
            }
            if let Some(dialect) = advertised["outputSchema"].get("$schema") {
                body["$schema"] = dialect.clone();
            }
            assert_eq!(body, *schema);
            assert_eq!(original["inputSchema"], advertised["inputSchema"]);
        }
    }

    #[test]
    fn proxy_schema_withdrawal_is_scoped_and_does_not_resolve_or_rewrite_any_refs() {
        for schema in [
            json!({ "$ref": "#", "$defs": { "node": { "$ref": "#/$defs/node" } } }),
            json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "$id": "https://invalid.example/schema", "$anchor": "result", "$dynamicRef": "#result" }),
            json!({ "$schema": "http://json-schema.org/draft-07/schema#", "$recursiveRef": "#", "$ref": "https://invalid.example/do-not-fetch" }),
            json!({ "$id": "relative/schema", "definitions": { "nested": { "$id": "nested", "$ref": "../other#anchor" } } }),
        ] {
            let canonical = json!({
                "name": "fixture",
                "inputSchema": { "type": "object" },
                "outputSchema": schema,
                "_meta": { "securitySchemes": [{ "type": "oauth2", "scopes": ["read"] }] },
            });
            let advertised = describe(canonical.clone(), true).unwrap();
            assert!(advertised.get("outputSchema").is_none());
            assert_eq!(canonical["outputSchema"], schema);
            assert_eq!(advertised["inputSchema"], canonical["inputSchema"]);
            assert_eq!(advertised["_meta"], canonical["_meta"]);
            assert!(
                advertised["description"]
                    .as_str()
                    .unwrap()
                    .contains("omits the upstream outputSchema")
            );
        }
    }
}
