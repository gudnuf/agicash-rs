package com.makeprisms.agicash

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.makeprisms.agicash.ui.AgicashRoot
import com.makeprisms.agicash.ui.theme.AgicashTheme
import com.makeprisms.agicash.ui.theme.ThemeViewModel
import com.makeprisms.agicash.wallet.WalletViewModel

class MainActivity : ComponentActivity() {
    private val viewModel: WalletViewModel by viewModels()
    private val themeViewModel: ThemeViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val theme by themeViewModel.selection.collectAsStateWithLifecycle()
            AgicashTheme(track = theme.track, mode = theme.mode) {
                AgicashRoot(viewModel = viewModel, themeViewModel = themeViewModel)
            }
        }
    }
}
