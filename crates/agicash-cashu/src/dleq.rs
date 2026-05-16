//! NUT-12 DLEQ verification helpers.
//!
//! Why this exists: `cdk::dhke::construct_proofs` only *records* the DLEQ
//! field on the resulting `Proof` — it never verifies it. CDK's own
//! `Wallet` saga (`cdk::wallet::{issue,receive,swap}::saga`) calls
//! `BlindSignature::verify_dleq` / `Proof::verify_dleq` opportunistically.
//! Agicash bypasses that saga (we talk to the mint directly via
//! `MintConnector::post_{swap,mint,melt}`), so the verification path is
//! bypassed too.
//!
//! See `docs/superpowers/specs/2026-05-15-cashu-nut-compliance-audit.md`
//! §"NUT-12: DLEQ proofs" for the original finding.
//!
//! ## Semantics
//!
//! Two flavors of verification:
//!
//! 1. [`verify_blind_signatures`] — call after every `post_swap` /
//!    `post_mint` / `post_melt` round-trip. For each returned
//!    `BlindSignature` paired with its outgoing `BlindedMessage`, call
//!    `BlindSignature::verify_dleq`. Mints not advertising NUT-12 may
//!    omit the DLEQ entirely (`MissingDleqProof`) — this is tolerated,
//!    matching CDK's saga semantics. An `InvalidDleqProof` (or any
//!    other error) is a hard fail: a malicious mint could have signed
//!    with a key it does not commit to, so we reject the whole batch.
//!
//! 2. [`verify_proof_dleq`] — call on every incoming peer-token proof
//!    that carries an inline DLEQ. Tokens without a DLEQ (`dleq: None`)
//!    are still accepted (NUT-12 inline DLEQ is opportunistic on the
//!    sender side too); only proofs that *claim* a DLEQ but whose
//!    DLEQ is invalid are rejected.

use cdk::nuts::nut12::Error as Nut12Error;
use cdk::nuts::{BlindSignature, BlindedMessage, KeySet, Proof};

/// Why verification failed.
///
/// Surfaced through each service's typed error enum (e.g.
/// [`crate::ReceiveSwapError::DleqVerificationFailed`]) so callers can
/// distinguish a mint-key mismatch from a missing amount key.
#[derive(Debug, thiserror::Error)]
pub enum DleqVerificationError {
    /// A returned `BlindSignature` carried a DLEQ that did not verify
    /// against the mint pubkey for its amount. Highest-severity failure:
    /// either the mint is malicious or the wire was tampered with.
    #[error("blind signature DLEQ invalid for amount {amount}: {source}")]
    BlindSignatureInvalid {
        amount: u64,
        #[source]
        source: Nut12Error,
    },

    /// An incoming peer-token `Proof` carried an inline DLEQ that did
    /// not verify. The sender's wallet shipped proofs that don't
    /// actually trace back to the mint's signing key.
    #[error("proof DLEQ invalid: {source}")]
    ProofInvalid {
        #[source]
        source: Nut12Error,
    },

    /// The keyset returned by the mint does not contain a pubkey for
    /// the amount we asked for. This should never happen for an honest
    /// mint serving the keyset it advertises, but verifying defensively
    /// avoids a panic-on-`expect`.
    #[error("no mint pubkey for amount {amount} in keyset {keyset_id}")]
    NoKeyForAmount { amount: u64, keyset_id: String },

    /// `signatures.len() != blinded_messages.len()`. Either the mint
    /// returned the wrong number of signatures (protocol violation) or
    /// our caller paired them incorrectly. Bail rather than verify a
    /// partial batch.
    #[error("signature/message count mismatch: {sigs} sigs vs {msgs} messages")]
    CountMismatch { sigs: usize, msgs: usize },
}

/// Verify every blind signature returned by the mint against the matching
/// outgoing blinded message and the mint's per-amount pubkey.
///
/// Pass `signatures` and `blinded_messages` in matching order: the mint
/// MUST return signatures in the same order as the messages it received
/// (NUT-03/04/05 wire contract).
///
/// Returns `Ok(())` if every signature either verifies or omits the DLEQ
/// (mint doesn't advertise NUT-12). Returns `Err(BlindSignatureInvalid)`
/// on the first hard failure — caller should reject the whole response.
pub fn verify_blind_signatures(
    signatures: &[BlindSignature],
    blinded_messages: &[BlindedMessage],
    keyset: &KeySet,
) -> Result<(), DleqVerificationError> {
    if signatures.len() != blinded_messages.len() {
        return Err(DleqVerificationError::CountMismatch {
            sigs: signatures.len(),
            msgs: blinded_messages.len(),
        });
    }
    for (sig, msg) in signatures.iter().zip(blinded_messages.iter()) {
        // If the signature has no DLEQ at all, skip (NUT-12 not
        // advertised by this mint, per CDK saga semantics).
        if sig.dleq.is_none() {
            continue;
        }
        let amount = u64::from(sig.amount);
        let mint_pubkey = keyset.keys.amount_key(sig.amount).ok_or_else(|| {
            DleqVerificationError::NoKeyForAmount {
                amount,
                keyset_id: keyset.id.to_string(),
            }
        })?;
        match sig.verify_dleq(mint_pubkey, msg.blinded_secret) {
            Ok(()) | Err(Nut12Error::MissingDleqProof) => {}
            Err(source) => {
                return Err(DleqVerificationError::BlindSignatureInvalid { amount, source });
            }
        }
    }
    Ok(())
}

/// Verify an incoming peer-token `Proof`'s inline DLEQ against the
/// mint's per-amount pubkey.
///
/// Proofs without a DLEQ (`dleq: None`) verify trivially — NUT-12
/// inline DLEQ is opportunistic on the sender side. Only proofs whose
/// DLEQ is present but cryptographically invalid are rejected.
pub fn verify_proof_dleq(proof: &Proof, keyset: &KeySet) -> Result<(), DleqVerificationError> {
    if proof.dleq.is_none() {
        return Ok(());
    }
    let amount = u64::from(proof.amount);
    let mint_pubkey = keyset.keys.amount_key(proof.amount).ok_or_else(|| {
        DleqVerificationError::NoKeyForAmount {
            amount,
            keyset_id: keyset.id.to_string(),
        }
    })?;
    proof
        .verify_dleq(mint_pubkey)
        .map_err(|source| DleqVerificationError::ProofInvalid { source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdk::nuts::nut00::BlindSignature;
    use cdk::nuts::nut01::{Keys, SecretKey};
    use cdk::nuts::nut02::Id as KeysetId;
    use cdk::Amount;
    use std::collections::BTreeMap;
    use std::str::FromStr;

    /// Build a one-amount keyset that maps `amount` -> the pubkey
    /// derived from `secret`. Mirrors the cashu-0.15.1 nut12 vectors
    /// so we can reuse their known-good blind signature.
    fn one_amount_keyset(secret: &SecretKey, amount: Amount) -> KeySet {
        let mut m: BTreeMap<Amount, cdk::nuts::nut01::PublicKey> = BTreeMap::new();
        m.insert(amount, secret.public_key());
        KeySet {
            id: KeysetId::from_str("00882760bfa2eb41").unwrap(),
            unit: cdk::nuts::CurrencyUnit::Sat,
            active: Some(true),
            keys: Keys::new(m),
            input_fee_ppk: 0,
            final_expiry: None,
        }
    }

    /// The known-good blind signature from cashu-0.15.1's nut12 test
    /// vectors. Pairs with `mint_secret = 0x...01`, `B' =
    /// 02a9acc1...ba2`, amount `8`.
    const VALID_BLIND_SIG_JSON: &str = r#"{
        "amount": 8,
        "id": "00882760bfa2eb41",
        "C_": "02a9acc1e48c25eeeb9289b5031cc57da9fe72f3fe2861d264bdc074209b107ba2",
        "dleq": {
            "e": "9818e061ee51d5c8edc3342369a554998ff7b4381c8652d724cdf46429be73d9",
            "s": "9818e061ee51d5c8edc3342369a554998ff7b4381c8652d724cdf46429be73da"
        }
    }"#;

    /// Same shape as above, but `e` is the all-zeros-plus-1 test value
    /// (cryptographically invalid for the same B'/C').
    const TAMPERED_BLIND_SIG_JSON: &str = r#"{
        "amount": 8,
        "id": "00882760bfa2eb41",
        "C_": "02a9acc1e48c25eeeb9289b5031cc57da9fe72f3fe2861d264bdc074209b107ba2",
        "dleq": {
            "e": "0000000000000000000000000000000000000000000000000000000000000001",
            "s": "0000000000000000000000000000000000000000000000000000000000000002"
        }
    }"#;

    fn stub_blinded_message() -> BlindedMessage {
        let blinded_secret = cdk::nuts::nut01::PublicKey::from_str(
            "02a9acc1e48c25eeeb9289b5031cc57da9fe72f3fe2861d264bdc074209b107ba2",
        )
        .unwrap();
        BlindedMessage::new(
            Amount::from(8),
            KeysetId::from_str("00882760bfa2eb41").unwrap(),
            blinded_secret,
        )
    }

    fn mint_secret() -> SecretKey {
        SecretKey::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
            .unwrap()
    }

    #[test]
    fn verify_blind_signatures_accepts_valid() {
        let secret = mint_secret();
        let keyset = one_amount_keyset(&secret, Amount::from(8));
        let sig: BlindSignature = serde_json::from_str(VALID_BLIND_SIG_JSON).unwrap();
        let msg = stub_blinded_message();
        verify_blind_signatures(&[sig], &[msg], &keyset).expect("valid dleq should verify");
    }

    #[test]
    fn verify_blind_signatures_rejects_tampered() {
        let secret = mint_secret();
        let keyset = one_amount_keyset(&secret, Amount::from(8));
        let sig: BlindSignature = serde_json::from_str(TAMPERED_BLIND_SIG_JSON).unwrap();
        let msg = stub_blinded_message();
        let err = verify_blind_signatures(&[sig], &[msg], &keyset).unwrap_err();
        assert!(
            matches!(
                err,
                DleqVerificationError::BlindSignatureInvalid { amount: 8, .. }
            ),
            "expected BlindSignatureInvalid(amount=8), got {err:?}"
        );
    }

    #[test]
    fn verify_blind_signatures_tolerates_missing_dleq() {
        // A signature with no DLEQ at all corresponds to a mint that
        // doesn't advertise NUT-12. CDK's saga tolerates this; we
        // match.
        let secret = mint_secret();
        let keyset = one_amount_keyset(&secret, Amount::from(8));
        let sig_no_dleq = BlindSignature {
            amount: Amount::from(8),
            keyset_id: KeysetId::from_str("00882760bfa2eb41").unwrap(),
            c: cdk::nuts::nut01::PublicKey::from_str(
                "02a9acc1e48c25eeeb9289b5031cc57da9fe72f3fe2861d264bdc074209b107ba2",
            )
            .unwrap(),
            dleq: None,
        };
        let msg = stub_blinded_message();
        verify_blind_signatures(&[sig_no_dleq], &[msg], &keyset)
            .expect("missing dleq should be tolerated");
    }

    #[test]
    fn verify_blind_signatures_rejects_count_mismatch() {
        let secret = mint_secret();
        let keyset = one_amount_keyset(&secret, Amount::from(8));
        let sig: BlindSignature = serde_json::from_str(VALID_BLIND_SIG_JSON).unwrap();
        let err = verify_blind_signatures(&[sig], &[], &keyset).unwrap_err();
        assert!(matches!(
            err,
            DleqVerificationError::CountMismatch { sigs: 1, msgs: 0 }
        ));
    }

    /// Known-good proof DLEQ from cashu-0.15.1's nut12 tests.
    const VALID_PROOF_JSON: &str = r#"{
        "amount": 1,
        "id": "00882760bfa2eb41",
        "secret": "daf4dd00a2b68a0858a80450f52c8a7d2ccf87d375e43e216e0c571f089f63e9",
        "C": "024369d2d22a80ecf78f3937da9d5f30c1b9f74f0c32684d583cca0fa6a61cdcfc",
        "dleq": {
            "e": "b31e58ac6527f34975ffab13e70a48b6d2b0d35abc4b03f0151f09ee1a9763d4",
            "s": "8fbae004c59e754d71df67e392b6ae4e29293113ddc2ec86592a0431d16306d8",
            "r": "a6d13fcd7a18442e6076f5e1e7c887ad5de40a019824bdfa9fe740d302e8d861"
        }
    }"#;

    /// Same proof, but `e` flipped on its last byte (still parses as a
    /// SecretKey but no longer satisfies the DLEQ relation).
    const TAMPERED_PROOF_JSON: &str = r#"{
        "amount": 1,
        "id": "00882760bfa2eb41",
        "secret": "daf4dd00a2b68a0858a80450f52c8a7d2ccf87d375e43e216e0c571f089f63e9",
        "C": "024369d2d22a80ecf78f3937da9d5f30c1b9f74f0c32684d583cca0fa6a61cdcfc",
        "dleq": {
            "e": "b31e58ac6527f34975ffab13e70a48b6d2b0d35abc4b03f0151f09ee1a9763d5",
            "s": "8fbae004c59e754d71df67e392b6ae4e29293113ddc2ec86592a0431d16306d8",
            "r": "a6d13fcd7a18442e6076f5e1e7c887ad5de40a019824bdfa9fe740d302e8d861"
        }
    }"#;

    fn proof_test_keyset() -> KeySet {
        // The proof above was created against the secp256k1 generator
        // point's secret (`0000...0001`). Reuse the same keyset shape.
        let secret =
            SecretKey::from_hex("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        one_amount_keyset(&secret, Amount::from(1))
    }

    #[test]
    fn verify_proof_accepts_valid_inline_dleq() {
        let proof: Proof = serde_json::from_str(VALID_PROOF_JSON).unwrap();
        let keyset = proof_test_keyset();
        verify_proof_dleq(&proof, &keyset).expect("valid inline DLEQ should verify");
    }

    #[test]
    fn verify_proof_rejects_tampered_inline_dleq() {
        let proof: Proof = serde_json::from_str(TAMPERED_PROOF_JSON).unwrap();
        let keyset = proof_test_keyset();
        let err = verify_proof_dleq(&proof, &keyset).unwrap_err();
        assert!(
            matches!(err, DleqVerificationError::ProofInvalid { .. }),
            "expected ProofInvalid, got {err:?}"
        );
    }

    #[test]
    fn verify_proof_tolerates_missing_dleq() {
        let mut proof: Proof = serde_json::from_str(VALID_PROOF_JSON).unwrap();
        proof.dleq = None;
        let keyset = proof_test_keyset();
        verify_proof_dleq(&proof, &keyset)
            .expect("proof without inline DLEQ should verify trivially");
    }
}
