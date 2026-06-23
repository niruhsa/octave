package dev.niruhsa.music.app

import android.util.Log
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * Delivers a transport-button press from the native media session / notification
 * back to the web player.
 *
 * A bare WebView can't reach the Tauri plugin event channel (ACL-gated) and
 * Kotlin can't emit a Tauri event directly, so we hit the app's in-app loopback
 * server instead (`<actionBaseUrl>/<action>`), which re-broadcasts it as a
 * `media-session-action` Tauri event the frontend listens for. The base URL is
 * pushed from JS on every `update` (it embeds the per-launch token).
 */
object MediaBridge {
  @Volatile
  var actionBaseUrl: String? = null

  fun send(action: String, positionMs: Long? = null) {
    val base = actionBaseUrl ?: return
    val url = if (positionMs != null) "$base/$action?pos=$positionMs" else "$base/$action"
    // Network must be off the main thread.
    thread {
      try {
        val conn = URL(url).openConnection() as HttpURLConnection
        conn.connectTimeout = 2000
        conn.readTimeout = 2000
        conn.requestMethod = "GET"
        conn.responseCode // fire the request
        conn.disconnect()
      } catch (e: Exception) {
        Log.w("MediaBridge", "failed to deliver action '$action'", e)
      }
    }
  }
}
