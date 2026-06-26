package dev.niruhsa.octave

import android.util.Log
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * Receives FCM messages (Phase 10 — real-time notifications).
 *
 * The server sends **notification messages**, which the OS auto-displays in the
 * system tray when the app is backgrounded or **swiped away** — without the app
 * process running, and without calling [onMessageReceived]. That auto-display is
 * the whole point: notifications arrive while the app is closed.
 *
 * [onMessageReceived] only fires while the app is in the **foreground** (or for
 * data-only messages). There, the in-app JS poller already surfaces the
 * notification + updates the badge, so we intentionally do nothing here to avoid
 * a duplicate.
 *
 * The auto-displayed notification uses the channel + icon declared via the
 * `com.google.firebase.messaging.default_notification_*` manifest meta-data; the
 * channel itself is created at app startup (see MainActivity).
 */
class OctaveFirebaseMessagingService : FirebaseMessagingService() {
  override fun onNewToken(token: String) {
    // The app re-fetches + re-registers the current token on next foreground
    // (push_register), and the server prunes a stale token on the first failed
    // send, so there's nothing to do here but note it.
    Log.i(TAG, "FCM registration token rotated")
  }

  override fun onMessageReceived(message: RemoteMessage) {
    // Foreground only — the in-app poller handles surfacing + the badge.
    Log.i(TAG, "FCM message received in foreground; in-app poller will surface it")
  }

  companion object {
    private const val TAG = "OctavePush"
  }
}
