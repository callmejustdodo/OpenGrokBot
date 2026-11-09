//! Translate durable Claude transcript records and final stream messages.
use crate::ports::Result;
use formation_proto::{
    common::{ApiError, ErrorCode, ItemId, Timestamp},
    harness::*,
};
use serde_json::Value;

pub(super) fn item_id(item: &ConversationItem) -> &ItemId {
    match item {
        ConversationItem::UserMessage { id, .. }
        | ConversationItem::AssistantText { id, .. }
        | ConversationItem::Step { id, .. }
        | ConversationItem::Approval { id, .. }
        | ConversationItem::Request { id, .. }
        | ConversationItem::Notice { id, .. } => id,
    }
}

pub(super) fn read_items(text: &str) -> Result<Vec<ConversationItem>> {
    let mut items = Vec::new();
    let mut steps = std::collections::HashMap::new();
    for line in text.split_inclusive('\n') {
        let record: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            // The writer can be in the middle of appending the last record.
            Err(_) if !line.ends_with('\n') => break,
            Err(e) => return Err(ApiError::new(ErrorCode::Internal, e.to_string())),
        };
        if !matches!(record["type"].as_str(), Some("user" | "assistant")) {
            continue;
        }
        let at = record["timestamp"]
            .as_str()
            .and_then(|s| s.parse::<Timestamp>().ok())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::Internal,
                    "transcript message has no valid timestamp",
                )
            })?;
        let mut translated = line_items(&record, at);
        for item in &translated {
            if let ConversationItem::Step { id, .. } = item {
                steps.insert(id.0.clone(), item.clone());
            }
        }
        translated.extend(tool_results(&record, &mut steps));
        for item in translated {
            if let Some(existing) = items.iter_mut().find(|old| item_id(old) == item_id(&item)) {
                *existing = item;
            } else {
                items.push(item);
            }
        }
    }
    Ok(items)
}

pub(super) fn line_items(record: &Value, at: Timestamp) -> Vec<ConversationItem> {
    let mut items = Vec::new();
    // Assistant message IDs also occur in partial stream events; prefer them over uuid.
    let id = record["message"]["id"]
        .as_str()
        .or_else(|| record["uuid"].as_str());
    let Some(id) = id else {
        return items;
    };
    let user = record["type"] == "user";
    let content = &record["message"]["content"];
    if let Some(text) = content.as_str() {
        items.push(ConversationItem::UserMessage {
            id: ItemId(id.into()),
            at,
            author: Author::Owner,
            text: text.into(),
        });
    }
    if let Some(blocks) = content.as_array() {
        for (index, block) in blocks.iter().enumerate() {
            let key = ItemId(format!("{id}:{index}"));
            match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or_default().into();
                    items.push(if user {
                        ConversationItem::UserMessage {
                            id: key,
                            at,
                            author: Author::Owner,
                            text,
                        }
                    } else {
                        ConversationItem::AssistantText { id: key, at, text }
                    });
                }
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or_default();
                    let input = &block["input"];
                    let kind = match name {
                        "Bash" => StepKind::Command,
                        "Edit" | "Write" | "MultiEdit" => StepKind::Edit,
                        "Read" | "Grep" | "Glob" => StepKind::Read,
                        "WebFetch" | "WebSearch" => StepKind::Web,
                        _ => StepKind::Tool,
                    };
                    let target = ["file_path", "command", "url", "pattern"]
                        .iter()
                        .find_map(|key| input[key].as_str())
                        .unwrap_or(name)
                        .to_owned();
                    items.push(ConversationItem::Step {
                        id: ItemId(block["id"].as_str().unwrap_or(id).into()),
                        at,
                        step: Step {
                            kind,
                            tool_name: name.into(),
                            target,
                            detail: Some(input.to_string()),
                            status: StepStatus::Running,
                        },
                    });
                }
                // Resolved against the existing tool item by read_items / the live actor.
                _ => {}
            }
        }
    }
    items
}

pub(super) fn tool_results(
    record: &Value,
    steps: &mut std::collections::HashMap<String, ConversationItem>,
) -> Vec<ConversationItem> {
    let mut updated = Vec::new();
    if let Some(blocks) = record["message"]["content"].as_array() {
        for block in blocks {
            if block["type"] != "tool_result" {
                continue;
            }
            if let Some(item) = block["tool_use_id"]
                .as_str()
                .and_then(|id| steps.get_mut(id))
            {
                if let ConversationItem::Step { step, .. } = item {
                    step.status = if block["is_error"] == true {
                        StepStatus::Failed
                    } else {
                        StepStatus::Done
                    };
                    step.detail = Some(
                        block["content"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| block["content"].to_string()),
                    );
                }
                updated.push(item.clone());
            }
        }
    }
    updated
}
