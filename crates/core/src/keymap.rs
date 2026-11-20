//! Mapping from Linux evdev key codes to Windows PC/AT set-1 scan codes,
//! as consumed by `SendInput` with `KEYEVENTF_SCANCODE`.
//!
//! # Status
//!
//! The table has not been verified on real Windows hardware. It was checked
//! against the references below only; the first real-world test (especially
//! the Japanese keys, the Korean Hangul/Hanja keys, and the browser/media
//! keys) may still turn up keys that Windows interprets differently.
//!
//! # References
//!
//! * Microsoft, "USB HID to PS/2 Scan Code Translation Table" (the "Set 1
//!   Make" column). This is the primary reference for every entry.
//! * Microsoft, "Keyboard Scan Code Specification" (Windows Platform Design
//!   Notes), for the main-block, keypad, navigation and Windows/Application
//!   key codes and the `E0` prefix rules.
//! * The Linux `atkbd` driver tables (`drivers/input/keyboard/atkbd.c`),
//!   used as a cross-check of the evdev side of the pairing.
//!
//! # Conventions
//!
//! `extended` is true for keys whose set-1 make code is prefixed with `0xE0`.
//! Only the make code is stored; the caller adds `KEYEVENTF_KEYUP` for
//! releases.
//!
//! # Deliberate gaps
//!
//! * `KEY_PAUSE`: the Pause key sends the multi-byte sequence
//!   `E1 1D 45 E1 9D C5` (and has no break code). `SendInput` cannot express
//!   an `E1` prefix, and sending plain `0x45` would press NumLock instead, so
//!   there is no faithful `ScanCode`. It maps to `None`. The Windows side
//!   should special-case it, for example by injecting `VK_PAUSE` through the
//!   virtual-key field.
//! * `KEY_POWER`, `KEY_SLEEP`, `KEY_WAKEUP`: the ACPI keys (`E0 5E`, `E0 5F`,
//!   `E0 63`) are not mapped, because a stray press would power off or
//!   suspend the Windows machine.
//! * `KEY_KPJPCOMMA` is not mapped. The Microsoft table also gives HID `LANG5`
//!   (Zenkaku/Hankaku) the code `0x76`, which collides with F24, so
//!   `KEY_ZENKAKUHANKAKU` uses `0x29` instead (see "Intentional aliases").
//!
//! # Korean keys
//!
//! `KEY_HANGEUL` maps to `0xF2` and `KEY_HANJA` to `0xF1`, as in the Microsoft
//! translation table (HID `LANG1` and `LANG2`). These are the raw set-1 values
//! that Korean keyboards send, and, unlike every other key in the table, the
//! specification lists them as make-only: there is no break code. Whether
//! Windows accepts them through `SendInput` (and how it treats the key-up
//! event) is unverified.
//!
//! # Intentional aliases
//!
//! A Japanese 106/109 keyboard has no separate Grave key: the key at that
//! position is Zenkaku/Hankaku and carries scan code `0x29`, the same code as
//! the US Grave key. `KEY_GRAVE` and `KEY_ZENKAKUHANKAKU` therefore map to the
//! same `ScanCode`; Windows picks the meaning from the active keyboard layout.
//! This is the only alias, and the tests enforce that.

/// A Windows scan code. `extended` corresponds to the `0xE0` prefix and maps
/// to `KEYEVENTF_EXTENDEDKEY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCode {
    pub code: u16,
    pub extended: bool,
}

// evdev key codes, from <linux/input-event-codes.h>.
const KEY_ESC: u16 = 1;
const KEY_1: u16 = 2;
const KEY_2: u16 = 3;
const KEY_3: u16 = 4;
const KEY_4: u16 = 5;
const KEY_5: u16 = 6;
const KEY_6: u16 = 7;
const KEY_7: u16 = 8;
const KEY_8: u16 = 9;
const KEY_9: u16 = 10;
const KEY_0: u16 = 11;
const KEY_MINUS: u16 = 12;
const KEY_EQUAL: u16 = 13;
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_Q: u16 = 16;
const KEY_W: u16 = 17;
const KEY_E: u16 = 18;
const KEY_R: u16 = 19;
const KEY_T: u16 = 20;
const KEY_Y: u16 = 21;
const KEY_U: u16 = 22;
const KEY_I: u16 = 23;
const KEY_O: u16 = 24;
const KEY_P: u16 = 25;
const KEY_LEFTBRACE: u16 = 26;
const KEY_RIGHTBRACE: u16 = 27;
const KEY_ENTER: u16 = 28;
const KEY_LEFTCTRL: u16 = 29;
const KEY_A: u16 = 30;
const KEY_S: u16 = 31;
const KEY_D: u16 = 32;
const KEY_F: u16 = 33;
const KEY_G: u16 = 34;
const KEY_H: u16 = 35;
const KEY_J: u16 = 36;
const KEY_K: u16 = 37;
const KEY_L: u16 = 38;
const KEY_SEMICOLON: u16 = 39;
const KEY_APOSTROPHE: u16 = 40;
const KEY_GRAVE: u16 = 41;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_BACKSLASH: u16 = 43;
const KEY_Z: u16 = 44;
const KEY_X: u16 = 45;
const KEY_C: u16 = 46;
const KEY_V: u16 = 47;
const KEY_B: u16 = 48;
const KEY_N: u16 = 49;
const KEY_M: u16 = 50;
const KEY_COMMA: u16 = 51;
const KEY_DOT: u16 = 52;
const KEY_SLASH: u16 = 53;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_KPASTERISK: u16 = 55;
const KEY_LEFTALT: u16 = 56;
const KEY_SPACE: u16 = 57;
const KEY_CAPSLOCK: u16 = 58;
const KEY_F1: u16 = 59;
const KEY_F2: u16 = 60;
const KEY_F3: u16 = 61;
const KEY_F4: u16 = 62;
const KEY_F5: u16 = 63;
const KEY_F6: u16 = 64;
const KEY_F7: u16 = 65;
const KEY_F8: u16 = 66;
const KEY_F9: u16 = 67;
const KEY_F10: u16 = 68;
const KEY_NUMLOCK: u16 = 69;
const KEY_SCROLLLOCK: u16 = 70;
const KEY_KP7: u16 = 71;
const KEY_KP8: u16 = 72;
const KEY_KP9: u16 = 73;
const KEY_KPMINUS: u16 = 74;
const KEY_KP4: u16 = 75;
const KEY_KP5: u16 = 76;
const KEY_KP6: u16 = 77;
const KEY_KPPLUS: u16 = 78;
const KEY_KP1: u16 = 79;
const KEY_KP2: u16 = 80;
const KEY_KP3: u16 = 81;
const KEY_KP0: u16 = 82;
const KEY_KPDOT: u16 = 83;
const KEY_ZENKAKUHANKAKU: u16 = 85;
const KEY_102ND: u16 = 86;
const KEY_F11: u16 = 87;
const KEY_F12: u16 = 88;
const KEY_RO: u16 = 89;
const KEY_KATAKANA: u16 = 90;
const KEY_HIRAGANA: u16 = 91;
const KEY_HENKAN: u16 = 92;
const KEY_KATAKANAHIRAGANA: u16 = 93;
const KEY_MUHENKAN: u16 = 94;
const KEY_KPENTER: u16 = 96;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_KPSLASH: u16 = 98;
const KEY_SYSRQ: u16 = 99;
const KEY_RIGHTALT: u16 = 100;
const KEY_HOME: u16 = 102;
const KEY_UP: u16 = 103;
const KEY_PAGEUP: u16 = 104;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_END: u16 = 107;
const KEY_DOWN: u16 = 108;
const KEY_PAGEDOWN: u16 = 109;
const KEY_INSERT: u16 = 110;
const KEY_DELETE: u16 = 111;
const KEY_MUTE: u16 = 113;
const KEY_VOLUMEDOWN: u16 = 114;
const KEY_VOLUMEUP: u16 = 115;
const KEY_KPEQUAL: u16 = 117;
const KEY_KPCOMMA: u16 = 121;
const KEY_HANGEUL: u16 = 122;
const KEY_HANJA: u16 = 123;
const KEY_YEN: u16 = 124;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTMETA: u16 = 126;
const KEY_COMPOSE: u16 = 127;
const KEY_STOP: u16 = 128;
const KEY_CALC: u16 = 140;
const KEY_MAIL: u16 = 155;
const KEY_BOOKMARKS: u16 = 156;
const KEY_COMPUTER: u16 = 157;
const KEY_BACK: u16 = 158;
const KEY_FORWARD: u16 = 159;
const KEY_NEXTSONG: u16 = 163;
const KEY_PLAYPAUSE: u16 = 164;
const KEY_PREVIOUSSONG: u16 = 165;
const KEY_STOPCD: u16 = 166;
const KEY_HOMEPAGE: u16 = 172;
const KEY_REFRESH: u16 = 173;
const KEY_F13: u16 = 183;
const KEY_F14: u16 = 184;
const KEY_F15: u16 = 185;
const KEY_F16: u16 = 186;
const KEY_F17: u16 = 187;
const KEY_F18: u16 = 188;
const KEY_F19: u16 = 189;
const KEY_F20: u16 = 190;
const KEY_F21: u16 = 191;
const KEY_F22: u16 = 192;
const KEY_F23: u16 = 193;
const KEY_F24: u16 = 194;
const KEY_SEARCH: u16 = 217;
const KEY_MEDIA: u16 = 226;

/// A scan code without the `0xE0` prefix.
const fn plain(code: u16) -> ScanCode {
    ScanCode {
        code,
        extended: false,
    }
}

/// A scan code with the `0xE0` prefix.
const fn ext(code: u16) -> ScanCode {
    ScanCode {
        code,
        extended: true,
    }
}

/// The translation table, strictly ascending by evdev code so that it can be
/// binary-searched. The `table_is_sorted` test enforces the ordering.
static TABLE: &[(u16, ScanCode)] = &[
    (KEY_ESC, plain(0x01)),
    (KEY_1, plain(0x02)),
    (KEY_2, plain(0x03)),
    (KEY_3, plain(0x04)),
    (KEY_4, plain(0x05)),
    (KEY_5, plain(0x06)),
    (KEY_6, plain(0x07)),
    (KEY_7, plain(0x08)),
    (KEY_8, plain(0x09)),
    (KEY_9, plain(0x0A)),
    (KEY_0, plain(0x0B)),
    (KEY_MINUS, plain(0x0C)),
    (KEY_EQUAL, plain(0x0D)),
    (KEY_BACKSPACE, plain(0x0E)),
    (KEY_TAB, plain(0x0F)),
    (KEY_Q, plain(0x10)),
    (KEY_W, plain(0x11)),
    (KEY_E, plain(0x12)),
    (KEY_R, plain(0x13)),
    (KEY_T, plain(0x14)),
    (KEY_Y, plain(0x15)),
    (KEY_U, plain(0x16)),
    (KEY_I, plain(0x17)),
    (KEY_O, plain(0x18)),
    (KEY_P, plain(0x19)),
    (KEY_LEFTBRACE, plain(0x1A)),
    (KEY_RIGHTBRACE, plain(0x1B)),
    (KEY_ENTER, plain(0x1C)),
    (KEY_LEFTCTRL, plain(0x1D)),
    (KEY_A, plain(0x1E)),
    (KEY_S, plain(0x1F)),
    (KEY_D, plain(0x20)),
    (KEY_F, plain(0x21)),
    (KEY_G, plain(0x22)),
    (KEY_H, plain(0x23)),
    (KEY_J, plain(0x24)),
    (KEY_K, plain(0x25)),
    (KEY_L, plain(0x26)),
    (KEY_SEMICOLON, plain(0x27)),
    (KEY_APOSTROPHE, plain(0x28)),
    (KEY_GRAVE, plain(0x29)),
    (KEY_LEFTSHIFT, plain(0x2A)),
    (KEY_BACKSLASH, plain(0x2B)),
    (KEY_Z, plain(0x2C)),
    (KEY_X, plain(0x2D)),
    (KEY_C, plain(0x2E)),
    (KEY_V, plain(0x2F)),
    (KEY_B, plain(0x30)),
    (KEY_N, plain(0x31)),
    (KEY_M, plain(0x32)),
    (KEY_COMMA, plain(0x33)),
    (KEY_DOT, plain(0x34)),
    (KEY_SLASH, plain(0x35)),
    (KEY_RIGHTSHIFT, plain(0x36)),
    (KEY_KPASTERISK, plain(0x37)),
    (KEY_LEFTALT, plain(0x38)),
    (KEY_SPACE, plain(0x39)),
    (KEY_CAPSLOCK, plain(0x3A)),
    (KEY_F1, plain(0x3B)),
    (KEY_F2, plain(0x3C)),
    (KEY_F3, plain(0x3D)),
    (KEY_F4, plain(0x3E)),
    (KEY_F5, plain(0x3F)),
    (KEY_F6, plain(0x40)),
    (KEY_F7, plain(0x41)),
    (KEY_F8, plain(0x42)),
    (KEY_F9, plain(0x43)),
    (KEY_F10, plain(0x44)),
    (KEY_NUMLOCK, plain(0x45)),
    (KEY_SCROLLLOCK, plain(0x46)),
    (KEY_KP7, plain(0x47)),
    (KEY_KP8, plain(0x48)),
    (KEY_KP9, plain(0x49)),
    (KEY_KPMINUS, plain(0x4A)),
    (KEY_KP4, plain(0x4B)),
    (KEY_KP5, plain(0x4C)),
    (KEY_KP6, plain(0x4D)),
    (KEY_KPPLUS, plain(0x4E)),
    (KEY_KP1, plain(0x4F)),
    (KEY_KP2, plain(0x50)),
    (KEY_KP3, plain(0x51)),
    (KEY_KP0, plain(0x52)),
    (KEY_KPDOT, plain(0x53)),
    // Alias of KEY_GRAVE: the Zenkaku/Hankaku key sits at the Grave position.
    (KEY_ZENKAKUHANKAKU, plain(0x29)),
    (KEY_102ND, plain(0x56)),
    (KEY_F11, plain(0x57)),
    (KEY_F12, plain(0x58)),
    (KEY_RO, plain(0x73)),
    // HID LANG3 / LANG4 in the Microsoft translation table.
    (KEY_KATAKANA, plain(0x78)),
    (KEY_HIRAGANA, plain(0x77)),
    (KEY_HENKAN, plain(0x79)),
    (KEY_KATAKANAHIRAGANA, plain(0x70)),
    (KEY_MUHENKAN, plain(0x7B)),
    (KEY_KPENTER, ext(0x1C)),
    (KEY_RIGHTCTRL, ext(0x1D)),
    (KEY_KPSLASH, ext(0x35)),
    // Print Screen. Alt+Print Screen (SysRq, plain 0x54) is not generated here.
    (KEY_SYSRQ, ext(0x37)),
    (KEY_RIGHTALT, ext(0x38)),
    (KEY_HOME, ext(0x47)),
    (KEY_UP, ext(0x48)),
    (KEY_PAGEUP, ext(0x49)),
    (KEY_LEFT, ext(0x4B)),
    (KEY_RIGHT, ext(0x4D)),
    (KEY_END, ext(0x4F)),
    (KEY_DOWN, ext(0x50)),
    (KEY_PAGEDOWN, ext(0x51)),
    (KEY_INSERT, ext(0x52)),
    (KEY_DELETE, ext(0x53)),
    (KEY_MUTE, ext(0x20)),
    (KEY_VOLUMEDOWN, ext(0x2E)),
    (KEY_VOLUMEUP, ext(0x30)),
    (KEY_KPEQUAL, plain(0x59)),
    // Keypad comma (HID 0x85, Brazilian ABNT keypad key 107).
    (KEY_KPCOMMA, plain(0x7E)),
    // Korean keys: make-only raw values, see the module documentation.
    (KEY_HANGEUL, plain(0xF2)),
    (KEY_HANJA, plain(0xF1)),
    (KEY_YEN, plain(0x7D)),
    (KEY_LEFTMETA, ext(0x5B)),
    (KEY_RIGHTMETA, ext(0x5C)),
    // The Menu / Application key.
    (KEY_COMPOSE, ext(0x5D)),
    // Browser stop.
    (KEY_STOP, ext(0x68)),
    (KEY_CALC, ext(0x21)),
    (KEY_MAIL, ext(0x6C)),
    // Browser favorites.
    (KEY_BOOKMARKS, ext(0x66)),
    // "My Computer".
    (KEY_COMPUTER, ext(0x6B)),
    (KEY_BACK, ext(0x6A)),
    (KEY_FORWARD, ext(0x69)),
    (KEY_NEXTSONG, ext(0x19)),
    (KEY_PLAYPAUSE, ext(0x22)),
    (KEY_PREVIOUSSONG, ext(0x10)),
    // Media stop.
    (KEY_STOPCD, ext(0x24)),
    (KEY_HOMEPAGE, ext(0x32)),
    (KEY_REFRESH, ext(0x67)),
    // F13..F23 are 0x64..0x6E; F24 is 0x76.
    (KEY_F13, plain(0x64)),
    (KEY_F14, plain(0x65)),
    (KEY_F15, plain(0x66)),
    (KEY_F16, plain(0x67)),
    (KEY_F17, plain(0x68)),
    (KEY_F18, plain(0x69)),
    (KEY_F19, plain(0x6A)),
    (KEY_F20, plain(0x6B)),
    (KEY_F21, plain(0x6C)),
    (KEY_F22, plain(0x6D)),
    (KEY_F23, plain(0x6E)),
    (KEY_F24, plain(0x76)),
    (KEY_SEARCH, ext(0x65)),
    // Media selection ("launch media player").
    (KEY_MEDIA, ext(0x6D)),
];

/// Translate an evdev key code. Returns `None` for keys without a mapping.
pub fn evdev_to_scancode(code: u16) -> Option<ScanCode> {
    TABLE
        .binary_search_by_key(&code, |&(evdev, _)| evdev)
        .ok()
        .map(|index| TABLE[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sc(code: u16) -> ScanCode {
        plain(code)
    }

    fn lookup(evdev: u16) -> ScanCode {
        evdev_to_scancode(evdev).unwrap_or_else(|| panic!("evdev code {evdev} is unmapped"))
    }

    #[test]
    fn table_is_sorted() {
        for pair in TABLE.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "evdev codes must be strictly ascending: {} then {}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    #[test]
    fn letters() {
        let rows: [(u16, u16); 26] = [
            (KEY_A, 0x1E),
            (KEY_B, 0x30),
            (KEY_C, 0x2E),
            (KEY_D, 0x20),
            (KEY_E, 0x12),
            (KEY_F, 0x21),
            (KEY_G, 0x22),
            (KEY_H, 0x23),
            (KEY_I, 0x17),
            (KEY_J, 0x24),
            (KEY_K, 0x25),
            (KEY_L, 0x26),
            (KEY_M, 0x32),
            (KEY_N, 0x31),
            (KEY_O, 0x18),
            (KEY_P, 0x19),
            (KEY_Q, 0x10),
            (KEY_R, 0x13),
            (KEY_S, 0x1F),
            (KEY_T, 0x14),
            (KEY_U, 0x16),
            (KEY_V, 0x2F),
            (KEY_W, 0x11),
            (KEY_X, 0x2D),
            (KEY_Y, 0x15),
            (KEY_Z, 0x2C),
        ];
        for (evdev, scan) in rows {
            assert_eq!(lookup(evdev), sc(scan), "evdev {evdev}");
        }
    }

    #[test]
    fn digits() {
        let rows: [(u16, u16); 10] = [
            (KEY_1, 0x02),
            (KEY_2, 0x03),
            (KEY_3, 0x04),
            (KEY_4, 0x05),
            (KEY_5, 0x06),
            (KEY_6, 0x07),
            (KEY_7, 0x08),
            (KEY_8, 0x09),
            (KEY_9, 0x0A),
            (KEY_0, 0x0B),
        ];
        for (evdev, scan) in rows {
            assert_eq!(lookup(evdev), sc(scan), "evdev {evdev}");
        }
    }

    #[test]
    fn function_keys() {
        assert_eq!(lookup(KEY_F1), sc(0x3B));
        assert_eq!(lookup(KEY_F10), sc(0x44));
        assert_eq!(lookup(KEY_F11), sc(0x57));
        assert_eq!(lookup(KEY_F12), sc(0x58));
        assert_eq!(lookup(KEY_F13), sc(0x64));
        assert_eq!(lookup(KEY_F23), sc(0x6E));
        assert_eq!(lookup(KEY_F24), sc(0x76));
        // F1..F10 and F13..F23 are contiguous in both numberings.
        for i in 0..10 {
            assert_eq!(lookup(KEY_F1 + i), sc(0x3B + i));
        }
        for i in 0..11 {
            assert_eq!(lookup(KEY_F13 + i), sc(0x64 + i));
        }
    }

    #[test]
    fn punctuation() {
        assert_eq!(lookup(KEY_MINUS), sc(0x0C));
        assert_eq!(lookup(KEY_EQUAL), sc(0x0D));
        assert_eq!(lookup(KEY_LEFTBRACE), sc(0x1A));
        assert_eq!(lookup(KEY_RIGHTBRACE), sc(0x1B));
        assert_eq!(lookup(KEY_SEMICOLON), sc(0x27));
        assert_eq!(lookup(KEY_APOSTROPHE), sc(0x28));
        assert_eq!(lookup(KEY_GRAVE), sc(0x29));
        assert_eq!(lookup(KEY_BACKSLASH), sc(0x2B));
        assert_eq!(lookup(KEY_COMMA), sc(0x33));
        assert_eq!(lookup(KEY_DOT), sc(0x34));
        assert_eq!(lookup(KEY_SLASH), sc(0x35));
    }

    #[test]
    fn enter_and_keypad_enter_differ_by_extended_flag() {
        assert_eq!(lookup(KEY_ENTER), sc(0x1C));
        assert_eq!(lookup(KEY_KPENTER), ext(0x1C));
        assert_ne!(lookup(KEY_ENTER), lookup(KEY_KPENTER));
        // Likewise for the keypad slash and the main-block slash.
        assert_eq!(lookup(KEY_SLASH), sc(0x35));
        assert_eq!(lookup(KEY_KPSLASH), ext(0x35));
    }

    #[test]
    fn modifiers() {
        assert_eq!(lookup(KEY_LEFTCTRL), sc(0x1D));
        assert_eq!(lookup(KEY_RIGHTCTRL), ext(0x1D));
        assert_eq!(lookup(KEY_LEFTSHIFT), sc(0x2A));
        assert_eq!(lookup(KEY_RIGHTSHIFT), sc(0x36));
        assert_eq!(lookup(KEY_LEFTALT), sc(0x38));
        assert_eq!(lookup(KEY_RIGHTALT), ext(0x38));
        assert_eq!(lookup(KEY_LEFTMETA), ext(0x5B));
        assert_eq!(lookup(KEY_RIGHTMETA), ext(0x5C));
        assert_eq!(lookup(KEY_COMPOSE), ext(0x5D));
        assert_eq!(lookup(KEY_CAPSLOCK), sc(0x3A));
    }

    #[test]
    fn navigation_keys_are_extended() {
        let rows: [(u16, u16); 10] = [
            (KEY_INSERT, 0x52),
            (KEY_DELETE, 0x53),
            (KEY_HOME, 0x47),
            (KEY_END, 0x4F),
            (KEY_PAGEUP, 0x49),
            (KEY_PAGEDOWN, 0x51),
            (KEY_UP, 0x48),
            (KEY_LEFT, 0x4B),
            (KEY_RIGHT, 0x4D),
            (KEY_DOWN, 0x50),
        ];
        for (evdev, scan) in rows {
            assert_eq!(lookup(evdev), ext(scan), "evdev {evdev}");
        }
    }

    #[test]
    fn keypad_and_locks() {
        assert_eq!(lookup(KEY_NUMLOCK), sc(0x45));
        assert_eq!(lookup(KEY_SCROLLLOCK), sc(0x46));
        assert_eq!(lookup(KEY_KP7), sc(0x47));
        assert_eq!(lookup(KEY_KP0), sc(0x52));
        assert_eq!(lookup(KEY_KPDOT), sc(0x53));
        assert_eq!(lookup(KEY_KPASTERISK), sc(0x37));
        assert_eq!(lookup(KEY_KPMINUS), sc(0x4A));
        assert_eq!(lookup(KEY_KPPLUS), sc(0x4E));
        assert_eq!(lookup(KEY_KPEQUAL), sc(0x59));
        // Keypad 7 and Home share a number and differ only in the prefix.
        assert_ne!(lookup(KEY_KP7), lookup(KEY_HOME));
    }

    #[test]
    fn print_screen_is_extended_and_pause_is_unmapped() {
        assert_eq!(lookup(KEY_SYSRQ), ext(0x37));
        assert_ne!(lookup(KEY_SYSRQ), lookup(KEY_KPASTERISK));
        // KEY_PAUSE (119) has no ScanCode representation, see module docs.
        assert_eq!(evdev_to_scancode(119), None);
    }

    #[test]
    fn japanese_keys() {
        assert_eq!(lookup(KEY_HENKAN), sc(0x79));
        assert_eq!(lookup(KEY_MUHENKAN), sc(0x7B));
        assert_eq!(lookup(KEY_KATAKANAHIRAGANA), sc(0x70));
        assert_eq!(lookup(KEY_ZENKAKUHANKAKU), sc(0x29));
        assert_eq!(lookup(KEY_YEN), sc(0x7D));
        assert_eq!(lookup(KEY_RO), sc(0x73));
        assert_eq!(lookup(KEY_102ND), sc(0x56));
    }

    #[test]
    fn korean_keys_are_raw_make_only_values() {
        assert_eq!(lookup(KEY_HANGEUL), sc(0xF2));
        assert_eq!(lookup(KEY_HANJA), sc(0xF1));
    }

    #[test]
    fn media_and_volume_keys_are_extended() {
        assert_eq!(lookup(KEY_MUTE), ext(0x20));
        assert_eq!(lookup(KEY_VOLUMEDOWN), ext(0x2E));
        assert_eq!(lookup(KEY_VOLUMEUP), ext(0x30));
        assert_eq!(lookup(KEY_PLAYPAUSE), ext(0x22));
        assert_eq!(lookup(KEY_STOPCD), ext(0x24));
        assert_eq!(lookup(KEY_PREVIOUSSONG), ext(0x10));
        assert_eq!(lookup(KEY_NEXTSONG), ext(0x19));
    }

    #[test]
    fn power_keys_are_deliberately_unmapped() {
        // KEY_POWER, KEY_SLEEP, KEY_WAKEUP
        for evdev in [116, 142, 143] {
            assert_eq!(evdev_to_scancode(evdev), None, "evdev {evdev}");
        }
    }

    #[test]
    fn unknown_codes_are_none() {
        assert_eq!(evdev_to_scancode(0), None); // KEY_RESERVED
        assert_eq!(evdev_to_scancode(84), None);
        assert_eq!(evdev_to_scancode(240), None); // KEY_UNKNOWN
        assert_eq!(evdev_to_scancode(0x110), None); // BTN_LEFT
        assert_eq!(evdev_to_scancode(u16::MAX), None);
    }

    /// Scan codes that two evdev codes may share on purpose.
    const ALIASED: &[ScanCode] = &[
        // KEY_GRAVE and KEY_ZENKAKUHANKAKU: same physical position.
        plain(0x29),
    ];

    #[test]
    fn no_unintended_duplicate_scancodes() {
        let mut seen: HashMap<(u16, bool), u16> = HashMap::new();
        for &(evdev, scan) in TABLE {
            if let Some(previous) = seen.insert((scan.code, scan.extended), evdev) {
                assert!(
                    ALIASED.contains(&scan),
                    "evdev {previous} and {evdev} both map to {scan:?}"
                );
            }
        }
    }

    #[test]
    fn documented_alias_holds() {
        assert_eq!(lookup(KEY_GRAVE), lookup(KEY_ZENKAKUHANKAKU));
    }
}
