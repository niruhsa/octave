package dev.niruhsa.octave

import android.app.Activity
import android.media.AudioAttributes
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class ConfigureAudioRouteCallbackArgs {
  var callbackBaseUrl: String = ""
}

/**
 * Native Android output discovery for the equalizer.
 *
 * Android reliably reports connected output devices. API 33 also exposes a
 * media-attributes query that predicts which devices would be used for media;
 * because Octave's audio is owned by a WebView rather than an AudioTrack we
 * label that result `predicted`, never `exact`. Older releases return
 * `connected_only`, which the resolver may use only for explicitly opted-in
 * connected-device rules.
 */
@TauriPlugin
class AudioRoutePlugin(private val activity: Activity) : Plugin(activity) {
  private val audioManager: AudioManager by lazy {
    activity.getSystemService(AudioManager::class.java)
  }

  private val callback = object : AudioDeviceCallback() {
    override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>) {
      AudioRouteBridge.routeChanged()
    }

    override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>) {
      AudioRouteBridge.routeChanged()
    }
  }

  override fun load(webView: WebView) {
    audioManager.registerAudioDeviceCallback(callback, Handler(Looper.getMainLooper()))
  }

  @Command
  fun configureCallback(invoke: Invoke) {
    val args = invoke.parseArgs(ConfigureAudioRouteCallbackArgs::class.java)
    AudioRouteBridge.callbackBaseUrl = args.callbackBaseUrl.takeIf { it.isNotBlank() }
    invoke.resolve()
    // Close the startup race: force one authenticated signal after the loopback
    // callback is configured so Rust immediately queries the current route.
    AudioRouteBridge.routeChanged()
  }

  @Command
  fun currentOutput(invoke: Invoke) {
    val selectedIds = predictedMediaDeviceIds()
    val outputs = outputDevices(selectedIds)
    // `getAudioDevicesForAttributes` may return multiple candidates. Their
    // order is not a routing priority, so only claim a selected output when
    // the prediction is unambiguous; otherwise native uses explicit
    // connected-fallback rules or the default/manual profile.
    val current = selectedIds.singleOrNull()?.let { selectedId ->
      outputs.firstOrNull { it.first == selectedId }?.second
    }
    val ret = JSObject()
    ret.put("output", current)
    invoke.resolve(ret)
  }

  @Command
  fun listOutputs(invoke: Invoke) {
    val selectedIds = predictedMediaDeviceIds()
    val array = JSArray()
    outputDevices(selectedIds).forEach { array.put(it.second) }
    val ret = JSObject()
    ret.put("outputs", array)
    invoke.resolve(ret)
  }

  /** Returns stable-for-this-connection Android ids only inside native code. */
  private fun outputDevices(selectedIds: Set<Int>): List<Pair<Int, JSObject>> {
    val devices = audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
    return devices
      .filter { isUsefulOutput(it.type) }
      .map { device ->
        val selected = selectedIds.size == 1 && selectedIds.contains(device.id)
        val accuracy = when {
          Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && selected -> "predicted"
          else -> "connected_only"
        }
        val value = JSObject()
        value.put("runtimeId", device.id.toString())
        value.put("displayName", device.productName?.toString()?.takeIf { it.isNotBlank() } ?: routeLabel(device.type))
        value.put("routeKind", routeKind(device.type))
        value.put("vendorId", null)
        value.put("productId", null)
        value.put("connected", true)
        value.put("selected", selected)
        value.put("accuracy", accuracy)
        // AudioDeviceInfo.id is not documented stable across reboot/reconnect.
        value.put("bindingStability", "session_only")
        Pair(device.id, value)
      }
  }

  private fun predictedMediaDeviceIds(): Set<Int> {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return emptySet()
    return try {
      val attributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
        .build()
      audioManager.getAudioDevicesForAttributes(attributes).map { it.id }.toSet()
    } catch (_: Exception) {
      emptySet()
    }
  }

  private fun isUsefulOutput(type: Int): Boolean = when (type) {
    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE,
    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
    AudioDeviceInfo.TYPE_WIRED_HEADSET,
    AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
    AudioDeviceInfo.TYPE_USB_DEVICE,
    AudioDeviceInfo.TYPE_USB_ACCESSORY,
    AudioDeviceInfo.TYPE_USB_HEADSET,
    AudioDeviceInfo.TYPE_HDMI,
    AudioDeviceInfo.TYPE_HDMI_ARC,
    AudioDeviceInfo.TYPE_LINE_ANALOG,
    AudioDeviceInfo.TYPE_LINE_DIGITAL,
    AudioDeviceInfo.TYPE_HEARING_AID,
    AudioDeviceInfo.TYPE_BLE_HEADSET,
    AudioDeviceInfo.TYPE_BLE_SPEAKER,
    AudioDeviceInfo.TYPE_BLE_BROADCAST -> true
    else -> false
  }

  private fun routeKind(type: Int): String = when (type) {
    AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
    AudioDeviceInfo.TYPE_HEARING_AID,
    AudioDeviceInfo.TYPE_BLE_HEADSET,
    AudioDeviceInfo.TYPE_BLE_SPEAKER,
    AudioDeviceInfo.TYPE_BLE_BROADCAST -> "bluetooth"
    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
    AudioDeviceInfo.TYPE_WIRED_HEADSET,
    AudioDeviceInfo.TYPE_LINE_ANALOG,
    AudioDeviceInfo.TYPE_LINE_DIGITAL -> "wired"
    AudioDeviceInfo.TYPE_USB_DEVICE,
    AudioDeviceInfo.TYPE_USB_ACCESSORY,
    AudioDeviceInfo.TYPE_USB_HEADSET -> "usb"
    AudioDeviceInfo.TYPE_HDMI,
    AudioDeviceInfo.TYPE_HDMI_ARC -> "hdmi"
    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> "builtin"
    else -> "unknown"
  }

  private fun routeLabel(type: Int): String = when (routeKind(type)) {
    "bluetooth" -> "Bluetooth audio"
    "wired" -> "Wired audio"
    "usb" -> "USB audio"
    "hdmi" -> "HDMI audio"
    "builtin" -> "Built-in audio"
    else -> "Audio output"
  }
}
