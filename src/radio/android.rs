// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Getting at a USB adapter from an Android app, with no Java source.
//!
//! Android will not let an app open a USB device by path. It hands out a file
//! descriptor for a device the user has granted, through `UsbManager`, and
//! libusb adopts that descriptor - which is the whole reason an unrooted
//! phone can run this driver at all.
//!
//! All of it is reached by reflection through JNI. That keeps the promise the
//! rest of this crate makes: one Rust crate, no Java sources, no dex, no
//! Gradle. It costs the verbosity below, which is the price of calling a
//! framework API by name and signature rather than by import.
//!
//! ```text
//!   getSystemService("usb")  -> UsbManager
//!   getDeviceList()          -> the devices plugged in
//!   getVendorId/ProductId    -> is this one an adapter we can drive
//!   hasPermission            -> may we touch it
//!   requestPermission        -> the system dialog, if not
//!   openDevice               -> UsbDeviceConnection
//!   getFileDescriptor        -> the int libusb wants
//! ```
//!
//! The permission dance is the part with a trick in it. `requestPermission`
//! answers by broadcasting to a `PendingIntent`, and receiving a broadcast is
//! the one thing that would need a Java class. It is not needed: the answer
//! also lands in `hasPermission`, so this asks and then polls. The
//! `PendingIntent` still has to exist and be well formed, because the system
//! validates it before showing anything.

use std::time::{Duration, Instant};

use jni::objects::{JObject, JValue};
use jni::JavaVM;

/// How long to wait for the user to answer the permission dialog.
///
/// Long enough to find the phone and read the prompt, short enough that a
/// dialog which never appeared - the usual sign of a device that went away
/// again - does not hang the app forever.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(60);

/// How often to ask whether permission has been granted yet.
const POLL: Duration = Duration::from_millis(200);

/// The action on the intent the system broadcasts its answer to. Nothing
/// receives it; it only has to be ours.
const PERMISSION_ACTION: &str = "rs.drone.app.USB_PERMISSION";

/// An open connection to a USB device, and the descriptor libusb wants.
///
/// The connection has to be held: dropping the Java object closes the
/// descriptor underneath libusb, and the failure that produces is a driver
/// that opens successfully and then reads nothing.
pub struct UsbHandle {
    fd: i32,
    connection: jni::objects::GlobalRef,
    vm: JavaVM,
    pub vid: u16,
    pub pid: u16,
}

impl UsbHandle {
    pub fn fd(&self) -> i32 {
        self.fd
    }
}

impl Drop for UsbHandle {
    fn drop(&mut self) {
        let Ok(mut env) = self.vm.attach_current_thread() else {
            return;
        };
        // Closing releases the descriptor and the interface claim. Skipping
        // it leaves the device unusable until the app is killed, which on a
        // phone means until the user notices and does it by hand.
        let _ = env.call_method(&self.connection, "close", "()V", &[]);
    }
}

/// Find a supported adapter, ask for permission if needed, and open it.
///
/// `wanted` pins one USB id; `None` accepts any adapter this build can drive.
pub fn open_adapter(wanted: Option<(u16, u16)>) -> Result<UsbHandle, String> {
    let context = ndk_context::android_context();
    if context.vm().is_null() || context.context().is_null() {
        return Err("no Android context: this is not running in an activity".into());
    }

    // SAFETY: the pointers come from the activity glue, which owns them for
    // the life of the process.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }
        .map_err(|err| format!("cannot reach the JVM: {err}"))?;
    let activity = unsafe { JObject::from_raw(context.context().cast()) };

    let mut env = vm
        .attach_current_thread()
        .map_err(|err| format!("cannot attach to the JVM: {err}"))?;

    // Built before the call rather than inside its argument list: the
    // argument borrows `env` immutably while `call_method` needs it mutably.
    let name: JObject<'_> = env.new_string("usb").map_err(jerr)?.into();
    let manager = env
        .call_method(
            &activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&name)],
        )
        .and_then(|v| v.l())
        .map_err(|err| format!("no USB service on this device: {err}"))?;

    let supported = super::ffi::supported_ids();
    let devices = device_list(&mut env, &manager)?;
    if devices.is_empty() {
        return Err("nothing is plugged into the USB port".into());
    }

    let mut seen = Vec::new();
    for device in devices {
        let vid = call_int(&mut env, &device, "getVendorId")? as u16;
        let pid = call_int(&mut env, &device, "getProductId")? as u16;
        seen.push(format!("{vid:04x}:{pid:04x}"));

        let wanted = match wanted {
            Some(id) => id == (vid, pid),
            None => supported.contains(&(vid, pid)),
        };
        if !wanted {
            continue;
        }

        log::info!("radio: found {vid:04x}:{pid:04x} on USB");
        ensure_permission(&mut env, &manager, &activity, &device)?;
        return open_device(&mut env, &vm, &manager, &device, vid, pid);
    }

    Err(format!(
        "no supported adapter among the USB devices attached ({})",
        seen.join(", ")
    ))
}

/// The values of `UsbManager.getDeviceList()`, which is a map keyed by path.
fn device_list<'a>(
    env: &mut jni::JNIEnv<'a>,
    manager: &JObject<'a>,
) -> Result<Vec<JObject<'a>>, String> {
    let map = env
        .call_method(manager, "getDeviceList", "()Ljava/util/HashMap;", &[])
        .and_then(|v| v.l())
        .map_err(|err| format!("cannot list USB devices: {err}"))?;

    let values = env
        .call_method(&map, "values", "()Ljava/util/Collection;", &[])
        .and_then(|v| v.l())
        .map_err(jerr)?;
    let iterator = env
        .call_method(&values, "iterator", "()Ljava/util/Iterator;", &[])
        .and_then(|v| v.l())
        .map_err(jerr)?;

    let mut devices = Vec::new();
    loop {
        let has_next = env
            .call_method(&iterator, "hasNext", "()Z", &[])
            .and_then(|v| v.z())
            .map_err(jerr)?;
        if !has_next {
            break;
        }
        let device = env
            .call_method(&iterator, "next", "()Ljava/lang/Object;", &[])
            .and_then(|v| v.l())
            .map_err(jerr)?;
        devices.push(device);
    }
    Ok(devices)
}

/// Ask for permission if the app does not already have it, then wait.
fn ensure_permission(
    env: &mut jni::JNIEnv<'_>,
    manager: &JObject<'_>,
    activity: &JObject<'_>,
    device: &JObject<'_>,
) -> Result<(), String> {
    if has_permission(env, manager, device)? {
        return Ok(());
    }

    log::info!("radio: asking the user for USB permission");
    let intent = permission_intent(env, activity)?;
    env.call_method(
        manager,
        "requestPermission",
        "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
        &[JValue::Object(device), JValue::Object(&intent)],
    )
    .map_err(|err| format!("cannot ask for USB permission: {err}"))?;

    // The system answers by broadcast, which would need a Java receiver to
    // hear. Polling the same state the broadcast reports avoids that
    // entirely, at the cost of noticing up to one poll interval late.
    let deadline = Instant::now() + PERMISSION_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(POLL);
        if has_permission(env, manager, device)? {
            return Ok(());
        }
    }
    Err("USB permission was not granted".into())
}

fn has_permission(
    env: &mut jni::JNIEnv<'_>,
    manager: &JObject<'_>,
    device: &JObject<'_>,
) -> Result<bool, String> {
    env.call_method(
        manager,
        "hasPermission",
        "(Landroid/hardware/usb/UsbDevice;)Z",
        &[JValue::Object(device)],
    )
    .and_then(|v| v.z())
    .map_err(|err| format!("cannot check USB permission: {err}"))
}

/// Build the `PendingIntent` `requestPermission` answers to.
///
/// Two details the system checks and that are easy to get wrong:
///
/// - the intent must name this package. An implicit broadcast intent has not
///   been deliverable since Android 12, and the request is rejected outright.
/// - from Android 12 a `PendingIntent` must say whether it is mutable. The
///   system fills in which device was granted, so it must be mutable, even
///   though nothing here reads those extras.
fn permission_intent<'local>(
    env: &mut jni::JNIEnv<'local>,
    activity: &JObject<'_>,
) -> Result<JObject<'local>, String> {
    let action = env.new_string(PERMISSION_ACTION).map_err(jerr)?;
    let intent = env
        .new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action.into())],
        )
        .map_err(|err| format!("cannot build the permission intent: {err}"))?;

    let package = env
        .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .map_err(jerr)?;
    env.call_method(
        &intent,
        "setPackage",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[JValue::Object(&package)],
    )
    .map_err(jerr)?;

    // FLAG_MUTABLE, and only from the release that has it.
    let flags = if sdk_int(env)? >= 31 { 0x0200_0000 } else { 0 };

    env.call_static_method(
        "android/app/PendingIntent",
        "getBroadcast",
        "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
        &[
            JValue::Object(activity),
            JValue::Int(0),
            JValue::Object(&intent),
            JValue::Int(flags),
        ],
    )
    .and_then(|v| v.l())
    .map_err(|err| format!("cannot build the permission callback: {err}"))
}

/// Open the device and take its file descriptor.
fn open_device(
    env: &mut jni::JNIEnv<'_>,
    vm: &JavaVM,
    manager: &JObject<'_>,
    device: &JObject<'_>,
    vid: u16,
    pid: u16,
) -> Result<UsbHandle, String> {
    let connection = env
        .call_method(
            manager,
            "openDevice",
            "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
            &[JValue::Object(device)],
        )
        .and_then(|v| v.l())
        .map_err(|err| format!("cannot open the adapter: {err}"))?;

    if connection.is_null() {
        return Err("the adapter could not be opened; it may be in use".into());
    }

    let fd = call_int(env, &connection, "getFileDescriptor")?;
    if fd < 0 {
        return Err("the adapter gave no file descriptor".into());
    }

    // A global reference, because the connection has to outlive this frame
    // and everything reachable from a local reference does not.
    let connection = env
        .new_global_ref(connection)
        .map_err(|err| format!("cannot hold the USB connection: {err}"))?;

    log::info!("radio: opened {vid:04x}:{pid:04x} as fd {fd}");
    Ok(UsbHandle {
        fd,
        connection,
        vm: unsafe { JavaVM::from_raw(vm.get_java_vm_pointer()) }.map_err(jerr)?,
        vid,
        pid,
    })
}

/// The platform release, for the API differences that cannot be worked around.
fn sdk_int(env: &mut jni::JNIEnv<'_>) -> Result<i32, String> {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .and_then(|v| v.i())
        .map_err(|err| format!("cannot read the Android version: {err}"))
}

fn call_int(env: &mut jni::JNIEnv<'_>, object: &JObject<'_>, method: &str) -> Result<i32, String> {
    env.call_method(object, method, "()I", &[])
        .and_then(|v| v.i())
        .map_err(|err| format!("{method} failed: {err}"))
}

fn jerr(err: jni::errors::Error) -> String {
    err.to_string()
}
