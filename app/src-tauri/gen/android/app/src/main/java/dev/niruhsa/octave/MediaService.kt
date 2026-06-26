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
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaControllerCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import androidx.core.app.NotificationCompat
import androidx.media.app.NotificationCompat.MediaStyle
import androidx.media.session.MediaButtonReceiver

/**
 * Foreground service that posts the MediaStyle notification (shade + lock
 * screen) for the active [MediaSessionCompat] owned by [MediaSessionPlugin].
 *
 * The plugin starts this service **once**, while the app is foregrounded (the
 * user pressed play), to satisfy Android 12+'s background foreground-service
 * restriction. After that, a [MediaControllerCompat.Callback] on the session
 * refreshes the notification + manages the foreground/paused transitions
 * whenever the session changes.
 *
 * Transport buttons are delivered to the web player via [MediaBridge]:
 *  - the notification's own action buttons (Android < 13) target this service
 *    directly via `getService` (the service is already running, so no
 *    background `startForegroundService` is needed — which would fail on 12+);
 *  - lock-screen / Android 13+ notification controls / Bluetooth / headset go
 *    through the [MediaSessionCompat] callback instead.
 */
class MediaService : Service() {
  companion object {
    const val CHANNEL_ID = "octave_playback"
    const val NOTIFICATION_ID = 1001

    const val ACTION_PREV = "dev.niruhsa.octave.PREV"
    const val ACTION_PLAY_PAUSE = "dev.niruhsa.octave.PLAY_PAUSE"
    const val ACTION_NEXT = "dev.niruhsa.octave.NEXT"
    const val ACTION_STOP = "dev.niruhsa.octave.STOP"

    @Volatile var session: MediaSessionCompat? = null
    @Volatile var running = false

    /** Ensure the foreground service exists. No-op if already running (later
     *  refreshes happen inside the service via the controller callback). */
    fun start(context: Context) {
      if (running) return
      val intent = Intent(context, MediaService::class.java)
      try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
          context.startForegroundService(intent)
        } else {
          context.startService(intent)
        }
      } catch (e: Exception) {
        // Background foreground-service start restriction (Android 12+). There's
        // nothing we can legally show from the background, so drop it.
      }
    }

    fun stop(context: Context) {
      context.stopService(Intent(context, MediaService::class.java))
    }
  }

  private var controller: MediaControllerCompat? = null
  private var controllerCallback: MediaControllerCompat.Callback? = null
  // The audio lives in the WebView's <audio> element, and track advance is JS
  // (`ended` → load-next → play). With the screen off that hand-off only survives
  // if the device stays awake and the network stays up:
  //  - the wake lock keeps the CPU running across the inter-track gap (otherwise
  //    Doze freezes the WebView and playback stops at the end of a track);
  //  - the WiFi lock keeps the WiFi radio from powering down, and avoids the
  //    next (streamed) track reusing a connection the radio dropped while idle —
  //    downloaded tracks need neither, which is why only streaming stalled.
  // The web player keeps the session PLAYING across the gap, so holding both for
  // the whole PLAYING span bridges it. Both released as soon as we pause/stop.
  private var wakeLock: PowerManager.WakeLock? = null
  private var wifiLock: WifiManager.WifiLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val s = session
    if (s == null) {
      stopSelf()
      return START_NOT_STICKY
    }
    running = true

    // A notification action button (Android < 13) → deliver to the web player.
    when (intent?.action) {
      ACTION_PREV -> { MediaBridge.send("prev"); return START_NOT_STICKY }
      ACTION_PLAY_PAUSE -> { MediaBridge.send("playpause"); return START_NOT_STICKY }
      ACTION_NEXT -> { MediaBridge.send("next"); return START_NOT_STICKY }
      ACTION_STOP -> { MediaBridge.send("stop"); return START_NOT_STICKY }
    }

    createChannel()

    if (controllerCallback == null) {
      val c = MediaControllerCompat(this, s)
      val cb = object : MediaControllerCompat.Callback() {
        override fun onMetadataChanged(metadata: MediaMetadataCompat?) = refresh(s)
        override fun onPlaybackStateChanged(state: PlaybackStateCompat?) = refresh(s)
        override fun onSessionDestroyed() {
          stopSelf()
        }
      }
      c.registerCallback(cb)
      controller = c
      controllerCallback = cb
    }

    refresh(s)
    // Hardware / Bluetooth media buttons routed here via MediaButtonReceiver.
    MediaButtonReceiver.handleIntent(s, intent)
    return START_NOT_STICKY
  }

  /** Rebuild the notification + (re)attach or detach foreground per play state. */
  private fun refresh(s: MediaSessionCompat) {
    val playing = s.controller?.playbackState?.state == PlaybackStateCompat.STATE_PLAYING
    if (playing) acquireLocks() else releaseLocks()
    val notif = buildNotification(s, playing)
    // Always promote to foreground at least once per start. The service is
    // launched via startForegroundService() (MediaService.start on O+), which
    // *requires* a matching startForeground() within 5s or Android crashes the
    // app — even when we're paused (e.g. a queue restored on launch but not yet
    // playing). startForeground from within a running service is allowed from
    // any state.
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      startForeground(NOTIFICATION_ID, notif, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
    } else {
      startForeground(NOTIFICATION_ID, notif)
    }
    if (!playing) {
      // Keep the notification but allow swipe-dismiss while paused.
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
        @Suppress("DEPRECATION") stopForeground(STOP_FOREGROUND_DETACH)
      } else {
        @Suppress("DEPRECATION") stopForeground(false)
      }
    }
  }

  /** Keep the CPU + WiFi radio alive while playing (screen-off streaming). */
  private fun acquireLocks() {
    val wl = wakeLock ?: run {
      val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
      pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "octave:playback").also {
        it.setReferenceCounted(false)
        wakeLock = it
      }
    }
    // Re-acquire each refresh so the 6h cap keeps resetting during playback
    // (resetting a held, non-ref-counted lock is cheap). The cap only guards
    // against a leak if a stop is ever missed — pause/stop releases it for real.
    wl.acquire(6 * 60 * 60 * 1000L)

    val wifi = wifiLock ?: run {
      val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
      val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        WifiManager.WIFI_MODE_FULL_LOW_LATENCY
      } else {
        @Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF
      }
      wm.createWifiLock(mode, "octave:playback").also {
        it.setReferenceCounted(false)
        wifiLock = it
      }
    }
    if (!wifi.isHeld) wifi.acquire()
  }

  private fun releaseLocks() {
    wakeLock?.let { if (it.isHeld) it.release() }
    wifiLock?.let { if (it.isHeld) it.release() }
  }

  private fun servicePendingIntent(action: String, reqCode: Int): PendingIntent {
    val intent = Intent(this, MediaService::class.java).setAction(action)
    return PendingIntent.getService(this, reqCode, intent, PendingIntent.FLAG_IMMUTABLE)
  }

  private fun buildNotification(s: MediaSessionCompat, playing: Boolean): Notification {
    val md = s.controller?.metadata
    val title = md?.getString(MediaMetadataCompat.METADATA_KEY_TITLE) ?: ""
    val artist = md?.getString(MediaMetadataCompat.METADATA_KEY_ARTIST) ?: ""
    val album = md?.getString(MediaMetadataCompat.METADATA_KEY_ALBUM)
    val art = md?.getBitmap(MediaMetadataCompat.METADATA_KEY_ALBUM_ART)

    val contentIntent = packageManager.getLaunchIntentForPackage(packageName)?.let {
      PendingIntent.getActivity(
        this, 0, it,
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
      )
    }
    val stopIntent = servicePendingIntent(ACTION_STOP, 4)

    val builder = NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(android.R.drawable.ic_media_play)
      .setContentTitle(title)
      .setContentText(artist)
      .setSubText(album)
      .setLargeIcon(art)
      .setContentIntent(contentIntent)
      .setColor(0xFFE0A84B.toInt())
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setOnlyAlertOnce(true)
      .setShowWhen(false)
      .setOngoing(playing)
      .setDeleteIntent(stopIntent)

    builder.addAction(
      NotificationCompat.Action(
        android.R.drawable.ic_media_previous, "Previous", servicePendingIntent(ACTION_PREV, 1),
      ),
    )
    builder.addAction(
      if (playing) {
        NotificationCompat.Action(
          android.R.drawable.ic_media_pause, "Pause", servicePendingIntent(ACTION_PLAY_PAUSE, 2),
        )
      } else {
        NotificationCompat.Action(
          android.R.drawable.ic_media_play, "Play", servicePendingIntent(ACTION_PLAY_PAUSE, 2),
        )
      },
    )
    builder.addAction(
      NotificationCompat.Action(
        android.R.drawable.ic_media_next, "Next", servicePendingIntent(ACTION_NEXT, 3),
      ),
    )

    builder.setStyle(
      MediaStyle()
        .setMediaSession(s.sessionToken)
        .setShowActionsInCompactView(0, 1, 2)
        .setShowCancelButton(true)
        .setCancelButtonIntent(stopIntent),
    )

    return builder.build()
  }

  private fun createChannel() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
      if (mgr.getNotificationChannel(CHANNEL_ID) == null) {
        val ch = NotificationChannel(CHANNEL_ID, "Playback", NotificationManager.IMPORTANCE_LOW)
        ch.setShowBadge(false)
        ch.lockscreenVisibility = Notification.VISIBILITY_PUBLIC
        ch.setSound(null, null)
        mgr.createNotificationChannel(ch)
      }
    }
  }

  override fun onDestroy() {
    controllerCallback?.let { cb -> controller?.unregisterCallback(cb) }
    controllerCallback = null
    controller = null
    releaseLocks()
    wakeLock = null
    wifiLock = null
    running = false
    super.onDestroy()
  }
}
