//! Native transport: `tokio-tungstenite`. TLS chain validation uses
//! native roots (matches `agicash-storage-supabase`'s platform-verifier
//! choice so the iOS-sim mkcert root / local dev cert is honoured).
//! tungstenite auto-Pongs WS Pings by default — we keep that.
use crate::error::TransportError;
use crate::transport::{RealtimeTransport, WsFrame};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Default)]
pub struct NativeTransport {
    sock: Option<Sock>,
}

impl std::fmt::Debug for NativeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeTransport")
            .field("connected", &self.sock.is_some())
            .finish_non_exhaustive()
    }
}

impl NativeTransport {
    #[must_use]
    pub fn new() -> Self {
        Self { sock: None }
    }
}

#[async_trait]
impl RealtimeTransport for NativeTransport {
    async fn connect(&mut self, url: &str) -> Result<(), TransportError> {
        let (sock, _resp) = connect_async(url)
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        self.sock = Some(sock);
        Ok(())
    }

    async fn send_text(&mut self, frame: String) -> Result<(), TransportError> {
        let s = self
            .sock
            .as_mut()
            .ok_or_else(|| TransportError::Send("not connected".into()))?;
        s.send(Message::Text(frame))
            .await
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    async fn recv(&mut self) -> Option<Result<WsFrame, TransportError>> {
        let s = self.sock.as_mut()?;
        loop {
            match s.next().await? {
                Ok(Message::Text(t)) => return Some(Ok(WsFrame::Text(t))),
                Ok(Message::Binary(b)) => return Some(Ok(WsFrame::Binary(b))),
                // Ping/Pong (tungstenite auto-Pongs) and raw Frame are
                // transport-internal — skip and await the next message.
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Ok(Message::Close(_)) => {
                    return Some(Err(TransportError::Closed("server close".into())))
                }
                Err(e) => return Some(Err(TransportError::Other(e.to_string()))),
            }
        }
    }

    async fn close(&mut self, _code: u16, _reason: &str) -> Result<(), TransportError> {
        if let Some(mut s) = self.sock.take() {
            let _ = s.close(None).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn connect_to_unreachable_errors_fast() {
        let mut t = NativeTransport::new();
        // TEST-NET-1 documentation block — connection can never complete.
        // Wrap in a timeout so a non-routable host (no RST, just silence)
        // can't hang the test suite; either the connect errors or the
        // timeout fires — both prove `connect` does not succeed here.
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            t.connect("ws://192.0.2.1:9/realtime/v1/websocket"),
        )
        .await;
        match r {
            Ok(inner) => assert!(inner.is_err(), "connect must not succeed"),
            Err(_elapsed) => { /* timed out → connect did not succeed */ }
        }
    }
}
