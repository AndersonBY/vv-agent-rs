use std::collections::BTreeMap;

const WINDOWS_CHILD_PROCESS_ENV_DEFAULTS: [(&str, &str); 2] =
    [("PYTHONUTF8", "1"), ("PYTHONIOENCODING", "utf-8")];

pub(super) fn build_process_env(
    extra_env: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    build_process_env_for_platform(extra_env, cfg!(target_os = "windows"))
}

fn build_process_env_for_platform(
    extra_env: Option<&BTreeMap<String, String>>,
    windows: bool,
) -> Option<BTreeMap<String, String>> {
    build_process_env_with_base(extra_env, windows, std::env::vars().collect())
}

fn build_process_env_with_base(
    extra_env: Option<&BTreeMap<String, String>>,
    windows: bool,
    mut base_env: BTreeMap<String, String>,
) -> Option<BTreeMap<String, String>> {
    if !windows && extra_env.is_none() {
        return None;
    }
    if windows {
        for (key, value) in WINDOWS_CHILD_PROCESS_ENV_DEFAULTS {
            base_env
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    if let Some(extra_env) = extra_env {
        base_env.extend(extra_env.clone());
    }
    Some(base_env)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::build_process_env_with_base;

    #[test]
    fn process_env_injects_windows_utf8_defaults() {
        let process_env =
            build_process_env_with_base(None, true, BTreeMap::new()).expect("windows env");

        assert_eq!(process_env["PYTHONUTF8"], "1");
        assert_eq!(process_env["PYTHONIOENCODING"], "utf-8");
    }

    #[test]
    fn process_env_preserves_explicit_windows_utf8_overrides() {
        let process_env = build_process_env_with_base(
            Some(&BTreeMap::from([(
                "PYTHONIOENCODING".to_string(),
                "utf-8:replace".to_string(),
            )])),
            true,
            BTreeMap::from([
                ("PYTHONUTF8".to_string(), "0".to_string()),
                ("PYTHONIOENCODING".to_string(), "gbk".to_string()),
            ]),
        )
        .expect("windows env");

        assert_eq!(process_env["PYTHONUTF8"], "0");
        assert_eq!(process_env["PYTHONIOENCODING"], "utf-8:replace");
    }
}
