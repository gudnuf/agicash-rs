pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // `rustls-platform-verifier-android` ships a Maven repo *inside the
        // Rust crate's source tree*. The Rust shim in `crates/agicash-ffi`
        // pulls the crate in transitively; we ask `cargo metadata` (filtered
        // to the Android target so the dep tree includes the android-only
        // wrapper) where it lives on disk, then point Gradle at the bundled
        // `maven/` dir. See the crate's README, "Gradle Setup" section.
        rustlsPlatformVerifierRepo()?.let { repoPath ->
            maven {
                url = uri(repoPath)
                metadataSources { artifact() }
            }
        }
    }
}

/**
 * Locate the on-disk Maven repo that the `rustls-platform-verifier-android`
 * crate ships alongside its `Cargo.toml`. Returns null only if `cargo` is not
 * on PATH at configuration time, in which case the build will fail later with
 * a clearer "unresolved dependency" message than a Gradle script crash.
 */
fun rustlsPlatformVerifierRepo(): String? {
    val cratesManifest = file("../../crates/agicash-ffi/Cargo.toml")
    if (!cratesManifest.exists()) {
        return null
    }
    val proc = ProcessBuilder(
        "cargo",
        "metadata",
        "--format-version",
        "1",
        "--filter-platform",
        "aarch64-linux-android",
        "--manifest-path",
        cratesManifest.absolutePath,
    )
        .redirectErrorStream(false)
        .start()
    val json = proc.inputStream.bufferedReader().readText()
    if (proc.waitFor() != 0) {
        return null
    }
    // Tiny ad-hoc grep over the JSON to avoid pulling a JSON library
    // into settings.gradle.kts. We can't safely use a `name`-based
    // anchor because the `rustls-platform-verifier` non-android crate
    // declares an `"android"` feature, so the string
    // `"name":"...android"` literally appears inside its features
    // object. Instead, walk every `manifest_path` entry and pick the
    // one whose path contains `rustls-platform-verifier-android-` —
    // crate manifest paths always live in
    // `<registry>/rustls-platform-verifier-android-<version>/Cargo.toml`,
    // a substring unique to the android crate.
    val manifestKey = "\"manifest_path\":\""
    var cursor = 0
    while (true) {
        val mp = json.indexOf(manifestKey, startIndex = cursor)
        if (mp < 0) return null
        val start = mp + manifestKey.length
        val end = json.indexOf('"', start)
        if (end < 0) return null
        val manifestPath = json.substring(start, end)
        if (manifestPath.contains("/rustls-platform-verifier-android-")) {
            return java.io.File(java.io.File(manifestPath).parentFile, "maven").absolutePath
        }
        cursor = end + 1
    }
    @Suppress("UNREACHABLE_CODE")
    return null
}

rootProject.name = "Agicash"
include(":app")
