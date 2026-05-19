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
use std::sync::atomic::Ordering;
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

/// Monotonic counter making each real-proof fund derive a unique NUT-13
/// seed, so blinded messages never collide across funds against the
/// shared mint (a replayed blinded message is rejected by the real mint).
static FUND_SEED_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);


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
    /// **Probe-first**, mirroring the proven `scripts/cdk-mint-e2e.sh`
    /// `is_up()` reuse model + the enclave attach-or-spawn pattern: if a
    /// cdk-mintd already answers `GET http://127.0.0.1:<port>/v1/info`
    /// 200 (the fixed `cdk-mint-e2e.sh` default port 8087, or
    /// `$CDK_MINT_PORT`), ATTACH to it — do NOT spawn, do NOT kill on
    /// drop (it is a standing reusable service; this also prevents
    /// process accumulation when the shared singleton is held in a
    /// never-dropped `static`). Otherwise resolve the cached binary,
    /// write the script's exact `FakeWallet` TOML into a tempdir, spawn,
    /// and poll readiness. Errors clearly if the binary isn't cached
    /// (does NOT auto-`cargo install` inside a test — the operator runs
    /// `bash scripts/cdk-mint-e2e.sh start` once).
    pub async fn start() -> Result<Self, String> {
        let listen_host = "127.0.0.1";
        // Fixed port = the proven `cdk-mint-e2e.sh` default (overridable),
        // so re-runs reuse a standing mint instead of accumulating one
        // per run on an ever-incrementing port.
        let port: u16 = std::env::var("CDK_MINT_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8087);
        let url = format!("http://{listen_host}:{port}");
        let info_url = format!("{url}/v1/info");

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| format!("MintProcess: reqwest client build failed: {e}"))?;

        // Probe-first: attach to a healthy standing mint (fast path,
        // never killed — the standing-service reuse model).
        if let Ok(resp) = http.get(&info_url).send().await {
            if resp.status().is_success() {
                eprintln!(
                    "[harness] cdk-mintd: ATTACHED to standing {url} \
                     (/v1/info 200) — reuse fast path, will NOT be killed"
                );
                // A throwaway tempdir keeps the field type uniform; it is
                // unused (we attached, not spawned) and auto-cleans.
                let state_dir = tempfile::tempdir().map_err(|e| {
                    format!("MintProcess: tempdir (attach placeholder): {e}")
                })?;
                let state_dir_path = state_dir.path().to_path_buf();
                return Ok(Self {
                    child: None, // attached → Drop must NOT kill it
                    url,
                    _state_dir: state_dir,
                    state_dir_path,
                });
            }
        }

        // Cold spawn.
        let bin = resolve_cdk_mintd_bin()?;
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

        // Readiness: GET {url}/v1/info → 200. A cold sqlite create +
        // keyset generation can exceed the shell script's 30s window on a
        // loaded host; widen to ≤120 × 0.5 s (60s).
        for attempt in 0..120u32 {
            if let Ok(resp) = http.get(&info_url).send().await {
                if resp.status().is_success() {
                    eprintln!(
                        "[harness] cdk-mintd: SPAWNED + READY at {url} \
                         (/v1/info 200 after {} probe(s))",
                        attempt + 1
                    );
                    return Ok(this);
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(format!(
            "MintProcess: cdk-mintd at {url} did not answer /v1/info 200 within 60s"
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
        // ATTACHED (child: None) → a standing reusable mint; NEVER kill
        // it (mirrors the enclave attach contract + the shell script's
        // reuse model). Only tear down a mint THIS handle spawned.
        let Some(mut child) = self.child.take() else {
            eprintln!(
                "[harness] cdk-mintd: leaving ATTACHED standing instance {} \
                 running (not harness-owned)",
                self.url
            );
            return;
        };
        // kill_on_drop handles the tokio child; belt-and-suspenders pkill
        // scoped to THIS handle's tempdir matches the shell script's
        // teardown (covers a re-exec'd grandchild). The TempDir
        // auto-cleans the sqlite db.
        let _ = child.start_kill();
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
// Shared long-lived services (one mint + one enclave per test BINARY) —
// Task 5
// ---------------------------------------------------------------------------

/// The long-lived processes, brought up ONCE per test binary and reused
/// by every `ServiceHarness::up()`. This mirrors the proven
/// `scripts/cdk-mint-e2e.sh` model (one mint, reused — `is_up()` short-
/// circuits a respawn) and the enclave attach-once fast path. Respawning
/// `cdk-mintd` per test was both slow (cold sqlite + keyset gen) and
/// flaky (port-release lag); a single shared mint is deterministic
/// (fixed mnemonic → stable keysets) and matches production's
/// one-mint-many-flows shape.
struct SharedServices {
    mint: MintProcess,
    // `EnclaveProcess` carries the attach/cold disposition + (only when
    // cold-spawned) the child to tear down at binary exit.
    enclave: EnclaveProcess,
}

// Held for the lifetime of the test binary. `tokio::sync::OnceCell`
// because bring-up is async (probes/seed/spawn). The processes' `Drop`
// runs at process exit (the cell is never cleared) — mint killed, an
// attached enclave deliberately left running.
static SHARED: tokio::sync::OnceCell<SharedServices> = tokio::sync::OnceCell::const_new();

async fn shared_services() -> Result<&'static SharedServices, String> {
    SHARED
        .get_or_try_init(|| async {
            // Order per the plan: assert pg (inside enclave.up) → enclave
            // (attach/spawn) → seed JWT (idempotent) → spawn the single
            // shared cdk-mintd.
            let enclave = EnclaveProcess::up().await?;
            seed_jwt_secret().await?;
            let mint = MintProcess::start().await?;
            eprintln!(
                "[harness] SHARED services UP (once per test binary) — \
                 enclave={:?} mint={} (no docker invoked)",
                enclave.disposition(),
                mint.url()
            );
            Ok(SharedServices { mint, enclave })
        })
        .await
}

// ---------------------------------------------------------------------------
// ServiceHarness — Task 5
// ---------------------------------------------------------------------------

/// The Tier 2 fixture. `up()` is cheap: it lazily brings the long-lived
/// real services online ONCE per test binary (assert pg → enclave
/// attach/spawn → seed JWT → spawn one cdk-mintd) and then, per call,
/// composes a **fresh** real `Arc<WalletClient>` — a fresh
/// `OpenSecretAuthClient` (so each test does its own real guest-auth /
/// JWT chain) over fresh in-memory storages, pointed at the shared real
/// mint. The shared processes live until process exit; only the
/// per-test `RealWallet` is dropped between tests.
///
/// Run Tier 2 tests `--test-threads=1` (mirrors the existing
/// `cdk_mint_money_flows` gate; the shared mint binds a fixed port).
#[derive(Debug)]
pub struct ServiceHarness {
    /// A fresh composed real wallet + its owned in-memory stores.
    wallet: super::wallet::RealWallet,
    mint_url: String,
    enclave_disposition: EnclaveDisposition,
}

impl ServiceHarness {
    /// Get (or lazily bring up once) the shared real services, then
    /// compose a fresh real wallet for this test.
    pub async fn up() -> Result<Self, String> {
        let shared = shared_services().await?;
        let wallet = super::wallet::RealWallet::compose(
            shared.enclave.url(),
            ENCLAVE_CLIENT_ID,
            shared.mint.url(),
        )?;
        eprintln!(
            "[harness] ServiceHarness::up — fresh wallet on shared \
             enclave={:?} mint={} (no docker invoked)",
            shared.enclave.disposition(),
            shared.mint.url()
        );
        Ok(Self {
            wallet,
            mint_url: shared.mint.url().to_string(),
            enclave_disposition: shared.enclave.disposition(),
        })
    }

    /// The composed real `Arc<WalletClient>` (real auth+mint, in-mem
    /// storage) plus owned handles to the in-memory stores.
    #[must_use]
    pub fn wallet(&self) -> &super::wallet::RealWallet {
        &self.wallet
    }

    /// The shared mint's base URL (use as the account `mint_url`).
    #[must_use]
    pub fn mint_url(&self) -> &str {
        &self.mint_url
    }

    /// Whether the enclave was attached (fast) or cold-spawned (slow).
    #[must_use]
    pub fn enclave_disposition(&self) -> EnclaveDisposition {
        self.enclave_disposition
    }

    /// Mint `amount` sat of **genuinely real** proofs from the shared
    /// real mint (real NUT-04 mint-quote → `FakeWallet` auto-settles →
    /// real `post_mint` → real `construct_proofs` crypto) and fund the
    /// account's in-memory send storage with them.
    ///
    /// WHY a helper and not the `WalletClient::complete_receive_lightning`
    /// facade path: the four in-memory fake storages are **independent**
    /// (the declared Tier 2 fake seam — the docker-wedge constraint). In
    /// the real Supabase backend they are views over one unified
    /// `wallet.cashu_*` schema, so a NUT-04-minted proof is spendable by
    /// `send_token`/`balance`; the in-memory fakes do NOT cross-populate
    /// (mint-quote storage ≠ send storage). Funding the **send** storage
    /// (which `compute_cashu_balance`/`send_token`/`begin_send_lightning`
    /// read) with proofs minted by the **real** protocol against the
    /// **real** mint keeps every cryptographic + wire link real; only the
    /// proofs' placement uses the storage seam that is fake by design.
    /// The real NUT-04 *facade* path is still asserted directly by
    /// `lightning_mint_quote_receive_credits_balance` /
    /// `add_mint_then_token_send_receive_round_trips` via
    /// `quote/poll/complete_receive_lightning`.
    pub async fn fund_account_with_real_proofs(
        &self,
        account: &agicash_domain::Account,
        amount: u64,
    ) -> Result<(), String> {
        let proofs = mint_real_proofs(&self.mint_url, amount).await?;
        self.wallet
            .send_storage()
            .fund_account(account.id, token_proofs(&proofs));
        Ok(())
    }

    /// Like [`Self::fund_account_with_real_proofs`] but also returns the
    /// **real `cdk::Proof`s** that were funded, so a Tier 2 test can
    /// assert the no-double-pay invariant at the **protocol level**
    /// (NUT-07 `post_check_state` against the real mint) — the
    /// storage-independent, hermetic-impossible assertion the proven
    /// `cdk_mint_money_flows.rs` P0 driver uses, lifted to the facade.
    pub async fn fund_account_returning_proofs(
        &self,
        account: &agicash_domain::Account,
        amount: u64,
    ) -> Result<Vec<cdk::nuts::Proof>, String> {
        let proofs = mint_real_proofs(&self.mint_url, amount).await?;
        self.wallet
            .send_storage()
            .fund_account(account.id, token_proofs(&proofs));
        Ok(proofs)
    }

    /// Protocol-level no-double-pay assertion: every one of `proofs` must
    /// report SPENT at the real mint via NUT-07 `post_check_state`. After
    /// a melt of these inputs reconciled to PAID, a SPENT result proves
    /// the Lightning payment happened exactly once and a replay is
    /// protocol-impossible (the real mint would reject a second
    /// `post_melt` of spent inputs). Independent of the in-memory storage
    /// (the fake seam) — it queries the real mint directly.
    pub async fn assert_all_proofs_spent_at_mint(
        &self,
        proofs: &[cdk::nuts::Proof],
    ) -> Result<(), String> {
        use std::str::FromStr;

        use cdk::mint_url::MintUrl;
        use cdk::wallet::{HttpClient, MintConnector};

        let url = MintUrl::from_str(&self.mint_url)
            .map_err(|e| format!("mint url {}: {e}", self.mint_url))?;
        let client = HttpClient::new(url, None);
        let ys = proofs
            .iter()
            .map(cdk::nuts::Proof::y)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("proof.y(): {e}"))?;
        let states = client
            .post_check_state(cdk::nuts::CheckStateRequest { ys })
            .await
            .map_err(|e| format!("post_check_state: {e}"))?;
        if states
            .states
            .iter()
            .all(|s| s.state == cdk::nuts::State::Spent)
        {
            Ok(())
        } else {
            Err(format!(
                "no-double-pay invariant VIOLATED: not all melt-input \
                 proofs are SPENT at the mint — states={:?}",
                states.states.iter().map(|s| s.state).collect::<Vec<_>>()
            ))
        }
    }
}

/// Map real `cdk::Proof`s into `agicash_cashu::TokenProof`s for
/// `InMemorySendSwapStorage::fund_account`.
fn token_proofs(proofs: &[cdk::nuts::Proof]) -> Vec<agicash_cashu::TokenProof> {
    proofs
        .iter()
        .map(|p| agicash_cashu::TokenProof {
            id: p.keyset_id.to_string(),
            amount: u64::from(p.amount),
            secret: p.secret.to_string(),
            c: p.c.to_hex(),
            dleq: None,
            witness: None,
        })
        .collect()
}

/// Mint `amount` sat of real proofs from `mint_url` via a raw
/// `cdk::wallet::HttpClient` / `MintConnector` — the exact proven
/// `mint_proofs` recipe from `crates/agicash-cashu/tests/
/// cdk_mint_money_flows.rs` (real NUT-04 mint-quote, `FakeWallet`
/// auto-settle, real `post_mint`, real `construct_proofs`). Returns the
/// real `cdk::Proof`s (map to `TokenProof` via [`token_proofs`] for
/// `fund_account`; keep the raw form for protocol-level NUT-07 checks).
async fn mint_real_proofs(
    mint_url: &str,
    amount: u64,
) -> Result<Vec<cdk::nuts::Proof>, String> {
    use std::str::FromStr;

    use cdk::amount::{FeeAndAmounts, SplitTarget};
    use cdk::dhke::construct_proofs;
    use cdk::mint_url::MintUrl;
    use cdk::nuts::{CurrencyUnit, MintQuoteBolt11Request, MintRequest, PaymentMethod, PreMintSecrets};
    use cdk::wallet::{HttpClient, MintConnector};
    use cdk::Amount;

    let url = MintUrl::from_str(mint_url).map_err(|e| format!("mint url {mint_url}: {e}"))?;
    let client = HttpClient::new(url, None);

    let keysets = client
        .get_mint_keysets()
        .await
        .map_err(|e| format!("get_mint_keysets: {e}"))?;
    let active = keysets
        .keysets
        .iter()
        .find(|k| k.unit == CurrencyUnit::Sat && k.active)
        .ok_or("no active sat keyset on the shared mint")?
        .clone();
    let keyset_id = active.id;

    let quote = client
        .post_mint_quote(MintQuoteBolt11Request {
            amount: Amount::from(amount),
            unit: CurrencyUnit::Sat,
            description: Some("agicash tier2 real-proof fund".into()),
            pubkey: None,
        })
        .await
        .map_err(|e| format!("post_mint_quote: {e}"))?;

    // FakeWallet (min/max_delay_time=0) auto-settles instantly; poll
    // defensively.
    let mut paid = false;
    for _ in 0..40 {
        let st = client
            .get_mint_quote_status(&quote.quote.to_string())
            .await
            .map_err(|e| format!("get_mint_quote_status: {e}"))?;
        if matches!(st.state, cdk::nuts::nut23::QuoteState::Paid) {
            paid = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !paid {
        return Err("shared FakeWallet mint did not auto-pay the funding quote".into());
    }

    // Run-unique seed so blinded messages never collide across funds.
    let mut seed = [0u8; 64];
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let c = u128::from(FUND_SEED_CTR.fetch_add(1, Ordering::SeqCst));
        let mut x = n ^ (c << 64) ^ 0x9E37_79B9_7F4A_7C15;
        for chunk in seed.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = x.to_le_bytes();
            for (i, slot) in chunk.iter_mut().enumerate() {
                *slot = b[i];
            }
        }
    }

    let fee_and_amounts = FeeAndAmounts::from((
        active.input_fee_ppk,
        (0..32).map(|i| 1u64 << i).collect::<Vec<_>>(),
    ));
    let pre_mint = PreMintSecrets::from_seed(
        keyset_id,
        0,
        &seed,
        Amount::from(amount),
        &SplitTarget::None,
        &fee_and_amounts,
    )
    .map_err(|e| format!("PreMintSecrets::from_seed: {e}"))?;

    let resp = client
        .post_mint(
            &PaymentMethod::BOLT11,
            MintRequest {
                quote: quote.quote.to_string(),
                outputs: pre_mint.blinded_messages(),
                signature: None,
            },
        )
        .await
        .map_err(|e| format!("post_mint: {e}"))?;

    let keyset = client
        .get_mint_keyset(keyset_id)
        .await
        .map_err(|e| format!("get_mint_keyset: {e}"))?;
    let proofs = construct_proofs(resp.signatures, pre_mint.rs(), pre_mint.secrets(), &keyset.keys)
        .map_err(|e| format!("construct_proofs: {e}"))?;

    Ok(proofs)
}
