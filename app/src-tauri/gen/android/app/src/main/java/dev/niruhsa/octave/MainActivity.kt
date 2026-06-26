package dev.niruhsa.octave

import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // Ensure the new-release notification channel exists so notifications shown
    // by FCM auto-display (app closed) and by the WorkManager fallback both have
    // a channel on Android 8+. Idempotent. The user logs in (and registers for
    // push) only after the app has run, so the channel is always present before
    // the first push arrives.
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val mgr = getSystemService(NotificationManager::class.java)
      if (mgr != null && mgr.getNotificationChannel(NotificationPollWorker.CHANNEL_ID) == null) {
        val ch = NotificationChannel(
          NotificationPollWorker.CHANNEL_ID,
          "New releases",
          NotificationManager.IMPORTANCE_DEFAULT,
        )
        ch.description = "New releases from artists you follow"
        mgr.createNotificationChannel(ch)
      }
    }

    // Edge-to-edge (enableEdgeToEdge / decorFitsSystemWindows=false) makes the
    // soft keyboard *overlay* the WebView instead of resizing it, so focused
    // inputs — e.g. the login username/password fields — end up hidden behind
    // the keyboard. Apply the IME inset as bottom padding on the content view
    // so the WebView shrinks to sit above the keyboard; the web layer then
    // scrolls the focused field into the visible area (see src/lib/viewport.ts).
    //
    // The insets are returned unconsumed so the WebView still receives the
    // system-bar insets it exposes to CSS via env(safe-area-inset-*), keeping
    // the edge-to-edge status/navigation-bar padding intact.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val imeBottom = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
      view.setPadding(view.paddingLeft, view.paddingTop, view.paddingRight, imeBottom)
      insets
    }
  }
}
