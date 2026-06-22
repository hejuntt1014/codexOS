use crate::DesktopInput;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_AUXILIARY_DATA: u8 = 1 << 5;

pub struct Ps2Keyboard {
    decoder: Ps2Set1Decoder,
}

impl Ps2Keyboard {
    pub const fn new() -> Self {
        Self {
            decoder: Ps2Set1Decoder::new(),
        }
    }

    pub fn poll_input(&mut self) -> Option<DesktopInput> {
        loop {
            let status = unsafe { inb(PS2_STATUS) };
            if status & STATUS_OUTPUT_FULL == 0 {
                return None;
            }

            let scancode = unsafe { inb(PS2_DATA) };
            if status & STATUS_AUXILIARY_DATA != 0 {
                continue;
            }
            if let Some(input) = self.decoder.feed(scancode) {
                return Some(input);
            }
        }
    }
}

impl Default for Ps2Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ps2Set1Decoder {
    extended: bool,
    left_shift: bool,
    right_shift: bool,
    caps_lock: bool,
}

impl Ps2Set1Decoder {
    pub const fn new() -> Self {
        Self {
            extended: false,
            left_shift: false,
            right_shift: false,
            caps_lock: false,
        }
    }

    pub fn feed(&mut self, scancode: u8) -> Option<DesktopInput> {
        if scancode == 0xE0 {
            self.extended = true;
            return None;
        }

        let extended = core::mem::take(&mut self.extended);
        let released = scancode & 0x80 != 0;
        let code = scancode & 0x7F;

        if extended {
            if released {
                return None;
            }
            return match code {
                0x48 => Some(DesktopInput::MoveUp),
                0x50 => Some(DesktopInput::MoveDown),
                0x4B => Some(DesktopInput::MoveLeft),
                0x4D => Some(DesktopInput::MoveRight),
                _ => None,
            };
        }

        match code {
            0x2A => {
                self.left_shift = !released;
                return None;
            }
            0x36 => {
                self.right_shift = !released;
                return None;
            }
            0x3A if !released => {
                self.caps_lock = !self.caps_lock;
                return None;
            }
            _ => {}
        }
        if released {
            return None;
        }

        match code {
            0x01 => Some(DesktopInput::Exit),
            0x0E => Some(DesktopInput::Backspace),
            0x0F => Some(DesktopInput::CycleFocus),
            0x1C => Some(DesktopInput::Submit),
            _ => map_printable(code, self.left_shift || self.right_shift, self.caps_lock)
                .map(DesktopInput::Character),
        }
    }
}

impl Default for Ps2Set1Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn map_printable(code: u8, shift: bool, caps_lock: bool) -> Option<char> {
    let letter = match code {
        0x10 => Some('q'),
        0x11 => Some('w'),
        0x12 => Some('e'),
        0x13 => Some('r'),
        0x14 => Some('t'),
        0x15 => Some('y'),
        0x16 => Some('u'),
        0x17 => Some('i'),
        0x18 => Some('o'),
        0x19 => Some('p'),
        0x1E => Some('a'),
        0x1F => Some('s'),
        0x20 => Some('d'),
        0x21 => Some('f'),
        0x22 => Some('g'),
        0x23 => Some('h'),
        0x24 => Some('j'),
        0x25 => Some('k'),
        0x26 => Some('l'),
        0x2C => Some('z'),
        0x2D => Some('x'),
        0x2E => Some('c'),
        0x2F => Some('v'),
        0x30 => Some('b'),
        0x31 => Some('n'),
        0x32 => Some('m'),
        _ => None,
    };
    if let Some(letter) = letter {
        return Some(if shift ^ caps_lock {
            letter.to_ascii_uppercase()
        } else {
            letter
        });
    }

    let pair = match code {
        0x02 => Some(('1', '!')),
        0x03 => Some(('2', '@')),
        0x04 => Some(('3', '#')),
        0x05 => Some(('4', '$')),
        0x06 => Some(('5', '%')),
        0x07 => Some(('6', '^')),
        0x08 => Some(('7', '&')),
        0x09 => Some(('8', '*')),
        0x0A => Some(('9', '(')),
        0x0B => Some(('0', ')')),
        0x0C => Some(('-', '_')),
        0x0D => Some(('=', '+')),
        0x1A => Some(('[', '{')),
        0x1B => Some((']', '}')),
        0x27 => Some((';', ':')),
        0x28 => Some(('\'', '"')),
        0x29 => Some(('`', '~')),
        0x2B => Some(('\\', '|')),
        0x33 => Some((',', '<')),
        0x34 => Some(('.', '>')),
        0x35 => Some(('/', '?')),
        0x39 => Some((' ', ' ')),
        _ => None,
    }?;
    Some(if shift { pair.1 } else { pair.0 })
}

#[cfg(target_arch = "x86_64")]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn inb(_port: u16) -> u8 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_with_shift_caps_and_punctuation() {
        let mut decoder = Ps2Set1Decoder::new();
        assert!(matches!(
            decoder.feed(0x1E),
            Some(DesktopInput::Character('a'))
        ));
        assert!(decoder.feed(0x2A).is_none());
        assert!(matches!(
            decoder.feed(0x1E),
            Some(DesktopInput::Character('A'))
        ));
        assert!(decoder.feed(0xAA).is_none());
        assert!(decoder.feed(0x3A).is_none());
        assert!(matches!(
            decoder.feed(0x1E),
            Some(DesktopInput::Character('A'))
        ));
        assert!(decoder.feed(0x2A).is_none());
        assert!(matches!(
            decoder.feed(0x1E),
            Some(DesktopInput::Character('a'))
        ));
        assert!(matches!(
            decoder.feed(0x02),
            Some(DesktopInput::Character('!'))
        ));
    }

    #[test]
    fn ignores_releases_and_decodes_extended_arrows() {
        let mut decoder = Ps2Set1Decoder::new();
        assert!(decoder.feed(0x9E).is_none());
        assert!(decoder.feed(0xE0).is_none());
        assert!(matches!(decoder.feed(0x48), Some(DesktopInput::MoveUp)));
        assert!(decoder.feed(0xE0).is_none());
        assert!(decoder.feed(0xC8).is_none());
    }

    #[test]
    fn decodes_desktop_control_keys() {
        let mut decoder = Ps2Set1Decoder::new();
        assert!(matches!(decoder.feed(0x0F), Some(DesktopInput::CycleFocus)));
        assert!(matches!(decoder.feed(0x0E), Some(DesktopInput::Backspace)));
        assert!(matches!(decoder.feed(0x1C), Some(DesktopInput::Submit)));
        assert!(matches!(decoder.feed(0x01), Some(DesktopInput::Exit)));
    }
}
