package com.makeprisms.agicash.ui.theme

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch

/**
 * Owns the live theme selection backed by [ThemePreferences] (DataStore).
 *
 * Mirrors React's `ThemeProvider` (`app/features/theme/theme-provider.tsx`):
 * holds the `(track, mode)` state, exposes setters that persist + update, and
 * the rendered tree re-composes off the resulting [StateFlow]. The setter
 * analogues are `setTheme` (track) and `setColorMode` (mode) from React's
 * `ThemeContextType`.
 *
 * Hoisted to the app root so [com.makeprisms.agicash.ui.theme.AgicashTheme]
 * wraps the whole UI in the resolved palette, and the Settings switcher reads
 * + mutates the same instance.
 */
class ThemeViewModel(app: Application) : AndroidViewModel(app) {
    private val prefs = ThemePreferences(app.applicationContext)

    val selection: StateFlow<ThemeSelection> = prefs.selection.stateIn(
        scope = viewModelScope,
        started = SharingStarted.Eagerly,
        initialValue = ThemeSelection(),
    )

    /** React `setColorMode` analogue — persists + updates the color mode. */
    fun setColorMode(mode: ColorMode) {
        viewModelScope.launch { prefs.setColorMode(mode) }
    }

    /**
     * React `setTheme` analogue — persists + updates the currency track.
     * Not driven by a user control today (React syncs it from the default
     * currency); exposed so a future currency-sync hook can call it.
     */
    fun setTrack(track: ThemeTrack) {
        viewModelScope.launch { prefs.setTrack(track) }
    }
}
