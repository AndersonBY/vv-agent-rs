use serde_json::Value;

pub(super) fn collect_raw_content(blocks: &mut Vec<Value>, chunk: Value) {
    match chunk {
        Value::Array(items) => {
            for item in items {
                collect_raw_content(blocks, item);
            }
        }
        Value::Object(object) => collect_raw_content_object(blocks, object),
        _ => {}
    }
}

fn collect_raw_content_object(blocks: &mut Vec<Value>, object: serde_json::Map<String, Value>) {
    let chunk_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match chunk_type {
        "thinking_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "thinking", object.get("thinking"));
        }
        "signature_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "signature", object.get("signature"));
        }
        "text_delta" => {
            let index = find_or_create_raw_block(blocks, "text", &[("text", "")]);
            append_raw_block_string(blocks, index, "text", object.get("text"));
        }
        "input_json_delta" => {}
        "thinking" | "text" | "tool_use" => {
            if !raw_block_exists(blocks, &object) {
                blocks.push(Value::Object(object));
            }
        }
        _ => blocks.push(Value::Object(object)),
    }
}

fn find_or_create_raw_block(
    blocks: &mut Vec<Value>,
    block_type: &str,
    defaults: &[(&str, &str)],
) -> usize {
    if let Some(index) = blocks.iter().position(|block| {
        block
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|candidate| candidate == block_type)
    }) {
        return index;
    }

    let mut block = serde_json::Map::new();
    block.insert("type".to_string(), Value::String(block_type.to_string()));
    for (key, value) in defaults {
        block.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    blocks.push(Value::Object(block));
    blocks.len() - 1
}

fn append_raw_block_string(
    blocks: &mut [Value],
    index: usize,
    key: &str,
    addition: Option<&Value>,
) {
    let addition = addition.and_then(Value::as_str).unwrap_or_default();
    let Some(block) = blocks.get_mut(index).and_then(Value::as_object_mut) else {
        return;
    };
    let mut value = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    value.push_str(addition);
    block.insert(key.to_string(), Value::String(value));
}

fn raw_block_exists(blocks: &[Value], candidate: &serde_json::Map<String, Value>) -> bool {
    let candidate_type = candidate.get("type");
    let candidate_id = candidate.get("id");
    blocks.iter().any(|block| {
        let Some(block) = block.as_object() else {
            return false;
        };
        if block.get("type") != candidate_type {
            return false;
        }
        if let Some(candidate_id) = candidate_id {
            return block.get("id") == Some(candidate_id);
        }
        block == candidate
    })
}
