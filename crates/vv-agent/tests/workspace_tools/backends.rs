use super::*;

#[test]
fn workspace_backends_report_utf8_bytes_written() {
    let workspace = tempfile::tempdir().expect("workspace");
    let local = LocalWorkspaceBackend::new(workspace.path());
    let memory = MemoryWorkspaceBackend::default();
    let s3 = S3WorkspaceBackend::from_object_store(InMemory::new(), "tenant/workspace")
        .expect("s3 backend");
    let content = "你好, world";

    assert_eq!(
        local
            .write_text("unicode.txt", content, false)
            .expect("local write"),
        content.len()
    );
    assert_eq!(
        memory
            .write_text("unicode.txt", content, false)
            .expect("memory write"),
        content.len()
    );
    assert_eq!(
        s3.write_text("unicode.txt", content, false)
            .expect("s3 write"),
        content.len()
    );
}

#[test]
fn workspace_backends_enforce_canonical_dot_segment_rules() {
    let workspace = tempfile::tempdir().expect("workspace");
    let local = LocalWorkspaceBackend::new(workspace.path());
    local
        .write_text("nested/../canonical.txt", "local", false)
        .expect("local normalized write");
    assert_eq!(
        local.read_text("./canonical.txt").expect("local read"),
        "local"
    );
    assert_eq!(
        local
            .write_text("../escape.txt", "blocked", false)
            .expect_err("local escape must fail")
            .kind(),
        ErrorKind::PermissionDenied
    );
    assert!(!local.exists("../escape.txt"));
    assert!(!local.is_file("../escape.txt"));

    let memory = MemoryWorkspaceBackend::default();
    memory
        .write_text("../../escape.txt", "memory", false)
        .expect("memory clamped write");
    assert_eq!(
        memory.read_text("/escape.txt").expect("memory read"),
        "memory"
    );
    assert_eq!(
        memory.list_files("..", "*.txt").expect("memory list"),
        vec!["escape.txt"]
    );

    let s3 = S3WorkspaceBackend::from_object_store(InMemory::new(), "tenant/workspace")
        .expect("s3 backend");
    s3.write_text("../../escape.txt", "s3", false)
        .expect("s3 clamped write");
    assert_eq!(s3.read_text("/escape.txt").expect("s3 read"), "s3");
    assert_eq!(
        s3.list_files("..", "*.txt").expect("s3 list"),
        vec!["escape.txt"]
    );
}

#[test]
fn local_workspace_backend_replaces_invalid_utf8_when_reading_text() {
    let workspace = tempfile::tempdir().expect("workspace");
    let path = workspace.path().join("mixed.log");
    std::fs::write(&path, b"ok\xffdone").expect("write invalid utf8");
    let local = LocalWorkspaceBackend::new(workspace.path());

    let text = local.read_text("mixed.log").expect("read text");

    assert_eq!(text, "ok\u{fffd}done");
}

#[cfg(unix)]
#[test]
fn local_workspace_backend_skips_unreadable_dirs_when_listing_files() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("visible.txt"), "visible").expect("visible file");
    let private_dir = workspace.path().join("private");
    std::fs::create_dir(&private_dir).expect("private dir");
    std::fs::write(private_dir.join("hidden.txt"), "hidden").expect("hidden file");
    std::fs::set_permissions(&private_dir, std::fs::Permissions::from_mode(0o000))
        .expect("make private dir unreadable");

    let local = LocalWorkspaceBackend::new(workspace.path());
    let listed = local.list_files(".", "**/*.txt");

    std::fs::set_permissions(&private_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore private dir permissions");

    assert_eq!(
        listed.expect("list should skip unreadable dir"),
        vec!["visible.txt"]
    );
}

#[test]
fn local_workspace_backend_matches_direct_file_backend_contract() {
    let workspace = tempfile::tempdir().expect("workspace");
    let local = LocalWorkspaceBackend::new(workspace.path());

    local
        .write_text("hello.txt", "world", false)
        .expect("write text");
    assert_eq!(local.read_text("hello.txt").expect("read text"), "world");

    local.write_text("log.txt", "a", false).expect("write log");
    local.write_text("log.txt", "b", true).expect("append log");
    assert_eq!(local.read_text("log.txt").expect("read log"), "ab");

    std::fs::write(workspace.path().join("bin.dat"), b"\x00\x01\x02").expect("write bytes");
    assert_eq!(
        local.read_bytes("bin.dat").expect("read bytes"),
        b"\x00\x01\x02"
    );

    local.write_text("a.py", "x", false).expect("write py");
    local.write_text("b.txt", "y", false).expect("write txt");
    assert_eq!(
        local.list_files(".", "**/*.py").expect("glob"),
        vec!["a.py"]
    );

    let info = local
        .file_info("hello.txt")
        .expect("file info")
        .expect("hello exists");
    assert_eq!(info.path, "hello.txt");
    assert!(info.is_file);
    assert!(!info.is_dir);
    assert_eq!(info.size, 5);
    assert_eq!(info.suffix, ".txt");
    assert!(local.file_info("missing.txt").expect("missing").is_none());

    local.write_text("empty.txt", "", false).expect("empty");
    assert!(local.exists("empty.txt"));
    assert!(local.is_file("empty.txt"));
    assert!(!local.exists("nope"));

    local.mkdir("deep/nested/dir").expect("mkdir");
    assert!(workspace.path().join("deep/nested/dir").is_dir());
}

#[test]
fn memory_workspace_backend_matches_direct_file_backend_contract() {
    let memory = MemoryWorkspaceBackend::default();

    memory
        .write_text("hello.txt", "world", false)
        .expect("write text");
    assert_eq!(memory.read_text("hello.txt").expect("read text"), "world");

    memory.write_text("log.txt", "a", false).expect("write log");
    memory.write_text("log.txt", "b", true).expect("append log");
    assert_eq!(memory.read_text("log.txt").expect("read log"), "ab");

    memory
        .write_text("data.txt", "abc", false)
        .expect("write data");
    assert_eq!(memory.read_bytes("data.txt").expect("read bytes"), b"abc");
    assert_eq!(
        memory.read_text("missing.txt").expect_err("missing").kind(),
        ErrorKind::NotFound
    );

    memory.write_text("a.py", "x", false).expect("write py");
    memory.write_text("b.txt", "y", false).expect("write txt");
    assert_eq!(
        memory.list_files(".", "**/*.py").expect("glob"),
        vec!["a.py".to_string()]
    );

    let info = memory
        .file_info("hello.txt")
        .expect("file info")
        .expect("hello exists");
    assert_eq!(info.path, "hello.txt");
    assert!(info.is_file);
    assert!(!info.is_dir);
    assert_eq!(info.size, 5);
    assert_eq!(info.suffix, ".txt");

    memory.mkdir("mydir").expect("mkdir mydir");
    let dir_info = memory
        .file_info("mydir")
        .expect("dir info")
        .expect("mydir exists");
    assert!(dir_info.is_dir);
    assert!(!dir_info.is_file);
    assert!(memory.file_info("nope.txt").expect("missing").is_none());

    memory.write_text("empty.txt", "", false).expect("empty");
    assert!(memory.exists("empty.txt"));
    assert!(memory.is_file("empty.txt"));
    assert!(!memory.exists("nope"));

    memory.mkdir("deep/nested/dir").expect("mkdir nested");
    assert!(memory.exists("deep"));
    assert!(memory.exists("deep/nested"));
    assert!(memory.exists("deep/nested/dir"));
}
#[test]
fn workspace_backends_honor_agent_glob_and_missing_file_semantics() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join("src/nested")).expect("dirs");
    std::fs::write(workspace.path().join("root.rs"), "fn root() {}").expect("root");
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}").expect("main");
    std::fs::write(workspace.path().join("src/readme.md"), "# readme").expect("readme");
    std::fs::write(workspace.path().join("src/nested/lib.rs"), "fn lib() {}").expect("lib");
    let local = LocalWorkspaceBackend::new(workspace.path());

    assert_eq!(
        local.list_files(".", "**/*.rs").expect("local root glob"),
        vec![
            "root.rs".to_string(),
            "src/main.rs".to_string(),
            "src/nested/lib.rs".to_string(),
        ]
    );
    assert_eq!(
        local.list_files("src", "*.rs").expect("local base glob"),
        vec!["src/main.rs".to_string()]
    );

    let memory = MemoryWorkspaceBackend::default();
    memory.mkdir("src/nested").expect("mkdir");
    memory
        .write_text("/src/main.rs", "fn main() {}", false)
        .expect("write main");
    memory
        .write_text("src/nested/lib.rs", "fn lib() {}", false)
        .expect("write lib");
    memory
        .write_text("src/readme.md", "# readme", false)
        .expect("write readme");

    assert_eq!(
        memory.list_files(".", "**/*.rs").expect("memory root glob"),
        vec!["src/main.rs".to_string(), "src/nested/lib.rs".to_string()]
    );
    assert_eq!(
        memory.list_files("src", "*.rs").expect("memory base glob"),
        vec!["src/main.rs".to_string()]
    );
    assert_eq!(
        memory.read_text("missing.txt").expect_err("missing").kind(),
        ErrorKind::NotFound
    );
    let dir_info = memory.file_info("src").expect("dir info").expect("src dir");
    assert!(dir_info.is_dir);
    assert!(!dir_info.is_file);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&dir_info.modified_at).is_ok(),
        "memory dir modified_at should match UTC ISO format, got {:?}",
        dir_info.modified_at
    );
    let file_info = memory
        .file_info("src/main.rs")
        .expect("file info")
        .expect("main file");
    assert!(
        chrono::DateTime::parse_from_rfc3339(&file_info.modified_at).is_ok(),
        "memory file modified_at should match UTC ISO format, got {:?}",
        file_info.modified_at
    );
    assert!(memory.exists("src"));
    assert!(!memory.is_file("src"));
}

#[test]
fn s3_workspace_backend_uses_prefix_and_object_store_semantics() {
    let backend = S3WorkspaceBackend::from_object_store(InMemory::new(), "tenant/workspace")
        .expect("s3 backend");

    backend
        .write_text("notes/a.txt", "one", false)
        .expect("write");
    backend
        .write_text("notes/a.txt", "+two", true)
        .expect("append");
    backend
        .write_text("src/main.rs", "fn main() {}", false)
        .expect("write rust");
    backend
        .write_text("src/readme.md", "# readme", false)
        .expect("write markdown");

    assert_eq!(backend.read_text("notes/a.txt").expect("read"), "one+two");
    assert_eq!(
        backend.read_bytes("notes/a.txt").expect("read bytes"),
        b"one+two"
    );
    assert_eq!(
        backend.list_files(".", "**/*.rs").expect("root glob"),
        vec!["src/main.rs".to_string()]
    );
    assert_eq!(
        backend.list_files("src", "*.rs").expect("base glob"),
        vec!["src/main.rs".to_string()]
    );

    let info = backend
        .file_info("notes/a.txt")
        .expect("file info")
        .expect("exists");
    assert_eq!(info.path, "notes/a.txt");
    assert!(info.is_file);
    assert!(!info.is_dir);
    assert_eq!(info.size, 7);
    assert_eq!(info.suffix, ".txt");
    assert!(!info.modified_at.is_empty());
    assert!(backend.exists("notes/a.txt"));
    assert!(backend.is_file("notes/a.txt"));
    assert!(!backend.exists("missing.txt"));
    assert_eq!(
        backend
            .read_text("missing.txt")
            .expect_err("missing")
            .kind(),
        ErrorKind::NotFound
    );

    backend.mkdir("empty/dir").expect("mkdir no-op");
}
