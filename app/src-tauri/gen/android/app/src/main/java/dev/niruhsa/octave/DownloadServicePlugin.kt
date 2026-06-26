package dev.niruhsa.octave

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class DownloadForegroundArgs {
  lateinit var title: String
  var body: String = ""
  /** 0..100 → determinate progress bar; < 0 → indeterminate. */
  var progress: Int = -1
}

/**
 * Bridge plugin (paired with the Rust `download_session` plugin) that drives the
 * download foreground service [DownloadService] from the native download job.
 * The download-direction mirror of [UploadServicePlugin].
 *
 * `start` brings the service up while the app is still foreground (Android 12+
 * forbids starting a foreground service from the background); `update` refreshes
 * the persistent notification per progress tick; `stop` tears it down. Commands
 * marshal onto the UI thread; `resolve()` fires immediately (the Rust side
 * doesn't await a result).
 */
@TauriPlugin
class DownloadServicePlugin(private val activity: Activity) : Plugin(activity) {
  @Command
  fun start(invoke: Invoke) {
    val args = invoke.parseArgs(DownloadForegroundArgs::class.java)
    activity.runOnUiThread {
      DownloadService.start(activity.applicationContext, args.title, args.body, args.progress)
    }
    invoke.resolve()
  }

  @Command
  fun update(invoke: Invoke) {
    val args = invoke.parseArgs(DownloadForegroundArgs::class.java)
    activity.runOnUiThread {
      DownloadService.update(activity.applicationContext, args.title, args.body, args.progress)
    }
    invoke.resolve()
  }

  @Command
  fun stop(invoke: Invoke) {
    activity.runOnUiThread {
      DownloadService.stop(activity.applicationContext)
    }
    invoke.resolve()
  }
}
