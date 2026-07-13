//! Android `AudioManager` bridge.

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};

use crate::equalizer::model::{AudioOutput, BindingStability, RouteAccuracy, RouteKind};
use crate::player::server::MediaServer;

pub struct AudioRouteHandle<R: Runtime>(pub std::sync::Mutex<Option<PluginHandle<R>>>);

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("audio-route")
        .setup(|app, api| {
            let handle =
                Some(api.register_android_plugin("dev.niruhsa.octave", "AudioRoutePlugin")?);
            app.manage(AudioRouteHandle(std::sync::Mutex::new(handle)));
            Ok(())
        })
        .build()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeAudioOutput {
    runtime_id: Option<String>,
    display_name: String,
    route_kind: RouteKind,
    vendor_id: Option<String>,
    product_id: Option<String>,
    connected: bool,
    selected: bool,
    accuracy: RouteAccuracy,
    binding_stability: BindingStability,
}

impl From<NativeAudioOutput> for AudioOutput {
    fn from(value: NativeAudioOutput) -> Self {
        Self {
            runtime_id: value.runtime_id,
            local_endpoint_key: None,
            display_name: value.display_name,
            route_kind: value.route_kind,
            vendor_id: value.vendor_id,
            product_id: value.product_id,
            connected: value.connected,
            selected: value.selected,
            accuracy: value.accuracy,
            binding_stability: value.binding_stability,
        }
    }
}

#[derive(Deserialize)]
struct OutputsResponse {
    outputs: Vec<NativeAudioOutput>,
}

fn with_handle<R: Runtime, T>(
    app: &AppHandle<R>,
    f: impl FnOnce(&PluginHandle<R>) -> Result<T, String>,
) -> Result<T, String> {
    let state = app.state::<AudioRouteHandle<R>>();
    let guard = state
        .0
        .lock()
        .map_err(|_| "audio-route plugin handle poisoned".to_string())?;
    let handle = guard
        .as_ref()
        .ok_or_else(|| "audio-route plugin is unavailable".to_string())?;
    f(handle)
}

pub fn query_outputs<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<AudioOutput>, String> {
    with_handle(app, |handle| {
        handle
            .run_mobile_plugin::<OutputsResponse>("listOutputs", ())
            .map(|r| r.outputs.into_iter().map(Into::into).collect())
            .map_err(|e| format!("query Android audio outputs: {e}"))
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureCallback<'a> {
    callback_base_url: &'a str,
}

pub fn configure_callback<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let media = app.state::<MediaServer>();
    let base = crate::player::server::action_base_url(media.port, &media.token);
    with_handle(app, |handle| {
        handle
            .run_mobile_plugin::<serde_json::Value>(
                "configureCallback",
                ConfigureCallback {
                    callback_base_url: &base,
                },
            )
            .map(|_| ())
            .map_err(|e| format!("configure Android audio-route callback: {e}"))
    })
}
