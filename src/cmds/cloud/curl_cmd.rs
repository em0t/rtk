//! Runs curl and preserves JSON responses while truncating long non-JSON output.

use crate::core::tee::force_tee_hint;
use crate::core::tracking;
use crate::core::{stream::exec_capture, utils::resolved_command};
use anyhow::{Context, Result};

const MAX_RESPONSE_SIZE: usize = 500;

/// Not using run_filtered: on failure, curl can return HTML error pages (404, 500)
/// that the JSON schema filter would mangle. The early exit skips filtering entirely.
pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let timer = tracking::TimedExecution::start();
    let mut cmd = resolved_command("curl");
    cmd.arg("-s"); // Silent mode (no progress bar)

    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: curl -s {}", args.join(" "));
    }

    let result = exec_capture(&mut cmd).context("Failed to run curl")?;

    // Early exit: don't feed HTTP error bodies (HTML 404 etc.) through JSON schema filter
    if !result.success() {
        let msg = if result.stderr.trim().is_empty() {
            result.stdout.trim().to_string()
        } else {
            result.stderr.trim().to_string()
        };
        eprintln!("FAILED: curl {}", msg);
        return Ok(result.exit_code);
    }

    let raw = result.stdout.clone();

    let result = filter_curl_output(&result.stdout);

    println!("{}", result.content);
    if let Some(hint) = &result.tee_hint {
        println!("{}", hint);
    }

    timer.track(
        &format!("curl {}", args.join(" ")),
        &format!("rtk curl {}", args.join(" ")),
        &raw,
        &result.content,
    );

    Ok(0)
}

fn filter_curl_output(raw: &str) -> FilterResult {
    let trimmed = raw.trim();

    // Valid JSON responses need values for API inspection and downstream parsers.
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && (trimmed.ends_with('}') || trimmed.ends_with(']'))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return FilterResult {
            content: trimmed.to_string(),
            tee_hint: None,
        };
    }

    let tee_hint = force_tee_hint(raw, "curl");

    // If the output is too long and we have a tee hint, truncate the output.
    let content = if trimmed.len() >= MAX_RESPONSE_SIZE && tee_hint.is_some() {
        let mut end = MAX_RESPONSE_SIZE;
        // Ensure we don't cut in the middle of a UTF-8 character.
        // .len() counts bytes, not chars.
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... ({} bytes total)", &trimmed[..end], trimmed.len())
    } else {
        trimmed.to_string()
    };

    FilterResult { content, tee_hint }
}

struct FilterResult {
    content: String,
    tee_hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_curl_json_small_no_tee_hint() {
        let output = r#"{"r2Ready":true,"status":"ok"}"#;
        let result = filter_curl_output(output);
        assert_eq!(result.content, output);
        assert!(result.tee_hint.is_none());
    }

    #[test]
    fn test_filter_curl_json() {
        // API JSON preserves values instead of collapsing to schema.
        let output = r#"{"name": "a very long user name here", "count": 42, "items": [1, 2, 3], "description": "a very long description that takes up many characters in the original JSON payload", "status": "active", "url": "https://example.com/api/v1/users/123"}"#;
        let result = filter_curl_output(output);
        assert_eq!(result.content, output);
        assert!(result.content.contains("a very long user name here"));
        assert!(result.content.contains("42"));
    }

    #[test]
    fn test_filter_curl_json_array() {
        let output = r#"[{"id": 1}, {"id": 2}]"#;
        let result = filter_curl_output(output);
        assert!(result.content.contains("id"));
    }

    #[test]
    fn test_filter_curl_single_line_json_array_preserves_values() {
        let output = r#"[{"name":"baoyu-article-illustrator","type":"dir"},{"name":"baoyu-comic","type":"dir"},{"name":"baoyu-url-to-markdown","type":"dir"}]"#;
        let result = filter_curl_output(output);
        assert_eq!(result.content, output);
        assert!(result.content.contains("baoyu-article-illustrator"));
        assert!(result.content.contains("baoyu-url-to-markdown"));
    }

    #[test]
    fn test_filter_curl_multiline_json_preserves_values() {
        let output = r#"{
  "description": "a very long description that takes up many characters in the original JSON payload",
  "name": "a very long user name here",
  "count": 42,
  "items": [1, 2, 3],
  "status": "active",
  "url": "https://example.com/api/v1/users/123"
}"#;
        let result = filter_curl_output(output);
        assert_eq!(result.content, output);
        assert!(result.content.contains("a very long user name here"));
        assert!(result
            .content
            .contains("https://example.com/api/v1/users/123"));
    }

    #[test]
    fn test_filter_curl_long_json_has_no_tee_hint() {
        let output = format!(
            r#"{{"items":[{}]}}"#,
            (0..80)
                .map(|i| format!(r#"{{"name":"item-{i}","type":"dir"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let result = filter_curl_output(&output);
        assert_eq!(result.content, output);
        assert!(result.tee_hint.is_none());
    }

    #[test]
    fn test_filter_curl_non_json() {
        let output = "Hello, World!\nThis is plain text.";
        let result = filter_curl_output(output);
        assert_eq!(result.content, output);
    }

    #[test]
    fn test_filter_curl_long_output_truncated() {
        let long: String = "x".repeat(1000);
        let result = filter_curl_output(&long);
        assert!(result.content.starts_with('x'));
        assert!(result.content.contains("bytes total"));
        assert!(result.content.contains("1000"));
        assert!(result.content.len() < 600);
    }

    #[test]
    fn test_filter_curl_multibyte_boundary() {
        let content = "a".repeat(499) + "é";
        let result = filter_curl_output(&content);
        assert!(result.content.contains("bytes total"));
        assert!(result.content.len() < 600);
    }

    #[test]
    fn test_filter_curl_exact_500_bytes() {
        let content = "a".repeat(500);
        let result = filter_curl_output(&content);
        assert!(result.content.contains("bytes total"));
    }
}
