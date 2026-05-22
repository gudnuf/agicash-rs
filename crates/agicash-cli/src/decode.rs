//! `decode` subcommand — offline inspection of a Cashu token, a BOLT-11
//! Lightning invoice, or a LUD-16 Lightning Address.
//!
//! This command takes **no session and no network**: it is pure parsing,
//! so an agent can inspect an artifact before deciding to act on it. The
//! input type is auto-detected from the string:
//!
//!   - contains `cashuA…` / `cashuB…` substring → a Cashu token,
//!     extracted via [`agicash_cashu::extract_cashu_token`] (URL with
//!     hash/query, `cashu:` URI, embedded text all work) then decoded
//!     via [`cdk::nuts::Token::from_str`] (V3 JSON / V4 CBOR). Proof
//!     amounts are summed offline — no mint round-trip.
//!   - starts with `lnbc`/`lntb`/`lnbcrt`/`lntbs` → a BOLT-11 invoice,
//!     decoded via [`cdk::Bolt11Invoice`].
//!   - `user@domain` → a LUD-16 Lightning Address, validated offline via
//!     [`agicash_lightning_address::parse_lightning_address`] (the
//!     well-known endpoint is NOT fetched).
//!
//! There is no auth-required error here — `decode` is offline, so exit
//! code 3 is never produced.

use std::str::FromStr;

use cdk::nuts::CurrencyUnit;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum DecodeCmdError {
    /// The input did not match any recognized artifact prefix.
    #[error(
        "unrecognized input; expected a `cashuA…`/`cashuB…` token, a \
`lnbc…` BOLT-11 invoice, or a `user@domain` Lightning Address"
    )]
    Unrecognized,
    /// The input looked like a Cashu token but failed to decode.
    #[error("invalid Cashu token: {0}")]
    InvalidToken(String),
    /// The input looked like a BOLT-11 invoice but failed to decode.
    #[error("invalid BOLT-11 invoice: {0}")]
    InvalidInvoice(String),
    /// The input looked like a Lightning Address but failed to parse.
    #[error("invalid Lightning Address: {0}")]
    InvalidLightningAddress(String),
}

/// Decoded Cashu token shape.
#[derive(Serialize)]
struct TokenDecoded {
    artifact: &'static str,
    version: u8,
    mint_url: String,
    amount: String,
    unit: String,
    proof_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    memo: Option<String>,
}

/// Decoded BOLT-11 invoice shape.
#[derive(Serialize)]
struct InvoiceDecoded {
    artifact: &'static str,
    network: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<String>,
    unit: &'static str,
    payment_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payee: Option<String>,
    timestamp: u64,
    expiry: u64,
    expired: bool,
}

/// Decoded Lightning Address shape.
#[derive(Serialize)]
struct LightningAddressDecoded {
    artifact: &'static str,
    address: String,
    local_part: String,
    domain: String,
    /// The LUD-16 well-known endpoint that `send lightning-address` will
    /// resolve. Constructed offline; not fetched.
    lnurlp_endpoint: String,
}

/// Public entry point — auto-detect the input type and print the decoded
/// JSON to stdout. Offline; never touches the network or a session.
pub fn cmd_decode(input: &str) -> Result<(), DecodeCmdError> {
    let trimmed = input.trim();
    // Step 1: try cashu extraction first. Handles raw `cashuA…`/`cashuB…`
    // plus URLs (`?token=…`, `#…`), the `cashu:` URI scheme, and tokens
    // embedded in arbitrary text. Returns the verbatim encoded token slice
    // that `decode_token` can hand to `cdk::nuts::Token::from_str`.
    let json = if let Some(encoded) = agicash_cashu::extract_cashu_token(trimmed) {
        decode_token(&encoded)?
    } else if starts_with_cashu_prefix(trimmed) {
        // The input clearly looks like a bare cashu token (starts
        // with `cashuA`/`cashuB`) but the extractor's structural
        // validation said no. Hand it to `decode_token` so the user
        // gets a precise "invalid Cashu token: …" message instead of
        // the catch-all "unrecognized input".
        decode_token(trimmed)?
    } else if is_bolt11(trimmed) {
        decode_invoice(trimmed)?
    } else if trimmed.contains('@') {
        decode_lightning_address(trimmed)?
    } else {
        return Err(DecodeCmdError::Unrecognized);
    };
    println!("{json}");
    Ok(())
}

/// True iff the string begins with one of the Cashu token prefixes.
/// Used as a fallback after `extract_cashu_token` returns None so a
/// bare-but-malformed `cashuB…` paste produces a typed
/// `invalid-token` error rather than `unrecognized-input`.
fn starts_with_cashu_prefix(s: &str) -> bool {
    s.starts_with("cashuA") || s.starts_with("cashuB")
}

/// BOLT-11 invoices are HRP-prefixed: `lnbc` (mainnet), `lntb` (testnet),
/// `lnbcrt` (regtest), `lntbs` (signet). Match case-insensitively.
fn is_bolt11(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("lnbc")
        || lower.starts_with("lntb")
        || lower.starts_with("lnbcrt")
        || lower.starts_with("lntbs")
        || lower.starts_with("lnsb")
}

fn unit_label(unit: &CurrencyUnit) -> String {
    match unit {
        CurrencyUnit::Sat => "sat".to_string(),
        CurrencyUnit::Msat => "msat".to_string(),
        CurrencyUnit::Usd => "usd".to_string(),
        CurrencyUnit::Eur => "eur".to_string(),
        other => other.to_string(),
    }
}

fn decode_token(raw: &str) -> Result<String, DecodeCmdError> {
    let token =
        cdk::nuts::Token::from_str(raw).map_err(|e| DecodeCmdError::InvalidToken(e.to_string()))?;
    let version = match &token {
        cdk::nuts::Token::TokenV3(_) => 3,
        cdk::nuts::Token::TokenV4(_) => 4,
    };
    let mint_url = token
        .mint_url()
        .map_err(|e| DecodeCmdError::InvalidToken(format!("mint URL: {e}")))?
        .to_string();
    // `value()` sums the proof amounts — offline, no keysets needed.
    let amount = token
        .value()
        .map_err(|e| DecodeCmdError::InvalidToken(format!("amount: {e}")))?;
    let unit = token
        .unit()
        .map_or_else(|| "unknown".to_string(), |u| unit_label(&u));
    let proof_count = token.token_secrets().len();
    let decoded = TokenDecoded {
        artifact: "cashu-token",
        version,
        mint_url,
        amount: u64::from(amount).to_string(),
        unit,
        proof_count,
        memo: token.memo().clone(),
    };
    Ok(serde_json::to_string(&decoded).expect("serialize decoded token"))
}

fn decode_invoice(raw: &str) -> Result<String, DecodeCmdError> {
    let invoice = cdk::Bolt11Invoice::from_str(raw)
        .map_err(|e| DecodeCmdError::InvalidInvoice(e.to_string()))?;
    let amount = invoice
        .amount_milli_satoshis()
        // BOLT-11 carries msat; report whole sats when the amount is an
        // exact number of sats, otherwise keep msat precision is lost —
        // the wallet only mints whole sats, so floor and note nothing.
        .map(|msat| (msat / 1000).to_string());
    let payment_hash = hex::encode(invoice.payment_hash());
    let description = match invoice.description() {
        cdk::lightning_invoice::Bolt11InvoiceDescriptionRef::Direct(d) => Some(d.to_string()),
        cdk::lightning_invoice::Bolt11InvoiceDescriptionRef::Hash(_) => None,
    };
    let payee = invoice.payee_pub_key().map(ToString::to_string);
    let timestamp = invoice
        .timestamp()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let decoded = InvoiceDecoded {
        artifact: "bolt11-invoice",
        network: invoice.network().to_string(),
        amount,
        unit: "sat",
        payment_hash,
        description,
        payee,
        timestamp,
        expiry: invoice.expiry_time().as_secs(),
        expired: invoice.is_expired(),
    };
    Ok(serde_json::to_string(&decoded).expect("serialize decoded invoice"))
}

fn decode_lightning_address(raw: &str) -> Result<String, DecodeCmdError> {
    let (local, domain) = agicash_lightning_address::parse_lightning_address(raw)
        .map_err(|e| DecodeCmdError::InvalidLightningAddress(e.to_string()))?;
    // localhost domains use http; everything else https — mirrors the
    // scheme choice in `agicash_lightning_address::resolve`.
    let scheme = if domain == "localhost" || domain.starts_with("localhost:") {
        "http"
    } else {
        "https"
    };
    let decoded = LightningAddressDecoded {
        artifact: "lightning-address",
        address: raw.to_string(),
        lnurlp_endpoint: format!("{scheme}://{domain}/.well-known/lnurlp/{local}"),
        local_part: local,
        domain,
    };
    Ok(serde_json::to_string(&decoded).expect("serialize decoded lightning address"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real V4 (`cashuB…`) token — `http://localhost:3338`, single
    // 1-sat proof, memo "Thank you". Captured from `cdk`'s own token
    // round-trip test fixtures; decodes fully offline.
    const TOKEN_V4: &str = "cashuBpGF0gaJhaUgArSaMTR9YJmFwgaNhYQFhc3hAOWE2ZGJiODQ3YmQyMzJiYTc2ZGIwZGYxOTcyMTZiMjlkM2I4Y2MxNDU1M2NkMjc4MjdmYzFjYzk0MmZlZGI0ZWFjWCEDhhhUP_trhpXfStS6vN6So0qWvc2X3O4NfM-Y1HISZ5JhZGlUaGFuayB5b3VhbXVodHRwOi8vbG9jYWxob3N0OjMzMzhhdWNzYXQ=";

    // A real V3 (`cashuA…`) token — `https://8333.space:3338`, two
    // proofs (2 + 8 = 10 sat), memo "Thank you.". From `cdk` fixtures.
    const TOKEN_V3: &str = "cashuAeyJ0b2tlbiI6W3sibWludCI6Imh0dHBzOi8vODMzMy5zcGFjZTozMzM4IiwicHJvb2ZzIjpbeyJhbW91bnQiOjIsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6IjQwNzkxNWJjMjEyYmU2MWE3N2UzZTZkMmFlYjRjNzI3OTgwYmRhNTFjZDA2YTZhZmMyOWUyODYxNzY4YTc4MzciLCJDIjoiMDJiYzkwOTc5OTdkODFhZmIyY2M3MzQ2YjVlNDM0NWE5MzQ2YmQyYTUwNmViNzk1ODU5OGE3MmYwY2Y4NTE2M2VhIn0seyJhbW91bnQiOjgsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6ImZlMTUxMDkzMTRlNjFkNzc1NmIwZjhlZTBmMjNhNjI0YWNhYTNmNGUwNDJmNjE0MzNjNzI4YzcwNTdiOTMxYmUiLCJDIjoiMDI5ZThlNTA1MGI4OTBhN2Q2YzA5NjhkYjE2YmMxZDVkNWZhMDQwZWExZGUyODRmNmVjNjlkNjEyOTlmNjcxMDU5In1dfV0sInVuaXQiOiJzYXQiLCJtZW1vIjoiVGhhbmsgeW91LiJ9";

    // A real mainnet BOLT-11 invoice for 100 sat (1 µBTC). From `cdk`
    // test fixtures.
    const BOLT11: &str = "lnbc1u1p53kkd9pp5ve8pd9zr60yjyvs6tn77mndavzrl5lwd2gx5hk934f6q8jwguzgsdqqcqzzsxqyz5vqrzjqvueefmrckfdwyyu39m0lf24sqzcr9vcrmxrvgfn6empxz7phrjxvrttncqq0lcqqyqqqqlgqqqqqqgq2qsp5482y73fxmlvg4t66nupdaph93h7dcmfsg2ud72wajf0cpk3a96rq9qxpqysgqujexd0l89u5dutn8hxnsec0c7jrt8wz0z67rut0eah0g7p6zhycn2vff0ts5vwn2h93kx8zzqy3tzu4gfhkya2zpdmqelg0ceqnjztcqma65pr";

    #[test]
    fn detects_bolt11_across_networks() {
        assert!(is_bolt11("lnbc1u1p..."));
        assert!(is_bolt11("LNBC1U1P..."));
        assert!(is_bolt11("lntb100n1..."));
        assert!(is_bolt11("lnbcrt500n1..."));
        assert!(!is_bolt11("cashuBxyz"));
        assert!(!is_bolt11("alice@example.com"));
    }

    #[test]
    fn decodes_v4_token() {
        let out = decode_token(TOKEN_V4).expect("decode V4 token");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["artifact"], "cashu-token");
        assert_eq!(v["version"], 4);
        assert_eq!(v["amount"], "1");
        assert_eq!(v["unit"], "sat");
        assert_eq!(v["proof_count"], 1);
        assert!(v["mint_url"].as_str().unwrap().contains("localhost"));
    }

    #[test]
    fn decodes_v3_token_with_memo() {
        let out = decode_token(TOKEN_V3).expect("decode V3 token");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["artifact"], "cashu-token");
        assert_eq!(v["version"], 3);
        // Two proofs: 2 + 8 = 10 sat.
        assert_eq!(v["amount"], "10");
        assert_eq!(v["unit"], "sat");
        assert_eq!(v["proof_count"], 2);
        assert_eq!(v["memo"], "Thank you.");
    }

    #[test]
    fn rejects_malformed_token() {
        let err = decode_token("cashuBnot-valid-cbor").unwrap_err();
        assert!(matches!(err, DecodeCmdError::InvalidToken(_)));
    }

    #[test]
    fn decodes_bolt11_invoice() {
        let out = decode_invoice(BOLT11).expect("decode invoice");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["artifact"], "bolt11-invoice");
        assert_eq!(v["network"], "bitcoin");
        assert_eq!(v["unit"], "sat");
        // 1 µBTC = 100 sat.
        assert_eq!(v["amount"], "100");
        assert!(v["payment_hash"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn rejects_malformed_invoice() {
        let err = decode_invoice("lnbcgarbage").unwrap_err();
        assert!(matches!(err, DecodeCmdError::InvalidInvoice(_)));
    }

    #[test]
    fn decodes_lightning_address() {
        let out = decode_lightning_address("alice@walletofsatoshi.com")
            .expect("decode lightning address");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["artifact"], "lightning-address");
        assert_eq!(v["local_part"], "alice");
        assert_eq!(v["domain"], "walletofsatoshi.com");
        assert_eq!(
            v["lnurlp_endpoint"],
            "https://walletofsatoshi.com/.well-known/lnurlp/alice"
        );
    }

    #[test]
    fn rejects_malformed_lightning_address() {
        let err = decode_lightning_address("not-an-address").unwrap_err();
        assert!(matches!(err, DecodeCmdError::InvalidLightningAddress(_)));
    }

    #[test]
    fn rejects_unrecognized_input() {
        let err = cmd_decode("just some random text").unwrap_err();
        assert!(matches!(err, DecodeCmdError::Unrecognized));
    }
}
