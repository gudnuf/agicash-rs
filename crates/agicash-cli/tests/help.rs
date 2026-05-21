use assert_cmd::Command;
use predicates::prelude::*;

/// Run `agicash <args...>` and return stdout as a `String`, asserting the
/// command exited successfully. Used by the help-tree snapshot tests.
fn help_text(args: &[&str]) -> String {
    let out = Command::cargo_bin("agicash")
        .unwrap()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).expect("help output is UTF-8")
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    Command::cargo_bin("agicash")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Agicash CLI"))
        .stdout(predicate::str::contains("Usage: agicash"));
}

#[test]
fn version_subcommand_prints_version_as_json() {
    let out = Command::cargo_bin("agicash")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("version stdout was not valid JSON ({e}): {s}"));
    assert_eq!(
        parsed.get("version").and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
    );
}

#[test]
fn json_flag_no_longer_recognized() {
    // `--json` was a parsed-but-unused flag; JSON is now the only output and
    // the flag has been removed. clap should reject it.
    Command::cargo_bin("agicash")
        .unwrap()
        .args(["--json", "version"])
        .assert()
        .failure();
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    Command::cargo_bin("agicash")
        .unwrap()
        .arg("nonsense-subcommand")
        .assert()
        .failure();
}

#[test]
fn auth_guest_help_lists_command() {
    Command::cargo_bin("agicash")
        .unwrap()
        .args(["auth", "guest", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("guest"));
}

#[test]
fn auth_login_help_requires_email_arg() {
    Command::cargo_bin("agicash")
        .unwrap()
        .args(["auth", "login", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("email"));
}

/// The help for `auth login` / `auth signup` must accurately describe
/// password input. The leaf-help template's whole value is that an agent
/// can trust it — a false claim is worse than no claim.
///
/// Pins the P0 fix: the pre-fix help carried the FALSE claim "The
/// password is read from stdin (never passed as an argument)" — the
/// password was NOT read from stdin, the bare `rpassword::prompt_password`
/// forced `/dev/tty`. The corrected help documents `--password-stdin`,
/// the `printf %s` pipe example, and the interactive-prompt path.
#[test]
fn auth_password_help_is_accurate_for_login_and_signup() {
    for sub in ["login", "signup"] {
        let text = help_text(&["auth", sub, "--help"]);

        // The false claim must be gone.
        assert!(
            !text.contains("password is read from stdin (never passed as an argument)"),
            "auth {sub} --help still carries the false stdin claim:\n{text}",
        );

        // It must document `--password-stdin` as the non-interactive path.
        assert!(
            text.contains("--password-stdin"),
            "auth {sub} --help must document --password-stdin:\n{text}",
        );

        // It must carry the concrete pipe example an agent copies.
        assert!(
            text.contains("printf %s")
                && text.contains(&format!(
                    "agicash auth {sub} alice@example.com --password-stdin"
                )),
            "auth {sub} --help must show the `printf %s | … --password-stdin` example:\n{text}",
        );

        // It must still describe the interactive (terminal-prompt) path
        // so a human is not told to pipe.
        assert!(
            text.to_lowercase().contains("interactive") && text.to_lowercase().contains("terminal"),
            "auth {sub} --help must still describe the interactive path:\n{text}",
        );

        // The new exit-2 condition is documented in the EXIT CODES block.
        assert!(
            text.contains("interactive-input-required"),
            "auth {sub} --help must document the interactive-input-required \
             exit-2 condition:\n{text}",
        );
    }
}

#[test]
fn account_help_lists_list_subcommand() {
    Command::cargo_bin("agicash")
        .unwrap()
        .args(["account", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"));
}

#[test]
fn account_list_help_works() {
    Command::cargo_bin("agicash")
        .unwrap()
        .args(["account", "list", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("List"));
}

#[test]
fn decode_help_states_it_is_offline() {
    // `decode` is the standout offline utility — its help must say so,
    // and it must document an `input` argument.
    let text = help_text(&["decode", "--help"]);
    assert!(
        text.to_lowercase().contains("offline"),
        "decode --help must state it is offline:\n{text}",
    );
    assert!(
        text.contains("input"),
        "decode --help must document the `input` argument:\n{text}",
    );
}

#[test]
fn mint_help_lists_both_add_and_list() {
    let text = help_text(&["mint", "--help"]);
    assert!(
        text.contains("add") && text.contains("list"),
        "mint --help should list both `add` and `list`:\n{text}",
    );
}

#[test]
fn account_help_lists_info_subcommand() {
    let text = help_text(&["account", "--help"]);
    assert!(
        text.contains("info"),
        "account --help should list the `info` subcommand:\n{text}",
    );
}

#[test]
fn account_list_without_session_exits_three_and_emits_json_error() {
    // "No session present" exit-code contract. Two paths exercise this test:
    //
    //   - On macOS dev machines the OS keyring is reachable, so the CLI picks
    //     `KeyringSessionStorage`; we pass a unique service id so it can't
    //     collide with a real session.
    //   - On Linux CI (no `dbus-daemon` + secret-service running), the
    //     keyring probe reports `BackendUnavailable` and the CLI falls
    //     through to `InMemorySessionStorage` (always available). The
    //     in-memory store starts empty, so `load()` returns `Ok(None)`.
    //
    // Either way the CLI must exit 3 with a `not-logged-in` JSON error.
    //
    // SUPABASE_URL etc. are stubbed so build_storage_deps succeeds; the
    // actual HTTP call is never made because the "not logged in" check fires
    // first on `load() -> None`.
    let pid = std::process::id();
    let service = format!("com.agicash.cli.test.{pid}.account-list");
    let out = Command::cargo_bin("agicash")
        .unwrap()
        .env("AGICASH_KEYRING_SERVICE", &service)
        .env("SUPABASE_URL", "https://test.invalid")
        .env("SUPABASE_ANON_KEY", "test-anon-key")
        .env("OPENSECRET_BASE_URL", "https://does-not-resolve.invalid")
        .env(
            "OPENSECRET_CLIENT_ID",
            "00000000-0000-0000-0000-000000000000",
        )
        .args(["account", "list"])
        .assert()
        .failure()
        .get_output()
        .clone();

    assert_eq!(
        out.status.code(),
        Some(3),
        "expected exit 3 for not-logged-in, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    // stderr is line-oriented: zero or more `note: …` warnings (e.g. the
    // keyring-unavailable diagnostic emitted by the fallback chain on Linux
    // CI) followed by the structured JSON error body. Scan for the JSON
    // line rather than parsing the whole blob.
    let stderr = String::from_utf8(out.stderr).unwrap();
    let json_line = stderr
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error line in stderr: {stderr}"));
    let parsed: serde_json::Value = serde_json::from_str(json_line.trim())
        .unwrap_or_else(|e| panic!("error line was not valid JSON ({e}): {json_line}"));
    assert_eq!(
        parsed.pointer("/error/code").and_then(|v| v.as_str()),
        Some("not-logged-in"),
        "unexpected error body: {parsed}",
    );
}

// ---------------------------------------------------------------------------
// Help-tree snapshot tests
//
// These guard the progressive-disclosure structure and the leaf-help house
// template (Description / Arguments / Example / Exit codes / See also). They
// double as a token-efficiency guard: any unexpected growth in help text
// shows up as a line-count ceiling breach. `insta` is not a workspace
// dependency, so these are plain string-assertion snapshots — they pin the
// *shape*, not byte-for-byte text.
// ---------------------------------------------------------------------------

/// Every existing leaf command, addressed by its full path.
const LEAF_COMMANDS: &[&[&str]] = &[
    &["version"],
    &["decode"],
    &["auth", "login"],
    &["auth", "signup"],
    &["auth", "guest"],
    &["auth", "logout"],
    &["auth", "status"],
    &["account", "list"],
    &["account", "info"],
    &["account", "default"],
    &["mint", "add"],
    &["mint", "list"],
    &["balance"],
    &["receive", "token"],
    &["receive", "lightning"],
    &["receive", "lightning-complete"],
    &["send", "token"],
    &["send", "lightning"],
    &["send", "lightning-complete"],
    &["send", "lightning-address"],
];

/// Top-level `--help` carries the orientation banner: output contract,
/// exit codes, getting started, discovery.
#[test]
fn top_level_help_has_orientation_banner() {
    let text = help_text(&["--help"]);
    for marker in [
        "OUTPUT CONTRACT",
        "EXIT CODES",
        "GETTING STARTED",
        "DISCOVERY",
        "agicash auth login",
        "<command> --help",
    ] {
        assert!(
            text.contains(marker),
            "top-level --help missing orientation marker `{marker}`:\n{text}",
        );
    }
}

/// `agicash --help` stays a terse map: one line per command group, and the
/// whole page stays small enough to be cheap for an agent to read.
#[test]
fn top_level_help_is_a_terse_map() {
    let text = help_text(&["--help"]);
    for group in [
        "version", "decode", "auth", "account", "mint", "balance", "receive", "send",
    ] {
        assert!(
            text.contains(group),
            "top-level --help missing command group `{group}`",
        );
    }
    // Token-bloat guard: the top-level page is the most-read help in the
    // tree; keep it lean. Generous ceiling — a real regression blows past.
    let lines = text.lines().count();
    assert!(
        lines < 60,
        "top-level --help grew to {lines} lines (ceiling 60) — token bloat?\n{text}",
    );
}

/// Each command *group* gains a `long_about` richer than its one-line
/// `about` — progressive disclosure between level 1 and level 2.
#[test]
fn group_help_drills_in_past_the_one_liner() {
    for group in [["auth"], ["account"], ["mint"], ["receive"], ["send"]] {
        let mut args: Vec<&str> = group.to_vec();
        args.push("--help");
        let text = help_text(&args);
        assert!(
            text.contains("--help` for detail"),
            "group `{}` --help lacks a drill-in long_about:\n{text}",
            group[0],
        );
    }
}

/// Every existing leaf command follows the 5-part house template.
#[test]
fn every_leaf_follows_the_house_template() {
    for leaf in LEAF_COMMANDS {
        let mut args: Vec<&str> = leaf.to_vec();
        args.push("--help");
        let text = help_text(&args);
        let path = leaf.join(" ");
        // Example block: one concrete invocation + its literal JSON.
        assert!(
            text.contains("EXAMPLE"),
            "leaf `{path}` --help missing EXAMPLE block:\n{text}",
        );
        // Exit-codes block.
        assert!(
            text.contains("EXIT CODES"),
            "leaf `{path}` --help missing EXIT CODES block:\n{text}",
        );
        // See-also block.
        assert!(
            text.contains("SEE ALSO"),
            "leaf `{path}` --help missing SEE ALSO block:\n{text}",
        );
        // Token-bloat guard: one example, no padding — a leaf page stays
        // compact. A generous ceiling that a real regression breaches.
        let lines = text.lines().count();
        assert!(
            lines < 70,
            "leaf `{path}` --help grew to {lines} lines (ceiling 70) — token bloat?\n{text}",
        );
    }
}

/// Every argument and flag on every leaf carries a non-empty help string.
/// Specifically pins the fix for the two previously blank `send
/// lightning-complete` arguments (`--poll-ms`, `--timeout-s`).
#[test]
fn no_leaf_argument_has_blank_help() {
    for leaf in LEAF_COMMANDS {
        let mut args: Vec<&str> = leaf.to_vec();
        args.push("--help");
        let text = help_text(&args);
        let path = leaf.join(" ");
        // In clap's long help, an arg with help text renders the flag on
        // one line and the help indented on the next. A blank-help arg
        // renders the flag with nothing under it. Scan: every line that
        // introduces a flag/arg must be followed by an indented help line.
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let is_flag_line = trimmed.starts_with("--") || trimmed.starts_with('<');
            if !is_flag_line {
                continue;
            }
            let next = lines.get(i + 1).map_or("", |s| s.trim());
            assert!(
                !next.is_empty(),
                "leaf `{path}` --help: argument line `{trimmed}` has blank help text",
            );
        }
    }
}

/// `send lightning-complete` specifically — its two previously help-less
/// arguments must now describe themselves.
#[test]
fn send_lightning_complete_args_are_documented() {
    let text = help_text(&["send", "lightning-complete", "--help"]);
    assert!(
        text.contains("--poll-ms") && text.contains("Polling interval"),
        "send lightning-complete --poll-ms still undocumented:\n{text}",
    );
    assert!(
        text.contains("--timeout-s") && text.contains("timeout in seconds"),
        "send lightning-complete --timeout-s still undocumented:\n{text}",
    );
}

/// `ValueEnum` arguments surface their valid values in `--help`.
#[test]
fn value_enum_args_show_possible_values() {
    let token = help_text(&["send", "token", "--help"]);
    assert!(
        token.contains("Possible values:") && token.contains("3:") && token.contains("4:"),
        "send token --help does not list token-version possible values:\n{token}",
    );
    let mint = help_text(&["mint", "add", "--help"]);
    assert!(
        mint.contains("Possible values:") && mint.contains("BTC:") && mint.contains("USD:"),
        "mint add --help does not list currency possible values:\n{mint}",
    );
}

/// The two-tier help works: `-h` is the terse scan, `--help` the full
/// page. `-h` must be strictly shorter than `--help` for a leaf with an
/// `after_long_help` block.
#[test]
fn dash_h_is_terser_than_double_dash_help() {
    let short = help_text(&["send", "token", "-h"]);
    let long = help_text(&["send", "token", "--help"]);
    assert!(
        short.len() < long.len(),
        "`send token -h` ({} bytes) should be shorter than `--help` ({} bytes)",
        short.len(),
        long.len(),
    );
    // The terse tier omits the after_long_help block...
    assert!(
        !short.contains("EXAMPLE"),
        "`-h` should not render the full EXAMPLE block:\n{short}",
    );
    // ...while the full tier includes it.
    assert!(
        long.contains("EXAMPLE"),
        "`--help` should render the EXAMPLE block:\n{long}",
    );
}

/// Spot-check `-h` at the top level: terse `about`, no banner.
#[test]
fn top_level_dash_h_omits_the_banner() {
    let short = help_text(&["-h"]);
    let long = help_text(&["--help"]);
    assert!(
        !short.contains("OUTPUT CONTRACT"),
        "top-level `-h` should not render the orientation banner:\n{short}",
    );
    assert!(
        long.contains("OUTPUT CONTRACT"),
        "top-level `--help` must render the orientation banner",
    );
}
