# 12a — Cashu Sub-Error Classification Inventory

> **Authoritative source for Task 3's match arms.** Variant names derived from
> actual source at `556e581f` (grep of `crates/agicash-cashu/src/**/error.rs`
> and `crates/agicash-traits/src/cashu_provider.rs`), NOT invented.

Classification rule (from plan Task 1 Step 2):

- transport/timeout/connection/DNS/HTTP/offline → **Network**
- state moved between read & write / "already {spent,paid,expired,claimed,issued}" /
  stale / conflict / version mismatch / duplicate-in-flight → **Concurrency**
- everything else (domain rejection, parse, invariant, validation, not-found,
  security/DLEQ) → **Cashu** (unchanged catch-all)

The six `From<…> for WalletError` impls in `crates/agicash-wallet/src/error.rs`
are: `CashuProviderError`, `ReceiveSwapError`, `SendSwapError`,
`MintQuoteError`, `MeltQuoteError`, `ReceiveFlowError`.

---

## `CashuProviderError` (agicash-traits/src/cashu_provider.rs:43)

This is the leaf transport enum. Several other sub-errors wrap it
(`Mint(CashuProviderError)`, `MintDiscovery(CashuProviderError)`), so its
classification propagates upward.

| Variant | classify-as | rationale |
|---|---|---|
| `Network(String)` | **Network** | "mint unreachable" — transport/connectivity |
| `InvalidUrl(String)` | Cashu | bad mint URL — caller/config domain error |
| `Protocol(String)` | Cashu | mint protocol error — domain, not transport, not retry-safe |

## `ReceiveSwapError` (agicash-cashu/src/receive_swap/error.rs:12)

| Variant | classify-as | rationale |
|---|---|---|
| `InvalidTransition { from, event }` | Cashu | state-machine invariant (impl bug) |
| `Storage(ReceiveSwapStorageError)` | Cashu | storage — not cashu-transport, conservative Never |
| `Mint(CashuProviderError)` | **Network** iff inner is `CashuProviderError::Network`, else Cashu | delegate to wrapped provider |
| `TokenParse(String)` | Cashu | parse/domain |
| `AmountTooSmall` | Cashu | domain rejection |
| `MintMismatch { token, account }` | Cashu | domain rejection |
| `CurrencyMismatch { token, account }` | Cashu | domain rejection |
| `DleqVerificationFailed(DleqVerificationError)` | Cashu | security failure — must NOT retry |

No own state-moved variant → **no Concurrency arm**. Gains a Network arm via `Mint`.

## `SendSwapError` (agicash-cashu/src/send_swap/error.rs:12)

| Variant | classify-as | rationale |
|---|---|---|
| `InvalidTransition { from, event }` | Cashu | state-machine invariant |
| `Storage(SendSwapStorageError)` | Cashu | storage |
| `Mint(CashuProviderError)` | **Network** iff inner `CashuProviderError::Network`, else Cashu | delegate |
| `InsufficientBalance { needed, have }` | Cashu | domain rejection |
| `AmountTooSmall` | Cashu | domain rejection |
| `CurrencyMismatch { account, request }` | Cashu | domain rejection |
| `TokenEncode(String)` | Cashu | encode/internal |
| `DleqVerificationFailed(DleqVerificationError)` | Cashu | security — must NOT retry |

No own state-moved variant → **no Concurrency arm**. Gains a Network arm via `Mint`.

## `MintQuoteError` (agicash-cashu/src/mint_quote/error.rs:12)

| Variant | classify-as | rationale |
|---|---|---|
| `InvalidTransition { from, event }` | Cashu | state-machine invariant |
| `Storage(MintQuoteStorageError)` | Cashu | storage |
| `Mint(CashuProviderError)` | **Network** iff inner `CashuProviderError::Network`, else Cashu | delegate |
| `AmountTooSmall` | Cashu | domain rejection |
| `CurrencyMismatch { account, request }` | Cashu | domain rejection |
| `QuoteNotPaid` | Cashu | domain — caller polled too early, not a race |
| `QuoteExpired` | **Concurrency** | state moved: quote expired between read & write |
| `Unrecoverable(String)` | Cashu | operational dead-end, not retry-safe |
| `DleqVerificationFailed(DleqVerificationError)` | Cashu | security — must NOT retry |

Gains **Network** (via `Mint`) and **Concurrency** (`QuoteExpired`).

## `MeltQuoteError` (agicash-cashu/src/melt_quote/error.rs:12)

| Variant | classify-as | rationale |
|---|---|---|
| `InvalidTransition { from, event }` | Cashu | state-machine invariant |
| `Storage(MeltQuoteStorageError)` | Cashu | storage |
| `DuplicatePayment` | **Concurrency** | active melt quote already exists for this invoice — in-flight conflict surfaced from a unique-index race |
| `Mint(CashuProviderError)` | **Network** iff inner `CashuProviderError::Network`, else Cashu | delegate |
| `InvalidInvoice(String)` | Cashu | parse/domain |
| `AmountlessInvoice` | Cashu | unsupported/domain |
| `AmountTooSmall` | Cashu | domain rejection |
| `CurrencyMismatch { account, request }` | Cashu | domain rejection |
| `InsufficientBalance { needed, have }` | Cashu | domain rejection |
| `QuoteExpired` | **Concurrency** | state moved: invoice expired before melt initiated |
| `QuoteNotPending` | Cashu | domain — caller polled too early, not a race |
| `MeltFailed(String)` | Cashu | mint reported failure — terminal domain outcome |
| `Unrecoverable(String)` | Cashu | operational dead-end |
| `DleqVerificationFailed(DleqVerificationError)` | Cashu | security — must NOT retry |

Gains **Network** (via `Mint`) and **Concurrency** (`DuplicatePayment`, `QuoteExpired`).

## `ReceiveFlowError` (agicash-cashu/src/receive_flow/error.rs:29)

| Variant | classify-as | rationale |
|---|---|---|
| `TokenParse(String)` | Cashu | parse/domain |
| `InvalidEvent { event, state }` | Cashu | state-machine invariant |
| `MintDiscovery(CashuProviderError)` | **Network** iff inner `CashuProviderError::Network`, else Cashu | delegate to wrapped provider (NUT-06 discovery) |
| `MintAdd(StorageError)` | Cashu | storage |
| `Swap(ReceiveSwapError)` | **Network** iff inner classifies Network (via `ReceiveSwapError::Mint`), else Cashu | delegate to wrapped swap |
| `Storage(StorageError)` | Cashu | storage |
| `Auth(String)` | Cashu | auth-string — conservative Never (not the typed `AuthError` path) |

No own state-moved variant → **no own Concurrency arm**. Gains **Network**
via `MintDiscovery` and via `Swap(ReceiveSwapError::Mint(Network))`.

---

## Summary — which sub-errors gain which arms

| Sub-error | Network arm? | Concurrency arm? |
|---|---|---|
| `CashuProviderError` | yes (`Network`) | no |
| `ReceiveSwapError` | yes (via `Mint`) | no |
| `SendSwapError` | yes (via `Mint`) | no |
| `MintQuoteError` | yes (via `Mint`) | yes (`QuoteExpired`) |
| `MeltQuoteError` | yes (via `Mint`) | yes (`DuplicatePayment`, `QuoteExpired`) |
| `ReceiveFlowError` | yes (via `MintDiscovery` + `Swap`) | no |

Domain representatives for the "stays Cashu" tests:
`CashuProviderError::Protocol`, `ReceiveSwapError::AmountTooSmall`,
`SendSwapError::AmountTooSmall`, `MintQuoteError::QuoteNotPaid`,
`MeltQuoteError::MeltFailed`, `ReceiveFlowError::TokenParse`.
