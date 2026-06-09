//! Warp-side custom names for claude sessions (`session_id` → label).
//!
//! claude owns the sessions; the *names* are ours, so users can label what a
//! session is for. Persisted as a small JSON map in Warp's state dir (next to
//! `warp.sqlite`) rather than in `~/.claude`, which Warp must not mutate.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const FILE_NAME: &str = "claude_session_names.json";

fn names_file() -> PathBuf {
    warp_core::paths::secure_state_dir()
        .unwrap_or_else(warp_core::paths::state_dir)
        .join(FILE_NAME)
}

/// Loads the `session_id` → custom name map. Missing or corrupt file → empty.
pub fn load() -> HashMap<String, String> {
    decode(&fs::read_to_string(names_file()).unwrap_or_default())
}

/// Sets the custom name for a session and persists. An empty/whitespace name
/// clears it. No-op on IO error (names are a convenience, not critical state).
pub fn set(session_id: &str, name: &str) {
    let mut map = load();
    let name = name.trim();
    if name.is_empty() {
        map.remove(session_id);
    } else {
        map.insert(session_id.to_string(), name.to_string());
    }
    let path = names_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = fs::write(path, text);
    }
}

/// Parses the persisted JSON map; any malformed content yields an empty map.
fn decode(text: &str) -> HashMap<String, String> {
    serde_json::from_str(text).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_empty_or_garbage_is_empty() {
        assert!(decode("").is_empty());
        assert!(decode("not json").is_empty());
        assert!(decode("[1,2,3]").is_empty());
    }

    #[test]
    fn decode_reads_map() {
        let map = decode(r#"{"abc":"my feature","def":"bugfix"}"#);
        assert_eq!(map.get("abc").map(String::as_str), Some("my feature"));
        assert_eq!(map.len(), 2);
    }
}
