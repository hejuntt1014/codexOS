use crate::{DesktopInput, PointerSample};

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUXILIARY_DATA: u8 = 1 << 5;
const CONTROLLER_READ_CONFIG: u8 = 0x20;
const CONTROLLER_WRITE_CONFIG: u8 = 0x60;
const CONTROLLER_ENABLE_AUXILIARY: u8 = 0xA8;
const CONTROLLER_WRITE_AUXILIARY: u8 = 0xD4;
const CONFIG_AUXILIARY_DISABLED: u8 = 1 << 5;
const MOUSE_ACKNOWLEDGED: u8 = 0xFA;
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_STREAMING: u8 = 0xF4;
const MOUSE_IDENTIFY: u8 = 0xF2;
const PS2_WAIT_LIMIT: usize = 100_000;
const PS2_DRAIN_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ps2PointerStatus {
    pub enabled: bool,
    pub device_id: Option<u8>,
    pub acknowledgements: u8,
}

#[derive(Clone, Copy, Debug)]
pub enum Ps2Event {
    Keyboard(DesktopInput),
    Pointer(PointerSample),
}

pub struct Ps2InputDevices {
    keyboard: Ps2Set1Decoder,
    mouse: Ps2MouseDecoder,
    pointer: Ps2PointerStatus,
}

impl Ps2InputDevices {
    pub fn initialize() -> Self {
        Self {
            keyboard: Ps2Set1Decoder::new(),
            mouse: Ps2MouseDecoder::new(),
            pointer: initialize_pointer_device(),
        }
    }

    pub const fn pointer_status(&self) -> Ps2PointerStatus {
        self.pointer
    }

    pub fn poll_event(&mut self) -> Option<Ps2Event> {
        loop {
            let status = unsafe { inb(PS2_STATUS) };
            if status & STATUS_OUTPUT_FULL == 0 {
                return None;
            }

            let byte = unsafe { inb(PS2_DATA) };
            if status & STATUS_AUXILIARY_DATA != 0 {
                if self.pointer.enabled
                    && let Some(sample) = self.mouse.feed(byte)
                {
                    return Some(Ps2Event::Pointer(sample));
                }
                continue;
            }

            if let Some(input) = self.keyboard.feed(byte) {
                return Some(Ps2Event::Keyboard(input));
            }
        }
    }
}

impl Default for Ps2InputDevices {
    fn default() -> Self {
        Self::initialize()
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ps2MouseDecoder {
    packet: [u8; 3],
    packet_index: usize,
}

impl Ps2MouseDecoder {
    pub const fn new() -> Self {
        Self {
            packet: [0; 3],
            packet_index: 0,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<PointerSample> {
        if self.packet_index == 0 && byte & 0x08 == 0 {
            return None;
        }

        self.packet[self.packet_index] = byte;
        self.packet_index += 1;
        if self.packet_index < self.packet.len() {
            return None;
        }
        self.packet_index = 0;

        let header = self.packet[0];
        if header & 0xC0 != 0 {
            return None;
        }

        let dx = sign_extend_mouse_delta(self.packet[1], header & 0x10 != 0);
        let ps2_y = sign_extend_mouse_delta(self.packet[2], header & 0x20 != 0);
        Some(PointerSample {
            delta_x: dx,
            delta_y: -ps2_y,
            left_button: header & 0x01 != 0,
            right_button: header & 0x02 != 0,
        })
    }
}

impl Default for Ps2MouseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn sign_extend_mouse_delta(byte: u8, negative: bool) -> i32 {
    if negative {
        i32::from(byte as i8)
    } else {
        i32::from(byte)
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

fn initialize_pointer_device() -> Ps2PointerStatus {
    drain_ps2_output();
    if !write_controller_command(CONTROLLER_ENABLE_AUXILIARY) {
        return pointer_status(false, None, 0);
    }

    if let Some(config) = read_controller_config() {
        let enabled_config = config & !CONFIG_AUXILIARY_DISABLED;
        let _ = write_controller_config(enabled_config);
    }

    let mut acknowledgements = 0;
    if send_mouse_command(MOUSE_SET_DEFAULTS) {
        acknowledgements += 1;
    } else {
        return pointer_status(false, None, acknowledgements);
    }

    let mut device_id = None;
    if send_mouse_command(MOUSE_IDENTIFY) {
        acknowledgements += 1;
        device_id = read_auxiliary_data_byte();
    }

    let enabled = if send_mouse_command(MOUSE_ENABLE_STREAMING) {
        acknowledgements += 1;
        true
    } else {
        false
    };

    pointer_status(enabled, device_id, acknowledgements)
}

const fn pointer_status(
    enabled: bool,
    device_id: Option<u8>,
    acknowledgements: u8,
) -> Ps2PointerStatus {
    Ps2PointerStatus {
        enabled,
        device_id,
        acknowledgements,
    }
}

fn read_controller_config() -> Option<u8> {
    if !write_controller_command(CONTROLLER_READ_CONFIG) {
        return None;
    }
    read_any_data_byte()
}

fn write_controller_config(config: u8) -> bool {
    write_controller_command(CONTROLLER_WRITE_CONFIG) && write_data_byte(config)
}

fn send_mouse_command(command: u8) -> bool {
    if !write_controller_command(CONTROLLER_WRITE_AUXILIARY) || !write_data_byte(command) {
        return false;
    }
    read_auxiliary_data_byte() == Some(MOUSE_ACKNOWLEDGED)
}

fn write_controller_command(command: u8) -> bool {
    if !wait_until_controller_input_clear() {
        return false;
    }
    unsafe { outb(PS2_STATUS, command) };
    true
}

fn write_data_byte(byte: u8) -> bool {
    if !wait_until_controller_input_clear() {
        return false;
    }
    unsafe { outb(PS2_DATA, byte) };
    true
}

fn wait_until_controller_input_clear() -> bool {
    for _ in 0..PS2_WAIT_LIMIT {
        if unsafe { inb(PS2_STATUS) } & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

fn read_any_data_byte() -> Option<u8> {
    for _ in 0..PS2_WAIT_LIMIT {
        if unsafe { inb(PS2_STATUS) } & STATUS_OUTPUT_FULL != 0 {
            return Some(unsafe { inb(PS2_DATA) });
        }
    }
    None
}

fn read_auxiliary_data_byte() -> Option<u8> {
    for _ in 0..PS2_WAIT_LIMIT {
        let status = unsafe { inb(PS2_STATUS) };
        if status & STATUS_OUTPUT_FULL == 0 {
            continue;
        }
        let byte = unsafe { inb(PS2_DATA) };
        if status & STATUS_AUXILIARY_DATA != 0 {
            return Some(byte);
        }
    }
    None
}

fn drain_ps2_output() {
    for _ in 0..PS2_DRAIN_LIMIT {
        if unsafe { inb(PS2_STATUS) } & STATUS_OUTPUT_FULL == 0 {
            return;
        }
        let _ = unsafe { inb(PS2_DATA) };
    }
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

#[cfg(target_arch = "x86_64")]
unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn outb(_port: u16, _value: u8) {}

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

    #[test]
    fn decodes_ps2_mouse_motion_buttons_and_screen_y_axis() {
        let mut decoder = Ps2MouseDecoder::new();
        assert!(decoder.feed(0x09).is_none());
        assert!(decoder.feed(5).is_none());
        let sample = decoder.feed(2).expect("mouse packet");
        assert_eq!(sample.delta_x, 5);
        assert_eq!(sample.delta_y, -2);
        assert!(sample.left_button);
        assert!(!sample.right_button);

        assert!(decoder.feed(0x3A).is_none());
        assert!(decoder.feed(0xFC).is_none());
        let sample = decoder.feed(0xFD).expect("signed mouse packet");
        assert_eq!(sample.delta_x, -4);
        assert_eq!(sample.delta_y, 3);
        assert!(!sample.left_button);
        assert!(sample.right_button);
    }

    #[test]
    fn rejects_mouse_overflow_and_resynchronizes_on_header_bit() {
        let mut decoder = Ps2MouseDecoder::new();
        assert!(decoder.feed(0x01).is_none());
        assert!(decoder.feed(0x4B).is_none());
        assert!(decoder.feed(1).is_none());
        assert!(decoder.feed(1).is_none());

        assert!(decoder.feed(0x08).is_none());
        assert!(decoder.feed(7).is_none());
        let sample = decoder.feed(0).expect("resynchronized mouse packet");
        assert_eq!(sample.delta_x, 7);
        assert_eq!(sample.delta_y, 0);
        assert!(!sample.left_button);
        assert!(!sample.right_button);
    }
}
