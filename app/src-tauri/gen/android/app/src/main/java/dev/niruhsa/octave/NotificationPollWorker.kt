package dev.niruhsa.octave

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit

/**
 * Periodic background poll for new-release notifications (Phase 10).
 *
 * The in-app (JavaScript) poller only runs while the app is open. To match how
 * other apps deliver notifications while closed, this WorkManager worker wakes
 * roughly every 15 minutes (the OS minimum for periodic work), independent of
 * the app process — it survives the app being swiped away and device reboots.
 *
 * It is fully self-contained Kotlin (the Rust core / WebView are *not* running
 * when the app is closed): it reads the server base URL + bearer token that
 * [NotificationSyncPlugin] stashed in private prefs, calls the existing
 * `GET /notifications?unread=true` REST endpoint itself, and posts a system
 * notification for each unread row it hasn't surfaced before.
 *
 * This is the lighter, no-Firebase alternative to true push (FCM/APNs): it
 * needs no server push transport, but it isn't instant (≥15 min, and Android
 * can defer it further under Doze / battery saver).
 */
class NotificationPollWorker(ctx: Context, params: WorkerParameters) :
  CoroutineWorker(ctx, params) {

  override suspend fun doWork(): Result {
    val prefs = applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    val base = prefs.getString(KEY_BASE, null)?.trimEnd('/')
    val token = prefs.getString(KEY_TOKEN, null)
    // No signed-in user (logged out, or a SECRET_KEY session that has no
    // per-user notifications) → nothing to poll.
    if (base.isNullOrEmpty() || token.isNullOrEmpty()) return Result.success()

    // While the app is foreground the in-app JS poller owns surfacing; skip so
    // the two paths don't double-post. Lifecycle state is main-thread-only.
    val foreground = withContext(Dispatchers.Main) {
      ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
    }
    if (foreground) return Result.success()

    val body = try {
      withContext(Dispatchers.IO) { fetchUnread(base, token) }
    } catch (e: Exception) {
      // Transient network/IO — let WorkManager retry on its backoff schedule.
      return Result.retry()
    } ?: return Result.success() // non-2xx (e.g. token expired) — wait quietly

    val arr = body.optJSONArray("notifications") ?: return Result.success()

    // `seen` tracks ids already surfaced. We prune it to the ids currently in
    // the unread feed at the end, so it stays bounded (read notifications drop
    // out of the feed and out of `seen`) and never grows without limit.
    val seen = prefs.getStringSet(KEY_SEEN, emptySet())!!.toMutableSet()
    val currentIds = HashSet<String>()
    val seeded = prefs.getBoolean(KEY_SEEDED, false)

    // First-ever background run: seed the current backlog silently (mirrors the
    // JS poller) so we don't post one notification per pre-existing unread row.
    if (!seeded) {
      for (i in 0 until arr.length()) {
        arr.optJSONObject(i)?.optString("id")?.let { if (it.isNotEmpty()) seen.add(it) }
      }
      prefs.edit().putStringSet(KEY_SEEN, seen).putBoolean(KEY_SEEDED, true).apply()
      return Result.success()
    }

    var changed = false
    for (i in 0 until arr.length()) {
      val o = arr.optJSONObject(i) ?: continue
      val id = o.optString("id")
      if (id.isEmpty()) continue
      currentIds.add(id)
      if (seen.contains(id)) continue
      val title = o.optString("title", "New release")
      val text = o.optString("body", "")
      post(id, title, text)
      seen.add(id)
      changed = true
    }

    // Keep only ids still unread (+ any we just added, which are in currentIds).
    if (seen.retainAll(currentIds) || changed) {
      prefs.edit().putStringSet(KEY_SEEN, seen).apply()
    }
    return Result.success()
  }

  /** `GET {base}/notifications?unread=true&limit=20`. Returns the parsed body,
   *  or `null` on a non-200 (e.g. an expired token) so the caller waits quietly. */
  private fun fetchUnread(base: String, token: String): JSONObject? {
    val conn = (URL("$base/notifications?unread=true&limit=20").openConnection()
      as HttpURLConnection).apply {
      requestMethod = "GET"
      setRequestProperty("Authorization", "Bearer $token")
      setRequestProperty("Accept", "application/json")
      connectTimeout = 15_000
      readTimeout = 15_000
    }
    return try {
      if (conn.responseCode != 200) return null
      JSONObject(conn.inputStream.bufferedReader().use { it.readText() })
    } finally {
      conn.disconnect()
    }
  }

  /** Post one new-release notification. Tapping it opens the app. */
  private fun post(id: String, title: String, text: String) {
    val ctx = applicationContext
    createChannel(ctx)
    val launch = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
      ?.apply { addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP) }
    val pi = launch?.let {
      PendingIntent.getActivity(
        ctx, id.hashCode(), it,
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
      )
    }
    val b = NotificationCompat.Builder(ctx, CHANNEL_ID)
      .setSmallIcon(android.R.drawable.ic_popup_reminder)
      .setContentTitle(title)
      .setContentText(text)
      .setAutoCancel(true)
      .setColor(0xFFE0A84B.toInt())
      .setCategory(NotificationCompat.CATEGORY_SOCIAL)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setPriority(NotificationCompat.PRIORITY_DEFAULT)
      .setContentIntent(pi)
    if (text.isNotEmpty()) b.setStyle(NotificationCompat.BigTextStyle().bigText(text))
    try {
      // Distinct id per notification so multiple new releases stack (and a
      // re-post of the same id updates rather than duplicates).
      NotificationManagerCompat.from(ctx).notify(id.hashCode(), b.build())
    } catch (e: SecurityException) {
      // POST_NOTIFICATIONS not granted — nothing to show.
    }
  }

  private fun createChannel(ctx: Context) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val mgr = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
      if (mgr.getNotificationChannel(CHANNEL_ID) == null) {
        val ch = NotificationChannel(
          CHANNEL_ID, "New releases", NotificationManager.IMPORTANCE_DEFAULT,
        )
        ch.description = "New releases from artists you follow"
        ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        mgr.createNotificationChannel(ch)
      }
    }
  }

  companion object {
    const val PREFS = "octave_notif_sync"
    const val KEY_BASE = "base_url"
    const val KEY_TOKEN = "token"
    const val KEY_SEEN = "seen_ids"
    const val KEY_SEEDED = "seeded"
    const val CHANNEL_ID = "octave_new_release"
    const val UNIQUE_WORK = "octave_notif_poll"

    /** Enqueue the periodic poll (idempotent — keeps an existing schedule). */
    fun enqueue(ctx: Context) {
      val req = PeriodicWorkRequestBuilder<NotificationPollWorker>(15, TimeUnit.MINUTES)
        .setConstraints(
          Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build(),
        )
        .build()
      WorkManager.getInstance(ctx)
        .enqueueUniquePeriodicWork(UNIQUE_WORK, ExistingPeriodicWorkPolicy.KEEP, req)
    }

    fun cancel(ctx: Context) {
      WorkManager.getInstance(ctx).cancelUniqueWork(UNIQUE_WORK)
    }
  }
}
