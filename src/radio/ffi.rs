// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! Declarations for `shim/devourer_shim.h`.
//!
//! Kept to a transcription of that header and nothing else. Every rule about
//! what may be called when, and what outlives what, is stated there and
//! enforced by [`super::Radio`]; this file only has to match the C.

use std::ffi::{c_char, c_int, c_void};

/// devourer's `ChannelWidth_t`, narrowed to the widths a wfb-ng link uses.
pub const DV_WIDTH_20: u8 = 0;
pub const DV_WIDTH_40: u8 = 1;

/// One received 802.11 frame. Mirrors `dv_packet`.
#[repr(C)]
pub struct DvPacket {
    pub data: *const u8,
    pub len: usize,
    /// Realtek's raw path gain. Subtract 110 for dBm; zero means no reading.
    pub rssi: [u8; 4],
    /// Half-decibels of signal to noise.
    pub snr: [i8; 4],
    pub tsf: u32,
    pub rate: u16,
    pub bandwidth: u8,
    pub crc_error: bool,
}

/// An opened adapter. Never dereferenced on this side.
#[repr(C)]
pub struct DvDevice {
    _opaque: [u8; 0],
}

pub type DvRxCallback = extern "C" fn(user: *mut c_void, packet: *const DvPacket);

extern "C" {
    pub fn dv_open_usb(vid: u16, pid: u16, err: *mut c_char, err_len: usize) -> *mut DvDevice;
    pub fn dv_open_fd(fd: c_int, err: *mut c_char, err_len: usize) -> *mut DvDevice;
    pub fn dv_start(
        dev: *mut DvDevice,
        channel: u8,
        width: u8,
        offset: u8,
        callback: DvRxCallback,
        user: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn dv_set_channel(
        dev: *mut DvDevice,
        channel: u8,
        width: u8,
        offset: u8,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    pub fn dv_stop(dev: *mut DvDevice);
    pub fn dv_close(dev: *mut DvDevice);
    pub fn dv_chip_name(dev: *mut DvDevice) -> *const c_char;
    pub fn dv_running(dev: *mut DvDevice) -> bool;
    pub fn dv_rx_error(dev: *mut DvDevice) -> *const c_char;
    pub fn dv_supported_count() -> usize;
    pub fn dv_supported_id(index: usize, vid: *mut u16, pid: *mut u16);
}

/// The USB ids this build of devourer can drive.
///
/// The list lives in the shim so there is one of it. The desktop path never
/// needs it - the shim enumerates - but Android is handed devices one at a
/// time and has to decide for itself which one to ask permission for.
pub fn supported_ids() -> Vec<(u16, u16)> {
    // SAFETY: the count bounds the index, and both out pointers are valid
    // for the duration of each call.
    unsafe {
        (0..dv_supported_count())
            .map(|i| {
                let (mut vid, mut pid) = (0u16, 0u16);
                dv_supported_id(i, &mut vid, &mut pid);
                (vid, pid)
            })
            .collect()
    }
}

/// A buffer for the shim's error messages, and the string it left there.
///
/// Every fallible entry point takes one. The shim writes a NUL-terminated
/// message and this reads it back, so no C string ever outlives the call it
/// came from.
pub struct ErrorBuffer([c_char; 256]);

impl ErrorBuffer {
    pub fn new() -> Self {
        Self([0; 256])
    }

    pub fn as_mut_ptr(&mut self) -> *mut c_char {
        self.0.as_mut_ptr()
    }

    /// How much room the shim has to write into. Named for what it is
    /// rather than `len`, which would suggest a message is already there.
    pub fn capacity(&self) -> usize {
        self.0.len()
    }

    /// What the shim wrote, or a stand-in if it wrote nothing.
    pub fn take(&self) -> String {
        // SAFETY: the shim always writes a NUL-terminated string within the
        // length it was given, and the buffer starts zeroed, so there is a
        // terminator either way.
        let text = unsafe { std::ffi::CStr::from_ptr(self.0.as_ptr()) };
        match text.to_string_lossy() {
            message if message.is_empty() => "the adapter failed for no stated reason".into(),
            message => message.into_owned(),
        }
    }
}

impl Default for ErrorBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Read a string the shim owns.
///
/// # Safety
/// `ptr` must be NULL or a NUL-terminated string that stays valid for the
/// duration of the call.
pub unsafe fn borrowed_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
}
