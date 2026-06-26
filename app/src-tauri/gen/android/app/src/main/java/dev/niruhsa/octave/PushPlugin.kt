package dev.niruhsa.octave

import android.app.Activity
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.firebase.messaging.FirebaseMessaging

/**
 * Bridge plugin (paired with the Rust `push` plugin) for FCM token management.
 *
 * `getToken` resolves `{ token }` — the device's current FCM registration token,
 * or `""` when FCM is unavailable (no `google-services.json` / no Google Play
 * Services), which tells the Rust side to fall back to the WorkManager poll.
 * `deleteToken` drops the token on sign-out. Token fetch is async; the command
 * resolves from the completion callback (the Rust `run_mobile_plugin` waits).
 */
@TauriPlugin
class PushPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun getToken(invoke: Invoke) {
    try {
      FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
        val ret = JSObject()
        if (task.isSuccessful) {
          ret.put("token", task.result ?: "")
        } else {
          Log.w(TAG, "getToken failed: ${task.exception?.message}")
          ret.put("token", "")
        }
        invoke.resolve(ret)
      }
    } catch (e: Exception) {
      // FirebaseApp not initialized (no google-services.json) / Play Services
      // missing — report "no token" so the caller uses the polling fallback.
      Log.w(TAG, "getToken unavailable: ${e.message}")
      val ret = JSObject()
      ret.put("token", "")
      invoke.resolve(ret)
    }
  }

  @Command
  fun deleteToken(invoke: Invoke) {
    try {
      FirebaseMessaging.getInstance().deleteToken()
    } catch (e: Exception) {
      Log.w(TAG, "deleteToken unavailable: ${e.message}")
    }
    invoke.resolve()
  }

  companion object {
    private const val TAG = "OctavePush"
  }
}
