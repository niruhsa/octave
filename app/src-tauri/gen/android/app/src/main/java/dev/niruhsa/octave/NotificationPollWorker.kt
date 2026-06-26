package dev.niruhsa.octave

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
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
 * can defer it further under Doze / battery saver / OEM battery management).
 *
 * Debug with: `adb logcat -s OctaveNotif`.
 */
class NotificationPollWorker(ctx: Context, params: WorkerParameters) :
  CoroutineWorker(ctx, params) {

  override suspend fun doWork(): Result {
    val prefs = applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    val base = prefs.getString(KEY_BASE, null)?.trimEnd('/')
    val token = prefs.getString(KEY_TOKEN, null)
    // No signed-in user (logged out, or a SECRET_KEY session that has no
    // per-user notifications) → nothing to poll.
    if (base.isNullOrEmpty() || token.isNullOrEmpty()) {
      Log.i(TAG, "run: no base/token → nothing to poll")
      return Result.success()
    }

    val body = try {
      withContext(Dispatchers.IO) { fetchUnread(base, token) }
    } catch (e: Exception) {
      Log.w(TAG, "run: fetch failed (${e.message}) → retry")
      return Result.retry() // transient network/IO — retry on backoff
    } ?: run {
      Log.i(TAG, "run: non-2xx (token expired?) → waiting")
      return Result.success()
    }

    val arr = body.optJSONArray("notifications") ?: return Result.success()
    val seen = prefs.getStringSet(KEY_SEEN, emptySet())!!.toMutableSet()
    val currentIds = HashSet<String>()
    val fresh = ArrayList<JSONObject>()
    for (i in 0 until arr.length()) {
      val o = arr.optJSONObject(i) ?: continue
      val id = o.optString("id")
      if (id.isEmpty()) continue
      currentIds.add(id)
      if (!seen.contains(id)) fresh.add(o)
    }

    // While the app is foreground the in-app JS poller owns surfacing — record
    // everything as seen (so we don't re-post after backgrounding) but don't
    // post. Fail open: any error reading lifecycle state → treat as background.
    val foreground = try {
      withContext(Dispatchers.Main) {
        ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)
      }
    } catch (e: Exception) {
      false
    }

    Log.i(TAG, "run: unread=${currentIds.size} fresh=${fresh.size} foreground=$foreground")

    if (!foreground) {
      when {
        fresh.isEmpty() -> {}
        // A handful → individual notifications (the common new-release case).
        fresh.size <= MAX_INDIVIDUAL -> fresh.forEach { o ->
          post(o.optString("id"), o.optString("title", "New release"), o.optString("body", ""))
        }
        // A large batch (e.g. first enable with a backlog) → one summary so we
        // never blast one notification per row.
        else -> postSummary(fresh.size)
      }
    }

    // Mark everything currently unread as seen (posted, summarised, or handled
    // by the foreground JS poller) and prune to the current feed so the set
    // stays bounded (read notifications drop out).
    seen.addAll(currentIds)
    seen.retainAll(currentIds)
    prefs.edit().putStringSet(KEY_SEEN, seen).apply()
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
      val code = conn.responseCode
      if (code != 200) {
        Log.i(TAG, "fetch: HTTP $code")
        return null
      }
      JSONObject(conn.inputStream.bufferedReader().use { it.readText() })
    } finally {
      conn.disconnect()
    }
  }

  /** Post one new-release notification. Tapping it opens the app. */
  private fun post(id: String, title: String, text: String) {
    if (id.isEmpty()) return
    val ctx = applicationContext
    createChannel(ctx)
    val b = baseBuilder(ctx, title, text, id.hashCode())
    if (text.isNotEmpty()) b.setStyle(NotificationCompat.BigTextStyle().bigText(text))
    notify(ctx, id.hashCode(), b)
  }

  /** Collapse a large batch into a single summary notification. */
  private fun postSummary(count: Int) {
    val ctx = applicationContext
    createChannel(ctx)
    val b = baseBuilder(
      ctx,
      "$count new releases",
      "From artists you follow",
      SUMMARY_ID,
    )
    notify(ctx, SUMMARY_ID, b)
  }

  private fun baseBuilder(
    ctx: Context,
    title: String,
    text: String,
    requestCode: Int,
  ): NotificationCompat.Builder {
    val launch = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
      ?.apply { addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP) }
    val pi = launch?.let {
      PendingIntent.getActivity(
        ctx, requestCode, it,
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
      )
    }
    return NotificationCompat.Builder(ctx, CHANNEL_ID)
      .setSmallIcon(android.R.drawable.ic_popup_reminder)
      .setContentTitle(title)
      .setContentText(text)
      .setAutoCancel(true)
      .setColor(0xFFE0A84B.toInt())
      .setCategory(NotificationCompat.CATEGORY_SOCIAL)
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setPriority(NotificationCompat.PRIORITY_DEFAULT)
      .setContentIntent(pi)
  }

  private fun notify(ctx: Context, notifId: Int, b: NotificationCompat.Builder) {
    try {
      NotificationManagerCompat.from(ctx).notify(notifId, b.build())
    } catch (e: SecurityException) {
      // POST_NOTIFICATIONS not granted — nothing to show.
      Log.w(TAG, "notify: POST_NOTIFICATIONS not granted")
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
    const val TAG = "OctaveNotif"
    const val PREFS = "octave_notif_sync"
    const val KEY_BASE = "base_url"
    const val KEY_TOKEN = "token"
    const val KEY_SEEN = "seen_ids"
    const val CHANNEL_ID = "octave_new_release"
    const val UNIQUE_WORK = "octave_notif_poll"
    private const val MAX_INDIVIDUAL = 5
    private val SUMMARY_ID = "octave_notif_summary".hashCode()

    /** Enqueue the periodic poll (idempotent — keeps an existing schedule). */
    fun enqueue(ctx: Context) {
      val req = PeriodicWorkRequestBuilder<NotificationPollWorker>(15, TimeUnit.MINUTES)
        .setConstraints(
          Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build(),
        )
        .build()
      WorkManager.getInstance(ctx)
        .enqueueUniquePeriodicWork(UNIQUE_WORK, ExistingPeriodicWorkPolicy.KEEP, req)
      Log.i(TAG, "enqueued unique periodic work ($UNIQUE_WORK, 15 min)")
    }

    fun cancel(ctx: Context) {
      WorkManager.getInstance(ctx).cancelUniqueWork(UNIQUE_WORK)
      Log.i(TAG, "cancelled unique work ($UNIQUE_WORK)")
    }
  }
}
