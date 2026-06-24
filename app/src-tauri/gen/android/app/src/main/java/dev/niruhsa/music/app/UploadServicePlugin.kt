package dev.niruhsa.music.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class ForegroundArgs {
  lateinit var title: String
  var body: String = ""
  /** 0..100 → determinate progress bar; < 0 → indeterminate. */
  var progress: Int = -1
}

@InvokeArg
class UrisArgs {
  var uris: List<String> = emptyList()
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

  /**
   * Take **persistable** read permission on the picked `content://` URIs so they
   * remain readable after the process is killed + relaunched — required to resume
   * an upload from disk without re-picking the files. Best-effort: a picker that
   * didn't return a persistable grant throws, which we swallow (resume then
   * degrades to "file no longer readable" → cancel-as-corrupted on relaunch).
   */
  @Command
  fun persistUriPermissions(invoke: Invoke) {
    val args = invoke.parseArgs(UrisArgs::class.java)
    for (raw in args.uris) {
      try {
        val uri = Uri.parse(raw)
        if (uri.scheme == "content") {
          activity.contentResolver.takePersistableUriPermission(
            uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION,
          )
        }
      } catch (e: Exception) {
        // Non-persistable grant (e.g. ACTION_GET_CONTENT) — ignore.
      }
    }
    invoke.resolve()
  }

  /**
   * Whether the app holds full "All files access" (Android 11+
   * `MANAGE_EXTERNAL_STORAGE`). On Android ≤10 there's no such mode, so we report
   * `true` (the legacy `READ_EXTERNAL_STORAGE` runtime grant covers broad access
   * there).
   */
  @Command
  fun hasAllFilesAccess(invoke: Invoke) {
    val granted =
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        Environment.isExternalStorageManager()
      } else {
        true
      }
    val ret = JSObject()
    ret.put("granted", granted)
    invoke.resolve(ret)
  }

  /**
   * Send the user to the system "All files access" settings screen to grant
   * `MANAGE_EXTERNAL_STORAGE` (it can't be granted by a normal runtime dialog).
   * No-op if already granted or on Android ≤10. Best-effort: falls back to the
   * generic all-files-access list if the per-app screen is unavailable.
   */
  @Command
  fun requestAllFilesAccess(invoke: Invoke) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R && !Environment.isExternalStorageManager()) {
      try {
        val intent =
          Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION).apply {
            data = Uri.parse("package:" + activity.packageName)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
          }
        activity.startActivity(intent)
      } catch (e: Exception) {
        try {
          activity.startActivity(
            Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
              .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
          )
        } catch (e2: Exception) {
          // No settings activity available — nothing we can do.
        }
      }
    }
    invoke.resolve()
  }
}
