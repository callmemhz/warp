use super::{friendly_project_path, relative_time};

#[test]
fn friendly_project_path_shows_last_two_components() {
    assert_eq!(
        friendly_project_path("/Users/me/src/warp/worktree-a"),
        "warp/worktree-a"
    );
}

#[test]
fn friendly_project_path_single_component() {
    assert_eq!(friendly_project_path("/warp"), "warp");
    assert_eq!(friendly_project_path("warp"), "warp");
}

#[test]
fn friendly_project_path_trailing_slash() {
    assert_eq!(friendly_project_path("/Users/me/warp/"), "me/warp");
}

#[test]
fn friendly_project_path_empty() {
    assert_eq!(friendly_project_path(""), "");
}

#[test]
fn relative_time_zero_is_blank() {
    assert_eq!(relative_time(0), "");
}

#[test]
fn relative_time_known_timestamp_is_nonempty() {
    // A recent, valid epoch-ms timestamp should produce a human-readable string.
    let one_hour_ago = (chrono::Utc::now().timestamp_millis() - 3_600_000) as u64;
    assert!(!relative_time(one_hour_ago).is_empty());
}
