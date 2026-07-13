package dev.niruhsa.octave

import android.util.Log
import java.net.HttpURLConnection
import java.net.URL
import kotlin.concurrent.thread

/**
 * Delivers a redacted "the audio route may have changed" signal to Rust.
 *
 * Device ids and labels never appear in this request. Rust authenticates the
 * per-launch loopback token, re-queries [AudioRoutePlugin], and runs the EQ
 * resolver before emitting a redacted effective-profile event to the WebView.
 */
object AudioRouteBridge {
  @Volatile
  var callbackBaseUrl: String? = null

  fun routeChanged() {
    val base = callbackBaseUrl ?: return
    thread(name = "octave-eq-route-signal") {
      try {
        val conn = URL("$base/equalizer-route-changed").openConnection() as HttpURLConnection
        conn.connectTimeout = 2000
        conn.readTimeout = 2000
        conn.requestMethod = "GET"
        conn.responseCode
        conn.disconnect()
      } catch (e: Exception) {
        Log.w(TAG, "failed to deliver audio-route signal", e)
      }
    }
  }

  private const val TAG = "OctaveEqRoute"
}
