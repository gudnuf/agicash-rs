//! wasm-bindgen surface for the receive-flow orchestrator.
//!
//! Mirrors the FFI [`crates/agicash-ffi/src/receive_flow.rs`] surface:
//! one long-lived handle the UI holds for the duration of one receive
//! interaction. Methods:
//!
//! - `currentState()` — snapshot the current state (no I/O).
//! - `start(token)` — dispatch `ReceiveFlowEvent::Start`.
//! - `confirmAddMint()` / `cancelAddMint()` — dispatch the confirm-prompt
//!   responses.
//! - `retry()` / `dismiss()` — drop a terminal state back to `Idle`.
//!
//! Each method returns a `JsValue` carrying the next stable state as a
//! `serde_wasm_bindgen`-encoded tagged-discriminator object
//! (`{ kind: "needsMintConfirmation", confirmation: {…} }` etc.). The
//! state envelope shape is `ReceiveFlowStateWasm` in `types.rs`.
//!
//! Single-threaded shape: wasm has no threads, so the inner
//! `ReceiveFlowService` lives behind `tokio::sync::Mutex` exactly as the
//! FFI handle does — the tokio `sync` feature works on wasm32 and the
//! locking is uncontended in practice (the JS event loop only ever has
//! one dispatch in flight at a time).
//!
//! No event-passing convention is invented here — JS calls one method
//! per event, mirroring the React reference's stateful hook surface.
//! The internal `ReceiveFlowEvent` enum stays sans-IO and is never
//! constructed on the JS side.

use crate::types::ReceiveFlowStateWasm;
use agicash_cashu::{ReceiveFlowEvent, ReceiveFlowService};
use std::rc::Rc;
use tokio::sync::Mutex;
use wasm_bindgen::prelude::*;

/// Long-lived handle the JS / Leptos side holds for one receive
/// interaction. Constructed via `AgicashWasmWallet.makeReceiveFlow()`.
///
/// Wraps a `ReceiveFlowService` behind an async `Mutex` so the wasm
/// shell can hand out shared references and still mutate the underlying
/// machine on each event. Each call to `makeReceiveFlow` returns a
/// fresh handle — flows are not persisted across constructions
/// (verbatim FFI semantics).
///
/// The inner service is wrapped in `Rc` (not `Arc`) — wasm32 is
/// single-threaded so `Rc` is sufficient and avoids the
/// `arc_with_non_send_sync` clippy noise that the facade already
/// suppresses for the same reason
/// (`agicash-wallet/src/client.rs::receive_flow`'s
/// `#[cfg_attr(target_arch = "wasm32", allow(…))]`).
#[wasm_bindgen]
pub struct AgicashReceiveFlow {
    inner: Rc<Mutex<ReceiveFlowService>>,
}

impl std::fmt::Debug for AgicashReceiveFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgicashReceiveFlow").finish_non_exhaustive()
    }
}

impl AgicashReceiveFlow {
    /// Construct a handle from an already-built service. Called only
    /// from `AgicashWasmWallet::make_receive_flow` — not part of the
    /// wasm-bindgen surface (the JS side never builds one directly).
    pub(crate) fn new(service: ReceiveFlowService) -> Self {
        Self {
            inner: Rc::new(Mutex::new(service)),
        }
    }
}

#[wasm_bindgen]
impl AgicashReceiveFlow {
    /// Snapshot the current state. Cheap; safe to call from a polling
    /// UI loop. Returns a `serde_wasm_bindgen`-encoded
    /// `ReceiveFlowStateWasm` tagged-union object.
    #[wasm_bindgen(js_name = currentState)]
    pub async fn current_state(&self) -> Result<JsValue, JsValue> {
        let guard = self.inner.lock().await;
        let state = crate::convert::receive_flow_state_from_inner(guard.current_state());
        encode_state(&state)
    }

    /// Begin a new flow with this token — dispatches
    /// [`ReceiveFlowEvent::Start`]. Returns the next stable state.
    /// Mirrors `ReceiveFlowEventFfi::Start { token }` on the FFI.
    #[wasm_bindgen]
    pub async fn start(&self, token: String) -> Result<JsValue, JsValue> {
        self.dispatch_event(ReceiveFlowEvent::Start { token }).await
    }

    /// User said yes to the "Add this mint?" prompt — dispatches
    /// [`ReceiveFlowEvent::ConfirmAddMint`]. Mirrors
    /// `ReceiveFlowEventFfi::ConfirmAddMint`.
    #[wasm_bindgen(js_name = confirmAddMint)]
    pub async fn confirm_add_mint(&self) -> Result<JsValue, JsValue> {
        self.dispatch_event(ReceiveFlowEvent::ConfirmAddMint).await
    }

    /// User said no to the "Add this mint?" prompt — dispatches
    /// [`ReceiveFlowEvent::CancelAddMint`]. The flow transitions to
    /// `Failed("user cancelled the add-mint prompt", "cancelled")`.
    /// Mirrors `ReceiveFlowEventFfi::CancelAddMint`.
    #[wasm_bindgen(js_name = cancelAddMint)]
    pub async fn cancel_add_mint(&self) -> Result<JsValue, JsValue> {
        self.dispatch_event(ReceiveFlowEvent::CancelAddMint).await
    }

    /// Reset a terminal state back to `Idle` so a new flow can start.
    /// Dispatches [`ReceiveFlowEvent::Retry`]. Mirrors
    /// `ReceiveFlowEventFfi::Retry`.
    #[wasm_bindgen]
    pub async fn retry(&self) -> Result<JsValue, JsValue> {
        self.dispatch_event(ReceiveFlowEvent::Retry).await
    }

    /// Drop a terminal state and go back to `Idle`. Dispatches
    /// [`ReceiveFlowEvent::Dismiss`]. Mirrors
    /// `ReceiveFlowEventFfi::Dismiss`.
    #[wasm_bindgen]
    pub async fn dismiss(&self) -> Result<JsValue, JsValue> {
        self.dispatch_event(ReceiveFlowEvent::Dismiss).await
    }
}

impl AgicashReceiveFlow {
    /// Internal helper — runs `ReceiveFlowService::dispatch`, maps the
    /// error path through `convert::receive_flow_error_to_js`, and
    /// JSON-encodes the resulting state envelope. Centralized so the
    /// six public event methods stay one-liners.
    async fn dispatch_event(&self, event: ReceiveFlowEvent) -> Result<JsValue, JsValue> {
        let mut guard = self.inner.lock().await;
        let next = guard
            .dispatch(event)
            .await
            .map_err(crate::convert::receive_flow_error_to_js)?;
        let state = crate::convert::receive_flow_state_from_inner(next);
        encode_state(&state)
    }
}

/// `ReceiveFlowStateWasm` → `JsValue`. Uses `serde_wasm_bindgen::to_value`
/// — the same crate the wasm shell already uses for `listAccounts`.
/// Serialization failures are wrapped as JS `Error` strings, matching
/// the shell's existing error idiom (verbatim `wallet_error_to_js`).
fn encode_state(state: &ReceiveFlowStateWasm) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(state)
        .map_err(|e| JsValue::from_str(&format!("serialize receive flow state: {e}")))
}
