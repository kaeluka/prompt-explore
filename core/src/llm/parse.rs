//! Interpreting structured LLM replies. Models sometimes wrap JSON in a
//! markdown code fence (```json … ```) or surround it with prose; the
//! deterministic layer's job is to tolerate that.

use serde::de::DeserializeOwned;

/// Extract the JSON payload from a model reply.
///
/// Order of attempts: a markdown code fence's contents; else the first
/// `{` … last `}` span (drops surrounding prose); else the whole trimmed
/// string.
pub fn extract_json(s: &str) -> &str {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // Drop the opening fence line (``` or ```json).
        let rest = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        // Drop the closing fence.
        if let Some(end) = rest.rfind("```") {
            let inner = rest[..end].trim();
            if !inner.is_empty() {
                return inner;
            }
        }
    }
    if let (Some(start), Some(end)) = (t.find('{'), t.rfind('}')) {
        if start < end {
            return &t[start..=end];
        }
    }
    t
}

/// Parse a model reply as JSON, tolerating code fences and prose.
pub fn parse_json<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(extract_json(s)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_json() {
        assert_eq!(extract_json("{\"a\": 1}"), "{\"a\": 1}");
    }

    #[test]
    fn fenced_json_with_lang() {
        let s = "```json\n{\"a\": 1}\n```";
        assert_eq!(extract_json(s), "{\"a\": 1}");
    }

    #[test]
    fn fenced_json_no_lang() {
        let s = "```\n{\"a\": 1}\n```";
        assert_eq!(extract_json(s), "{\"a\": 1}");
    }

    #[test]
    fn prose_around_json() {
        let s = "Here is the reply:\n{\"a\": 1}\nHope that helps.";
        assert_eq!(extract_json(s), "{\"a\": 1}");
    }

    #[test]
    fn fenced_with_pretty_json() {
        let s = "```json\n{\n  \"a\": 1,\n  \"b\": {\"c\": 2}\n}\n```";
        assert_eq!(extract_json(s), "{\n  \"a\": 1,\n  \"b\": {\"c\": 2}\n}");
    }

    #[test]
    fn parse_fenced_reply() {
        #[derive(serde::Deserialize)]
        struct R {
            response: String,
        }
        let r: R = parse_json("```json\n{\"response\": \"hi\"}\n```").unwrap();
        assert_eq!(r.response, "hi");
    }
}
