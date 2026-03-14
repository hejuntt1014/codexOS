#![no_std]
#![no_main]

use bootinfo::{BootInfo, FrameBufferInfo};
use core::hint::spin_loop;
use core::panic::PanicInfo;
use gfx::{Canvas, Color};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct KernelImageDescriptor {
    pub magic: [u8; 8],
    pub abi_version: u32,
    pub reserved: u32,
    pub entry_hint: u64,
}

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_DESCRIPTOR: KernelImageDescriptor = KernelImageDescriptor {
    magic: *b"CDXKERN\0",
    abi_version: 1,
    reserved: 0,
    entry_hint: 0,
};

#[unsafe(no_mangle)]
#[used]
pub static CODEXOS_KERNEL_BANNER: [u8; 23] = *b"codexOS kernel image v1";

#[unsafe(no_mangle)]
#[used]
pub static mut CODEXOS_EARLY_BSS: [u8; 4096] = [0; 4096];

const COM1: u16 = 0x3F8;
const PS2_STATUS: u16 = 0x64;
const PS2_DATA: u16 = 0x60;

#[unsafe(no_mangle)]
pub extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    serial_init();
    serial_write_str("codexOS standalone kernel entered\r\n");

    if let Some(boot_info) = unsafe { boot_info.as_ref() } {
        serial_write_str("standalone boot info present\r\n");
        if !run_standalone_scene(boot_info) {
            serial_write_str("standalone framebuffer unavailable\r\n");
        }
    } else {
        serial_write_str("standalone boot info missing\r\n");
    }

    loop {
        spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        spin_loop();
    }
}

fn run_standalone_scene(boot_info: &BootInfo) -> bool {
    let mut desktop = StandaloneDesktop::new(boot_info);
    let mut keyboard = Ps2Keyboard::new();
    let mut announced = false;
    let mut frame_divider = 0u32;

    loop {
        let mut had_input = false;
        while let Some(action) = keyboard.poll_action() {
            desktop.handle_input(action);
            had_input = true;
        }

        frame_divider = frame_divider.wrapping_add(1);
        if had_input || frame_divider % 2 == 0 {
            desktop.advance(boot_info.kernel_image.segments().len().min(2));
        }

        if !desktop.render(boot_info) {
            return false;
        }

        if !announced {
            serial_write_str("standalone framebuffer takeover complete\r\n");
            serial_write_str("standalone desktop rendered\r\n");
            serial_write_str("standalone keyboard polling active\r\n");
            announced = true;
        }

        let idle_budget = if had_input {
            20_000
        } else if frame_divider % 2 == 0 {
            120_000
        } else {
            200_000
        };
        busy_wait(idle_budget);
    }
}

#[derive(Clone, Copy)]
struct WindowState {
    title: &'static str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    progress: i32,
    accent: Color,
}

impl WindowState {
    const fn new(
        title: &'static str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        progress: i32,
        accent: Color,
    ) -> Self {
        Self {
            title,
            x,
            y,
            width,
            height,
            progress,
            accent,
        }
    }
}

#[derive(Clone, Copy)]
struct TextBuffer {
    bytes: [u8; 32],
    len: usize,
}

impl TextBuffer {
    const fn empty() -> Self {
        Self {
            bytes: [0; 32],
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.bytes = [0; 32];
        self.len = 0;
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len >= self.bytes.len() {
            return;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
    }

    fn pop_byte(&mut self) {
        if self.len == 0 {
            return;
        }
        self.len -= 1;
        self.bytes[self.len] = 0;
    }

    fn append_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn append_buffer(&mut self, other: &Self) {
        self.append_bytes(&other.bytes[..other.len]);
    }

    fn equals(&self, bytes: &[u8]) -> bool {
        if self.len != bytes.len() {
            return false;
        }

        let mut index = 0;
        while index < self.len {
            if self.bytes[index] != bytes[index] {
                return false;
            }
            index += 1;
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn append_decimal(&mut self, mut value: u64) {
        if value == 0 {
            self.push_byte(b'0');
            return;
        }

        let mut digits = [0u8; 20];
        let mut len = 0usize;
        while value != 0 && len < digits.len() {
            digits[len] = b'0' + (value % 10) as u8;
            value /= 10;
            len += 1;
        }

        while len != 0 {
            len -= 1;
            self.push_byte(digits[len]);
        }
    }
}

enum InputAction {
    CycleFocus,
    FocusShell,
    FocusView,
    FocusTask,
    InsertByte(u8),
    Backspace,
    Submit,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
}

#[derive(Clone, Copy)]
enum InputHint {
    Boot,
    Tab,
    One,
    Two,
    Three,
    Type,
    Run,
    Back,
    Left,
    Right,
    Up,
    Down,
}

impl InputHint {
    const fn label(self) -> &'static str {
        match self {
            Self::Boot => "BOOT",
            Self::Tab => "TAB",
            Self::One => "ONE",
            Self::Two => "TWO",
            Self::Three => "THREE",
            Self::Type => "TYPE",
            Self::Run => "RUN",
            Self::Back => "BACK",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
            Self::Up => "UP",
            Self::Down => "DOWN",
        }
    }
}

struct Ps2Keyboard {
    extended: bool,
}

impl Ps2Keyboard {
    const fn new() -> Self {
        Self { extended: false }
    }

    fn poll_action(&mut self) -> Option<InputAction> {
        if unsafe { inb(PS2_STATUS) } & 0x01 == 0 {
            return None;
        }

        let scancode = unsafe { inb(PS2_DATA) };
        if scancode == 0xE0 {
            self.extended = true;
            return None;
        }

        let extended = self.extended;
        self.extended = false;

        if scancode & 0x80 != 0 {
            return None;
        }

        if extended {
            match scancode {
                0x48 => Some(InputAction::MoveUp),
                0x50 => Some(InputAction::MoveDown),
                0x4B => Some(InputAction::MoveLeft),
                0x4D => Some(InputAction::MoveRight),
                _ => None,
            }
        } else {
            match scancode {
                0x0F => Some(InputAction::CycleFocus),
                0x02 => Some(InputAction::FocusShell),
                0x03 => Some(InputAction::FocusView),
                0x04 => Some(InputAction::FocusTask),
                0x0E => Some(InputAction::Backspace),
                0x1C => Some(InputAction::Submit),
                0x39 => Some(InputAction::InsertByte(b' ')),
                0x10 => Some(InputAction::InsertByte(b'Q')),
                0x11 => Some(InputAction::InsertByte(b'W')),
                0x12 => Some(InputAction::InsertByte(b'E')),
                0x13 => Some(InputAction::InsertByte(b'R')),
                0x14 => Some(InputAction::InsertByte(b'T')),
                0x15 => Some(InputAction::InsertByte(b'Y')),
                0x16 => Some(InputAction::InsertByte(b'U')),
                0x17 => Some(InputAction::InsertByte(b'I')),
                0x18 => Some(InputAction::InsertByte(b'O')),
                0x19 => Some(InputAction::InsertByte(b'P')),
                0x1E => Some(InputAction::InsertByte(b'A')),
                0x1F => Some(InputAction::InsertByte(b'S')),
                0x20 => Some(InputAction::InsertByte(b'D')),
                0x21 => Some(InputAction::InsertByte(b'F')),
                0x22 => Some(InputAction::InsertByte(b'G')),
                0x23 => Some(InputAction::InsertByte(b'H')),
                0x24 => Some(InputAction::InsertByte(b'J')),
                0x25 => Some(InputAction::InsertByte(b'K')),
                0x26 => Some(InputAction::InsertByte(b'L')),
                0x2C => Some(InputAction::InsertByte(b'Z')),
                0x2D => Some(InputAction::InsertByte(b'X')),
                0x2E => Some(InputAction::InsertByte(b'C')),
                0x2F => Some(InputAction::InsertByte(b'V')),
                0x30 => Some(InputAction::InsertByte(b'B')),
                0x31 => Some(InputAction::InsertByte(b'N')),
                0x32 => Some(InputAction::InsertByte(b'M')),
                _ => None,
            }
        }
    }
}

struct StandaloneDesktop {
    screen_width: i32,
    screen_height: i32,
    frame: u32,
    pulse: i32,
    cursor_bob: i32,
    segment_highlight: usize,
    active_window: usize,
    last_input: InputHint,
    input_flash: i32,
    post_ebs: bool,
    memory_region_count: usize,
    usable_mib: u64,
    kernel_segment_count: usize,
    shell_input: TextBuffer,
    shell_log: [TextBuffer; 4],
    shell_history: [TextBuffer; 4],
    shell_history_count: usize,
    windows: [WindowState; 3],
}

impl StandaloneDesktop {
    fn new(boot_info: &BootInfo) -> Self {
        let mut desktop = Self {
            screen_width: boot_info.framebuffer.width as i32,
            screen_height: boot_info.framebuffer.height as i32,
            frame: 0,
            pulse: 0,
            cursor_bob: 0,
            segment_highlight: 0,
            active_window: 0,
            last_input: InputHint::Boot,
            input_flash: 100,
            post_ebs: boot_info.firmware_mode.as_str() == "post-exit-boot-services",
            memory_region_count: boot_info.memory_region_count,
            usable_mib: boot_info.usable_memory_bytes() / (1024 * 1024),
            kernel_segment_count: boot_info.kernel_image.load_segment_count as usize,
            shell_input: TextBuffer::empty(),
            shell_log: [TextBuffer::empty(); 4],
            shell_history: [TextBuffer::empty(); 4],
            shell_history_count: 0,
            windows: [
                WindowState::new(
                    "SHELL",
                    118,
                    156,
                    248,
                    140,
                    12,
                    Color::rgb(32, 92, 128),
                ),
                WindowState::new(
                    "VIEW",
                    938,
                    180,
                    226,
                    126,
                    18,
                    Color::rgb(36, 127, 170),
                ),
                WindowState::new(
                    "TASK",
                    500,
                    550,
                    280,
                    118,
                    24,
                    Color::rgb(56, 118, 149),
                ),
            ],
        };
        desktop.push_shell_line_bytes(b"READY");
        desktop.push_shell_line_bytes(b"HELP STATUS BOOT MEM");
        desktop.push_shell_line_bytes(b"KERNEL LEFT RIGHT");
        desktop.push_shell_line_bytes(b"UP DOWN CLEAR VIEW TASK");
        desktop.push_shell_line_bytes(b"HISTORY AGAIN");
        desktop
    }

    fn advance(&mut self, segment_count: usize) {
        self.frame = self.frame.wrapping_add(1);
        self.pulse = ((self.frame / 6) % 80) as i32;
        self.cursor_bob = ((self.frame / 10) % 12) as i32;
        self.segment_highlight = if segment_count == 0 {
            0
        } else {
            ((self.frame / 24) as usize) % segment_count
        };

        self.windows[0].progress = (self.windows[0].progress + 3).min(100);
        self.windows[1].progress = (self.windows[1].progress + 2).min(100);
        self.windows[2].progress = (self.windows[2].progress + 4).min(100);

        if self.frame % 80 == 0 {
            match self.active_window {
                0 => self.windows[1].progress = (self.windows[1].progress + 11).min(100),
                1 => self.windows[2].progress = (self.windows[2].progress + 9).min(100),
                _ => self.windows[0].progress = (self.windows[0].progress + 7).min(100),
            }
        }

        if self.frame % 180 == 0 {
            self.windows[0].progress = 12;
            self.windows[1].progress = 18;
            self.windows[2].progress = 24;
        }

        self.input_flash = self.input_flash.saturating_sub(1);
    }

    fn handle_input(&mut self, action: InputAction) {
        match action {
            InputAction::CycleFocus => {
                self.active_window = (self.active_window + 1) % self.windows.len();
                self.note_input(InputHint::Tab);
            }
            InputAction::FocusShell => self.set_focus(0, InputHint::One),
            InputAction::FocusView => self.set_focus(1, InputHint::Two),
            InputAction::FocusTask => self.set_focus(2, InputHint::Three),
            InputAction::InsertByte(byte) => {
                if self.active_window == 0 {
                    self.shell_input.push_byte(byte);
                    self.note_input(InputHint::Type);
                }
            }
            InputAction::Backspace => {
                if self.active_window == 0 {
                    self.shell_input.pop_byte();
                    self.note_input(InputHint::Back);
                }
            }
            InputAction::Submit => {
                if self.active_window == 0 {
                    self.execute_shell_command();
                    self.note_input(InputHint::Run);
                }
            }
            InputAction::MoveLeft => {
                self.move_active_window(-18, 0);
                self.note_input(InputHint::Left);
            }
            InputAction::MoveRight => {
                self.move_active_window(18, 0);
                self.note_input(InputHint::Right);
            }
            InputAction::MoveUp => {
                self.move_active_window(0, -18);
                self.note_input(InputHint::Up);
            }
            InputAction::MoveDown => {
                self.move_active_window(0, 18);
                self.note_input(InputHint::Down);
            }
        }
    }

    fn set_focus(&mut self, index: usize, hint: InputHint) {
        self.active_window = index.min(self.windows.len().saturating_sub(1));
        self.note_input(hint);
    }

    fn note_input(&mut self, hint: InputHint) {
        self.last_input = hint;
        self.input_flash = 42;
    }

    fn push_shell_line(&mut self, line: TextBuffer) {
        let last = self.shell_log.len().saturating_sub(1);
        let mut index = 0;
        while index < last {
            self.shell_log[index] = self.shell_log[index + 1];
            index += 1;
        }
        self.shell_log[last] = line;
    }

    fn push_shell_line_bytes(&mut self, bytes: &[u8]) {
        let mut line = TextBuffer::empty();
        line.append_bytes(bytes);
        self.push_shell_line(line);
    }

    fn push_shell_command_buffer(&mut self, prefix: &[u8], command: &TextBuffer) {
        let mut line = TextBuffer::empty();
        line.append_bytes(prefix);
        if !command.is_empty() {
            line.push_byte(b' ');
            line.append_buffer(command);
        }
        self.push_shell_line(line);
    }

    fn clear_shell_log(&mut self) {
        self.shell_log = [TextBuffer::empty(); 4];
    }

    fn push_shell_history(&mut self, command: TextBuffer) {
        let last = self.shell_history.len().saturating_sub(1);
        let mut index = 0;
        while index < last {
            self.shell_history[index] = self.shell_history[index + 1];
            index += 1;
        }
        self.shell_history[last] = command;
        self.shell_history_count = (self.shell_history_count + 1).min(self.shell_history.len());
    }

    fn latest_shell_history(&self) -> Option<TextBuffer> {
        if self.shell_history_count == 0 {
            None
        } else {
            Some(self.shell_history[self.shell_history.len() - 1])
        }
    }

    fn execute_shell_command(&mut self) {
        if self.shell_input.is_empty() {
            self.push_shell_line_bytes(b"EMPTY COMMAND");
            return;
        }

        let command = self.shell_input;
        self.shell_input.clear();
        self.run_shell_command(command, true);
    }

    fn run_shell_command(&mut self, command: TextBuffer, record_history: bool) {
        self.push_shell_command_buffer(b"RUN", &command);

        if record_history && !command.equals(b"AGAIN") {
            self.push_shell_history(command);
        }

        if command.equals(b"HELP") {
            self.push_shell_line_bytes(b"HELP STATUS BOOT MEM");
            self.push_shell_line_bytes(b"KERNEL LEFT RIGHT");
            self.push_shell_line_bytes(b"UP DOWN CLEAR VIEW TASK");
            self.push_shell_line_bytes(b"HISTORY AGAIN");
        } else if command.equals(b"STATUS") {
            let mut line = TextBuffer::empty();
            line.append_bytes(b"ACTIVE ");
            line.append_bytes(self.active_window_label());
            line.append_bytes(b" REG ");
            line.append_decimal(self.memory_region_count as u64);
            line.append_bytes(b" HIST ");
            line.append_decimal(self.shell_history_count as u64);
            self.push_shell_line(line);
        } else if command.equals(b"BOOT") {
            if self.post_ebs {
                self.push_shell_line_bytes(b"MODE POST EBS");
            } else {
                self.push_shell_line_bytes(b"MODE UEFI");
            }
        } else if command.equals(b"MEM") {
            let mut line = TextBuffer::empty();
            line.append_bytes(b"MEM ");
            line.append_decimal(self.usable_mib);
            line.append_bytes(b" MIB");
            self.push_shell_line(line);
        } else if command.equals(b"KERNEL") {
            let mut line = TextBuffer::empty();
            line.append_bytes(b"SEGMENTS ");
            line.append_decimal(self.kernel_segment_count as u64);
            self.push_shell_line(line);
        } else if command.equals(b"HISTORY") {
            if self.shell_history_count == 0 {
                self.push_shell_line_bytes(b"NO HISTORY");
            } else {
                let start = self.shell_history.len() - self.shell_history_count;
                let mut index = start;
                while index < self.shell_history.len() {
                    let mut line = TextBuffer::empty();
                    line.append_bytes(b"H ");
                    line.append_decimal((index - start + 1) as u64);
                    line.push_byte(b' ');
                    line.append_buffer(&self.shell_history[index]);
                    self.push_shell_line(line);
                    index += 1;
                }
            }
        } else if command.equals(b"AGAIN") {
            if let Some(previous) = self.latest_shell_history() {
                self.push_shell_command_buffer(b"REPLAY", &previous);
                self.run_shell_command(previous, false);
            } else {
                self.push_shell_line_bytes(b"NO HISTORY");
            }
        } else if command.equals(b"CLEAR") {
            self.clear_shell_log();
            self.push_shell_line_bytes(b"READY");
        } else if command.equals(b"FOCUS") {
            self.set_focus(0, InputHint::Run);
            self.push_shell_line_bytes(b"FOCUS SHELL");
        } else if command.equals(b"VIEW") {
            self.set_focus(1, InputHint::Run);
            self.push_shell_line_bytes(b"FOCUS VIEW");
        } else if command.equals(b"TASK") {
            self.set_focus(2, InputHint::Run);
            self.push_shell_line_bytes(b"FOCUS TASK");
        } else if command.equals(b"LEFT") {
            self.move_active_window(-24, 0);
            self.push_shell_line_bytes(b"MOVE LEFT");
        } else if command.equals(b"RIGHT") {
            self.move_active_window(24, 0);
            self.push_shell_line_bytes(b"MOVE RIGHT");
        } else if command.equals(b"UP") {
            self.move_active_window(0, -24);
            self.push_shell_line_bytes(b"MOVE UP");
        } else if command.equals(b"DOWN") {
            self.move_active_window(0, 24);
            self.push_shell_line_bytes(b"MOVE DOWN");
        } else if command.equals(b"MOVE") {
            self.move_active_window(24, 0);
            self.push_shell_line_bytes(b"MOVE ACTIVE");
        } else {
            self.push_shell_line_bytes(b"UNKNOWN CMD");
        }
    }

    fn active_window_label(&self) -> &'static [u8] {
        match self.active_window {
            0 => b"SHELL",
            1 => b"VIEW",
            _ => b"TASK",
        }
    }

    fn move_active_window(&mut self, dx: i32, dy: i32) {
        let window = &mut self.windows[self.active_window];
        let min_x = 48;
        let min_y = 112;
        let max_x = (self.screen_width - window.width - 48).max(min_x);
        let max_y = (self.screen_height - window.height - 124).max(min_y);
        window.x = (window.x + dx).clamp(min_x, max_x);
        window.y = (window.y + dy).clamp(min_y, max_y);
    }

    fn render(&self, boot_info: &BootInfo) -> bool {
        let Some(framebuffer) = runtime_framebuffer(boot_info) else {
            return false;
        };

        let width = framebuffer.width as i32;
        let height = framebuffer.height as i32;
        let mut canvas = unsafe { Canvas::from_framebuffer(framebuffer) };

        canvas.vertical_gradient(Color::rgb(11, 17, 24), Color::rgb(26, 44, 61));
        canvas.checkerboard(
            48,
            Color::rgb(17, 26, 35),
            Color::rgb(38, 78, 110),
            1,
            6,
        );

        canvas.fill_rect(24, 24, width - 48, height - 48, Color::rgb(243, 246, 248));
        canvas.draw_shadow(24, 24, width - 48, height - 48, 6, Color::rgb(10, 15, 20));
        canvas.fill_rect(24, 24, width - 48, 72, Color::rgb(19, 88, 126));
        canvas.fill_rect(24, height - 92, width - 48, 44, Color::rgb(229, 235, 238));
        canvas.fill_rect(24, height - 92, width - 48, 1, Color::rgb(197, 205, 210));
        canvas.fill_rect(width - 146, 40, 88 + self.pulse / 2, 8, Color::rgb(83, 166, 201));

        self.draw_header(&mut canvas, boot_info, width);
        self.draw_runtime_windows(&mut canvas);
        self.draw_overview_card(&mut canvas, boot_info, 56, 128, width / 2 - 72, 292);
        self.draw_kernel_card(
            &mut canvas,
            boot_info,
            width / 2 + 16,
            128,
            width / 2 - 72,
            292,
        );
        self.draw_footer(&mut canvas, width, height);
        self.draw_cursor_accent(&mut canvas, width, height);

        true
    }

    fn draw_header(&self, canvas: &mut Canvas, boot_info: &BootInfo, width: i32) {
        let ink = Color::rgb(245, 248, 250);
        let sub = Color::rgb(205, 223, 231);
        draw_label(canvas, 52, 42, "CODEXOS", ink, 3);
        draw_label(canvas, 52, 68, "STANDALONE", sub, 2);
        draw_label(canvas, 52, 92, "CHAINLOAD DESKTOP", sub, 1);
        let mode_label = if boot_info.firmware_mode.as_str() == "post-exit-boot-services" {
            "MODE POST EBS"
        } else {
            "MODE UEFI"
        };
        draw_label(canvas, width - 286, 48, mode_label, ink, 1);
        draw_label(canvas, width - 286, 92, "TAB ARROWS 123 CMD", sub, 1);
        canvas.fill_rect(width - 286, 72, 120 + self.pulse, 10, Color::rgb(83, 166, 201));
    }

    fn draw_runtime_windows(&self, canvas: &mut Canvas) {
        for (index, window) in self.windows.iter().enumerate() {
            let active = index == self.active_window;
            self.draw_window(canvas, *window, active);
            match index {
                0 => {
                    self.draw_shell_window(canvas, *window);
                }
                1 => {
                    canvas.fill_rect(
                        window.x + 18,
                        window.y + 42,
                        80,
                        48,
                        Color::rgb(223, 231, 235),
                    );
                    canvas.fill_rect(
                        window.x + 112,
                        window.y + 42,
                        96,
                        12,
                        Color::rgb(83, 166, 201),
                    );
                    self.draw_progress_track(
                        canvas,
                        window.x + 112,
                        window.y + 62,
                        90,
                        window.progress,
                        Color::rgb(32, 92, 128),
                    );
                    draw_label(canvas, window.x + 18, window.y + 80, "FOCUS VIEW", window.accent, 1);
                    draw_label(canvas, window.x + 112, window.y + 80, "KEY TWO", Color::rgb(78, 91, 101), 1);
                }
                _ => {
                    self.draw_progress_track(
                        canvas,
                        window.x + 20,
                        window.y + 44,
                        228,
                        window.progress,
                        Color::rgb(83, 166, 201),
                    );
                    canvas.fill_rect(
                        window.x + 20,
                        window.y + 66,
                        184,
                        8,
                        Color::rgb(121, 131, 139),
                    );
                    canvas.fill_rect(
                        window.x + 20,
                        window.y + 84,
                        148 + self.cursor_bob * 3,
                        8,
                        Color::rgb(32, 92, 128),
                    );
                    draw_label(canvas, window.x + 20, window.y + 98, "FOCUS TASK", window.accent, 1);
                    draw_label(canvas, window.x + 132, window.y + 98, "KEY THREE", Color::rgb(78, 91, 101), 1);
                }
            }
        }
    }

    fn draw_window(&self, canvas: &mut Canvas, window: WindowState, active: bool) {
        let body = if active {
            Color::rgb(251, 253, 254)
        } else {
            Color::rgb(242, 246, 248)
        };
        let border = if active {
            window.accent
        } else {
            Color::rgb(210, 217, 221)
        };
        let topbar = if active {
            Color::rgb(227, 238, 243)
        } else {
            Color::rgb(234, 239, 242)
        };

        canvas.draw_shadow(window.x, window.y, window.width, window.height, 3, Color::rgb(12, 19, 24));
        canvas.fill_rect(window.x, window.y, window.width, window.height, body);
        canvas.draw_rect(window.x, window.y, window.width, window.height, border);
        canvas.fill_rect(window.x, window.y, window.width, 28, topbar);
        if active {
            canvas.fill_rect(window.x + 1, window.y + 1, window.width - 2, 3, window.accent);
        }
        draw_label(canvas, window.x + 12, window.y + 10, window.title, window.accent, 1);
        canvas.fill_rect(window.x + window.width - 48, window.y + 10, 8, 8, Color::rgb(83, 166, 201));
        canvas.fill_rect(window.x + window.width - 34, window.y + 10, 8, 8, Color::rgb(121, 131, 139));
    }

    fn draw_shell_window(&self, canvas: &mut Canvas, window: WindowState) {
        let text = Color::rgb(78, 91, 101);
        let accent = window.accent;
        let mut status = TextBuffer::empty();
        status.append_bytes(b"HIST ");
        status.append_decimal(self.shell_history_count as u64);
        draw_text_buffer(canvas, window.x + 18, window.y + 42, &status, accent, 1);

        let mut row_y = window.y + 58;
        for line in self.shell_log.iter() {
            draw_text_buffer(canvas, window.x + 18, row_y, line, text, 1);
            row_y += 16;
        }

        canvas.fill_rect(
            window.x + 16,
            window.y + window.height - 42,
            window.width - 32,
            22,
            Color::rgb(236, 242, 245),
        );
        canvas.draw_rect(
            window.x + 16,
            window.y + window.height - 42,
            window.width - 32,
            22,
            Color::rgb(204, 214, 220),
        );
        draw_label(canvas, window.x + 22, window.y + window.height - 34, ">", accent, 1);
        draw_text_buffer(
            canvas,
            window.x + 34,
            window.y + window.height - 34,
            &self.shell_input,
            accent,
            1,
        );

        if self.active_window == 0 && self.frame % 36 < 18 {
            let cursor_x = window.x + 36 + self.shell_input.len as i32 * 4;
            canvas.fill_rect(cursor_x, window.y + window.height - 36, 2, 8, accent);
        }
    }

    fn draw_overview_card(
        &self,
        canvas: &mut Canvas,
        boot_info: &BootInfo,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        let title = Color::rgb(24, 35, 44);
        let body = Color::rgb(70, 82, 92);
        let accent = Color::rgb(36, 127, 170);
        let panel = Color::rgb(255, 255, 255);

        canvas.fill_rect(x, y, width, height, panel);
        canvas.draw_rect(x, y, width, height, Color::rgb(211, 218, 223));
        canvas.fill_rect(x, y, width, 40, Color::rgb(233, 239, 242));
        draw_label(canvas, x + 18, y + 14, "BOOT SNAPSHOT", title, 1);

        draw_label(canvas, x + 18, y + 58, "FRAMEBUFFER", body, 1);
        draw_label(canvas, x + 18, y + 82, "MEMORY MAP", body, 1);
        draw_label(canvas, x + 18, y + 106, "USABLE MEMORY", body, 1);

        canvas.fill_rect(x + 18, y + 138, width - 36, 14, Color::rgb(225, 231, 234));
        let usage_width = ((width - 36) as u64).saturating_mul(boot_info.usable_memory_bytes())
            / boot_info.total_memory_bytes().max(1);
        canvas.fill_rect(x + 18, y + 138, usage_width as i32, 14, accent);
        canvas.fill_rect(
            x + 18 + ((self.pulse * 3) % (width - 58).max(1)),
            y + 138,
            18,
            14,
            Color::rgb(83, 166, 201),
        );

        draw_label(canvas, x + 18, y + 176, "RESERVED OBJECTS", title, 1);
        draw_label(canvas, x + 18, y + 204, "LOW MEMORY", body, 1);
        draw_label(canvas, x + 18, y + 226, "LOADER IMAGE", body, 1);
        draw_label(canvas, x + 18, y + 248, "FRAMEBUFFER", body, 1);
        draw_label(canvas, x + 18, y + 270, "INPUT", title, 1);
        draw_label(canvas, x + 108, y + 270, self.last_input.label(), accent, 1);

        canvas.fill_rect(x + 18, y + 286, width - 36, 10, Color::rgb(225, 231, 234));
        canvas.fill_rect(
            x + 18,
            y + 286,
            (width - 36) * self.input_flash.clamp(0, 42) / 42,
            10,
            Color::rgb(83, 166, 201),
        );

        canvas.fill_rect(x + 18, y + height - 70, width - 36, 44, Color::rgb(241, 246, 248));
        canvas.draw_rect(
            x + 18,
            y + height - 70,
            width - 36,
            44,
            Color::rgb(214, 221, 226),
        );
        draw_label(canvas, x + 32, y + height - 56, "FRAMEBUFFER ACTIVE", accent, 1);
    }

    fn draw_kernel_card(
        &self,
        canvas: &mut Canvas,
        boot_info: &BootInfo,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) {
        let title = Color::rgb(24, 35, 44);
        let body = Color::rgb(70, 82, 92);
        let accent = Color::rgb(32, 92, 128);

        canvas.fill_rect(x, y, width, height, Color::rgb(248, 250, 251));
        canvas.draw_rect(x, y, width, height, Color::rgb(211, 218, 223));
        canvas.fill_rect(x, y, width, 40, Color::rgb(232, 238, 241));
        draw_label(canvas, x + 18, y + 14, "KERNEL IMAGE", title, 1);

        draw_label(canvas, x + 18, y + 58, "CHAINLOAD ENTRY", body, 1);
        draw_label(canvas, x + 18, y + 82, "STAGED PAGES", body, 1);
        draw_label(canvas, x + 18, y + 106, "DIRECT FRAMEBUFFER", body, 1);

        draw_label(canvas, x + 18, y + 146, "LOAD SEGMENTS", title, 1);
        let mut row_y = y + 174;
        for (index, segment) in boot_info.kernel_image.segments().iter().take(2).enumerate() {
            canvas.fill_rect(x + 18, row_y - 4, width - 36, 42, Color::rgb(239, 244, 246));
            canvas.draw_rect(
                x + 18,
                row_y - 4,
                width - 36,
                42,
                Color::rgb(217, 223, 227),
            );

            let bar_width = ((width - 86) as u64).saturating_mul(segment.load_page_count)
                / boot_info.kernel_image.load_page_count.max(1);
            if index == 0 {
                draw_label(canvas, x + 30, row_y + 12, "TEXT SEGMENT", accent, 1);
                canvas.fill_rect(x + 30, row_y + 26, width - 86, 6, Color::rgb(217, 223, 227));
                canvas.fill_rect(x + 30, row_y + 26, bar_width as i32, 6, accent);
            } else {
                draw_label(canvas, x + 30, row_y + 12, "DATA SEGMENT", accent, 1);
                canvas.fill_rect(x + 30, row_y + 26, width - 86, 6, Color::rgb(217, 223, 227));
                canvas.fill_rect(x + 30, row_y + 26, bar_width as i32, 6, body);
            }

            let pulse_color = if index == self.segment_highlight {
                Color::rgb(83, 166, 201)
            } else {
                Color::rgb(36, 127, 170)
            };
            canvas.fill_rect(
                x + 30 + self.pulse + (index as i32 * 12),
                row_y + 24,
                6,
                10,
                pulse_color,
            );
            row_y += 54;
        }

        canvas.fill_rect(x + 18, y + height - 120, width - 36, 84, Color::rgb(32, 92, 128));
        draw_label(
            canvas,
            x + 28,
            y + height - 104,
            "NEXT MILESTONE",
            Color::rgb(238, 245, 248),
            1,
        );
        draw_label(
            canvas,
            x + 28,
            y + height - 82,
            "OWN EVENT LOOP",
            Color::rgb(214, 229, 236),
            1,
        );
        draw_label(
            canvas,
            x + 28,
            y + height - 62,
            "SHELL ACTIVE",
            Color::rgb(214, 229, 236),
            1,
        );
    }

    fn draw_footer(&self, canvas: &mut Canvas, width: i32, height: i32) {
        let focus_label = match self.active_window {
            0 => "ACTIVE SHELL",
            1 => "ACTIVE VIEW",
            _ => "ACTIVE TASK",
        };
        draw_label(canvas, 42, height - 76, focus_label, Color::rgb(78, 91, 101), 1);
        draw_label(
            canvas,
            42,
            height - 58,
            self.last_input.label(),
            Color::rgb(32, 92, 128),
            1,
        );
        draw_label(
            canvas,
            width - 262,
            height - 76,
            "SERIAL DESKTOP RENDERED",
            Color::rgb(32, 92, 128),
            1,
        );
        canvas.fill_rect(44, height - 58, 144 + self.pulse, 6, Color::rgb(83, 166, 201));
        canvas.fill_rect(
            width - 262,
            height - 58,
            96,
            6,
            Color::rgb(214, 222, 227),
        );
        canvas.fill_rect(
            width - 262,
            height - 58,
            96 * self.input_flash.clamp(0, 42) / 42,
            6,
            Color::rgb(36, 127, 170),
        );
    }

    fn draw_cursor_accent(&self, canvas: &mut Canvas, width: i32, height: i32) {
        canvas.fill_rect(width - 126, 122, 14, 14, Color::rgb(32, 92, 128));
        canvas.fill_rect(width - 98, 122, 14, 14, Color::rgb(47, 127, 164));
        canvas.fill_rect(width - 70, 122, 14, 14, Color::rgb(83, 166, 201));
        canvas.draw_cursor(width - 112 + self.cursor_bob, height - 148 - self.cursor_bob / 2);
    }

    fn draw_progress_track(
        &self,
        canvas: &mut Canvas,
        x: i32,
        y: i32,
        width: i32,
        progress: i32,
        color: Color,
    ) {
        let progress = progress.clamp(0, 100);
        canvas.fill_rect(x, y, width, 10, Color::rgb(217, 223, 227));
        canvas.fill_rect(x, y, width * progress / 100, 10, color);
    }
}

fn runtime_framebuffer(boot_info: &BootInfo) -> Option<FrameBufferInfo> {
    let mut framebuffer = boot_info.framebuffer;
    let mapped_base = boot_info
        .runtime_hhdm_base
        .checked_add(framebuffer.base as u64)?;
    framebuffer.base = mapped_base as *mut u8;
    Some(framebuffer)
}

fn busy_wait(iterations: usize) {
    for _ in 0..iterations {
        spin_loop();
    }
}

fn serial_init() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x03);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

fn serial_write_str(value: &str) {
    for byte in value.bytes() {
        unsafe {
            while (inb(COM1 + 5) & 0x20) == 0 {}
            outb(COM1, byte);
        }
    }
}

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

fn draw_label(canvas: &mut Canvas, x: i32, y: i32, text: &str, color: Color, scale: i32) {
    if scale <= 0 {
        return;
    }

    let mut cursor_x = x;
    for ch in text.chars() {
        if let Some(pattern) = glyph_pattern(ch) {
            for (row, bits) in pattern.iter().enumerate() {
                for col in 0..3 {
                    if (bits >> (2 - col)) & 1 == 1 {
                        canvas.fill_rect(
                            cursor_x + col * scale,
                            y + row as i32 * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
        }
        cursor_x += 4 * scale;
    }
}

fn draw_text_buffer(
    canvas: &mut Canvas,
    x: i32,
    y: i32,
    text: &TextBuffer,
    color: Color,
    scale: i32,
) {
    if scale <= 0 {
        return;
    }

    let mut cursor_x = x;
    let mut index = 0;
    while index < text.len {
        if let Some(pattern) = glyph_pattern(text.bytes[index] as char) {
            for (row, bits) in pattern.iter().enumerate() {
                for col in 0..3 {
                    if (bits >> (2 - col)) & 1 == 1 {
                        canvas.fill_rect(
                            cursor_x + col * scale,
                            y + row as i32 * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
        }
        cursor_x += 4 * scale;
        index += 1;
    }
}

fn glyph_pattern(ch: char) -> Option<[u8; 5]> {
    match ch {
        'A' => Some([0b010, 0b101, 0b111, 0b101, 0b101]),
        'B' => Some([0b110, 0b101, 0b110, 0b101, 0b110]),
        'C' => Some([0b011, 0b100, 0b100, 0b100, 0b011]),
        'D' => Some([0b110, 0b101, 0b101, 0b101, 0b110]),
        'E' => Some([0b111, 0b100, 0b110, 0b100, 0b111]),
        'F' => Some([0b111, 0b100, 0b110, 0b100, 0b100]),
        'G' => Some([0b011, 0b100, 0b101, 0b101, 0b011]),
        'H' => Some([0b101, 0b101, 0b111, 0b101, 0b101]),
        'I' => Some([0b111, 0b010, 0b010, 0b010, 0b111]),
        'J' => Some([0b001, 0b001, 0b001, 0b101, 0b010]),
        'K' => Some([0b101, 0b101, 0b110, 0b101, 0b101]),
        'L' => Some([0b100, 0b100, 0b100, 0b100, 0b111]),
        'M' => Some([0b111, 0b111, 0b101, 0b101, 0b101]),
        'N' => Some([0b101, 0b111, 0b111, 0b111, 0b101]),
        'O' => Some([0b010, 0b101, 0b101, 0b101, 0b010]),
        'P' => Some([0b110, 0b101, 0b110, 0b100, 0b100]),
        'Q' => Some([0b010, 0b101, 0b101, 0b111, 0b011]),
        'R' => Some([0b110, 0b101, 0b110, 0b101, 0b101]),
        'S' => Some([0b011, 0b100, 0b010, 0b001, 0b110]),
        'T' => Some([0b111, 0b010, 0b010, 0b010, 0b010]),
        'U' => Some([0b101, 0b101, 0b101, 0b101, 0b111]),
        'V' => Some([0b101, 0b101, 0b101, 0b101, 0b010]),
        'W' => Some([0b101, 0b101, 0b101, 0b111, 0b111]),
        'X' => Some([0b101, 0b101, 0b010, 0b101, 0b101]),
        'Y' => Some([0b101, 0b101, 0b010, 0b010, 0b010]),
        'Z' => Some([0b111, 0b001, 0b010, 0b100, 0b111]),
        '0' => Some([0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => Some([0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => Some([0b110, 0b001, 0b010, 0b100, 0b111]),
        '3' => Some([0b110, 0b001, 0b010, 0b001, 0b110]),
        '4' => Some([0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => Some([0b111, 0b100, 0b110, 0b001, 0b110]),
        '6' => Some([0b011, 0b100, 0b110, 0b101, 0b010]),
        '7' => Some([0b111, 0b001, 0b010, 0b100, 0b100]),
        '8' => Some([0b010, 0b101, 0b010, 0b101, 0b010]),
        '9' => Some([0b010, 0b101, 0b011, 0b001, 0b110]),
        ' ' => Some([0, 0, 0, 0, 0]),
        _ => None,
    }
}
