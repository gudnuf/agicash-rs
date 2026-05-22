//! Contract test for the `agicash decode` subcommand.
//!
//! `decode` is offline + deterministic — no session, no network, no env
//! vars — so unlike the other CLI contract tests this file is fully
//! hermetic and runs in CI without the `real-*-tests` features.
//!
//! Verifies the success + error JSON shapes and the typed exit codes for
//! every input type `decode` auto-detects (Cashu token, BOLT-11 invoice,
//! Lightning Address) and the catch-all unrecognized-input error.
//!
//! Run:
//!     cargo test -p agicash-cli --test decode

use assert_cmd::Command;
use serde_json::Value;

/// A real V4 (`cashuB…`) token — single 1-sat proof, memo "Thank you".
const TOKEN_V4: &str = "cashuBpGF0gaJhaUgArSaMTR9YJmFwgaNhYQFhc3hAOWE2ZGJiODQ3YmQyMzJiYTc2ZGIwZGYxOTcyMTZiMjlkM2I4Y2MxNDU1M2NkMjc4MjdmYzFjYzk0MmZlZGI0ZWFjWCEDhhhUP_trhpXfStS6vN6So0qWvc2X3O4NfM-Y1HISZ5JhZGlUaGFuayB5b3VhbXVodHRwOi8vbG9jYWxob3N0OjMzMzhhdWNzYXQ=";

/// A real V3 (`cashuA…`) token — two proofs (2 + 8 = 10 sat), memo.
const TOKEN_V3: &str = "cashuAeyJ0b2tlbiI6W3sibWludCI6Imh0dHBzOi8vODMzMy5zcGFjZTozMzM4IiwicHJvb2ZzIjpbeyJhbW91bnQiOjIsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6IjQwNzkxNWJjMjEyYmU2MWE3N2UzZTZkMmFlYjRjNzI3OTgwYmRhNTFjZDA2YTZhZmMyOWUyODYxNzY4YTc4MzciLCJDIjoiMDJiYzkwOTc5OTdkODFhZmIyY2M3MzQ2YjVlNDM0NWE5MzQ2YmQyYTUwNmViNzk1ODU5OGE3MmYwY2Y4NTE2M2VhIn0seyJhbW91bnQiOjgsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6ImZlMTUxMDkzMTRlNjFkNzc1NmIwZjhlZTBmMjNhNjI0YWNhYTNmNGUwNDJmNjE0MzNjNzI4YzcwNTdiOTMxYmUiLCJDIjoiMDI5ZThlNTA1MGI4OTBhN2Q2YzA5NjhkYjE2YmMxZDVkNWZhMDQwZWExZGUyODRmNmVjNjlkNjEyOTlmNjcxMDU5In1dfV0sInVuaXQiOiJzYXQiLCJtZW1vIjoiVGhhbmsgeW91LiJ9";

/// A real mainnet BOLT-11 invoice for 100 sat (one micro-bitcoin).
const BOLT11: &str = "lnbc1u1p53kkd9pp5ve8pd9zr60yjyvs6tn77mndavzrl5lwd2gx5hk934f6q8jwguzgsdqqcqzzsxqyz5vqrzjqvueefmrckfdwyyu39m0lf24sqzcr9vcrmxrvgfn6empxz7phrjxvrttncqq0lcqqyqqqqlgqqqqqqgq2qsp5482y73fxmlvg4t66nupdaph93h7dcmfsg2ud72wajf0cpk3a96rq9qxpqysgqujexd0l89u5dutn8hxnsec0c7jrt8wz0z67rut0eah0g7p6zhycn2vff0ts5vwn2h93kx8zzqy3tzu4gfhkya2zpdmqelg0ceqnjztcqma65pr";

/// Run `agicash decode <input>`, asserting success, and return parsed
/// stdout JSON.
fn decode_ok(input: &str) -> Value {
    let out = Command::cargo_bin("agicash")
        .unwrap()
        .args(["decode", input])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("decode stdout is UTF-8");
    serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("decode stdout was not valid JSON ({e}): {s}"))
}

/// Run `agicash decode <input>` expecting failure; return (exit code,
/// parsed stderr `error` body).
fn decode_err(input: &str) -> (i32, Value) {
    let out = Command::cargo_bin("agicash")
        .unwrap()
        .args(["decode", input])
        .assert()
        .failure()
        .get_output()
        .clone();
    let code = out.status.code().expect("decode produced an exit code");
    let stderr = String::from_utf8(out.stderr).expect("decode stderr is UTF-8");
    let json_line = stderr
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error line in stderr: {stderr}"));
    let parsed: Value = serde_json::from_str(json_line.trim())
        .unwrap_or_else(|e| panic!("error line was not valid JSON ({e}): {json_line}"));
    (code, parsed)
}

#[test]
fn decode_v4_token_emits_token_shape() {
    let v = decode_ok(TOKEN_V4);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 4);
    assert_eq!(v["amount"], "1");
    assert_eq!(v["unit"], "sat");
    assert_eq!(v["proof_count"], 1);
    assert!(v["mint_url"].as_str().unwrap().contains("localhost"));
}

#[test]
fn decode_v3_token_emits_token_shape() {
    let v = decode_ok(TOKEN_V3);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 3);
    // Two proofs: 2 + 8 = 10 sat.
    assert_eq!(v["amount"], "10");
    assert_eq!(v["proof_count"], 2);
}

#[test]
fn decode_v3_token_carries_memo() {
    let v = decode_ok(TOKEN_V3);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 3);
    assert_eq!(v["memo"], "Thank you.");
}

#[test]
fn decode_bolt11_emits_invoice_shape() {
    let v = decode_ok(BOLT11);
    assert_eq!(v["artifact"], "bolt11-invoice");
    assert_eq!(v["network"], "bitcoin");
    assert_eq!(v["unit"], "sat");
    assert_eq!(v["amount"], "100", "1 µBTC = 100 sat");
    assert_eq!(
        v["payment_hash"].as_str().map(str::len),
        Some(64),
        "payment_hash should be a 32-byte hex string",
    );
}

#[test]
fn decode_lightning_address_emits_address_shape() {
    let v = decode_ok("alice@walletofsatoshi.com");
    assert_eq!(v["artifact"], "lightning-address");
    assert_eq!(v["local_part"], "alice");
    assert_eq!(v["domain"], "walletofsatoshi.com");
    assert_eq!(
        v["lnurlp_endpoint"],
        "https://walletofsatoshi.com/.well-known/lnurlp/alice",
    );
}

#[test]
fn decode_extracts_token_from_url_query() {
    // Step 0 of the receive flow: the user often pastes a redeem URL,
    // not the raw encoded token. `decode` must extract the token from
    // the URL (mirrors the iOS/Android/Leptos paste-handler behaviour
    // exposed via `agicash_cashu::extract_cashu_token`).
    let url = format!("https://wallet.example/redeem?token={TOKEN_V4}");
    let v = decode_ok(&url);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 4);
    assert_eq!(v["amount"], "1");
}

#[test]
fn decode_extracts_token_from_url_hash() {
    // `cashu:`-style deep links + share URLs often use the fragment.
    let url = format!("https://wallet.example/r#{TOKEN_V3}");
    let v = decode_ok(&url);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 3);
    assert_eq!(v["amount"], "10");
}

#[test]
fn decode_extracts_token_from_cashu_uri() {
    // Apps that DO register the `cashu:` URI scheme route through the
    // same extraction step — `cashu:cashuB…` works because the regex
    // skips the prefix and finds the encoded token.
    let uri = format!("cashu:{TOKEN_V4}");
    let v = decode_ok(&uri);
    assert_eq!(v["artifact"], "cashu-token");
    assert_eq!(v["version"], 4);
}

#[test]
fn decode_malformed_token_exits_one_with_invalid_token() {
    let (code, body) = decode_err("cashuBnot-real-cbor");
    assert_eq!(code, 1, "decode is offline — malformed input is code 1");
    assert_eq!(body.pointer("/error/code").unwrap(), "invalid-token");
}

#[test]
fn decode_malformed_invoice_exits_one_with_invalid_invoice() {
    let (code, body) = decode_err("lnbcgarbage");
    assert_eq!(code, 1);
    assert_eq!(body.pointer("/error/code").unwrap(), "invalid-invoice");
}

#[test]
fn decode_malformed_lightning_address_exits_one() {
    let (code, body) = decode_err("alice@");
    assert_eq!(code, 1);
    assert_eq!(
        body.pointer("/error/code").unwrap(),
        "invalid-lightning-address",
    );
}

#[test]
fn decode_unrecognized_input_exits_one() {
    let (code, body) = decode_err("just plain text");
    assert_eq!(code, 1);
    assert_eq!(body.pointer("/error/code").unwrap(), "unrecognized-input");
}

#[test]
fn decode_never_produces_auth_required() {
    // `decode` is offline — no session, no network. None of its error
    // paths may surface exit code 3 (auth required).
    for bad in ["cashuBbad", "lnbcbad", "x@", "nonsense"] {
        let (code, _) = decode_err(bad);
        assert_ne!(code, 3, "`decode {bad}` must never exit 3 (auth required)");
    }
}
