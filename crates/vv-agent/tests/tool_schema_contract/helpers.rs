use std::path::Path;

pub(crate) fn description(registry: &vv_agent::ToolRegistry, tool_name: &str) -> String {
    registry
        .get_schema(tool_name)
        .and_then(|schema| {
            schema["function"]["description"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub(crate) fn property_description(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_name: &str,
) -> String {
    registry
        .get_schema(tool_name)
        .and_then(|schema| {
            schema["function"]["parameters"]["properties"][property_name]["description"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub(crate) fn nested_property_description(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> String {
    let mut cursor =
        &registry.get_schema(tool_name).expect("schema")["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["description"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_default()
}

pub(crate) fn property_names(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    path: &[&str],
) -> Vec<String> {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema;
    for segment in path {
        cursor = &cursor[*segment];
    }
    cursor
        .as_object()
        .expect("properties object")
        .keys()
        .cloned()
        .collect()
}

pub(crate) fn enum_values(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> Vec<String> {
    let mut cursor =
        &registry.get_schema(tool_name).expect("schema")["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .map(|value| value.as_str().expect("enum string").to_string())
        .collect()
}

pub(crate) fn schema_type(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
) -> String {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    cursor["type"].as_str().unwrap_or_default().to_string()
}

pub(crate) fn sorted(values: Vec<&str>) -> Vec<&str> {
    let mut sorted = values;
    sorted.sort_unstable();
    sorted
}

pub(crate) fn collect_rust_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_file() {
            if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            stack.push(entry.path());
        }
    }
    files
}

pub(crate) fn assert_description_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    expected_fragments: &[&str],
) {
    let actual = description(registry, tool_name);
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}"
        );
    }
}

pub(crate) fn assert_property_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_name: &str,
    expected_fragments: &[&str],
) {
    let actual = property_description(registry, tool_name, property_name);
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name}.{property_name} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}"
        );
    }
}

pub(crate) fn assert_nested_property_contains(
    registry: &vv_agent::ToolRegistry,
    tool_name: &str,
    property_path: &[&str],
    expected_fragments: &[&str],
) {
    let schema = registry.get_schema(tool_name).expect("schema");
    let mut cursor = &schema["function"]["parameters"]["properties"];
    for (index, segment) in property_path.iter().enumerate() {
        if index > 0 && *segment != "items" {
            cursor = &cursor["properties"];
        }
        cursor = &cursor[*segment];
    }
    let actual = cursor["description"].as_str().unwrap_or_default();
    for expected in expected_fragments {
        assert!(
            actual.contains(expected),
            "{tool_name}.{} description should preserve expected schema guidance:\n{expected}\n\nactual:\n{actual}",
            property_path.join("."),
        );
    }
}
