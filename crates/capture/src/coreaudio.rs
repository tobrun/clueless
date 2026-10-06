//! The one unsafe lookup of an audio device's transport type, by device UID.
//!
//! cpal hands out the CoreAudio device UID (`device.id()`); to ask
//! CoreAudio for the transport we enumerate `kAudioHardwarePropertyDevices`
//! and read each device's `kAudioDevicePropertyDeviceUID` until one matches
//! (the direct `kAudioHardwarePropertyTranslateUIDToDevice` property answers
//! `'what'` on current macOS for both C and Swift callers, so it is not
//! used). Declarations are hand-rolled in the style of `sck.rs`; the
//! frameworks link through cpal's CoreAudio dependency.

use std::ffi::c_void;
use std::os::raw::c_char;

use crate::mic_select::{K_TRANSPORT_UNKNOWN, Transport, classify};

/// `AudioObjectPropertyAddress` (CoreAudio Base).
#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

// CoreAudio object ids, property selectors and constants.
// `kAudioObjectSystemObject` is 1 (per Apple's own bindings), not `'sys '`.
const K_AUDIO_OBJECT_SYSTEM: u32 = 1;
const K_AUDIO_OBJECT_SCOPE_GLOBAL: u32 = 0;
const K_AUDIO_OBJECT_ELEMENT_MAIN: u32 = 0;
const K_AUDIO_HARDWARE_PROPERTY_DEVICES: u32 = crate::mic_select::fourcc(*b"dev#");
const K_AUDIO_DEVICE_PROPERTY_DEVICE_UID: u32 = crate::mic_select::fourcc(*b"uid ");
const K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE: u32 = crate::mic_select::fourcc(*b"tran");
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier_data: *mut c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectGetPropertyDataSize(
        object_id: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier_data: *mut c_void,
        data_size: *mut u32,
    ) -> i32;
    fn CFStringGetCString(
        cf: *const c_void,
        buffer: *mut c_char,
        buffer_size: i64,
        encoding: u32,
    ) -> bool;
    fn CFStringGetLength(cf: *const c_void) -> i64;
    fn CFRelease(cf: *const c_void);
}

/// Read a u32 property, `None` when CoreAudio refuses.
fn get_u32(object: u32, selector: u32) -> Option<u32> {
    let address = Address {
        selector,
        scope: K_AUDIO_OBJECT_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_ELEMENT_MAIN,
    };
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address,
            0,
            std::ptr::null_mut(),
            &mut size,
            &mut value as *mut u32 as *mut c_void,
        )
    };
    (status == 0).then_some(value)
}

/// All live AudioDeviceIDs.
fn device_ids() -> Vec<u32> {
    let address = Address {
        selector: K_AUDIO_HARDWARE_PROPERTY_DEVICES,
        scope: K_AUDIO_OBJECT_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_ELEMENT_MAIN,
    };
    // The size must come from AudioObjectGetPropertyDataSize: the
    // GetPropertyData-with-size-0 idiom answers "0 bytes" on current macOS.
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            K_AUDIO_OBJECT_SYSTEM,
            &address,
            0,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0u32; size as usize / std::mem::size_of::<u32>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM,
            &address,
            0,
            std::ptr::null_mut(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids
}

/// The CoreAudio device UID string of one device, via the CFString
/// property (fast ptr, then bytes fallback, as Apple prescribes).
fn uid_of_device(device: u32) -> Option<String> {
    let address = Address {
        selector: K_AUDIO_DEVICE_PROPERTY_DEVICE_UID,
        scope: K_AUDIO_OBJECT_SCOPE_GLOBAL,
        element: K_AUDIO_OBJECT_ELEMENT_MAIN,
    };
    let mut cf: *const c_void = std::ptr::null();
    let mut size = std::mem::size_of::<*const c_void>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &address,
            0,
            std::ptr::null_mut(),
            &mut size,
            &mut cf as *mut *const c_void as *mut c_void,
        )
    };
    if status != 0 || cf.is_null() {
        return None;
    }
    let text = unsafe {
        // Copy into a Rust buffer and read it in place. (CString::from_raw
        // would try to free this Rust allocation through libc free - UB.)
        let len = CFStringGetLength(cf);
        let capacity = len * 4 + 1;
        let mut buffer = vec![0i8; capacity as usize];
        let ok = CFStringGetCString(cf, buffer.as_mut_ptr(), capacity, K_CF_STRING_ENCODING_UTF8);
        if ok {
            std::ffi::CStr::from_ptr(buffer.as_ptr())
                .to_str()
                .ok()
                .map(str::to_owned)
        } else {
            None
        }
    };
    unsafe { CFRelease(cf) };
    text
}

/// The transport of the input device with this CoreAudio UID, `None` when
/// CoreAudio cannot say (device unplugged mid-call, property unsupported).
///
/// cpal decorates the raw uid with its `coreaudio:` host prefix; some
/// devices' raw uids themselves end in `:input` / `:output`. Match either.
pub fn transport_of_uid(cpal_uid: &str) -> Option<Transport> {
    let raw = cpal_uid.strip_prefix("coreaudio:").unwrap_or(cpal_uid);
    for device in device_ids() {
        let matches = uid_of_device(device).is_some_and(|uid| {
            uid == raw || uid == format!("{raw}:input") || uid == format!("{raw}:output")
        });
        if matches {
            let raw_transport = get_u32(device, K_AUDIO_DEVICE_PROPERTY_TRANSPORT_TYPE)?;
            if raw_transport == K_TRANSPORT_UNKNOWN {
                return None;
            }
            return Some(classify(raw_transport));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    #[test]
    fn the_real_host_answers_for_its_own_device_uids() {
        // Open the real host and ask for every input device's transport by
        // its cpal-wrapped UID: the system must answer for every device that
        // is currently listed, and the built-in mic must report as BuiltIn.
        let host = cpal::default_host();
        let Ok(devices) = host.input_devices() else {
            return; // no inputs at all on this machine
        };
        for device in devices {
            let uid = device.id().map(|id| id.to_string()).unwrap_or_default();
            let name = crate::mic::device_name(&device);
            let transport = transport_of_uid(&uid);
            if name.starts_with("MacBook Pro Microphone") {
                assert_eq!(transport, Some(Transport::BuiltIn), "for {name} ({uid})");
            }
            // a listed device is a live device; CoreAudio knows its transport
            assert!(transport.is_some(), "no transport for {name} ({uid})");
        }
    }

    #[test]
    fn an_unknown_uid_reports_none() {
        assert!(transport_of_uid("not-a-coreaudio-uid").is_none());
    }
}
