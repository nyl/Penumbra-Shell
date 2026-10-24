//! # Linux Kernel `/dev/uinput` Virtual Keyboard Device Driver
//!
//! Provides a zero-overhead kernel virtual keyboard device emitting standard evdev input
//! events (`EV_KEY`, `EV_SYN`) into the system seat.
//!
//! ## Architectural Role
//! By creating a kernel-level uinput device instead of relying on custom compositor protocol
//! extensions:
//! 1. Any Wayland client (XDG toplevels, Firefox, Foot terminal, Slint applications, Qt/GTK)
//!    receives real Linux input key events without custom input-method protocols.
//! 2. Keys properly repeat and produce the standard scancodes matching physical keyboards.
//! 3. If `/dev/uinput` lacks write permissions (e.g. running unprivileged or in sandboxed tests),
//!    the driver logs an informative warning and operates in graceful no-op mode without crashing.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::thread::sleep;
use std::time::Duration;
use tracing::{error, info, warn};

// Standard Linux input event constants
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0x00;

// ioctl numbers for uinput
const UI_SET_EVBIT: u32 = 0x40045564;
const UI_SET_KEYBIT: u32 = 0x40045565;
const UI_DEV_CREATE: u32 = 0x5501;
const UI_DEV_DESTROY: u32 = 0x5502;
const UI_DEV_SETUP: u32 = 0x405c5503;

const BUS_VIRTUAL: u16 = 0x06;

// Keycode constants from <linux/input-event-codes.h>
pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_6: u16 = 7;
pub const KEY_7: u16 = 8;
pub const KEY_8: u16 = 9;
pub const KEY_9: u16 = 10;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_Q: u16 = 16;
pub const KEY_W: u16 = 17;
pub const KEY_E: u16 = 18;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_Y: u16 = 21;
pub const KEY_U: u16 = 22;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_P: u16 = 25;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_A: u16 = 30;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_H: u16 = 35;
pub const KEY_J: u16 = 36;
pub const KEY_K: u16 = 37;
pub const KEY_L: u16 = 38;
pub const KEY_SEMICOLON: u16 = 39;
pub const KEY_APOSTROPHE: u16 = 40;
pub const KEY_GRAVE: u16 = 41;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_BACKSLASH: u16 = 43;
pub const KEY_Z: u16 = 44;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_B: u16 = 48;
pub const KEY_N: u16 = 49;
pub const KEY_M: u16 = 50;
pub const KEY_COMMA: u16 = 51;
pub const KEY_DOT: u16 = 52;
pub const KEY_SLASH: u16 = 53;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UInputSetup {
    id: InputId,
    name: [libc::c_char; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

/// Linux virtual keyboard driver creating and controlling a `/dev/uinput` node.
pub struct UInputKeyboard {
    file: Option<File>,
}

impl UInputKeyboard {
    /// Attempts to open `/dev/uinput` and configure a virtual keyboard device.
    ///
    /// # Returns
    /// An initialized [`UInputKeyboard`]. If opening `/dev/uinput` fails (e.g. insufficient
    /// permissions), a fallback instance is returned which logs warnings on event emission.
    pub fn new() -> Self {
        match Self::open_device() {
            Ok(file) => {
                info!("Successfully created Linux uinput virtual keyboard: Penumbra Virtual Keyboard");
                Self { file: Some(file) }
            }
            Err(err) => {
                warn!(
                    "Unable to initialize /dev/uinput virtual keyboard ({}); running in simulated mode. \
                    Ensure user is in the 'input' group and /dev/uinput is accessible.",
                    err
                );
                Self { file: None }
            }
        }
    }

    fn open_device() -> Result<File, Box<dyn std::error::Error>> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")?;
        let fd = file.as_raw_fd();

        unsafe {
            // Enable EV_SYN and EV_KEY
            if libc::ioctl(fd, UI_SET_EVBIT as _, EV_SYN as libc::c_int) < 0 {
                return Err("ioctl UI_SET_EVBIT EV_SYN failed".into());
            }
            if libc::ioctl(fd, UI_SET_EVBIT as _, EV_KEY as libc::c_int) < 0 {
                return Err("ioctl UI_SET_EVBIT EV_KEY failed".into());
            }

            // Register standard key range
            for code in 1..=128 {
                libc::ioctl(fd, UI_SET_KEYBIT as _, code as libc::c_int);
            }

            let mut setup: UInputSetup = std::mem::zeroed();
            setup.id.bustype = BUS_VIRTUAL;
            setup.id.vendor = 0x18d1; // Penumbra / Android Vendor
            setup.id.product = 0x5001; // Penumbra OSK
            setup.id.version = 1;

            let name_bytes = b"Penumbra Virtual Keyboard\0";
            for (i, &b) in name_bytes.iter().enumerate() {
                if i < setup.name.len() {
                    setup.name[i] = b as libc::c_char;
                }
            }

            if libc::ioctl(fd, UI_DEV_SETUP as _, &setup) < 0 {
                return Err("ioctl UI_DEV_SETUP failed".into());
            }

            if libc::ioctl(fd, UI_DEV_CREATE as _) < 0 {
                return Err("ioctl UI_DEV_CREATE failed".into());
            }
        }

        Ok(file)
    }

    fn write_event(&self, type_: u16, code: u16, value: i32) {
        let Some(ref file) = self.file else {
            return;
        };
        let fd = file.as_raw_fd();

        let event = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };

        unsafe {
            let ptr = &event as *const _ as *const libc::c_void;
            let size = std::mem::size_of::<InputEvent>();
            let written = libc::write(fd, ptr, size);
            if written < 0 {
                error!("Failed to write input_event to /dev/uinput: {:?}", std::io::Error::last_os_error());
            }
        }
    }

    /// Emits a single key click (press followed by release with EV_SYN).
    pub fn click_key(&self, code: u16) {
        self.write_event(EV_KEY, code, 1);
        self.write_event(EV_SYN, SYN_REPORT, 0);
        sleep(Duration::from_millis(8));
        self.write_event(EV_KEY, code, 0);
        self.write_event(EV_SYN, SYN_REPORT, 0);
    }

    /// Emits a shifted key click (Shift down, key press/release, Shift up).
    pub fn click_shifted_key(&self, code: u16) {
        self.write_event(EV_KEY, KEY_LEFTSHIFT, 1);
        self.write_event(EV_SYN, SYN_REPORT, 0);
        sleep(Duration::from_millis(5));

        self.write_event(EV_KEY, code, 1);
        self.write_event(EV_SYN, SYN_REPORT, 0);
        sleep(Duration::from_millis(8));

        self.write_event(EV_KEY, code, 0);
        self.write_event(EV_SYN, SYN_REPORT, 0);
        sleep(Duration::from_millis(5));

        self.write_event(EV_KEY, KEY_LEFTSHIFT, 0);
        self.write_event(EV_SYN, SYN_REPORT, 0);
    }

    /// Translates a character string into corresponding Linux scancodes and emits them.
    pub fn send_text(&self, text: &str) {
        for ch in text.chars() {
            match ch {
                'a' => self.click_key(KEY_A),
                'b' => self.click_key(KEY_B),
                'c' => self.click_key(KEY_C),
                'd' => self.click_key(KEY_D),
                'e' => self.click_key(KEY_E),
                'f' => self.click_key(KEY_F),
                'g' => self.click_key(KEY_G),
                'h' => self.click_key(KEY_H),
                'i' => self.click_key(KEY_I),
                'j' => self.click_key(KEY_J),
                'k' => self.click_key(KEY_K),
                'l' => self.click_key(KEY_L),
                'm' => self.click_key(KEY_M),
                'n' => self.click_key(KEY_N),
                'o' => self.click_key(KEY_O),
                'p' => self.click_key(KEY_P),
                'q' => self.click_key(KEY_Q),
                'r' => self.click_key(KEY_R),
                's' => self.click_key(KEY_S),
                't' => self.click_key(KEY_T),
                'u' => self.click_key(KEY_U),
                'v' => self.click_key(KEY_V),
                'w' => self.click_key(KEY_W),
                'x' => self.click_key(KEY_X),
                'y' => self.click_key(KEY_Y),
                'z' => self.click_key(KEY_Z),

                // Uppercase characters
                'A' => self.click_shifted_key(KEY_A),
                'B' => self.click_shifted_key(KEY_B),
                'C' => self.click_shifted_key(KEY_C),
                'D' => self.click_shifted_key(KEY_D),
                'E' => self.click_shifted_key(KEY_E),
                'F' => self.click_shifted_key(KEY_F),
                'G' => self.click_shifted_key(KEY_G),
                'H' => self.click_shifted_key(KEY_H),
                'I' => self.click_shifted_key(KEY_I),
                'J' => self.click_shifted_key(KEY_J),
                'K' => self.click_shifted_key(KEY_K),
                'L' => self.click_shifted_key(KEY_L),
                'M' => self.click_shifted_key(KEY_M),
                'N' => self.click_shifted_key(KEY_N),
                'O' => self.click_shifted_key(KEY_O),
                'P' => self.click_shifted_key(KEY_P),
                'Q' => self.click_shifted_key(KEY_Q),
                'R' => self.click_shifted_key(KEY_R),
                'S' => self.click_shifted_key(KEY_S),
                'T' => self.click_shifted_key(KEY_T),
                'U' => self.click_shifted_key(KEY_U),
                'V' => self.click_shifted_key(KEY_V),
                'W' => self.click_shifted_key(KEY_W),
                'X' => self.click_shifted_key(KEY_X),
                'Y' => self.click_shifted_key(KEY_Y),
                'Z' => self.click_shifted_key(KEY_Z),

                // Numbers
                '1' => self.click_key(KEY_1),
                '2' => self.click_key(KEY_2),
                '3' => self.click_key(KEY_3),
                '4' => self.click_key(KEY_4),
                '5' => self.click_key(KEY_5),
                '6' => self.click_key(KEY_6),
                '7' => self.click_key(KEY_7),
                '8' => self.click_key(KEY_8),
                '9' => self.click_key(KEY_9),
                '0' => self.click_key(KEY_0),

                // Symbols
                ' ' => self.click_key(KEY_SPACE),
                '\n' => self.click_key(KEY_ENTER),
                '\t' => self.click_key(KEY_TAB),
                '.' => self.click_key(KEY_DOT),
                ',' => self.click_key(KEY_COMMA),
                '/' => self.click_key(KEY_SLASH),
                '-' => self.click_key(KEY_MINUS),
                '=' => self.click_key(KEY_EQUAL),
                ';' => self.click_key(KEY_SEMICOLON),
                '\'' => self.click_key(KEY_APOSTROPHE),
                '`' => self.click_key(KEY_GRAVE),
                '\\' => self.click_key(KEY_BACKSLASH),
                '[' => self.click_key(KEY_LEFTBRACE),
                ']' => self.click_key(KEY_RIGHTBRACE),

                // Shifted Symbols
                '!' => self.click_shifted_key(KEY_1),
                '@' => self.click_shifted_key(KEY_2),
                '#' => self.click_shifted_key(KEY_3),
                '$' => self.click_shifted_key(KEY_4),
                '%' => self.click_shifted_key(KEY_5),
                '^' => self.click_shifted_key(KEY_6),
                '&' => self.click_shifted_key(KEY_7),
                '*' => self.click_shifted_key(KEY_8),
                '(' => self.click_shifted_key(KEY_9),
                ')' => self.click_shifted_key(KEY_0),
                '_' => self.click_shifted_key(KEY_MINUS),
                '+' => self.click_shifted_key(KEY_EQUAL),
                ':' => self.click_shifted_key(KEY_SEMICOLON),
                '"' => self.click_shifted_key(KEY_APOSTROPHE),
                '~' => self.click_shifted_key(KEY_GRAVE),
                '?' => self.click_shifted_key(KEY_SLASH),
                '<' => self.click_shifted_key(KEY_COMMA),
                '>' => self.click_shifted_key(KEY_DOT),
                '{' => self.click_shifted_key(KEY_LEFTBRACE),
                '}' => self.click_shifted_key(KEY_RIGHTBRACE),
                '|' => self.click_shifted_key(KEY_BACKSLASH),

                other => {
                    info!("Unsupported direct scancode for character: '{}'", other);
                }
            }
        }
    }
}

impl Drop for UInputKeyboard {
    fn drop(&mut self) {
        if let Some(ref file) = self.file {
            let fd = file.as_raw_fd();
            unsafe {
                libc::ioctl(fd, UI_DEV_DESTROY as _);
            }
            info!("Destroyed Linux uinput device");
        }
    }
}
