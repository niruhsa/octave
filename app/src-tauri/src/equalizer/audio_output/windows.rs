//! Windows Core Audio default-render endpoint discovery.
//!
//! Octave cannot observe a Windows Volume Mixer per-app override, so even the
//! selected endpoint is reported as `default`, not `exact`.

use windows::core::{implement, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};

use tokio::sync::mpsc::UnboundedSender;

use crate::equalizer::model::{AudioOutput, BindingStability, RouteAccuracy, RouteKind};

struct ComApartment(bool);

impl ComApartment {
    unsafe fn initialize() -> Self {
        // S_OK and S_FALSE both mean this thread may use COM. RPC_E_CHANGED_MODE
        // means it was already initialized with another apartment model; Core
        // Audio remains callable, but we must not uninitialize it here.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(result.is_ok())
    }
}

#[implement(IMMNotificationClient)]
struct EndpointNotifications {
    changed: UnboundedSender<()>,
}

impl EndpointNotifications {
    fn signal(&self) {
        let _ = self.changed.send(());
    }
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for EndpointNotifications_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        self.signal();
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        self.signal();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        self.signal();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.signal();
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        self.signal();
        Ok(())
    }
}

/// Register Core Audio endpoint notifications on a dedicated COM thread.
/// The thread and callback intentionally live for the process lifetime.
pub fn spawn_change_listener(changed: UnboundedSender<()>) {
    let _ = std::thread::Builder::new()
        .name("octave-eq-core-audio".into())
        .spawn(move || {
            let result = (|| -> windows::core::Result<()> {
                let _apartment = unsafe { ComApartment::initialize() };
                let enumerator: IMMDeviceEnumerator =
                    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
                let callback: IMMNotificationClient = EndpointNotifications { changed }.into();
                unsafe { enumerator.RegisterEndpointNotificationCallback(&callback)? };
                // `enumerator` + `callback` must remain alive for callbacks.
                loop {
                    std::thread::park();
                }
                #[allow(unreachable_code)]
                windows::core::Result::Ok(())
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "register Windows audio endpoint listener failed");
            }
        });
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

pub fn query_outputs() -> Result<Vec<AudioOutput>, String> {
    unsafe { query_outputs_inner() }.map_err(|e| format!("Core Audio output query: {e}"))
}

unsafe fn query_outputs_inner() -> windows::core::Result<Vec<AudioOutput>> {
    let _apartment = unsafe { ComApartment::initialize() };
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let default = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }.ok();
    let default_id = default
        .as_ref()
        .and_then(|device| unsafe { endpoint_id(device) }.ok());
    let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { collection.GetCount()? };
    let mut outputs = Vec::with_capacity(count as usize);
    for index in 0..count {
        let device = unsafe { collection.Item(index)? };
        let runtime_id = unsafe { endpoint_id(&device)? };
        let display_name = unsafe { friendly_name(&device) }
            .unwrap_or_else(|_| "Windows audio output".to_string());
        let selected = default_id.as_deref() == Some(runtime_id.as_str());
        outputs.push(AudioOutput {
            runtime_id: Some(runtime_id),
            local_endpoint_key: None,
            route_kind: classify_route(&display_name),
            display_name,
            vendor_id: None,
            product_id: None,
            connected: true,
            selected,
            accuracy: if selected {
                RouteAccuracy::Default
            } else {
                RouteAccuracy::ConnectedOnly
            },
            // Windows endpoint IDs are stable across restart/reconnect unless
            // the endpoint is removed/reinstalled. Only an HMAC of this value
            // is persisted by the EQ service.
            binding_stability: BindingStability::PersistentExact,
        });
    }
    Ok(outputs)
}

unsafe fn endpoint_id(device: &IMMDevice) -> windows::core::Result<String> {
    let value = unsafe { device.GetId()? };
    let result = unsafe { value.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(value.as_ptr().cast())) };
    Ok(result)
}

unsafe fn friendly_name(device: &IMMDevice) -> windows::core::Result<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ)? };
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName)? };
    let text = unsafe { PropVariantToStringAlloc(&value)? };
    let result = unsafe { text.to_string() }.unwrap_or_else(|_| "Windows audio output".into());
    unsafe { CoTaskMemFree(Some(text.as_ptr().cast())) };
    Ok(result)
}

fn classify_route(name: &str) -> RouteKind {
    let lower = name.to_lowercase();
    if lower.contains("bluetooth") || lower.contains("a2dp") || lower.contains("hands-free") {
        RouteKind::Bluetooth
    } else if lower.contains("usb") || lower.contains("dac") {
        RouteKind::Usb
    } else if lower.contains("hdmi") || lower.contains("display audio") {
        RouteKind::Hdmi
    } else if lower.contains("headphone") || lower.contains("headset") {
        RouteKind::Wired
    } else if lower.contains("speaker") || lower.contains("built-in") || lower.contains("realtek") {
        RouteKind::Builtin
    } else {
        RouteKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_endpoint_labels() {
        assert_eq!(classify_route("WH-1000XM5 Bluetooth"), RouteKind::Bluetooth);
        assert_eq!(classify_route("USB DAC"), RouteKind::Usb);
        assert_eq!(classify_route("NVIDIA HDMI Output"), RouteKind::Hdmi);
        assert_eq!(classify_route("Headphones"), RouteKind::Wired);
        assert_eq!(classify_route("Speakers (Realtek)"), RouteKind::Builtin);
    }
}
