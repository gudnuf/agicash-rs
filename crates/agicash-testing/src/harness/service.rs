//! Service-lifecycle primitives for the Tier 2 e2e harness.
//!
//! Each primitive encapsulates an already-proven operator recipe (it
//! does NOT reinvent the bring-up):
//!
//! - [`MintProcess`] — `scripts/cdk-mint-e2e.sh` in Rust: resolve the
//!   cached `cdk-mintd` binary, write the script's exact `FakeWallet`
//!   TOML into a tempdir, spawn it, poll `/v1/info`, kill-on-drop.
//! - [`EnclaveProcess`] — `project_opensecret_local_stack` +
//!   `reference_jwt_chain_recipe`: assert the standing nix-native
//!   postgres `:5432`, probe-first the `OpenSecret` enclave on `:3999`
//!   (attach if healthy — NEVER kill an attached process; cold-spawn
//!   only if down, a multi-minute build flagged clearly), seed the
//!   idempotent `THIRD_PARTY_JWT_SECRET`.
//! - [`ServiceHarness`] — composes the two into a real
//!   `Arc<WalletClient>` (real auth + real mint + in-memory storage)
//!   with RAII teardown of only harness-spawned processes.
//!
//! **No docker anywhere.** cdk-mintd is a plain cached binary, the
//! enclave is a host `cargo run` process, postgres is the standing
//! nix-native instance. The harness never invokes `docker`.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use tokio::process::{Child, Command};

// ---------------------------------------------------------------------------
// Shared constants — lifted verbatim from the proven recipes.
// ---------------------------------------------------------------------------

/// Standing nix-native postgres (the documented docker-wedge bypass —
/// `feedback_docker_wedge_bypass`). The harness asserts this; it NEVER
/// manages it and NEVER falls back to docker.
const PG_ADDR: &str = "127.0.0.1:5432";

/// The standing `OpenSecret` enclave bind (source-patched `:3000` → `:3999`,
/// `project_opensecret_local_stack`).
const ENCLAVE_URL: &str = "http://127.0.0.1:3999";

/// The enclave health endpoint that answers 200 when it is up (probed
/// 2026-05-19: `/health-check` → 200, `/health` → 404).
const ENCLAVE_HEALTH_PATH: &str = "/health-check";

/// Pre-seeded project (`project_opensecret_local_stack`): org `OpenSecret`
/// id=1, project `Maple` `client_id` below, id=1.
const ENCLAVE_CLIENT_ID: &str = "ba5a14b5-d915-47b1-b7b1-afda52bc5fc6";

/// The fixed JWT secret aligned across the three places
/// (`reference_jwt_chain_recipe`); only the Supabase-RPC link actually
/// needs it (Tier 2 fakes storage) but seeding is cheap + idempotent so a
/// future real-Supabase Tier 3 is one seam swap away.
const JWT_SECRET: &str = "super-secret-jwt-token-with-at-least-32-characters-long";

/// Per-process listen-port allocator so a future parallel run can't
/// collide on the cdk-mintd port. Tier 2 tests still run
/// `--test-threads=1` (the spawned mint binds a fixed port per process);
/// this base is the `scripts/cdk-mint-e2e.sh` default.
static MINT_PORT: AtomicU16 = AtomicU16::new(8087);

fn next_mint_port() -> u16 {
    MINT_PORT.fetch_add(1, Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// MintProcess — Task 4
// ---------------------------------------------------------------------------

/// A harness-spawned `cdk-mintd` (`FakeWallet` LN backend) with kill-on-drop.
///
/// Encapsulates `scripts/cdk-mint-e2e.sh` exactly: same binary-resolution
/// order, the same `FakeWallet` TOML (instant deterministic settle, fixed
/// test mnemonic → stable keysets), `/v1/info` readiness probe. The
/// sqlite db + keys live in an auto-cleaned `tempfile::TempDir`.
pub struct MintProcess {
    child: Option<Child>,
    url: String,
    /// Held for RAII auto-clean of the sqlite/state dir; never read.
    _state_dir: tempfile::TempDir,
    state_dir_path: PathBuf,
}

// `Child`/`TempDir` are not `Debug`, so a derive is impossible and this
// hand-rolled impl intentionally omits them — represented by the
// `running` flag instead.
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for MintProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintProcess")
            .field("url", &self.url)
            .field("running", &self.child.is_some())
            .field("state_dir", &self.state_dir_path)
            .finish()
    }
}

impl MintProcess {
    /// Resolve the cached `cdk-mintd`, write the `FakeWallet` config into a
    /// fresh tempdir, spawn it, and poll `GET {url}/v1/info` until 200
    /// (≤60 × 0.5 s). Errors clearly if the binary isn't cached (does NOT
    /// auto-`cargo install` inside a test — too slow/networked; the
    /// operator runs `bash scripts/cdk-mint-e2e.sh start` once).
    pub async fn start() -> Result<Self, String> {
        let bin = resolve_cdk_mintd_bin()?;
        let port = next_mint_port();
        let listen_host = "127.0.0.1";
        let url = format!("http://{listen_host}:{port}");

        let state_dir = tempfile::tempdir()
            .map_err(|e| format!("MintProcess: tempdir for cdk-mintd state failed: {e}"))?;
        let state_dir_path = state_dir.path().to_path_buf();
        let cfg_path = state_dir_path.join("config.toml");
        std::fs::write(&cfg_path, fakewallet_config(&url, listen_host, port))
            .map_err(|e| format!("MintProcess: write cdk-mintd config failed: {e}"))?;

        eprintln!(
            "[harness] cdk-mintd: spawning {} -w {} (port {port}, no docker)",
            bin.display(),
            state_dir_path.display()
        );

        let child = Command::new(&bin)
            .arg("-w")
            .arg(&state_dir_path)
            .arg("--config")
            .arg(&cfg_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("MintProcess: spawn {} failed: {e}", bin.display()))?;

        let this = Self {
            child: Some(child),
            url: url.clone(),
            _state_dir: state_dir,
            state_dir_path,
        };

        // Readiness: GET {url}/v1/info → 200, ≤60 × 0.5 s (same window as
        // the shell script's `is_up` loop).
        let info_url = format!("{url}/v1/info");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("MintProcess: reqwest client build failed: {e}"))?;
        for attempt in 0..60u32 {
            if let Ok(resp) = http.get(&info_url).send().await {
                if resp.status().is_success() {
                    eprintln!(
                        "[harness] cdk-mintd: READY at {url} (/v1/info 200 after \
                         {} probe(s))",
                        attempt + 1
                    );
                    return Ok(this);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(format!(
            "MintProcess: cdk-mintd at {url} did not answer /v1/info 200 within 30s"
        ))
    }

    /// The base mint URL this instance listens on (use as the account's
    /// `details.mint_url`).
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for MintProcess {
    fn drop(&mut self) {
        // kill_on_drop handles the tokio child; belt-and-suspenders
        // pkill matches the shell script's teardown (covers a
        // re-exec'd grandchild). The TempDir auto-cleans the sqlite db.
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        let pat = format!("cdk-mintd -w {}", self.state_dir_path.display());
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(&pat)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        eprintln!(
            "[harness] cdk-mintd: torn down (spawned-by-harness) {}",
            self.url
        );
    }
}

/// Binary-resolution order, verbatim from `scripts/cdk-mint-e2e.sh`:
/// `$CDK_MINTD_BIN` → `~/.cache/cdk-mint-e2e/install/bin/cdk-mintd` →
/// `~/agicash/.claude/cdk-mintd-install/bin/cdk-mintd`. No auto-install.
fn resolve_cdk_mintd_bin() -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("CDK_MINTD_BIN") {
        if !explicit.is_empty() {
            candidates.push(PathBuf::from(explicit));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".cache/cdk-mint-e2e/install/bin/cdk-mintd"));
        candidates.push(home.join("agicash/.claude/cdk-mintd-install/bin/cdk-mintd"));
    }
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "cdk-mintd binary not found (looked: {}). Run \
         `bash scripts/cdk-mint-e2e.sh start` once to install/cache it \
         (the harness deliberately does NOT cargo-install inside a test).",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The `FakeWallet` `config.toml` — byte-for-byte the
/// `scripts/cdk-mint-e2e.sh` `write_config` heredoc (instant
/// deterministic settle: `min/max_delay_time = 0`; fixed test mnemonic →
/// stable keysets across restarts; sqlite engine; in-memory http cache).
fn fakewallet_config(mint_url: &str, listen_host: &str, listen_port: u16) -> String {
    format!(
        r#"[info]
url = "{mint_url}/"
listen_host = "{listen_host}"
listen_port = {listen_port}
mnemonic = "test test test test test test test test test test test junk"

[info.quote_ttl]
mint_ttl = 3600
melt_ttl = 600

[info.http_cache]
backend = "memory"
ttl = 60
tti = 60

[mint_management_rpc]
enabled = false

[mint_info]
name = "agicash-rs e2e fakewallet mint"

[database]
engine = "sqlite"

[ln]
ln_backend = "fakewallet"

[fake_wallet]
supported_units = ["sat"]
fee_percent = 0.0
reserve_fee_min = 0
min_delay_time = 0
max_delay_time = 0

[limits]
max_inputs = 1000
max_outputs = 1000
"#
    )
}

// ---------------------------------------------------------------------------
// EnclaveProcess — Task 5
// ---------------------------------------------------------------------------

/// Disposition of the `OpenSecret` enclave for a harness run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnclaveDisposition {
    /// A healthy standing enclave was found on `:3999` and attached to.
    /// The harness does NOT own it and will NEVER kill it on drop (it is
    /// the operator's standing local stack — the fast path).
    Attached,
    /// No enclave was up; the harness cold-spawned one (a multi-minute
    /// `cargo run --bin opensecret` build — the slow path, flagged
    /// clearly, NOT a hang). The harness owns + tears down this one.
    ColdSpawned,
}

/// The `OpenSecret` enclave for a harness run. Probe-first: attach to a
/// healthy standing instance (fast path, never killed); cold-spawn only
/// if down (slow path, owned + killed on drop). Asserts the standing
/// nix-native postgres `:5432` first (fail-fast with a revive pointer).
pub struct EnclaveProcess {
    disposition: EnclaveDisposition,
    /// `Some` only when [`EnclaveDisposition::ColdSpawned`].
    child: Option<Child>,
    url: String,
}

impl std::fmt::Debug for EnclaveProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnclaveProcess")
            .field("disposition", &self.disposition)
            .field("url", &self.url)
            .field("harness_owned", &self.child.is_some())
            .finish()
    }
}

impl EnclaveProcess {
    /// Assert nix-pg `:5432`, then probe the enclave on `:3999`:
    /// attach-if-healthy else cold-spawn `cd ~/opensecret && nix develop
    /// -c cargo run --bin opensecret`.
    pub async fn up() -> Result<Self, String> {
        assert_nix_pg().await?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| format!("EnclaveProcess: reqwest build failed: {e}"))?;
        let health_url = format!("{ENCLAVE_URL}{ENCLAVE_HEALTH_PATH}");

        if let Ok(resp) = http.get(&health_url).send().await {
            if resp.status().is_success() {
                eprintln!(
                    "[harness] enclave: ATTACHED to standing {ENCLAVE_URL} \
                     ({ENCLAVE_HEALTH_PATH} 200) — fast path, will NOT be killed"
                );
                return Ok(Self {
                    disposition: EnclaveDisposition::Attached,
                    child: None,
                    url: ENCLAVE_URL.to_string(),
                });
            }
        }

        // Cold spawn — the SLOW path (a fresh OpenSecret Rust build can
        // take minutes). Flag it loudly so a watcher never concludes
        // "hung".
        eprintln!(
            "[harness] enclave: no healthy instance on {ENCLAVE_URL}; \
             COLD-SPAWNING `cd ~/opensecret && nix develop -c cargo run \
             --bin opensecret` — this is the SLOW path (multi-minute Rust \
             build on a cold cache), NOT a hang."
        );
        let opensecret_dir = home_path("opensecret")?;
        if !opensecret_dir.is_dir() {
            return Err(format!(
                "EnclaveProcess: ~/opensecret not found at {} — a cold \
                 enclave spawn needs the source checkout (see \
                 project_opensecret_local_stack)",
                opensecret_dir.display()
            ));
        }
        let child = Command::new("nix")
            .current_dir(&opensecret_dir)
            .arg("develop")
            .arg("-c")
            .arg("cargo")
            .arg("run")
            .arg("--bin")
            .arg("opensecret")
            .env("APP_MODE", "local")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("EnclaveProcess: cold spawn failed: {e}"))?;

        // Generous readiness window for a cold compile (≤20 min, 2 s
        // interval). The fast path above means this only bites a truly
        // cold host.
        for attempt in 0..600u32 {
            if let Ok(resp) = http.get(&health_url).send().await {
                if resp.status().is_success() {
                    eprintln!(
                        "[harness] enclave: COLD-SPAWNED + READY at {ENCLAVE_URL} \
                         (after {} probe(s)) — harness owns + will tear it down",
                        attempt + 1
                    );
                    return Ok(Self {
                        disposition: EnclaveDisposition::ColdSpawned,
                        child: Some(child),
                        url: ENCLAVE_URL.to_string(),
                    });
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!(
            "EnclaveProcess: cold-spawned enclave did not become healthy \
             at {health_url} within ~20min"
        ))
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn disposition(&self) -> EnclaveDisposition {
        self.disposition
    }
}

impl Drop for EnclaveProcess {
    fn drop(&mut self) {
        match self.disposition {
            EnclaveDisposition::Attached => {
                // NEVER kill an attached standing instance — it is the
                // operator's local stack.
                eprintln!(
                    "[harness] enclave: leaving ATTACHED standing instance \
                     {} running (not harness-owned)",
                    self.url
                );
            }
            EnclaveDisposition::ColdSpawned => {
                if let Some(mut child) = self.child.take() {
                    let _ = child.start_kill();
                }
                eprintln!(
                    "[harness] enclave: torn down (cold-spawned, harness-owned) {}",
                    self.url
                );
            }
        }
    }
}

/// Assert the standing nix-native postgres on `:5432` is reachable.
/// Fail-fast with the revive pointer; NEVER attempt docker.
async fn assert_nix_pg() -> Result<(), String> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::TcpStream::connect(PG_ADDR),
    )
    .await
    {
        Ok(Ok(_stream)) => {
            eprintln!("[harness] postgres: standing nix-native {PG_ADDR} reachable (not managed by harness)");
            Ok(())
        }
        _ => Err(format!(
            "ServiceHarness: standing nix-native postgres NOT reachable at \
             {PG_ADDR}. Bring it up per project_opensecret_local_stack \
             (nix-native pg is the documented docker-wedge bypass — do NOT \
             use docker)."
        )),
    }
}

/// One-shot idempotent JWT-secret seed:
/// `cd ~/opensecret && SECRET=… PROJECT_ID=1 nix develop -c cargo run
/// --bin seed-project-secret` (`reference_jwt_chain_recipe`). Tolerates
/// "already seeded" (the bin UPSERTs); OS picks it up next request, no
/// restart. Only the Supabase-RPC link needs it (Tier 2 fakes storage) —
/// seeded anyway so a future real-Supabase Tier 3 is one seam swap away.
async fn seed_jwt_secret() -> Result<(), String> {
    let opensecret_dir = home_path("opensecret")?;
    if !opensecret_dir.is_dir() {
        // Non-fatal: Tier 2 correctness does NOT depend on the Supabase
        // JWT link (storage is faked in-memory). Warn + continue.
        eprintln!(
            "[harness] jwt-seed: ~/opensecret absent at {} — skipping \
             (Tier 2 storage is faked; the JWT link is only needed for a \
             future real-Supabase Tier 3)",
            opensecret_dir.display()
        );
        return Ok(());
    }
    eprintln!("[harness] jwt-seed: one-shot idempotent seed-project-secret (PROJECT_ID=1)");
    let out = Command::new("nix")
        .current_dir(&opensecret_dir)
        .arg("develop")
        .arg("-c")
        .arg("cargo")
        .arg("run")
        .arg("--bin")
        .arg("seed-project-secret")
        .env("SECRET", JWT_SECRET)
        .env("PROJECT_ID", "1")
        .output()
        .await
        .map_err(|e| format!("seed_jwt_secret: spawn failed: {e}"))?;
    if out.status.success() {
        eprintln!("[harness] jwt-seed: OK (idempotent UPSERT into org_project_secrets)");
        Ok(())
    } else {
        // Idempotent UPSERT: a non-zero exit is most likely "already
        // seeded" / a benign race. Surface but do not fail the harness —
        // Tier 2 correctness does not depend on this link.
        eprintln!(
            "[harness] jwt-seed: non-zero exit (tolerated — likely \
             already-seeded; Tier 2 storage is faked). stderr tail: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("")
        );
        Ok(())
    }
}

fn home_path(sub: &str) -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(sub))
        .ok_or_else(|| "HOME not set".to_string())
}

// ---------------------------------------------------------------------------
// ServiceHarness — Task 5
// ---------------------------------------------------------------------------

/// The RAII Tier 2 fixture. `up()` brings the real services online
/// (assert pg → enclave attach/spawn → seed JWT → spawn cdk-mintd) and
/// composes a real `Arc<WalletClient>` (real `OpenSecret` auth + real
/// `CdkCashuProvider` against the spawned mint + in-memory storage).
///
/// Field drop order matters: `wallet` first (drops the `Arc`s), then the
/// processes. `MintProcess`/`EnclaveProcess` own their own RAII teardown
/// (mint always killed; enclave killed ONLY if cold-spawned, never if
/// attached).
#[derive(Debug)]
pub struct ServiceHarness {
    /// The composed real wallet + owned in-memory stores for assertions.
    wallet: super::wallet::RealWallet,
    mint: MintProcess,
    enclave: EnclaveProcess,
}

impl ServiceHarness {
    /// Bring everything up and compose the real wallet. Run Tier 2 tests
    /// `--test-threads=1` (the spawned mint binds a fixed port per
    /// process; mirrors the existing `cdk_mint_money_flows` gate).
    pub async fn up() -> Result<Self, String> {
        // Order per the plan: assert pg (inside enclave.up) → enclave
        // (attach/spawn) → seed JWT → mint.
        let enclave = EnclaveProcess::up().await?;
        seed_jwt_secret().await?;
        let mint = MintProcess::start().await?;

        let wallet = super::wallet::RealWallet::compose(
            enclave.url(),
            ENCLAVE_CLIENT_ID,
            mint.url(),
        )?;

        eprintln!(
            "[harness] ServiceHarness UP — enclave={:?} mint={} (no docker invoked)",
            enclave.disposition(),
            mint.url()
        );
        Ok(Self {
            wallet,
            mint,
            enclave,
        })
    }

    /// The composed real `Arc<WalletClient>` (real auth+mint, in-mem
    /// storage) plus owned handles to the in-memory stores.
    #[must_use]
    pub fn wallet(&self) -> &super::wallet::RealWallet {
        &self.wallet
    }

    /// The spawned mint's base URL (use as the account `mint_url`).
    #[must_use]
    pub fn mint_url(&self) -> &str {
        self.mint.url()
    }

    /// Whether the enclave was attached (fast) or cold-spawned (slow).
    #[must_use]
    pub fn enclave_disposition(&self) -> EnclaveDisposition {
        self.enclave.disposition()
    }
}
