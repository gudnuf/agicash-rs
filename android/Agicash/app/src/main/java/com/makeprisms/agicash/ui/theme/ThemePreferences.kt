package com.makeprisms.agicash.ui.theme

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

/**
 * Theme axes — the Kotlin mirror of React's two-class theme scoping
 * (`app/features/theme/theme.types.ts`).
 *
 * The web app composes the theme on two independent axes:
 *   - **track** (`Theme` in React) — `btc` | `usd`, the currency palette.
 *   - **color mode** (`ColorMode` in React) — `light` | `dark` | `system`.
 *
 * **Track is NOT a user control on any client.** React derives it from the
 * user's default currency (`useSyncThemeWithDefaultCurrency` in
 * `app/features/wallet/wallet.tsx`: `defaultCurrency === 'BTC' ? 'btc' : 'usd'`).
 * We persist it so a future currency-sync hook can drive it without a schema
 * change, defaulting to `btc` to match React `defaultTheme = 'btc'`.
 *
 * The only user-facing switcher React exposes is the **color-mode** control
 * (`ColorModeToggle`, a light/dark/system dropdown) — so that is the only
 * switcher we surface in Settings. See [com.makeprisms.agicash.ui.screens].
 */
enum class ThemeTrack(val storageValue: String) {
    Btc("btc"),
    Usd("usd"),
    ;

    companion object {
        fun fromStorage(value: String?): ThemeTrack =
            entries.firstOrNull { it.storageValue == value } ?: Btc
    }
}

/**
 * Color mode axis. Mirrors React `ColorMode = 'light' | 'dark' | 'system'`
 * (`app/features/theme/theme.constants.ts`). `System` resolves to the platform
 * dark-mode setting at render time (`isSystemInDarkTheme()` in [AgicashTheme]),
 * the Compose analogue of React's `prefers-color-scheme` / `matchMedia`.
 */
enum class ColorMode(val storageValue: String) {
    Light("light"),
    Dark("dark"),
    System("system"),
    ;

    companion object {
        fun fromStorage(value: String?): ColorMode =
            entries.firstOrNull { it.storageValue == value } ?: defaultColorMode
    }
}

/**
 * Defaults match React's first-paint behaviour:
 *   - `defaultTheme = 'btc'`        (theme.constants.ts)
 *   - `defaultColorMode = 'system'` (theme.constants.ts)
 *
 * React's `defaultColorMode` is `system`; the very first paint with no cookie
 * resolves system → light (`defaultSystemColorMode = 'light'`). On Android,
 * `System` likewise resolves through `isSystemInDarkTheme()` at render time,
 * so a fresh install follows the device setting just like the web.
 */
val defaultTrack: ThemeTrack = ThemeTrack.Btc
val defaultColorMode: ColorMode = ColorMode.System

/** The persisted selection. */
data class ThemeSelection(
    val track: ThemeTrack = defaultTrack,
    val mode: ColorMode = defaultColorMode,
)

/**
 * Jetpack DataStore (Preferences) persistence for the theme selection so it
 * survives app restart — the Android analogue of React's theme cookies
 * (`theme-cookies.client.ts`). Single process-wide DataStore named `theme`.
 */
private val Context.themeDataStore: DataStore<Preferences> by preferencesDataStore(name = "theme")

private val TRACK_KEY = stringPreferencesKey("theme-track")
private val COLOR_MODE_KEY = stringPreferencesKey("color-mode")

class ThemePreferences(private val context: Context) {
    /** Persisted selection, emitting the default until a value is written. */
    val selection: Flow<ThemeSelection> = context.themeDataStore.data.map { prefs ->
        ThemeSelection(
            track = ThemeTrack.fromStorage(prefs[TRACK_KEY]),
            mode = ColorMode.fromStorage(prefs[COLOR_MODE_KEY]),
        )
    }

    suspend fun setTrack(track: ThemeTrack) {
        context.themeDataStore.edit { it[TRACK_KEY] = track.storageValue }
    }

    suspend fun setColorMode(mode: ColorMode) {
        context.themeDataStore.edit { it[COLOR_MODE_KEY] = mode.storageValue }
    }
}
