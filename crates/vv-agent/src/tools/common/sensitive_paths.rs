pub(crate) fn is_sensitive_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let parts = normalized
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(name) = parts.last().map(|value| value.to_ascii_lowercase()) else {
        return false;
    };

    const EXACT: &[&str] = &[
        ".env",
        ".npmrc",
        ".pypirc",
        ".netrc",
        "credentials",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];
    const SUFFIXES: &[&str] = &[".key", ".pem", ".p8", ".p12", ".pfx"];
    const TOKENS: &[&str] = &[
        "credential",
        "credentials",
        "secret",
        "secrets",
        "token",
        "private_key",
    ];
    const CONFIG_DIRS: &[&str] = &[
        ".config", "config", "configs", "keys", "secrets", ".ssh", ".aws", ".gcp",
    ];

    if EXACT.contains(&name.as_str()) {
        return true;
    }
    if name.starts_with(".env.")
        && !matches!(
            name.as_str(),
            ".env.example" | ".env.sample" | ".env.template"
        )
    {
        return true;
    }
    if ["secrets.", "secret."]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }
    if SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
        return true;
    }
    if name.ends_with(".env") {
        return true;
    }
    if TOKENS.iter().any(|token| name.contains(token)) {
        return parts[..parts.len().saturating_sub(1)]
            .iter()
            .any(|part| CONFIG_DIRS.contains(&part.to_ascii_lowercase().as_str()));
    }
    false
}

pub(crate) fn sensitive_rg_exclude_globs() -> &'static [&'static str] {
    &[
        "!**/.env",
        "!**/.npmrc",
        "!**/.pypirc",
        "!**/.netrc",
        "!**/credentials",
        "!**/id_rsa",
        "!**/id_dsa",
        "!**/id_ecdsa",
        "!**/id_ed25519",
        "!**/*.key",
        "!**/*.pem",
        "!**/*.p8",
        "!**/*.p12",
        "!**/*.pfx",
        "!**/secret.*",
        "!**/secrets.*",
        "!**/config/**/*token*",
        "!**/config/**/*credential*",
        "!**/config/**/*secret*",
        "!**/config/**/*private_key*",
        "!**/configs/**/*token*",
        "!**/configs/**/*credential*",
        "!**/configs/**/*secret*",
        "!**/configs/**/*private_key*",
        "!**/keys/**/*token*",
        "!**/keys/**/*credential*",
        "!**/keys/**/*secret*",
        "!**/keys/**/*private_key*",
        "!**/secrets/**/*token*",
        "!**/secrets/**/*credential*",
        "!**/secrets/**/*secret*",
        "!**/secrets/**/*private_key*",
        "!**/.ssh/**/*token*",
        "!**/.ssh/**/*credential*",
        "!**/.ssh/**/*secret*",
        "!**/.ssh/**/*private_key*",
        "!**/.aws/**/*token*",
        "!**/.aws/**/*credential*",
        "!**/.aws/**/*secret*",
        "!**/.aws/**/*private_key*",
        "!**/.gcp/**/*token*",
        "!**/.gcp/**/*credential*",
        "!**/.gcp/**/*secret*",
        "!**/.gcp/**/*private_key*",
    ]
}

pub(crate) fn sensitive_path_is_covered_by_rg_excludes(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let parts = normalized
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some(name) = parts.last() else {
        return false;
    };

    const EXACT: &[&str] = &[
        ".env",
        ".npmrc",
        ".pypirc",
        ".netrc",
        "credentials",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];
    const SUFFIXES: &[&str] = &[".key", ".pem", ".p8", ".p12", ".pfx"];
    const TOKENS: &[&str] = &[
        "credential",
        "credentials",
        "secret",
        "secrets",
        "token",
        "private_key",
    ];
    const CONFIG_DIRS: &[&str] = &[
        "config", "configs", "keys", "secrets", ".ssh", ".aws", ".gcp",
    ];

    if EXACT.contains(name) {
        return true;
    }
    if ["secret.", "secrets."]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }
    if SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
        return true;
    }
    if TOKENS.iter().any(|token| name.contains(token)) {
        return parts[..parts.len().saturating_sub(1)]
            .iter()
            .any(|part| CONFIG_DIRS.contains(part));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive_path, sensitive_path_is_covered_by_rg_excludes};

    #[test]
    fn flags_common_secret_paths() {
        for path in [
            ".env",
            ".env.local",
            "keys/AuthKey_ABC123.p8",
            "keys/private.key",
            ".ssh/id_rsa",
            "config/service_token.json",
            "secrets.production",
            ".npmrc",
        ] {
            assert!(is_sensitive_path(path), "{path}");
        }
    }

    #[test]
    fn allows_normal_project_files() {
        for path in [
            ".env.example",
            "src/tokenizer.rs",
            "docs/secrets-management.md",
            "config/example.json",
        ] {
            assert!(!is_sensitive_path(path), "{path}");
        }
    }

    #[test]
    fn rg_exclude_coverage_is_case_sensitive_like_rg_globs() {
        assert!(sensitive_path_is_covered_by_rg_excludes("private.pem"));
        assert!(sensitive_path_is_covered_by_rg_excludes(
            "keys/AuthKey_ABC123.p8"
        ));
        assert!(!sensitive_path_is_covered_by_rg_excludes(
            "keys/AuthKey_ABC123.P8"
        ));
        assert!(!sensitive_path_is_covered_by_rg_excludes(
            ".config/service_token.json"
        ));
    }
}
