use subspace::tools::*;
use std::path::PathBuf;

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("subspace_test_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn noop_update(_: String) {}

#[tokio::test]
async fn test_write_and_read() {
    let dir = test_dir("write_and_read");
    let write_tool = write::WriteTool::new(dir.clone());
    let read_tool = read::ReadTool::new(dir.clone());

    let result = write_tool
        .execute(
            serde_json::json!({ "path": "hello.txt", "content": "line1\nline2\nline3" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(!result.is_error);

    let result = read_tool
        .execute(serde_json::json!({ "path": "hello.txt" }), &noop_update)
        .await
        .unwrap();
    assert!(!result.is_error);

    let text = result.content[0].as_text().unwrap();
    assert!(text.contains("line1"));
    assert!(text.contains("line2"));
    assert!(text.contains("line3"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_read_with_offset_limit() {
    let dir = test_dir("read_offset_limit");
    let write_tool = write::WriteTool::new(dir.clone());
    let read_tool = read::ReadTool::new(dir.clone());

    let content = (1..=10)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_tool
        .execute(
            serde_json::json!({ "path": "lines.txt", "content": content }),
            &noop_update,
        )
        .await
        .unwrap();

    let result = read_tool
        .execute(
            serde_json::json!({ "path": "lines.txt", "offset": 3, "limit": 3 }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(!result.is_error);

    let text = result.content[0].as_text().unwrap();
    assert!(text.contains("line 3"));
    assert!(text.contains("line 5"));
    assert!(!text.contains("line 2"));
    assert!(!text.contains("line 6"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_read_file_not_found() {
    let dir = test_dir("read_not_found");
    let read_tool = read::ReadTool::new(dir.clone());

    let result = read_tool
        .execute(
            serde_json::json!({ "path": "nonexistent.txt" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(result.is_error);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_write_creates_parents() {
    let dir = test_dir("write_parents");
    let write_tool = write::WriteTool::new(dir.clone());

    let result = write_tool
        .execute(
            serde_json::json!({ "path": "a/b/c/deep.txt", "content": "deep" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert!(dir.join("a/b/c/deep.txt").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_edit_exact_match() {
    let dir = test_dir("edit_exact");
    let write_tool = write::WriteTool::new(dir.clone());
    let edit_tool = edit::EditTool::new(dir.clone());
    let read_tool = read::ReadTool::new(dir.clone());

    write_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "content": "hello world" }),
            &noop_update,
        )
        .await
        .unwrap();

    let result = edit_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "oldText": "hello", "newText": "goodbye" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(!result.is_error);

    let result = read_tool
        .execute(serde_json::json!({ "path": "edit.txt" }), &noop_update)
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap();
    assert!(text.contains("goodbye world"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_edit_no_match() {
    let dir = test_dir("edit_no_match");
    let write_tool = write::WriteTool::new(dir.clone());
    let edit_tool = edit::EditTool::new(dir.clone());

    write_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "content": "hello world" }),
            &noop_update,
        )
        .await
        .unwrap();

    let result = edit_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "oldText": "not here", "newText": "x" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(result.is_error);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_edit_multiple_matches() {
    let dir = test_dir("edit_multi");
    let write_tool = write::WriteTool::new(dir.clone());
    let edit_tool = edit::EditTool::new(dir.clone());

    write_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "content": "aaa bbb aaa" }),
            &noop_update,
        )
        .await
        .unwrap();

    let result = edit_tool
        .execute(
            serde_json::json!({ "path": "edit.txt", "oldText": "aaa", "newText": "ccc" }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(result.is_error);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_bash_simple() {
    let dir = test_dir("bash_simple");
    let bash_tool = bash::BashTool::new(dir.clone());

    let result = bash_tool
        .execute(serde_json::json!({ "command": "echo hello" }), &noop_update)
        .await
        .unwrap();
    assert!(!result.is_error);

    let text = result.content[0].as_text().unwrap();
    assert!(text.contains("hello"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_bash_exit_code() {
    let dir = test_dir("bash_exit");
    let bash_tool = bash::BashTool::new(dir.clone());

    let result = bash_tool
        .execute(serde_json::json!({ "command": "exit 1" }), &noop_update)
        .await
        .unwrap();
    assert!(result.is_error);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn test_bash_timeout() {
    let dir = test_dir("bash_timeout");
    let bash_tool = bash::BashTool::new(dir.clone());

    let result = bash_tool
        .execute(
            serde_json::json!({ "command": "sleep 10", "timeout": 1 }),
            &noop_update,
        )
        .await
        .unwrap();
    assert!(result.is_error);

    let text = result.content[0].as_text().unwrap();
    assert!(text.contains("timed out"));

    let _ = std::fs::remove_dir_all(&dir);
}
