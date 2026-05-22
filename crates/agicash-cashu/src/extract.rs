//! Cashu token extraction from arbitrary text.
//!
//! Pure, offline, no-I/O. Finds a `cashuA…` (V3) or `cashuB…` (V4)
//! encoded token inside arbitrary text — URL, `cashu:` URI, embedded
//! prose, raw paste — and returns the verbatim matched substring iff it
//! also structurally decodes via [`cdk::nuts::Token::from_str`].
//!
//! This is the "step 0" the iOS / Android / Leptos paste handlers all
//! need before passing the encoded token to the receive flow. Without
//! it, the strict [`cdk::nuts::Token::from_str`] rejects anything
//! wrapped (URL with hash/query, scheme-prefixed, or just surrounded by
//! whitespace + words).
//!
//! Mirrors the canonical React reference at
//! `app/lib/cashu/token.ts::extractCashuToken` on the archived
//! `archive/react-web-app` branch. First-match semantics; charset
//! match + structural validation (no scanning past the first candidate
//! — invalid-but-charset-matching trailing tails still return `None`,
//! matching React behavior).

use std::str::FromStr;

/// Find a Cashu token inside arbitrary text and return the verbatim
/// matched substring.
///
/// Scans the input for the **first** substring matching the cashu
/// token charset (`cashu[AB]` followed by base64-url-safe characters
/// and optional `=`/`==` padding). The candidate is then validated
/// by calling [`cdk::nuts::Token::from_str`]. If structural decoding
/// fails the function returns `None` — it does not keep scanning for
/// another candidate (matches the React reference).
///
/// Returns `Some(substring)` (the verbatim slice, **not** a
/// re-canonicalized re-encoding — lets callers show the user "what
/// you pasted") on success.
///
/// Returns `None` for:
/// - empty input
/// - input with no `cashu[AB]…` substring
/// - input whose first candidate substring fails to decode
///
/// # Examples
///
/// ```ignore
/// use agicash_cashu::extract_cashu_token;
///
/// // Raw token
/// let raw = "cashuAeyJ0b2tlbiI6...";
/// assert_eq!(extract_cashu_token(raw), Some(raw.to_string()));
///
/// // URL with hash
/// let url = format!("https://wallet.app/redeem#{raw}");
/// assert_eq!(extract_cashu_token(&url), Some(raw.to_string()));
///
/// // Embedded in text
/// let prose = format!("hey try this: {raw} thanks");
/// assert_eq!(extract_cashu_token(&prose), Some(raw.to_string()));
///
/// // Garbage
/// assert_eq!(extract_cashu_token("hello world"), None);
/// ```
#[must_use]
pub fn extract_cashu_token(input: &str) -> Option<String> {
    let candidate = find_token_candidate(input)?;
    // Validation: a charset match isn't enough — `cashuAXXX` matches
    // but won't structurally decode. We use `Token::from_str` for the
    // same reason React uses `getTokenMetadata`: it's the offline,
    // no-network structural check that filters garbage tails.
    cdk::nuts::Token::from_str(&candidate).ok()?;
    Some(candidate)
}

/// Find the first substring matching the cashu token charset:
/// `cashu[AB][A-Za-z0-9_-]+={0,2}`.
///
/// Hand-rolled single-pass scan — the `regex` crate isn't in the
/// workspace and a 30-line charset scan keeps the dep graph honest.
/// Returns the verbatim slice from the input as an owned `String`.
fn find_token_candidate(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        // Look for `cashuA` or `cashuB` (literal ASCII).
        if &bytes[i..i + 5] == b"cashu" && matches!(bytes[i + 5], b'A' | b'B') {
            // Scan forward as long as we see token-charset chars.
            let mut end = i + 6;
            while end < bytes.len() && is_token_char(bytes[end]) {
                end += 1;
            }
            // Greedy: consume up to two trailing `=` padding bytes.
            let mut pad = 0;
            while pad < 2 && end < bytes.len() && bytes[end] == b'=' {
                end += 1;
                pad += 1;
            }
            // Require at least one char after the `cashu[AB]` prefix
            // so we don't return a bare `cashuA` / `cashuB`.
            if end > i + 6 {
                // Slice is valid UTF-8 since all chars matched are
                // ASCII; safe to use `str::from_utf8` (will not fail).
                return std::str::from_utf8(&bytes[i..end]).ok().map(str::to_string);
            }
            // Fall through and advance past this `cashuA`/`cashuB`
            // sequence so we don't infinite-loop on it.
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

/// Base64-url-safe character set (RFC 4648 §5) — the encoding used
/// for both V3 (JSON-in-base64url) and V4 (CBOR-in-base64url) tokens.
/// Excludes `=` so the padding pass can count it separately.
const fn is_token_char(b: u8) -> bool {
    matches!(b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known-good V3 (`cashuA…`) token — `https://8333.space:3338`, two
    // proofs (2 + 8 = 10 sat), memo "Thank you very much.". From cdk
    // upstream test fixtures; decodes offline.
    const V3_TOKEN: &str = "cashuAeyJ0b2tlbiI6W3sibWludCI6Imh0dHBzOi8vODMzMy5zcGFjZTozMzM4IiwicHJvb2ZzIjpbeyJhbW91bnQiOjIsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6IjQwNzkxNWJjMjEyYmU2MWE3N2UzZTZkMmFlYjRjNzI3OTgwYmRhNTFjZDA2YTZhZmMyOWUyODYxNzY4YTc4MzciLCJDIjoiMDJiYzkwOTc5OTdkODFhZmIyY2M3MzQ2YjVlNDM0NWE5MzQ2YmQyYTUwNmViNzk1ODU5OGE3MmYwY2Y4NTE2M2VhIn0seyJhbW91bnQiOjgsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6ImZlMTUxMDkzMTRlNjFkNzc1NmIwZjhlZTBmMjNhNjI0YWNhYTNmNGUwNDJmNjE0MzNjNzI4YzcwNTdiOTMxYmUiLCJDIjoiMDI5ZThlNTA1MGI4OTBhN2Q2YzA5NjhkYjE2YmMxZDVkNWZhMDQwZWExZGUyODRmNmVjNjlkNjEyOTlmNjcxMDU5In1dfV0sInVuaXQiOiJzYXQiLCJtZW1vIjoiVGhhbmsgeW91IHZlcnkgbXVjaC4ifQ==";

    // Known-good V4 (`cashuB…`) token — `http://localhost:3338`,
    // single 1-sat proof, memo "Thank you". From cdk upstream test
    // fixtures; decodes offline.
    const V4_TOKEN: &str = "cashuBpGF0gaJhaUgArSaMTR9YJmFwgaNhYQFhc3hAOWE2ZGJiODQ3YmQyMzJiYTc2ZGIwZGYxOTcyMTZiMjlkM2I4Y2MxNDU1M2NkMjc4MjdmYzFjYzk0MmZlZGI0ZWFjWCEDhhhUP_trhpXfStS6vN6So0qWvc2X3O4NfM-Y1HISZ5JhZGlUaGFuayB5b3VhbXVodHRwOi8vbG9jYWxob3N0OjMzMzhhdWNzYXQ=";

    // A second V4 token for "multiple tokens, first match" test.
    // Reusing V3_TOKEN as the second hit is fine — semantically the
    // assertion is just "first wins".
    const V4_TOKEN_OTHER: &str = V4_TOKEN;

    #[test]
    fn extracts_raw_v3_token() {
        assert_eq!(extract_cashu_token(V3_TOKEN), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn extracts_raw_v4_token() {
        assert_eq!(extract_cashu_token(V4_TOKEN), Some(V4_TOKEN.to_string()));
    }

    #[test]
    fn strips_surrounding_whitespace() {
        let padded = format!("  \n {V3_TOKEN}  \t\n ");
        assert_eq!(extract_cashu_token(&padded), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn extracts_from_embedded_text() {
        let prose = format!("hey try {V3_TOKEN} thanks");
        assert_eq!(extract_cashu_token(&prose), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn extracts_from_url_hash() {
        let url = format!("https://example.com/redeem#token={V3_TOKEN}");
        assert_eq!(extract_cashu_token(&url), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn extracts_from_url_query() {
        let url = format!("https://example.com/redeem?token={V4_TOKEN}");
        assert_eq!(extract_cashu_token(&url), Some(V4_TOKEN.to_string()));
    }

    #[test]
    fn extracts_from_cashu_uri_scheme() {
        let uri = format!("cashu:{V3_TOKEN}");
        assert_eq!(extract_cashu_token(&uri), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn returns_none_for_garbage() {
        assert_eq!(extract_cashu_token("hello world"), None);
        assert_eq!(extract_cashu_token("cashu but no token"), None);
    }

    #[test]
    fn returns_none_for_empty_string() {
        assert_eq!(extract_cashu_token(""), None);
    }

    #[test]
    fn returns_none_for_malformed_candidate() {
        // Matches the charset regex but fails structural decode.
        // Per spec: do NOT scan past the first match — return None.
        assert_eq!(extract_cashu_token("cashuAXXXX"), None);
        assert_eq!(extract_cashu_token("cashuBnotrealdata"), None);
    }

    #[test]
    fn returns_first_match_when_multiple_present() {
        let combined = format!("first: {V3_TOKEN} second: {V4_TOKEN_OTHER}");
        assert_eq!(extract_cashu_token(&combined), Some(V3_TOKEN.to_string()));
    }

    #[test]
    fn handles_token_at_string_end() {
        // No trailing chars after the token; common in raw paste.
        let input = format!("text {V4_TOKEN}");
        assert_eq!(extract_cashu_token(&input), Some(V4_TOKEN.to_string()));
    }

    #[test]
    fn stops_at_non_charset_char() {
        // Trailing `!` is outside the base64-url-safe charset and
        // not `=`; the scan must stop before it.
        let input = format!("{V4_TOKEN}!extra");
        assert_eq!(extract_cashu_token(&input), Some(V4_TOKEN.to_string()));
    }

    #[test]
    fn bare_prefix_returns_none() {
        // `cashuA` / `cashuB` with no payload at all isn't a candidate.
        assert_eq!(extract_cashu_token("cashuA"), None);
        assert_eq!(extract_cashu_token("cashuB"), None);
        assert_eq!(extract_cashu_token("see cashuA in this text"), None);
    }
}
