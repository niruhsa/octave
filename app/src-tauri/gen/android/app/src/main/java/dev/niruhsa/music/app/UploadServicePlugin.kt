package dev.niruhsa.music.app

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class ForegroundArgs {
  lateinit var title: String
  var body: String = ""
  /** 0..100 → determinate progress bar; < 0 → indeterminate. */
  var progress: Int = -1
}

/**
 * Bridge plugin (paired with the Rust `upload_session` plugin) that drives the
 * upload foreground service [UploadService] from the native upload job.
 *
 * `start` brings the service up while the app is still foreground (Android 12+
 * forbids starting a foreground service from the background); `update` refreshes
 * the persistent notification per progress tick; `stop` tears it down. Commands
 * are dispatched off the main thread, so the work is marshalled onto the UI
 * thread; `resolve()` fires immediately (the Rust side doesn't await a result).
 */
@TauriPlugin
class UploadServicePlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun start(invoke: Invoke) {
    val args = invoke.parseArgs(ForegroundArgs::class.java)
    activity.runOnUiThread {
      UploadService.start(activity.applicationContext, args.title, args.body, args.progress)
    }
    invoke.resolve()
  }

  @Command
  fun update(invoke: Invoke) {
    val args = invoke.parseArgs(ForegroundArgs::class.java)
    activity.runOnUiThread {
      UploadService.update(activity.applicationContext, args.title, args.body, args.progress)
    }
    invoke.resolve()
  }

  @Command
  fun stop(invoke: Invoke) {
    activity.runOnUiThread {
      UploadService.stop(activity.applicationContext)
    }
    invoke.resolve()
  }
}
