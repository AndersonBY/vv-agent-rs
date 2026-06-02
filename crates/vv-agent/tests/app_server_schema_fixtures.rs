use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use vv_agent::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle,
};

#[test]
fn app_server_schema_fixtures_match_generated_output() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let json_dir = manifest_dir.join("schema/app-server/json");
    let ts_dir = manifest_dir.join("schema/app-server/typescript");

    let mut generated = BTreeMap::new();
    for (name, content) in generate_app_server_json_schema_bundle().expect("json schema bundle") {
        generated.insert(format!("{name}.json"), content);
    }
    for (name, content) in generate_app_server_typescript_bundle().expect("typescript bundle") {
        generated.insert(name, content);
    }

    let fixtures = read_fixture_files(&json_dir)
        .into_iter()
        .chain(read_fixture_files(&ts_dir))
        .collect::<BTreeMap<_, _>>();

    assert!(
        fixtures.len() >= 10,
        "App Server should commit at least 10 schema fixture files"
    );
    assert_eq!(
        fixtures.keys().collect::<Vec<_>>(),
        generated.keys().collect::<Vec<_>>()
    );
    for (file_name, fixture) in fixtures {
        let generated = generated.get(&file_name).expect("generated fixture");
        assert_eq!(&fixture, generated, "schema fixture changed: {file_name}");
    }
}

fn read_fixture_files(dir: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(dir).expect("fixture dir") {
        let entry = entry.expect("fixture entry");
        let path = entry.path();
        if path.is_file() {
            files.insert(
                path.file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned(),
                fs::read_to_string(&path).expect("fixture contents"),
            );
        }
    }
    files
}
