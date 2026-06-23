package dev.niruhsa.music.app

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.SystemClock
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin
import java.net.URL
import kotlin.concurrent.thread

@InvokeArg
class UpdateArgs {
  lateinit var title: String
  var artist: String = ""
  var album: String = ""
  var artworkUrl: String? = null
  var actionBaseUrl: String = ""
  var durationMs: Long = 0
  var positionMs: Long = 0
  var playing: Boolean = false
}

@InvokeArg
class PlaybackArgs {
  var positionMs: Long = 0
  var durationMs: Long = 0
  var playing: Boolean = false
}

/**
 * Native media session + system notification bridge (paired with the Rust
 * `media_session` plugin). A bare WebView doesn't surface the Web Media Session
 * API to Android's system notification, so the native side owns a
 * [MediaSessionCompat] and a foreground [MediaService] that posts the MediaStyle
 * notification (shade + lock screen). Transport callbacks (lock screen, A13+
 * notification controls, Bluetooth, headset) are delivered back to the web
 * player via [MediaBridge].
 */
@TauriPlugin
class MediaSessionPlugin(private val activity: Activity) : Plugin(activity) {
  private var session: MediaSessionCompat? = null
  private var currentArtUrl: String? = null
  private var currentArt: Bitmap? = null

  override fun load(webView: WebView) {
    ensureSession()
  }

  private fun ensureSession(): MediaSessionCompat {
    session?.let { return it }
    val ctx = activity.applicationContext
    val s = MediaSessionCompat(ctx, "OctaveMedia")
    s.setCallback(object : MediaSessionCompat.Callback() {
      override fun onPlay() = MediaBridge.send("playpause")
      override fun onPause() = MediaBridge.send("playpause")
      override fun onSkipToNext() = MediaBridge.send("next")
      override fun onSkipToPrevious() = MediaBridge.send("prev")
      override fun onStop() = MediaBridge.send("stop")
      override fun onSeekTo(pos: Long) = MediaBridge.send("seek", pos)
    })
    session = s
    MediaService.session = s
    return s
  }

  // Commands are dispatched off the main thread; MediaSessionCompat must be
  // touched on the thread it was created on (main), so the work is marshalled
  // onto the UI thread. `resolve()` fires immediately — JS doesn't await the
  // native side.

  @Command
  fun update(invoke: Invoke) {
    val args = invoke.parseArgs(UpdateArgs::class.java)
    if (args.actionBaseUrl.isNotEmpty()) {
      MediaBridge.actionBaseUrl = args.actionBaseUrl
    }
    activity.runOnUiThread {
      val s = ensureSession()

      // Fetch artwork off the main thread when it changes, then re-apply the
      // metadata (with the bitmap) and refresh the notification.
      if (args.artworkUrl != currentArtUrl) {
        currentArtUrl = args.artworkUrl
        currentArt = null
        val url = args.artworkUrl
        if (url != null) {
          thread {
            val bmp = try {
              URL(url).openStream().use { BitmapFactory.decodeStream(it) }
            } catch (e: Exception) {
              null
            }
            activity.runOnUiThread {
              if (currentArtUrl == url) {
                currentArt = bmp
                applyMetadata(s, args)
                MediaService.start(activity.applicationContext)
              }
            }
          }
        }
      }

      applyMetadata(s, args)
      applyPlayback(s, args.playing, args.positionMs)
      s.isActive = true
      MediaService.start(activity.applicationContext)
    }
    invoke.resolve()
  }

  @Command
  fun setPlayback(invoke: Invoke) {
    val args = invoke.parseArgs(PlaybackArgs::class.java)
    activity.runOnUiThread {
      val s = ensureSession()
      applyPlayback(s, args.playing, args.positionMs)
      MediaService.start(activity.applicationContext)
    }
    invoke.resolve()
  }

  @Command
  fun clear(invoke: Invoke) {
    activity.runOnUiThread {
      session?.let {
        applyPlayback(it, false, 0)
        it.isActive = false
      }
      currentArtUrl = null
      currentArt = null
      MediaService.stop(activity.applicationContext)
    }
    invoke.resolve()
  }

  private fun applyMetadata(s: MediaSessionCompat, args: UpdateArgs) {
    val b = MediaMetadataCompat.Builder()
      .putString(MediaMetadataCompat.METADATA_KEY_TITLE, args.title)
      .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, args.artist)
      .putString(MediaMetadataCompat.METADATA_KEY_ALBUM, args.album)
      .putLong(MediaMetadataCompat.METADATA_KEY_DURATION, args.durationMs)
    currentArt?.let {
      b.putBitmap(MediaMetadataCompat.METADATA_KEY_ALBUM_ART, it)
      b.putBitmap(MediaMetadataCompat.METADATA_KEY_DISPLAY_ICON, it)
    }
    s.setMetadata(b.build())
  }

  private fun applyPlayback(s: MediaSessionCompat, playing: Boolean, positionMs: Long) {
    val state = if (playing) PlaybackStateCompat.STATE_PLAYING else PlaybackStateCompat.STATE_PAUSED
    val actions = PlaybackStateCompat.ACTION_PLAY or
      PlaybackStateCompat.ACTION_PAUSE or
      PlaybackStateCompat.ACTION_PLAY_PAUSE or
      PlaybackStateCompat.ACTION_SKIP_TO_NEXT or
      PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS or
      PlaybackStateCompat.ACTION_SEEK_TO or
      PlaybackStateCompat.ACTION_STOP
    // position + 1.0x speed + the current clock lets the system extrapolate the
    // scrubber between updates, so we don't push on every tick.
    val ps = PlaybackStateCompat.Builder()
      .setActions(actions)
      .setState(state, positionMs, 1.0f, SystemClock.elapsedRealtime())
      .build()
    s.setPlaybackState(ps)
  }
}
