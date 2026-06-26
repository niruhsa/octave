package dev.niruhsa.octave

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

/**
 * Foreground service that keeps a background offline download alive while the app
 * is backgrounded or the screen is locked / off. The exact mirror of
 * [UploadService] for the download direction.
 *
 * The download itself runs in the Rust core (a tokio task streaming each track
 * file — see `commands::download_commands` / `downloads::manager`). Without a
 * foreground service, Android applies its background network restriction + Doze
 * the moment the app loses foreground, which severs the in-flight transfer and
 * fails the download. An `ongoing` notification alone does **not** do this — it
 * only stops the user swiping it away; it grants no background execution at all.
 *
 * So this service, started while the app is still foreground (the user tapped
 * Download), does three things for the duration of the download:
 *  - posts a persistent, non-dismissible `dataSync` foreground notification that
 *    updates with progress and is removed only when the download finishes;
 *  - holds a **partial wake lock** so the CPU keeps running with the screen off;
 *  - holds a **WiFi lock** so the WiFi radio doesn't power down with the screen
 *    off (a common cause of dropped transfers on lock).
 *
 * It is driven entirely from Rust via [DownloadServicePlugin]: `start` → `update`
 * (per progress tick) → `stop`. Modelled on [UploadService] / [MediaService].
 */
class DownloadService : Service() {
  companion object {
    const val CHANNEL_ID = "octave_download"
    // Distinct from MediaService's 1001 and UploadService's 1002 so playback,
    // upload and download foreground notifications can all coexist.
    const val NOTIFICATION_ID = 1003

    @Volatile var running = false
    @Volatile private var title: String = "Downloading music"
    @Volatile private var body: String = ""
    @Volatile private var progress: Int = -1

    /** Bring the foreground service up (idempotent — refreshes if already up). */
    fun start(context: Context, title: String, body: String, progress: Int) {
      this.title = title
      this.body = body
      this.progress = progress
      if (running) {
        update(context, title, body, progress)
        return
      }
      // Set running before the async start so a tick that arrives in the gap
      // before onStartCommand runs updates the (same-id) notification rather
      // than trying to start a second service.
      running = true
      val intent = Intent(context, DownloadService::class.java)
      try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
          context.startForegroundService(intent)
        } else {
          context.startService(intent)
        }
      } catch (e: Exception) {
        // Background foreground-service start restriction (Android 12+). We
        // always start from the foreground, so this shouldn't fire; if it does,
        // there's nothing we can legally show — the download still runs
        // best-effort.
        running = false
      }
    }

    /** Refresh the notification text / progress. No-op if not running (so a late
     *  tick after [stop] can't resurrect the service). */
    fun update(context: Context, title: String, body: String, progress: Int) {
      this.title = title
      this.body = body
      this.progress = progress
      if (!running) return
      try {
        NotificationManagerCompat.from(context)
          .notify(NOTIFICATION_ID, build(context, title, body, progress))
      } catch (e: SecurityException) {
        // POST_NOTIFICATIONS not granted — the service still runs (the download
        // is kept alive); only the visible notification is suppressed.
      }
    }

    /** Tear the service down (releases the wake / WiFi locks in onDestroy). */
    fun stop(context: Context) {
      running = false
      context.stopService(Intent(context, DownloadService::class.java))
    }

    private fun build(ctx: Context, title: String, body: String, progress: Int): Notification {
      val launch = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)?.let {
        PendingIntent.getActivity(
          ctx, 0, it,
          PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
      }
      val b = NotificationCompat.Builder(ctx, CHANNEL_ID)
        .setSmallIcon(android.R.drawable.stat_sys_download)
        .setContentTitle(title)
        .setContentText(body)
        .setOngoing(true)
        .setOnlyAlertOnce(true)
        .setShowWhen(false)
        .setColor(0xFFE0A84B.toInt())
        .setCategory(NotificationCompat.CATEGORY_PROGRESS)
        .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        .setContentIntent(launch)
      when {
        progress in 0..100 -> b.setProgress(100, progress, false)
        progress < 0 -> b.setProgress(0, 0, true) // indeterminate (preparing)
      }
      return b.build()
    }
  }

  private var wakeLock: PowerManager.WakeLock? = null
  private var wifiLock: WifiManager.WifiLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    createChannel()
    running = true
    val notif = build(this, title, body, progress)
    // startForeground must be called within ~5s of startForegroundService; we
    // do it immediately. The dataSync type requires the matching manifest
    // foregroundServiceType + FOREGROUND_SERVICE_DATA_SYNC permission (API 34+).
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      startForeground(NOTIFICATION_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
    } else {
      startForeground(NOTIFICATION_ID, notif)
    }
    acquireLocks()
    return START_NOT_STICKY
  }

  /** Keep the CPU + WiFi radio alive with the screen off for the transfer. */
  private fun acquireLocks() {
    if (wakeLock == null) {
      val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
      wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "octave:download").apply {
        setReferenceCounted(false)
        // 6h safety cap so a missed stop can never leak a permanent wake lock;
        // released explicitly on stop well before this.
        acquire(6 * 60 * 60 * 1000L)
      }
    }
    if (wifiLock == null) {
      val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
      val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        WifiManager.WIFI_MODE_FULL_LOW_LATENCY
      } else {
        @Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF
      }
      wifiLock = wm.createWifiLock(mode, "octave:download").apply {
        setReferenceCounted(false)
        acquire()
      }
    }
  }

  private fun releaseLocks() {
    wakeLock?.let { if (it.isHeld) it.release() }
    wakeLock = null
    wifiLock?.let { if (it.isHeld) it.release() }
    wifiLock = null
  }

  private fun createChannel() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
      if (mgr.getNotificationChannel(CHANNEL_ID) == null) {
        val ch = NotificationChannel(CHANNEL_ID, "Downloads", NotificationManager.IMPORTANCE_LOW)
        ch.description = "Ongoing music downloads"
        ch.setShowBadge(false)
        ch.setSound(null, null)
        ch.enableVibration(false)
        ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        mgr.createNotificationChannel(ch)
      }
    }
  }

  override fun onDestroy() {
    releaseLocks()
    running = false
    super.onDestroy()
  }
}
