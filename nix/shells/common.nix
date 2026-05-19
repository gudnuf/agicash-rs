# Shared inputs for all agicash dev shells.
#
# Returns an attrset of packages and toolchain handles every shell can use.
# Each shell extends this with its platform-specific bits.
{ pkgs, lib }:

let
  # Pin to match crates/rust-toolchain.toml (slice 4: 1.88.0).
  #
  # Build the toolchain straight from the rustup toolchain file so EVERY
  # component (rustc, cargo, clippy, rustfmt) comes from the SAME pinned
  # 1.88.0 channel manifest. The previous form —
  # `pkgs.rust-bin.stable."1.88.0".default.override { extensions = [...]; }`
  # — resolved each extension from rust-overlay's current component set, so
  # `clippy` drifted to a 1.95-era build (clippy 0.1.95 / 2026-04-14) while
  # rustc stayed 1.88.0. That 10-month mismatch red-baselined
  # `cargo clippy --workspace --all-targets -- -D warnings` (and `aclippy`)
  # at the clean tree. `fromRustupToolchainFile` builds the whole channel
  # coherently, guaranteeing `clippy 0.1.88` matches `rustc 1.88.0`.
  #
  # The toolchain file pins channel + rustfmt/clippy + the wasm/android
  # cross targets. We `.override` to additionally bundle the iOS targets
  # and rust-src/rust-analyzer the dev shells expect; the override extends
  # the SAME 1.88.0 channel, so clippy stays matched.
  rustToolchain =
    (pkgs.rust-bin.fromRustupToolchainFile ../../crates/rust-toolchain.toml).override {
      extensions = [ "rustfmt" "clippy" "rust-src" "rust-analyzer" ];
      targets = [
        "wasm32-unknown-unknown"
        "aarch64-apple-ios"
        "aarch64-apple-ios-sim"
        "aarch64-linux-android"
        "armv7-linux-androideabi"
        "x86_64-linux-android"
      ];
    };

  # Devshell-isolated CARGO_HOME.
  #
  # cargo resolves an external subcommand (`cargo clippy`, `cargo fmt`, …)
  # from `$CARGO_HOME/bin` BEFORE falling back to $PATH. On this host
  # `~/.cargo/bin` is a rustup install: `cargo-clippy -> rustup`,
  # `clippy-driver -> rustup`. So even with the pinned 1.88.0 toolchain
  # first on PATH, `cargo clippy` dispatched through the rustup proxy to a
  # 1.95-era clippy (0.1.95 / 2026-04-14) — re-introducing the exact
  # rustc/clippy mismatch the toolchain pin fixes, and red-baselining
  # `aclippy` (which runs `cargo clippy`). Point CARGO_HOME at a
  # devshell-managed dir with a CLEAN bin/ (no rustup shims) so cargo's
  # subcommand lookup falls through to the pinned nix toolchain on PATH.
  # The expensive registry/git caches are symlinked back to the real
  # ~/.cargo so the 1.8G crate cache stays shared across worktrees (same
  # HOME-rooted, share-everything spirit as CARGO_TARGET_DIR/SCCACHE_DIR).
  cargoHomeHook = ''
    # Isolate CARGO_HOME from the host rustup install (see common.nix).
    export CARGO_HOME="''${AGICASH_CARGO_HOME:-$HOME/.cache/agicash-cargo-home}"
    mkdir -p "$CARGO_HOME"
    # Share the costly download caches with the real ~/.cargo; keep bin/
    # local and rustup-free so `cargo <subcmd>` resolves the pinned
    # toolchain, not ~/.cargo/bin/<subcmd> -> rustup.
    for _sub in registry git; do
      if [ -d "$HOME/.cargo/$_sub" ] && [ ! -e "$CARGO_HOME/$_sub" ]; then
        ln -s "$HOME/.cargo/$_sub" "$CARGO_HOME/$_sub"
      fi
    done
    unset _sub
  '';

  # generate-ssl-cert script (preserves the behaviour of the old devenv
  # `scripts.generate-ssl-cert.exec` hook). Wrapped as a Nix derivation so
  # it lands on PATH and so `mkcert` resolves to the flake-pinned version.
  generate-ssl-cert = pkgs.writeShellApplication {
    name = "generate-ssl-cert";
    runtimeInputs = [ pkgs.mkcert pkgs.nss.tools pkgs.openssl ];
    text = builtins.readFile ../../tools/dev/generate-ssl-cert.sh;
  };

  basePackages = [
    rustToolchain

    # General dev tools (survivors from devenv.nix).
    pkgs.git
    pkgs.gh
    pkgs.jq
    pkgs.curl
    pkgs.openssl
    pkgs.pkg-config
    pkgs.just

    # Local Supabase HTTPS + JWT chain.
    # mkcert + nss.tools install the local CA into Firefox/Chrome trust
    # stores (see memory project_opensecret_local_stack.md).
    pkgs.mkcert
    pkgs.nss.tools

    # Supabase CLI — `supabase start` runs the local postgres/auth/storage
    # stack used by the rust storage crate + opensecret JWT chain.
    pkgs.supabase-cli

    # Build acceleration — shared cargo target + sccache wrapper (per memory
    # feedback_dev_loop_cache.md: 10-30× warm-cache speedup across worktrees).
    pkgs.sccache

    # generate-ssl-cert script (defined above).
    generate-ssl-cert
  ];
in
{
  inherit rustToolchain basePackages cargoHomeHook;
}
