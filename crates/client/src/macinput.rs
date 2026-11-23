//! Replays a remote keyboard and mouse as synthetic macOS input events
//! (CoreGraphics). Needs the Accessibility permission; without it macOS
//! silently drops the events.

use std::ffi::c_void;

use anyhow::{bail, Result};

use crate::hid::{Event, Input};

type CFTypeRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGPoint,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        kind: u32,
        at: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        ...
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFTypeRef;
}

const HID_SYSTEM_STATE: i32 = 1;
const HID_EVENT_TAP: u32 = 0;
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;
const SCROLL_LINES: u32 = 1;
const FIELD_DELTA_X: u32 = 4;
const FIELD_DELTA_Y: u32 = 5;
const FIELD_BUTTON_NUMBER: u32 = 3;

const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_OPTION: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

/// Whether farplug may post input events. With `prompt`, macOS shows its
/// dialog that leads to the Accessibility settings.
pub fn trusted(prompt: bool) -> bool {
    // SAFETY: the dictionary holds two framework constants and is released
    // after the call; AXIsProcessTrustedWithOptions accepts a null options.
    unsafe {
        if !prompt {
            return AXIsProcessTrustedWithOptions(std::ptr::null());
        }
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
        );
        let ok = AXIsProcessTrustedWithOptions(opts);
        if !opts.is_null() {
            CFRelease(opts);
        }
        ok
    }
}

pub struct MacInput {
    source: CGEventSourceRef,
    flags: u64,
    buttons: u8,
}

// SAFETY: the event source is only used from the thread holding the input's
// mutex; CoreGraphics event sources are not tied to a thread.
unsafe impl Send for MacInput {}

impl MacInput {
    pub fn new() -> Result<Self> {
        if !trusted(true) {
            bail!(
                "farplug needs the Accessibility permission to type and move the pointer: \
                 System Settings > Privacy & Security > Accessibility, then turn on farplug"
            );
        }
        // SAFETY: plain constructor; null on failure, checked below.
        let source = unsafe { CGEventSourceCreate(HID_SYSTEM_STATE) };
        if source.is_null() {
            bail!("could not create an input event source");
        }
        Ok(Self {
            source,
            flags: 0,
            buttons: 0,
        })
    }

    fn post(&self, ev: CGEventRef) {
        if ev.is_null() {
            return;
        }
        // SAFETY: ev is a live event we own; released after posting.
        unsafe {
            CGEventPost(HID_EVENT_TAP, ev);
            CFRelease(ev);
        }
    }

    fn cursor(&self) -> CGPoint {
        // SAFETY: a fresh event only to read the cursor location.
        unsafe {
            let ev = CGEventCreate(self.source);
            if ev.is_null() {
                return CGPoint::default();
            }
            let at = CGEventGetLocation(ev);
            CFRelease(ev);
            at
        }
    }

    fn key(&mut self, usage: u8, down: bool) {
        let flag = match usage {
            0xe0 | 0xe4 => FLAG_CONTROL,
            0xe1 | 0xe5 => FLAG_SHIFT,
            0xe2 | 0xe6 => FLAG_OPTION,
            0xe3 | 0xe7 => FLAG_COMMAND,
            _ => 0,
        };
        if down {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
        let Some(code) = mac_keycode(usage) else {
            return;
        };
        // SAFETY: the source is valid; the event is posted and released.
        unsafe {
            let ev = CGEventCreateKeyboardEvent(self.source, code, down);
            if !ev.is_null() {
                // macOS does not derive modifier state from synthetic
                // modifier presses, so every key event carries it.
                CGEventSetFlags(ev, self.flags);
            }
            self.post(ev);
        }
    }

    fn button(&mut self, button: u8, down: bool) {
        if down {
            self.buttons |= 1 << button;
        } else {
            self.buttons &= !(1 << button);
        }
        let kind = match (button, down) {
            (0, true) => LEFT_DOWN,
            (0, false) => LEFT_UP,
            (1, true) => RIGHT_DOWN,
            (1, false) => RIGHT_UP,
            (_, true) => OTHER_DOWN,
            (_, false) => OTHER_UP,
        };
        let at = self.cursor();
        // SAFETY: as in key().
        unsafe {
            let ev = CGEventCreateMouseEvent(self.source, kind, at, button.into());
            if !ev.is_null() {
                CGEventSetIntegerValueField(ev, FIELD_BUTTON_NUMBER, button.into());
            }
            self.post(ev);
        }
    }

    fn motion(&mut self, dx: i32, dy: i32) {
        let at = self.cursor();
        let to = clamp_to_displays(CGPoint {
            x: at.x + f64::from(dx),
            y: at.y + f64::from(dy),
        });
        let (kind, button) = if self.buttons & 1 != 0 {
            (LEFT_DRAGGED, 0)
        } else if self.buttons & 2 != 0 {
            (RIGHT_DRAGGED, 1)
        } else if self.buttons != 0 {
            (OTHER_DRAGGED, 2)
        } else {
            (MOVED, 0)
        };
        // SAFETY: as in key().
        unsafe {
            let ev = CGEventCreateMouseEvent(self.source, kind, to, button);
            if !ev.is_null() {
                // Games and 3D apps read the raw deltas rather than positions.
                CGEventSetIntegerValueField(ev, FIELD_DELTA_X, dx.into());
                CGEventSetIntegerValueField(ev, FIELD_DELTA_Y, dy.into());
            }
            self.post(ev);
        }
    }

    fn scroll(&mut self, lines: i32) {
        // SAFETY: as in key().
        unsafe {
            let ev = CGEventCreateScrollWheelEvent(self.source, SCROLL_LINES, 1, lines);
            self.post(ev);
        }
    }
}

impl Drop for MacInput {
    fn drop(&mut self) {
        // SAFETY: created in new(), released once.
        unsafe { CFRelease(self.source) };
    }
}

impl Input for MacInput {
    fn send(&mut self, e: Event) {
        match e {
            Event::Key { usage, down } => self.key(usage, down),
            Event::Button { button, down } => self.button(button, down),
            Event::Motion { dx, dy } => self.motion(dx, dy),
            Event::Scroll { lines } => self.scroll(lines),
        }
    }
}

/// Keeps the pointer on some display.
fn clamp_to_displays(p: CGPoint) -> CGPoint {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    // SAFETY: ids has room for 16 display ids.
    if unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) } != 0 || n == 0 {
        return p;
    }
    let rects: Vec<CGRect> = ids[..n as usize]
        .iter()
        .map(|&d| unsafe { CGDisplayBounds(d) })
        .collect();
    let inside = |r: &CGRect| {
        p.x >= r.origin.x
            && p.x < r.origin.x + r.size.x
            && p.y >= r.origin.y
            && p.y < r.origin.y + r.size.y
    };
    if rects.iter().any(inside) {
        return p;
    }
    // Off every display: pull it to the nearest point of the nearest one.
    rects
        .iter()
        .map(|r| CGPoint {
            x: p.x.clamp(r.origin.x, r.origin.x + r.size.x - 1.0),
            y: p.y.clamp(r.origin.y, r.origin.y + r.size.y - 1.0),
        })
        .min_by(|a, b| {
            let d = |q: &CGPoint| (q.x - p.x).powi(2) + (q.y - p.y).powi(2);
            d(a).total_cmp(&d(b))
        })
        .unwrap_or(p)
}

/// HID keyboard-page usage to macOS virtual key code (HIToolbox kVK_*).
fn mac_keycode(usage: u8) -> Option<u16> {
    const LETTERS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    const DIGITS: [u16; 10] = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19, 0x1d];
    const F1_F12: [u16; 12] = [
        0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f,
    ];
    const KEYPAD_1_9: [u16; 9] = [0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5b, 0x5c];
    Some(match usage {
        0x04..=0x1d => LETTERS[usize::from(usage - 0x04)],
        0x1e..=0x27 => DIGITS[usize::from(usage - 0x1e)],
        0x28 => 0x24,        // Return
        0x29 => 0x35,        // Escape
        0x2a => 0x33,        // Backspace
        0x2b => 0x30,        // Tab
        0x2c => 0x31,        // Space
        0x2d => 0x1b,        // -
        0x2e => 0x18,        // =
        0x2f => 0x21,        // [
        0x30 => 0x1e,        // ]
        0x31 | 0x32 => 0x2a, // \ and the non-US #
        0x33 => 0x29,        // ;
        0x34 => 0x27,        // '
        0x35 => 0x32,        // `
        0x36 => 0x2b,        // ,
        0x37 => 0x2f,        // .
        0x38 => 0x2c,        // /
        0x39 => 0x39,        // Caps Lock
        0x3a..=0x45 => F1_F12[usize::from(usage - 0x3a)],
        0x46 => 0x69, // Print Screen as F13
        0x47 => 0x6b, // Scroll Lock as F14
        0x48 => 0x71, // Pause as F15
        0x49 => 0x72, // Insert as Help
        0x4a => 0x73, // Home
        0x4b => 0x74, // Page Up
        0x4c => 0x75, // Delete forward
        0x4d => 0x77, // End
        0x4e => 0x79, // Page Down
        0x4f => 0x7c, // Right
        0x50 => 0x7b, // Left
        0x51 => 0x7d, // Down
        0x52 => 0x7e, // Up
        0x53 => 0x47, // Num Lock as keypad Clear
        0x54 => 0x4b, // keypad /
        0x55 => 0x43, // keypad *
        0x56 => 0x4e, // keypad -
        0x57 => 0x45, // keypad +
        0x58 => 0x4c, // keypad Enter
        0x59..=0x61 => KEYPAD_1_9[usize::from(usage - 0x59)],
        0x62 => 0x52, // keypad 0
        0x63 => 0x41, // keypad .
        0x64 => 0x0a, // non-US \ (ISO section key)
        0x67 => 0x51, // keypad =
        0x68 => 0x69, // F13
        0x69 => 0x6b, // F14
        0x6a => 0x71, // F15
        0x6b => 0x6a, // F16
        0x6c => 0x40, // F17
        0x6d => 0x4f, // F18
        0x6e => 0x50, // F19
        0x6f => 0x5a, // F20
        0x7f => 0x4a, // Mute
        0x80 => 0x48, // Volume Up
        0x81 => 0x49, // Volume Down
        0xe0 => 0x3b, // left Control
        0xe1 => 0x38, // left Shift
        0xe2 => 0x3a, // left Option (Alt)
        0xe3 => 0x37, // left Command (GUI)
        0xe4 => 0x3e, // right Control
        0xe5 => 0x3c, // right Shift
        0xe6 => 0x3d, // right Option
        0xe7 => 0x36, // right Command
        _ => return None,
    })
}
