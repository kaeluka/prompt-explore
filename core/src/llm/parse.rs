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
///
/// Two-stage: strict parse of the extracted payload first; on failure, a
/// repair pass doubles any backslash that isn't a valid JSON escape
/// (`\ ` -> `\\ `) and retries. Well-formed JSON contains no bare
/// backslashes outside valid escapes, so the repair is a no-op on
/// well-formed input.
pub fn parse_json<T: DeserializeOwned>(s: &str) -> Option<T> {
    let extracted = extract_json(s);
    serde_json::from_str(extracted)
        .ok()
        .or_else(|| serde_json::from_str(&repair_escapes(extracted)).ok())
}

/// Escape any `\` not followed by a valid JSON escape char
/// (`" \ / b f n r t u`). Models rendering content with literal
/// backslashes (line continuations, Windows paths, regexes) sometimes
/// emit them unescaped.
fn repair_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some(&n) if matches!(n, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                out.push('\\');
                out.push(n);
                chars.next();
            }
            _ => {
                out.push('\\');
                out.push('\\');
            }
        }
    }
    out
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

    #[test]
    fn repairs_invalid_escape() {
        // The sc2 failure: a Python line-continuation serialized as `\ `.
        #[derive(serde::Deserialize)]
        struct R {
            response: String,
        }
        let r: R = parse_json("{\"response\": \"data = None, \\\\   next\"}").unwrap();
        assert_eq!(r.response, "data = None, \\   next");
    }

    #[test]
    fn repair_is_noop_on_valid_escapes() {
        let valid = "{\"a\": \"x\\ny\\\\z\\\"q\"}";
        let v: serde_json::Value = parse_json(valid).unwrap();
        assert_eq!(v["a"], "x\ny\\z\"q");
    }

    #[test]
    fn fenced_and_invalid_escape() {
        #[derive(serde::Deserialize)]
        struct R {
            response: String,
        }
        let r: R = parse_json("```json\n{\"response\": \"path C:\\\\ Users\"}\n```").unwrap();
        assert_eq!(r.response, "path C:\\ Users");
    }
}
