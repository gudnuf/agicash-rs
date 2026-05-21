use crate::SupabaseStorageConfig;
use agicash_traits::{StorageError, TokenProvider};
use base64::Engine;
use std::sync::Arc;

// Native-only TLS plumbing. On wasm the browser handles TLS inside
// `fetch`, so the rustls/ring/platform-verifier stack is dead weight.
#[cfg(not(target_arch = "wasm32"))]
use rustls_platform_verifier::ConfigVerifierExt;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

/// Extract just the `sub` claim from a JWT for logging.
///
/// Mirrors `agicash_ffi::observability::jwt_sub` — duplicated rather
/// than depending on `agicash-ffi` because that would invert the
/// dependency graph (FFI already depends on storage). The function
/// is tiny and stateless; keeping the logging-only helper local
/// avoids the cycle.
///
/// Returns the user-id portion only. The full token never leaves
/// this function.
fn jwt_sub_for_log(jwt: &str) -> String {
    let mut parts = jwt.split('.');
    let _header = parts.next();
    let Some(payload_b64) = parts.next() else {
        return "<unparseable>".into();
    };
    let Ok(payload_bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)
    else {
        return "<unparseable>".into();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload_bytes) else {
        return "<unparseable>".into();
    };
    value
        .get("sub")
        .and_then(|v| v.as_str())
        .map_or_else(|| "<missing-sub>".into(), std::string::ToString::to_string)
}

/// Schema name in the Supabase project where all wallet tables live.
pub(crate) const WALLET_SCHEMA: &str = "wallet";

/// Build the shared `reqwest::Client` once and reuse it.
///
/// Native: TLS chain validation is delegated to the platform's native
/// verifier (Security.framework on macOS/iOS, `SChannel` on Windows,
/// system roots on Linux) so the system trust store — including any
/// user-installed mkcert root in the iOS simulator keychain — is
/// honoured. Replaces the previous `rustls-tls-native-roots` approach
/// which silently failed on iOS targets.
///
/// wasm32: the browser handles TLS inside `fetch`; reqwest's wasm
/// backend wraps `fetch` directly. Connect/read timeouts are not
/// configurable from the reqwest wasm builder (the browser's own
/// timeout policy applies), so this path is just `Client::new()`.
#[cfg(not(target_arch = "wasm32"))]
fn http_client() -> Result<reqwest::Client, StorageError> {
    // `rustls-platform-verifier` builds a `ClientConfig` against the process
    // default `CryptoProvider`. Install ring exactly once before the first
    // call. `install_default` returns `Err` if a provider is already
    // installed, which is fine — we just need *some* provider available.
    static PROVIDER_INSTALL: OnceLock<()> = OnceLock::new();
    PROVIDER_INSTALL.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });

    let tls_config = rustls::ClientConfig::with_platform_verifier()
        .map_err(|e| StorageError::Backend(format!("rustls platform verifier: {e}")))?;

    // Fail fast on unreachable endpoints rather than spinning forever.
    // The iOS sim debug session of 2026-05-16 burned hours on a stale
    // Keychain session pointing at `http://127.0.0.1:3999`, which was
    // listening but unresponsive on `/health`. Without these timeouts
    // the app hung silently with no UI signal. Connect timeout covers
    // the TCP handshake; the overall request timeout bounds any single
    // HTTP exchange. Values chosen to be generous for slow mobile
    // networks while still surfacing real outages in < 30s.
    reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| StorageError::Backend(format!("reqwest client build: {e}")))
}

/// wasm32: browser handles TLS inside `fetch`. No timeout knobs exist
/// in the reqwest wasm builder — the browser's own policy applies.
// The `Result` wrapper is infallible on wasm32 but kept deliberately
// uniform with the `cfg(not(wasm32))` variant above so call sites
// (`http_client()?`) stay identical across targets — no cfg-gated
// callers. Hence the narrow allow rather than dropping the `Result`.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::unnecessary_wraps)]
fn http_client() -> Result<reqwest::Client, StorageError> {
    Ok(reqwest::Client::new())
}

#[derive(Clone)]
pub struct SupabaseStorage {
    /// REST endpoint base (e.g. `https://xxx.supabase.co/rest/v1`).
    pub(crate) rest_url: String,
    pub(crate) anon_key: String,
    /// Native: `Arc<dyn TokenProvider + Send + Sync>` so the wallet can
    /// share the provider across tokio worker threads. wasm: drop the
    /// `Send + Sync` bound because the browser is single-threaded and
    /// `TokenProvider` itself is `?Send` on wasm.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) tokens: Arc<dyn TokenProvider + Send + Sync>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) tokens: Arc<dyn TokenProvider>,
    /// Native: reqwest wired to the platform-native TLS verifier.
    /// wasm: reqwest's `fetch`-backed client.
    /// Either way, shared across RPC/select calls so the connection
    /// pool (native) / browser keep-alive (wasm) is reused.
    pub(crate) http: reqwest::Client,
}

impl std::fmt::Debug for SupabaseStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupabaseStorage")
            .field("rest_url", &self.rest_url)
            .field("anon_key", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl SupabaseStorage {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(
        config: SupabaseStorageConfig,
        tokens: Arc<dyn TokenProvider + Send + Sync>,
    ) -> Result<Self, StorageError> {
        // Normalize `<base>` -> `<base>/rest/v1`. Strip a trailing slash if any.
        let base = config.url.trim_end_matches('/');
        let rest_url = format!("{base}/rest/v1");
        let http = http_client()?;
        Ok(Self {
            rest_url,
            anon_key: config.anon_key,
            tokens,
            http,
        })
    }

    /// wasm: identical surface to the native `new`, but the token
    /// provider does NOT carry `Send + Sync` — `TokenProvider` is
    /// `?Send`-gated on wasm and the browser is single-threaded.
    #[cfg(target_arch = "wasm32")]
    pub fn new(
        config: SupabaseStorageConfig,
        tokens: Arc<dyn TokenProvider>,
    ) -> Result<Self, StorageError> {
        let base = config.url.trim_end_matches('/');
        let rest_url = format!("{base}/rest/v1");
        let http = http_client()?;
        Ok(Self {
            rest_url,
            anon_key: config.anon_key,
            tokens,
            http,
        })
    }

    /// Supabase project base URL with the `/rest/v1` suffix stripped
    /// (e.g. `https://xxx.supabase.co`). The realtime client's
    /// `build_connect_url` appends `/realtime/v1/websocket`, so it needs
    /// the project root, not the REST endpoint. Additive read-only
    /// getter for the slice-10 realtime FFI bridge.
    #[must_use]
    pub fn supabase_base_url(&self) -> String {
        self.rest_url
            .trim_end_matches('/')
            .strip_suffix("/rest/v1")
            .unwrap_or(&self.rest_url)
            .to_string()
    }

    /// The Supabase anon (public) API key. Realtime joins it as the
    /// socket `apikey` query param (the per-user JWT is the channel
    /// `access_token`, supplied separately). Additive read-only getter.
    #[must_use]
    pub fn anon_key_for_realtime(&self) -> String {
        self.anon_key.clone()
    }

    /// Build a `postgrest::Postgrest` instance scoped to the `wallet` schema
    /// with per-request auth headers. Called once per RPC/select. Reuses the
    /// shared `reqwest::Client` so platform-verifier TLS settings are applied
    /// to every request.
    pub(crate) async fn authenticated_client(&self) -> Result<postgrest::Postgrest, StorageError> {
        let jwt = self
            .tokens
            .get_jwt()
            .await
            .map_err(|e| StorageError::Backend(format!("token provider: {e}")))?;
        tracing::info!(
            target: "agicash_storage_supabase::client",
            jwt_sub = %jwt_sub_for_log(&jwt),
            jwt_len = jwt.len(),
            "authenticated_client: jwt issued"
        );
        let client = postgrest::Postgrest::new_with_client(&self.rest_url, self.http.clone())
            .schema(WALLET_SCHEMA)
            .insert_header("apikey", &self.anon_key)
            .insert_header("Authorization", format!("Bearer {jwt}"));
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_traits::AuthError;
    use async_trait::async_trait;

    struct StubTokens;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl TokenProvider for StubTokens {
        async fn get_jwt(&self) -> Result<String, AuthError> {
            Ok("stub.jwt.token".into())
        }
    }

    #[test]
    fn constructor_normalizes_trailing_slash() {
        let cfg = SupabaseStorageConfig {
            url: "https://test.supabase.co/".into(),
            anon_key: "anon".into(),
        };
        let s = SupabaseStorage::new(cfg, Arc::new(StubTokens)).unwrap();
        assert_eq!(s.rest_url, "https://test.supabase.co/rest/v1");
    }

    #[tokio::test]
    async fn authenticated_client_calls_token_provider() {
        let cfg = SupabaseStorageConfig {
            url: "https://test.supabase.co".into(),
            anon_key: "anon-key".into(),
        };
        let s = SupabaseStorage::new(cfg, Arc::new(StubTokens)).unwrap();
        let _client = s.authenticated_client().await.unwrap();
    }

    /// Regression guard for the iOS sim hang of 2026-05-16: a stale
    /// Keychain session pointed `wallet.setSession()` at a local
    /// endpoint that accepted the TCP handshake but never answered.
    /// Without `connect_timeout`/`timeout` the call hangs forever.
    /// We use TEST-NET-1 (`192.0.2.1`), an IANA-reserved
    /// documentation/non-routable block, so the request *cannot*
    /// complete a connection in any environment — the connect timeout
    /// is the only thing that can return us.
    ///
    /// The 5s connect-timeout target leaves a 2s slack budget to
    /// absorb scheduler/runtime jitter on a loaded CI box.
    #[tokio::test]
    async fn connect_timeout_fires_within_budget() {
        let client = http_client().expect("client builds");
        let start = std::time::Instant::now();
        let result = client.get("http://192.0.2.1:80").send().await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected connect error, got: {result:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(7),
            "connect_timeout did not fire within budget: elapsed={elapsed:?}"
        );
    }
}
