package dev.niruhsa.octave

import android.app.Activity
import android.content.Context
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class StartArgs {
  lateinit var baseUrl: String
  lateinit var token: String
}

/**
 * Bridge plugin (paired with the Rust `notify_sync` plugin) for the background
 * notification poll.
 *
 * `start` stashes the server base URL + bearer token in app-private prefs and
 * enqueues the periodic [NotificationPollWorker]; `stop` cancels the work and
 * clears the prefs (so a stale token is never used and a re-enable re-seeds).
 *
 * The token is handed in by the Rust side (which owns the secure credential
 * store) — it never passes through the WebView/JS layer.
 */
@TauriPlugin
class NotificationSyncPlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun start(invoke: Invoke) {
    val args = invoke.parseArgs(StartArgs::class.java)
    val ctx = activity.applicationContext
    Log.i(NotificationPollWorker.TAG, "plugin.start: base=${args.baseUrl}")
    ctx.getSharedPreferences(NotificationPollWorker.PREFS, Context.MODE_PRIVATE)
      .edit()
      .putString(NotificationPollWorker.KEY_BASE, args.baseUrl)
      .putString(NotificationPollWorker.KEY_TOKEN, args.token)
      .apply()
    NotificationPollWorker.enqueue(ctx)
    invoke.resolve()
  }

  @Command
  fun stop(invoke: Invoke) {
    val ctx = activity.applicationContext
    Log.i(NotificationPollWorker.TAG, "plugin.stop")
    NotificationPollWorker.cancel(ctx)
    // Clear everything (token + seen-set + seeded flag) so a different/next
    // session starts fresh and an expired token is never reused.
    ctx.getSharedPreferences(NotificationPollWorker.PREFS, Context.MODE_PRIVATE)
      .edit()
      .clear()
      .apply()
    invoke.resolve()
  }
}
