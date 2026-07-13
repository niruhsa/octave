//! macOS CoreAudio default-output discovery.
//!
//! The CoreAudio UID remains native-only and is HMAC-derived before local
//! persistence. Like Windows, the system default can differ from an app-level
//! override, so accuracy is `default`.

use std::ffi::{c_char, c_void};
use std::sync::OnceLock;

use tokio::sync::mpsc::UnboundedSender;

use crate::equalizer::model::{AudioOutput, BindingStability, RouteAccuracy, RouteKind};

type AudioObjectId = u32;
type OsStatus = i32;
type UInt32 = u32;
type CfStringRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const SYSTEM_OBJECT: AudioObjectId = 1;
const SELECTOR_DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
const SELECTOR_NAME: u32 = u32::from_be_bytes(*b"lnam");
const SELECTOR_UID: u32 = u32::from_be_bytes(*b"uid ");
const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const ELEMENT_MAIN: u32 = 0;
const UTF8_ENCODING: u32 = 0x0800_0100;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: AudioObjectId,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: UInt32,
        qualifier_data: *const c_void,
        io_data_size: *mut UInt32,
        out_data: *mut c_void,
    ) -> OsStatus;
    fn AudioObjectAddPropertyListener(
        object_id: AudioObjectId,
        address: *const AudioObjectPropertyAddress,
        listener: unsafe extern "C" fn(
            AudioObjectId,
            UInt32,
            *const AudioObjectPropertyAddress,
            *mut c_void,
        ) -> OsStatus,
        client_data: *mut c_void,
    ) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetLength(value: CfStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        value: CfStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(value: CfStringRef);
}

static CHANGE_SENDER: OnceLock<UnboundedSender<()>> = OnceLock::new();

unsafe extern "C" fn default_output_changed(
    _object_id: AudioObjectId,
    _address_count: UInt32,
    _addresses: *const AudioObjectPropertyAddress,
    _client_data: *mut c_void,
) -> OsStatus {
    if let Some(sender) = CHANGE_SENDER.get() {
        let _ = sender.send(());
    }
    0
}

/// Register the CoreAudio default-output property listener. CoreAudio invokes
/// the callback on its own worker; the registration thread stays alive for the
/// application lifetime to make ownership explicit.
pub fn spawn_change_listener(changed: UnboundedSender<()>) {
    if CHANGE_SENDER.set(changed).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("octave-eq-coreaudio".into())
        .spawn(|| {
            let address = AudioObjectPropertyAddress {
                selector: SELECTOR_DEFAULT_OUTPUT,
                scope: SCOPE_GLOBAL,
                element: ELEMENT_MAIN,
            };
            let status = unsafe {
                AudioObjectAddPropertyListener(
                    SYSTEM_OBJECT,
                    &address,
                    default_output_changed,
                    std::ptr::null_mut(),
                )
            };
            if status != 0 {
                tracing::warn!(status, "register macOS default-output listener failed");
                return;
            }
            loop {
                std::thread::park();
            }
        });
}

pub fn query_outputs() -> Result<Vec<AudioOutput>, String> {
    let device = get_object_id(SYSTEM_OBJECT, SELECTOR_DEFAULT_OUTPUT)?;
    if device == 0 {
        return Ok(Vec::new());
    }
    let display_name = get_cf_string(device, SELECTOR_NAME)
        .unwrap_or_else(|_| "System default output".to_string());
    let uid = get_cf_string(device, SELECTOR_UID)?;
    Ok(vec![AudioOutput {
        runtime_id: Some(uid),
        local_endpoint_key: None,
        route_kind: classify_route(&display_name),
        display_name,
        vendor_id: None,
        product_id: None,
        connected: true,
        selected: true,
        accuracy: RouteAccuracy::Default,
        binding_stability: BindingStability::PersistentExact,
    }])
}

fn get_object_id(object: AudioObjectId, selector: u32) -> Result<AudioObjectId, String> {
    let address = AudioObjectPropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut u32).cast(),
        )
    };
    if status == 0 {
        Ok(value)
    } else {
        Err(format!(
            "CoreAudio property {selector:#x} failed ({status})"
        ))
    }
}

fn get_cf_string(object: AudioObjectId, selector: u32) -> Result<String, String> {
    let address = AudioObjectPropertyAddress {
        selector,
        scope: SCOPE_GLOBAL,
        element: ELEMENT_MAIN,
    };
    let mut value: CfStringRef = std::ptr::null();
    let mut size = std::mem::size_of::<CfStringRef>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            std::ptr::null(),
            &mut size,
            (&mut value as *mut CfStringRef).cast(),
        )
    };
    if status != 0 || value.is_null() {
        return Err(format!(
            "CoreAudio string property {selector:#x} failed ({status})"
        ));
    }

    let string = unsafe {
        let length = CFStringGetLength(value);
        let capacity = CFStringGetMaximumSizeForEncoding(length, UTF8_ENCODING) + 1;
        let mut buffer = vec![0u8; capacity.max(1) as usize];
        let ok = CFStringGetCString(value, buffer.as_mut_ptr().cast(), capacity, UTF8_ENCODING);
        CFRelease(value);
        if !ok {
            return Err("convert CoreAudio string to UTF-8".to_string());
        }
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    };
    Ok(string)
}

fn classify_route(name: &str) -> RouteKind {
    let lower = name.to_lowercase();
    if lower.contains("airplay") {
        RouteKind::Airplay
    } else if lower.contains("bluetooth") || lower.contains("airpods") || lower.contains("beats") {
        RouteKind::Bluetooth
    } else if lower.contains("usb") || lower.contains("dac") {
        RouteKind::Usb
    } else if lower.contains("hdmi") || lower.contains("display") {
        RouteKind::Hdmi
    } else if lower.contains("headphone") || lower.contains("headset") {
        RouteKind::Wired
    } else if lower.contains("speaker") || lower.contains("macbook") || lower.contains("imac") {
        RouteKind::Builtin
    } else {
        RouteKind::Unknown
    }
}
