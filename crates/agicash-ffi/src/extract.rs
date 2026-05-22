//! FFI surface for `extract_cashu_token`.
//!
//! Wallet-agnostic helper — exposed as a module-level
//! `#[uniffi::export]` free function (no `AgicashWallet` instance
//! needed) so iOS / Android paste handlers can call it before any
//! wallet round-trip. Same shape as the LUD-16 free functions in
//! `lightning_address.rs`.
//!
//! Pure delegate to [`agicash_cashu::extract_cashu_token`]; see that
//! function's doc-comment for the full contract.

/// Extract a Cashu token (`cashuA…` / `cashuB…`) from arbitrary text.
///
/// Returns the verbatim matched substring iff it also structurally
/// decodes; otherwise `None`. Pure, offline, no I/O — safe to call
/// from any thread, no `async` runtime required.
///
/// iOS calls this as `AgicashSDK.extractCashuToken(input:)`; Android
/// calls it as `extractCashuToken(input)` on the package-level top.
/// Both shells should call this **before** trimming + handing the
/// string to `wallet.receiveToken(...)` — the inner CDK
/// `Token::from_str` is strict and rejects anything wrapped (URL with
/// hash, `cashu:` scheme, embedded in prose).
///
/// On `None`, the UI should surface "no Cashu token found in that
/// text" — do not fall back to passing the raw paste through
/// `receiveToken`, which just re-creates the bug the extractor fixes.
// `input` is taken by value because UniFFI marshals strings as owned
// `String` across the FFI boundary; clippy's `needless_pass_by_value`
// is a false positive in that context (same pattern used by the
// `parse_lightning_address` free function in `lightning_address.rs`,
// which silences `unused_async` for the analogous reason).
#[uniffi::export]
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn extract_cashu_token(input: String) -> Option<String> {
    agicash_cashu::extract_cashu_token(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same V4 fixture as the cashu crate — single 1-sat proof,
    // `http://localhost:3338`. Round-tripping it confirms the FFI
    // free function actually delegates, not just stubs.
    const V4_TOKEN: &str = "cashuBpGF0gaJhaUgArSaMTR9YJmFwgaNhYQFhc3hAOWE2ZGJiODQ3YmQyMzJiYTc2ZGIwZGYxOTcyMTZiMjlkM2I4Y2MxNDU1M2NkMjc4MjdmYzFjYzk0MmZlZGI0ZWFjWCEDhhhUP_trhpXfStS6vN6So0qWvc2X3O4NfM-Y1HISZ5JhZGlUaGFuayB5b3VhbXVodHRwOi8vbG9jYWxob3N0OjMzMzhhdWNzYXQ=";

    #[test]
    fn ffi_extract_round_trips_url() {
        let url = format!("https://example.com/redeem?token={V4_TOKEN}");
        assert_eq!(extract_cashu_token(url), Some(V4_TOKEN.to_string()));
    }

    #[test]
    fn ffi_extract_returns_none_for_garbage() {
        assert_eq!(extract_cashu_token("hello world".to_string()), None);
    }
}
