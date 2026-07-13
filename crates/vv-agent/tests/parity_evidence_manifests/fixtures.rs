#[test]
fn evidence_json_is_canonical_utf8() {
    for name in CANONICAL_FIXTURES {
        let raw = fs::read(fixture_path(name)).expect("read canonical fixture");
        let value: Value = serde_json::from_slice(&raw).expect("parse canonical fixture");
        let canonical = format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("serialize canonical fixture")
        );
        assert_eq!(raw, canonical.as_bytes(), "{name}");
    }
}

#[test]
fn sha256sums_covers_every_parity_fixture() {
    let directory = fixture_path("");
    let mut fixture_names = fs::read_dir(&directory)
        .expect("read parity directory")
        .map(|entry| entry.expect("parity directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name != "SHA256SUMS")
        .collect::<Vec<_>>();
    fixture_names.sort();

    let checksum_text = fs::read_to_string(directory.join("SHA256SUMS")).expect("SHA256SUMS");
    let mut listed_names = Vec::new();
    let mut entries = BTreeMap::new();
    for line in checksum_text.lines() {
        let (digest, name) = line.split_once("  ").expect("checksum line");
        assert!(entries
            .insert(name.to_string(), digest.to_string())
            .is_none());
        listed_names.push(name.to_string());
    }
    assert_eq!(listed_names, fixture_names);

    for name in fixture_names {
        let bytes = fs::read(directory.join(&name)).expect("fixture bytes");
        assert_eq!(entries[&name], format!("{:x}", Sha256::digest(bytes)));
    }
}
