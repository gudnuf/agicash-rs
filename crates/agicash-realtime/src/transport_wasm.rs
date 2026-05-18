//! Wasm transport: `web_sys::WebSocket`. `WebSocket` is `!Send` and
//! callback-driven; its `Closure`s are `.forget()`-leaked (same pattern
//! as `wallet_context.rs`) and inbound frames are bridged into an
//! `async-broadcast` channel so the codec/client layers never name
//! `WebSocket`. `binaryType=arraybuffer` mirrors realtime-js.
//!
//! This file is the ONLY place that names `web_sys::WebSocket`; the
//! `!Send` boundary is contained here (Risk 2, plan §"Top risks").
use crate::error::TransportError;
use crate::transport::{RealtimeTransport, WsFrame};
use async_trait::async_trait;
use futures_util::StreamExt;
use wasm_bindgen::{closure::Closure, JsCast};
use web_sys::{BinaryType, MessageEvent, WebSocket};

pub struct WasmTransport {
    ws: Option<WebSocket>,
    rx: Option<async_broadcast::Receiver<Result<WsFrame, TransportError>>>,
}

impl std::fmt::Debug for WasmTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmTransport")
            .field("connected", &self.ws.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for WasmTransport {
    fn default() -> Self {
        Self { ws: None, rx: None }
    }
}

impl WasmTransport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait(?Send)]
impl RealtimeTransport for WasmTransport {
    async fn connect(&mut self, url: &str) -> Result<(), TransportError> {
        let ws = WebSocket::new(url).map_err(|e| TransportError::Connect(format!("{e:?}")))?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let (tx, rx) = async_broadcast::broadcast(256);

        let txm = tx.clone();
        let on_msg = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            if let Ok(buf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                let _ = txm.try_broadcast(Ok(WsFrame::Binary(bytes)));
            } else if let Some(s) = e.data().as_string() {
                let _ = txm.try_broadcast(Ok(WsFrame::Text(s)));
            }
        });
        ws.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
        on_msg.forget();

        let txc = tx.clone();
        let on_close =
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |_e: web_sys::CloseEvent| {
                let _ = txc.try_broadcast(Err(TransportError::Closed("ws close".into())));
            });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();

        self.ws = Some(ws);
        self.rx = Some(rx);
        Ok(())
    }

    async fn send_text(&mut self, frame: String) -> Result<(), TransportError> {
        self.ws
            .as_ref()
            .ok_or_else(|| TransportError::Send("not connected".into()))?
            .send_with_str(&frame)
            .map_err(|e| TransportError::Send(format!("{e:?}")))
    }

    async fn recv(&mut self) -> Option<Result<WsFrame, TransportError>> {
        self.rx.as_mut()?.next().await
    }

    async fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError> {
        if let Some(ws) = self.ws.take() {
            let _ = ws.close_with_code_and_reason(code, reason);
        }
        Ok(())
    }
}
