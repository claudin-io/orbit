use std::path::{Path, PathBuf};

const MAX_PATH_LEN: usize = 40;

pub fn fmt_tool_call(name: &str, raw: &Option<serde_json::Value>, cwd: &Path) -> Option<String> {
    let v = raw.as_ref()?;
    if v.is_null() || !v.is_object() {
        return None;
    }
    let obj = v.as_object()?;
    if obj.is_empty() {
        return None;
    }

    let (result, handled) = match name {
        "read" => (fmt_read(obj, cwd), true),
        "write" | "edit" => (fmt_file_op(name, obj, cwd), true),
        "grep" => (fmt_grep(obj), true),
        "glob" => (fmt_glob(obj), true),
        "bash" => (fmt_bash(obj), true),
        "task" => (fmt_task(obj), true),
        "thought" => (None, true),
        _ => (fmt_fallback(v), false),
    };

    if handled {
        result
    } else {
        result.or_else(|| fmt_fallback(v))
    }
}

fn shorten_path(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    let rel = p.strip_prefix(cwd).unwrap_or(p).to_string_lossy().to_string();
    if rel.starts_with('/') || rel.starts_with("\\") {
        return rel;
    }
    let prefixed = format!("./{}", rel);
    if prefixed.len() > MAX_PATH_LEN
        && let Some(last) = Path::new(&rel).file_name() {
            let last = last.to_string_lossy();
            let first = Path::new(&rel).components().take(2).collect::<PathBuf>();
            let first = first.to_string_lossy();
            return format!("./{}/.../{}", first, last);
        }
    prefixed
}

fn fmt_read(obj: &serde_json::Map<String, serde_json::Value>, cwd: &Path) -> Option<String> {
    let path = obj.get("filePath")?.as_str()?;
    let rel = shorten_path(path, cwd);
    let offset = obj.get("offset").and_then(|v| v.as_u64());
    let limit = obj.get("limit").and_then(|v| v.as_u64());
    match (offset, limit) {
        (Some(o), Some(l)) => Some(format!("({} offset={} len={})", rel, o, l)),
        (Some(o), None) => Some(format!("({} offset={})", rel, o)),
        (None, Some(l)) => Some(format!("({} len={})", rel, l)),
        (None, None) => Some(format!("({})", rel)),
    }
}

fn fmt_file_op(_name: &str, obj: &serde_json::Map<String, serde_json::Value>, cwd: &Path) -> Option<String> {
    let path = obj.get("filePath")?.as_str()?;
    let rel = shorten_path(path, cwd);
    Some(format!("({})", rel))
}

fn fmt_grep(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let pattern = obj.get("pattern").and_then(|v| v.as_str()).map(truncate_ellipsis)?;
    let include = obj.get("include").and_then(|v| v.as_str());
    match include {
        Some(inc) => Some(format!("({}, {})", pattern, inc)),
        None => Some(format!("({})", pattern)),
    }
}

fn fmt_task(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let desc = obj.get("description").and_then(|v| v.as_str())?;
    let prompt = obj.get("prompt").and_then(|v| v.as_str());
    match prompt {
        Some(p) if p.len() > 80 => Some(format!("({} — {:.80}…)", desc, p)),
        Some(p) => Some(format!("({} — {})", desc, p)),
        None => Some(format!("({})", desc)),
    }
}

fn fmt_glob(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let pattern = obj.get("pattern").and_then(|v| v.as_str())?;
    Some(format!("({})", pattern))
}

fn fmt_bash(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let desc = obj.get("description").and_then(|v| v.as_str());
    if let Some(d) = desc {
        return Some(format!("({})", d));
    }
    let cmd = obj.get("command").and_then(|v| v.as_str())?;
    Some(format!("($ {})", truncate_ellipsis(cmd)))
}

fn fmt_fallback(v: &serde_json::Value) -> Option<String> {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.is_empty() {
        return None;
    }
    if s.len() > 200 {
        Some(format!("{}…", &s[..200]))
    } else {
        Some(s)
    }
}

fn truncate_ellipsis(s: &str) -> String {
    if s.len() > 60 {
        format!("{}…", &s[..57])
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        PathBuf::from("/Users/victortavernari/orbit")
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/cli.rs","offset":55,"limit":35});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/cli.rs offset=55 len=35)".into()));
    }

    #[test]
    fn test_read_no_offset() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/cli.rs","limit":35});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/cli.rs len=35)".into()));
    }

    #[test]
    fn test_read_no_params() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/cli.rs"});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/cli.rs)".into()));
    }

    #[test]
    fn test_write() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/config.rs"});
        let result = fmt_tool_call("write", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/config.rs)".into()));
    }

    #[test]
    fn test_edit() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/config.rs","oldString":"foo","newString":"bar"});
        let result = fmt_tool_call("edit", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/config.rs)".into()));
    }

    #[test]
    fn test_grep_with_include() {
        let raw = json!({"pattern":"fn save_acp_default|fn load_acp_default","include":"*.rs"});
        let result = fmt_tool_call("grep", &Some(raw), &cwd());
        assert_eq!(result, Some("(fn save_acp_default|fn load_acp_default, *.rs)".into()));
    }

    #[test]
    fn test_grep_without_include() {
        let raw = json!({"pattern":"OrbitError"});
        let result = fmt_tool_call("grep", &Some(raw), &cwd());
        assert_eq!(result, Some("(OrbitError)".into()));
    }

    #[test]
    fn test_glob() {
        let raw = json!({"pattern":"**/*.rs"});
        let result = fmt_tool_call("glob", &Some(raw), &cwd());
        assert_eq!(result, Some("(**/*.rs)".into()));
    }

    #[test]
    fn test_glob_with_path() {
        let raw = json!({"pattern":"src/**/*.tsx","path":"/Users/victortavernari/orbit"});
        let result = fmt_tool_call("glob", &Some(raw), &cwd());
        assert_eq!(result, Some("(src/**/*.tsx)".into()));
    }

    #[test]
    fn test_bash_with_description() {
        let raw = json!({"command":"npm test","description":"Run frontend tests"});
        let result = fmt_tool_call("bash", &Some(raw), &cwd());
        assert_eq!(result, Some("(Run frontend tests)".into()));
    }

    #[test]
    fn test_bash_without_description() {
        let raw = json!({"command":"npm test -- --watch"});
        let result = fmt_tool_call("bash", &Some(raw), &cwd());
        assert_eq!(result, Some("($ npm test -- --watch)".into()));
    }

    #[test]
    fn test_null_params() {
        let result = fmt_tool_call("read", &None, &cwd());
        assert_eq!(result, None);
    }

    #[test]
    fn test_empty_object_params() {
        let raw = json!({});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, None);
    }

    #[test]
    fn test_null_value_params() {
        let raw = serde_json::Value::Null;
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, None);
    }

    #[test]
    fn test_unrecognized_tool_falls_back() {
        let raw = json!({"someKey":"someValue","anotherKey":42});
        let result = fmt_tool_call("unknown_tool", &Some(raw), &cwd());
        assert_eq!(result, Some(r#"{"anotherKey":42,"someKey":"someValue"}"#.into()));
    }

    #[test]
    fn test_path_shortening() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/some/deeply/nested/directory/file.rs"});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/some/.../file.rs)".into()));
    }

    #[test]
    fn test_short_path_no_shortening() {
        let raw = json!({"filePath":"/Users/victortavernari/orbit/src/main.rs"});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(./src/main.rs)".into()));
    }

    #[test]
    fn test_path_outside_project() {
        let raw = json!({"filePath":"/tmp/some-file.rs"});
        let result = fmt_tool_call("read", &Some(raw), &cwd());
        assert_eq!(result, Some("(/tmp/some-file.rs)".into()));
    }

    #[test]
    fn test_grep_long_pattern_truncated() {
        let long = "a".repeat(100);
        let raw = json!({"pattern": long});
        let result = fmt_tool_call("grep", &Some(raw), &cwd());
        assert!(result.is_some());
        let s = result.unwrap();
        assert!(s.starts_with('('));
        assert!(s.ends_with(')'));
        assert!(s.len() < 80);
    }

    #[test]
    fn test_thought_returns_none() {
        let raw = json!({"text":"thinking about the solution..."});
        let result = fmt_tool_call("thought", &Some(raw), &cwd());
        assert_eq!(result, None);
    }
}
