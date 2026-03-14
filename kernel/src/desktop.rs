use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use bootinfo::{
    BootInfo, FirmwareMode, FrameBufferInfo, KERNEL_SEGMENT_FLAG_EXECUTE,
    KERNEL_SEGMENT_FLAG_READ, KERNEL_SEGMENT_FLAG_WRITE, KernelImageInfo, KernelImageSegment,
    MemoryRegion, ReservedMemoryRange,
};
use gfx::{Canvas, Color, blit_buffer_to_framebuffer};

use crate::{boot, memory, vm};

const TERMINAL_WINDOW: usize = 0;
const MAX_TERMINAL_LINES: usize = 18;
const PROMPT: &str = "codexOS> ";

#[derive(Debug, Clone, Copy)]
pub struct PointerSample {
    pub delta_x: i32,
    pub delta_y: i32,
    pub left_button: bool,
    pub right_button: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum DesktopInput {
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    CycleFocus,
    Exit,
    Backspace,
    Submit,
    Character(char),
}

#[derive(Debug, Clone, Copy)]
struct Window {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    title_color: Color,
    body_color: Color,
    title: &'static str,
}

pub struct DesktopApp {
    screen_width: i32,
    screen_height: i32,
    framebuffer: FrameBufferInfo,
    windows: [Window; 2],
    focused_window: usize,
    cursor_x: i32,
    cursor_y: i32,
    left_button_down: bool,
    right_button_down: bool,
    dragging_window: Option<usize>,
    drag_offset_x: i32,
    drag_offset_y: i32,
    accent_flip: bool,
    should_exit: bool,
    terminal: TerminalState,
    firmware_mode: FirmwareMode,
    memory_regions: Vec<MemoryRegion>,
    memory_region_total: usize,
    usable_memory_bytes: u64,
    total_memory_bytes: u64,
    reserved_memory: Vec<ReservedMemoryRange>,
    reserved_memory_bytes: u64,
    kernel_image: KernelImageInfo,
    backbuffer: Vec<u8>,
    dirty: bool,
}

struct TerminalState {
    input: String,
    lines: Vec<String>,
}

impl TerminalState {
    fn new() -> Self {
        let mut terminal = Self {
            input: String::new(),
            lines: Vec::new(),
        };
        terminal.push_line("codexOS shell ready");
        terminal.push_line("type `help` to see commands");
        terminal
    }

    fn push_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        if self.lines.len() > MAX_TERMINAL_LINES {
            let overflow = self.lines.len() - MAX_TERMINAL_LINES;
            self.lines.drain(0..overflow);
        }
    }
}

impl DesktopApp {
    pub fn new(boot_info: &BootInfo) -> Self {
        let screen_width = boot_info.framebuffer.width as i32;
        let screen_height = boot_info.framebuffer.height as i32;

        let mut app = Self {
            screen_width,
            screen_height,
            framebuffer: boot_info.framebuffer,
            windows: [
                Window {
                    x: 72,
                    y: 108,
                    width: screen_width / 2,
                    height: screen_height / 2,
                    title_color: Color::rgb(59, 130, 246),
                    body_color: Color::rgb(241, 245, 249),
                    title: "Terminal",
                },
                Window {
                    x: screen_width / 2 + 32,
                    y: 140,
                    width: screen_width / 3,
                    height: screen_height / 3,
                    title_color: Color::rgb(251, 191, 36),
                    body_color: Color::rgb(255, 251, 235),
                    title: "Inspector",
                },
            ],
            focused_window: 0,
            cursor_x: screen_width - 180,
            cursor_y: screen_height / 2,
            left_button_down: false,
            right_button_down: false,
            dragging_window: None,
            drag_offset_x: 0,
            drag_offset_y: 0,
            accent_flip: false,
            should_exit: false,
            terminal: TerminalState::new(),
            firmware_mode: boot_info.firmware_mode,
            memory_regions: boot_info.memory_regions().to_vec(),
            memory_region_total: boot_info.memory_region_total,
            usable_memory_bytes: boot_info.usable_memory_bytes(),
            total_memory_bytes: boot_info.total_memory_bytes(),
            reserved_memory: boot_info.reserved_memory().to_vec(),
            reserved_memory_bytes: boot_info.reserved_memory_bytes(),
            kernel_image: boot_info.kernel_image,
            backbuffer: alloc::vec![0; boot_info.framebuffer.size],
            dirty: true,
        };
        app.append_boot_message(screen_width, screen_height);
        app
    }

    pub fn render(&mut self, boot_info: &BootInfo) {
        let mut canvas = Canvas::from_buffer(&mut self.backbuffer, boot_info.framebuffer);

        let bg_top = if self.accent_flip {
            Color::rgb(30, 41, 59)
        } else {
            Color::rgb(17, 24, 39)
        };
        let bg_bottom = if self.accent_flip {
            Color::rgb(67, 56, 202)
        } else {
            Color::rgb(15, 118, 110)
        };
        let mist = Color::rgb(255, 255, 255);
        let panel = Color::rgb(3, 7, 18);
        let panel_edge = Color::rgb(71, 85, 105);
        let accent = self.accent_color();

        canvas.vertical_gradient(bg_top, bg_bottom);
        canvas.checkerboard(48, bg_bottom, mist, 1, 9);

        canvas.fill_rect(56, 48, self.screen_width - 112, 32, Color::rgb(10, 15, 28));
        canvas.draw_rect(56, 48, self.screen_width - 112, 32, Color::rgb(51, 65, 85));
        canvas.fill_rect(72, 56, 112, 16, accent);
        canvas.draw_text(76, 58, "codexOS desk", Color::rgb(248, 250, 252), 1);
        canvas.fill_rect(
            self.screen_width - 220,
            56,
            148,
            16,
            Color::rgb(22, 163, 74),
        );
        canvas.draw_text(
            self.screen_width - 212,
            58,
            "shell + gui live",
            Color::rgb(248, 250, 252),
            1,
        );

        for index in 0..self.windows.len() {
            let focused = self.focused_window == index;
            self.draw_window_frame(&mut canvas, self.windows[index], focused);

            if index == TERMINAL_WINDOW {
                self.draw_terminal(&mut canvas, self.windows[index], focused);
            } else {
                self.draw_inspector(&mut canvas, self.windows[index], focused);
            }
        }

        draw_dock(&mut canvas, self.screen_height, self.focused_window);
        self.draw_hud(&mut canvas);
        canvas.draw_panel(44, panel, panel_edge);
        canvas.draw_text(
            24,
            self.screen_height - 34,
            "Tab focus  Enter run  Esc exit",
            Color::rgb(226, 232, 240),
            1,
        );
        canvas.draw_cursor(self.cursor_x, self.cursor_y);
        blit_buffer_to_framebuffer(&self.backbuffer, runtime_framebuffer(boot_info.framebuffer));
        self.dirty = false;
    }

    pub fn needs_redraw(&self) -> bool {
        self.dirty
    }

    pub fn handle_input(&mut self, input: DesktopInput) {
        match input {
            DesktopInput::MoveLeft => self.nudge_focused_window(-18, 0),
            DesktopInput::MoveRight => self.nudge_focused_window(18, 0),
            DesktopInput::MoveUp => self.nudge_focused_window(0, -18),
            DesktopInput::MoveDown => self.nudge_focused_window(0, 18),
            DesktopInput::CycleFocus => self.cycle_focus(),
            DesktopInput::Exit => self.should_exit = true,
            DesktopInput::Backspace => {
                if self.focused_window == TERMINAL_WINDOW {
                    if self.terminal.input.pop().is_some() {
                        self.dirty = true;
                    }
                }
            }
            DesktopInput::Submit => {
                if self.focused_window == TERMINAL_WINDOW {
                    self.submit_command();
                }
            }
            DesktopInput::Character(ch) => {
                if self.focused_window == TERMINAL_WINDOW && !ch.is_control() {
                    self.terminal.input.push(ch);
                    self.dirty = true;
                }
            }
        }
    }

    pub fn handle_pointer(&mut self, sample: PointerSample) {
        let previous_cursor = (self.cursor_x, self.cursor_y);
        self.cursor_x = (self.cursor_x + sample.delta_x).clamp(0, self.screen_width - 12);
        self.cursor_y = (self.cursor_y + sample.delta_y).clamp(0, self.screen_height - 18);

        let left_just_pressed = sample.left_button && !self.left_button_down;
        let left_just_released = !sample.left_button && self.left_button_down;
        let right_just_pressed = sample.right_button && !self.right_button_down;
        self.left_button_down = sample.left_button;
        self.right_button_down = sample.right_button;

        if left_just_pressed {
            if let Some(window_index) = self.hit_test_title_bar(self.cursor_x, self.cursor_y) {
                self.focused_window = window_index;
                self.dragging_window = Some(window_index);
                self.drag_offset_x = self.cursor_x - self.windows[window_index].x;
                self.drag_offset_y = self.cursor_y - self.windows[window_index].y;
                self.dirty = true;
            } else if let Some(window_index) = self.hit_test_window(self.cursor_x, self.cursor_y) {
                self.focused_window = window_index;
                self.dirty = true;
            }
        }

        if let Some(window_index) = self.dragging_window {
            if sample.left_button {
                let x = self.cursor_x - self.drag_offset_x;
                let y = self.cursor_y - self.drag_offset_y;
                self.move_window_to(window_index, x, y);
            }
        }

        if left_just_released {
            self.dragging_window = None;
            self.dirty = true;
        }

        if right_just_pressed {
            self.cycle_focus();
        }

        if previous_cursor != (self.cursor_x, self.cursor_y) {
            self.dirty = true;
        }
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub fn note_handoff_complete(&mut self) {
        self.terminal
            .push_line("boot services exited; desktop is now firmware-detached");
        self.dirty = true;
    }

    fn draw_window_frame(&self, canvas: &mut Canvas, window: Window, focused: bool) {
        let border = if focused {
            Color::rgb(15, 23, 42)
        } else {
            Color::rgb(148, 163, 184)
        };
        let title = if focused {
            window.title_color
        } else {
            dim(window.title_color)
        };

        canvas.draw_shadow(
            window.x + 8,
            window.y + 12,
            window.width,
            window.height,
            6,
            Color::rgb(0, 0, 0),
        );
        canvas.fill_rect(
            window.x,
            window.y,
            window.width,
            window.height,
            window.body_color,
        );
        canvas.draw_rect(window.x, window.y, window.width, window.height, border);
        canvas.fill_rect(window.x, window.y, window.width, 28, title);
        canvas.fill_rect(
            window.x + 16,
            window.y + 9,
            10,
            10,
            Color::rgb(255, 255, 255),
        );
        canvas.fill_rect(
            window.x + 34,
            window.y + 9,
            10,
            10,
            Color::rgb(219, 234, 254),
        );
        canvas.fill_rect(
            window.x + 52,
            window.y + 9,
            10,
            10,
            Color::rgb(191, 219, 254),
        );
        canvas.draw_text(
            window.x + 74,
            window.y + 10,
            window.title,
            Color::rgb(248, 250, 252),
            1,
        );
        canvas.fill_rect(
            window.x + window.width - 56,
            window.y + 8,
            16,
            12,
            Color::rgb(255, 255, 255),
        );
        canvas.fill_rect(
            window.x + window.width - 32,
            window.y + 8,
            16,
            12,
            Color::rgb(15, 23, 42),
        );
    }

    fn draw_terminal(&self, canvas: &mut Canvas, window: Window, focused: bool) {
        let shell = Color::rgb(2, 6, 23);
        let line = if focused {
            Color::rgb(56, 189, 248)
        } else {
            Color::rgb(30, 41, 59)
        };
        let text = Color::rgb(226, 232, 240);
        let muted = Color::rgb(100, 116, 139);
        let glow = if focused {
            Color::rgb(34, 197, 94)
        } else {
            Color::rgb(74, 222, 128)
        };

        let x = window.x + 20;
        let y = window.y + 42;
        let width = window.width - 40;
        let height = window.height - 62;

        canvas.fill_rect(x, y, width, height, shell);
        canvas.draw_rect(x, y, width, height, line);
        canvas.fill_rect(x, y, width, 18, Color::rgb(15, 23, 42));
        canvas.fill_rect(x + 12, y + 6, 6, 6, Color::rgb(239, 68, 68));
        canvas.fill_rect(x + 24, y + 6, 6, 6, Color::rgb(234, 179, 8));
        canvas.fill_rect(x + 36, y + 6, 6, 6, glow);
        canvas.draw_text(x + 56, y + 5, "shell", muted, 1);

        let mut line_y = y + 28;
        for line_text in &self.terminal.lines {
            canvas.draw_text_box(x + 12, line_y, width - 24, line_text, text, 1);
            line_y += 12;
        }

        let prompt = format_prompt(&self.terminal.input);
        canvas.draw_text_box(x + 12, y + height - 22, width - 24, &prompt, glow, 1);
        let cursor_x = x + 12 + ((prompt.len() as i32) * 8);
        canvas.fill_rect(cursor_x, y + height - 12, 8, 2, glow);
    }

    fn draw_inspector(&self, canvas: &mut Canvas, window: Window, focused: bool) {
        let x = window.x + 20;
        let y = window.y + 46;
        let width = window.width - 40;
        let accent = self.accent_color();
        let ink = Color::rgb(15, 23, 42);
        let soft = Color::rgb(100, 116, 139);

        canvas.draw_text(x, y, "System", ink, 1);
        canvas.draw_text(
            x,
            y + 16,
            if focused {
                "focus: inspector"
            } else {
                "focus: inactive"
            },
            soft,
            1,
        );

        let cards = [
            format!("boot: {}", self.firmware_mode.as_str()),
            format!(
                "regions: {}/{}",
                self.memory_regions.len(),
                self.memory_region_total
            ),
            format!("usable: {}", format_mib(self.usable_memory_bytes)),
            format!(
                "reserved: {} in {} slots",
                format_mib(self.reserved_memory_bytes),
                self.reserved_memory.len()
            ),
            format!("kernel: {}", format_kernel_card(self.kernel_image)),
            format!("pages: {}", format_page_stats()),
            format!("vm: {}", format_vm_stats()),
        ];

        for (index, label) in cards.iter().enumerate() {
            let top = y + 40 + index as i32 * 32;
            canvas.fill_rect(x, top, width, 32, Color::rgb(248, 250, 252));
            canvas.draw_rect(x, top, width, 32, Color::rgb(203, 213, 225));
            canvas.fill_rect(x + 10, top + 8, 12, 12, accent);
            canvas.draw_text_box(x + 32, top + 8, width - 44, label, ink, 1);
        }

        canvas.fill_rect(x, y + 240, width, 56, Color::rgb(15, 23, 42));
        canvas.draw_rect(x, y + 240, width, 56, accent);
        canvas.draw_text_box(
            x + 12,
            y + 252,
            width - 24,
            "commands: boot kernel vm aliases",
            Color::rgb(226, 232, 240),
            1,
        );
    }

    fn draw_hud(&self, canvas: &mut Canvas) {
        let hud_x = 24;
        let hud_y = self.screen_height - 146;
        canvas.fill_rect(hud_x, hud_y, 248, 96, Color::rgb(248, 250, 252));
        canvas.draw_rect(hud_x, hud_y, 248, 96, Color::rgb(148, 163, 184));

        let primary = self.accent_color();

        canvas.draw_text(
            hud_x + 16,
            hud_y + 14,
            "Overview",
            Color::rgb(15, 23, 42),
            1,
        );
        canvas.fill_rect(hud_x + 16, hud_y + 30, 108, 10, primary);
        canvas.draw_text(
            hud_x + 16,
            hud_y + 50,
            if self.focused_window == 0 {
                "focus: terminal"
            } else {
                "focus: inspector"
            },
            Color::rgb(51, 65, 85),
            1,
        );
        canvas.draw_text(
            hud_x + 16,
            hud_y + 64,
            "shell is now stateful",
            Color::rgb(100, 116, 139),
            1,
        );
        canvas.fill_rect(hud_x + 184, hud_y + 18, 40, 18, primary);
        canvas.draw_text(hud_x + 190, hud_y + 24, "CLI", Color::rgb(248, 250, 252), 1);
    }

    fn append_boot_message(&mut self, screen_width: i32, screen_height: i32) {
        self.terminal
            .push_line(format!("display {}x{} ready", screen_width, screen_height));
        self.terminal.push_line(format!(
            "memory {} usable across {} regions",
            format_mib(self.usable_memory_bytes),
            self.memory_region_total
        ));
        self.terminal
            .push_line(format!("boot mode {}", self.firmware_mode.as_str()));
        self.terminal.push_line(format!(
            "reserved {} across {} ranges",
            format_mib(self.reserved_memory_bytes),
            self.reserved_memory.len()
        ));
        self.terminal
            .push_line(format!("kernel {}", format_kernel_card(self.kernel_image)));
        self.terminal
            .push_line(format!("allocator {}", format_page_stats()));
        self.terminal.push_line(format!("vm {}", format_vm_stats()));
    }

    fn accent_color(&self) -> Color {
        if self.accent_flip {
            Color::rgb(236, 72, 153)
        } else {
            Color::rgb(59, 130, 246)
        }
    }

    fn submit_command(&mut self) {
        let command_line = self.terminal.input.trim().to_string();
        self.terminal
            .push_line(format!("{}{}", PROMPT, command_line));
        self.terminal.input.clear();
        self.dirty = true;

        if command_line.is_empty() {
            return;
        }

        let mut parts = command_line.split_whitespace();
        let command = parts.next().unwrap_or_default();

        match command {
            "help" => {
                self.terminal
                    .push_line("help status boot kernel mem reserved regions region <n>");
                self.terminal.push_line("alloc-page alloc <count>");
                self.terminal
                    .push_line("vm vm-sync map-test map <count> translate <hex>");
                self.terminal
                    .push_line("walk <hex> hhdm <phys> fb aliases phases pt-entry <table> <index>");
                self.terminal.push_line("theme clear focus");
                self.terminal
                    .push_line("move-left move-right move-up move-down");
                self.terminal.push_line("about exit");
            }
            "status" => {
                self.terminal.push_line(format!(
                    "focus={} cursor=({}, {})",
                    self.focused_window, self.cursor_x, self.cursor_y
                ));
                self.terminal.push_line(format!(
                    "window0=({}, {}) window1=({}, {})",
                    self.windows[0].x, self.windows[0].y, self.windows[1].x, self.windows[1].y
                ));
            }
            "mem" => {
                self.terminal.push_line(format!(
                    "usable={} total={} stored={} total-regions={}",
                    format_mib(self.usable_memory_bytes),
                    format_mib(self.total_memory_bytes),
                    self.memory_regions.len(),
                    self.memory_region_total
                ));
                self.terminal
                    .push_line(format!("allocator {}", format_page_stats()));
            }
            "boot" => {
                let snapshot = boot::snapshot();
                self.terminal.push_line(format!(
                    "mode={} phase={} reserved={}",
                    self.firmware_mode.as_str(),
                    snapshot.phase.as_str(),
                    format_mib(self.reserved_memory_bytes)
                ));
                self.terminal.push_line(format!(
                    "usable-now={} total-regions={}",
                    format_mib(self.usable_memory_bytes),
                    self.memory_region_total
                ));
                self.terminal.push_line(format!(
                    "reserved-ranges={} root=0x{:016x}",
                    self.reserved_memory.len(),
                    snapshot.root_table_phys
                ));
                if let Some(report) = snapshot.boot_report {
                    self.terminal.push_line(format!(
                        "ident={} ranges/{} pages kernel={} ranges/{} pages stack=0x{:016x}/{}",
                        report.identity_ranges,
                        report.identity_pages,
                        report.kernel_image_ranges,
                        report.kernel_image_pages,
                        report.stack_window_start,
                        report.stack_window_pages
                    ));
                    self.terminal.push_line(format!(
                        "hhdm={} ranges/{} pages @ 0x{:016x}",
                        report.higher_half_ranges,
                        report.higher_half_pages,
                        report.higher_half_base
                    ));
                }
            }
            "kernel" => {
                self.terminal
                    .push_line(format_kernel_summary(self.kernel_image));
                if !self.kernel_image.is_present() {
                    return;
                }
                for (index, segment) in self.kernel_image.segments().iter().enumerate() {
                    self.terminal
                        .push_line(format_kernel_segment(index, *segment));
                }
            }
            "vm" => {
                let snapshot = boot::snapshot();
                self.terminal.push_line(format!("vm {}", format_vm_stats()));
                self.terminal
                    .push_line(format!("phase {}", snapshot.phase.as_str()));
                self.terminal
                    .push_line(format!("hhdm base 0x{:016x}", snapshot.hhdm_base));
                if let Some(report) = snapshot.boot_report {
                    self.terminal.push_line(format!(
                        "boot layout ident={} kernel={} hhdm={}",
                        report.identity_pages, report.kernel_image_pages, report.higher_half_pages
                    ));
                }
                for (index, mapping) in vm::mappings().iter().take(3).enumerate() {
                    self.terminal
                        .push_line(format_mapping_line(index, *mapping));
                }
            }
            "vm-sync" => match vm::sync() {
                Ok(root) => self
                    .terminal
                    .push_line(format!("page tables synced at 0x{:016x}", root)),
                Err(err) => self
                    .terminal
                    .push_line(format!("vm-sync failed: {}", describe_vm_error(err))),
            },
            "regions" => {
                self.terminal.push_line("first memory regions:");
                for (index, region) in self.memory_regions.iter().take(4).enumerate() {
                    self.terminal.push_line(format_region_line(index, *region));
                }
            }
            "reserved" => {
                self.terminal.push_line("reserved memory ranges:");
                for (index, range) in self.reserved_memory.iter().take(4).enumerate() {
                    self.terminal.push_line(format_reserved_line(index, *range));
                }
            }
            "region" => {
                let Some(index) = parts.next() else {
                    self.terminal.push_line("usage: region <index>");
                    return;
                };

                match index.parse::<usize>() {
                    Ok(index) => {
                        if let Some(region) = self.memory_regions.get(index).copied() {
                            self.terminal.push_line(format_region_line(index, region));
                        } else {
                            self.terminal.push_line("region index out of range");
                        }
                    }
                    Err(_) => self.terminal.push_line("region index must be a number"),
                }
            }
            "alloc-page" => {
                match memory::allocate_page() {
                    Some(address) => self
                        .terminal
                        .push_line(format!("allocated page at 0x{:016x}", address)),
                    None => self.terminal.push_line("no free physical pages available"),
                }
                self.terminal
                    .push_line(format!("allocator {}", format_page_stats()));
            }
            "alloc" => {
                let Some(raw_count) = parts.next() else {
                    self.terminal.push_line("usage: alloc <count>");
                    return;
                };

                match raw_count.parse::<usize>() {
                    Ok(count) => self.allocate_pages(count.min(16)),
                    Err(_) => self.terminal.push_line("alloc count must be numeric"),
                }
            }
            "map-test" => match vm::map_demo_range(1) {
                Ok(mapping) => {
                    self.terminal
                        .push_line(format!("mapped 1 page at 0x{:016x}", mapping.virt_start));
                    self.terminal.push_line(format!("vm {}", format_vm_stats()));
                }
                Err(err) => self
                    .terminal
                    .push_line(format!("map-test failed: {}", describe_vm_error(err))),
            },
            "map" => {
                let Some(raw_count) = parts.next() else {
                    self.terminal.push_line("usage: map <count>");
                    return;
                };

                match raw_count.parse::<usize>() {
                    Ok(count) => match vm::map_demo_range(count.min(32)) {
                        Ok(mapping) => self.terminal.push_line(format!(
                            "mapped {} pages at 0x{:016x}",
                            mapping.page_count, mapping.virt_start
                        )),
                        Err(err) => self
                            .terminal
                            .push_line(format!("map failed: {}", describe_vm_error(err))),
                    },
                    Err(_) => self.terminal.push_line("map count must be numeric"),
                }
                self.terminal.push_line(format!("vm {}", format_vm_stats()));
            }
            "translate" => {
                let Some(raw_addr) = parts.next() else {
                    self.terminal.push_line("usage: translate <hex-address>");
                    return;
                };

                match parse_hex_or_decimal(raw_addr) {
                    Some(virt) => match vm::translate(virt) {
                        Some(phys) => self
                            .terminal
                            .push_line(format!("0x{:016x} -> 0x{:016x}", virt, phys)),
                        None => self.terminal.push_line("virtual address is unmapped"),
                    },
                    None => self.terminal.push_line("address parse failed"),
                }
            }
            "walk" => {
                let Some(raw_addr) = parts.next() else {
                    self.terminal.push_line("usage: walk <hex-address>");
                    return;
                };

                match parse_hex_or_decimal(raw_addr) {
                    Some(virt) => {
                        let walk = vm::walk(virt);
                        self.terminal
                            .push_line(format!("virt 0x{:016x}", walk.virt));
                        self.terminal.push_line(format!(
                            "idx p4={} p3={} p2={} p1={}",
                            walk.indices[0], walk.indices[1], walk.indices[2], walk.indices[3]
                        ));
                        self.terminal
                            .push_line(format!("l4 {}", format_entry(walk.l4_entry)));
                        self.terminal
                            .push_line(format!("l3 {}", format_entry(walk.l3_entry)));
                        self.terminal
                            .push_line(format!("l2 {}", format_entry(walk.l2_entry)));
                        self.terminal
                            .push_line(format!("l1 {}", format_entry(walk.l1_entry)));
                        match walk.phys {
                            Some(phys) => self.terminal.push_line(format!("phys 0x{:016x}", phys)),
                            None => self.terminal.push_line("phys unmapped"),
                        }
                    }
                    None => self.terminal.push_line("address parse failed"),
                }
            }
            "hhdm" => {
                let Some(raw_phys) = parts.next() else {
                    self.terminal.push_line("usage: hhdm <physical-address>");
                    return;
                };

                match parse_hex_or_decimal(raw_phys) {
                    Some(phys) => match vm::physical_to_high_half(phys) {
                        Some(virt) => {
                            self.terminal
                                .push_line(format!("phys 0x{:016x} -> hhdm 0x{:016x}", phys, virt));
                            match vm::translate(virt) {
                                Some(back_phys) => self.terminal.push_line(format!(
                                    "translate 0x{:016x} -> 0x{:016x}",
                                    virt, back_phys
                                )),
                                None => self.terminal.push_line("hhdm address is unmapped"),
                            }
                        }
                        None => self
                            .terminal
                            .push_line("physical address is outside hhdm range"),
                    },
                    None => self.terminal.push_line("physical address parse failed"),
                }
            }
            "fb" => {
                let framebuffer = runtime_framebuffer(self.framebuffer);
                self.terminal.push_line(format!(
                    "fb phys 0x{:016x} active 0x{:016x}",
                    self.framebuffer.base as u64, framebuffer.base as u64
                ));
                self.terminal.push_line(format!(
                    "fb {}x{} stride={} bytes={}",
                    framebuffer.width, framebuffer.height, framebuffer.stride, framebuffer.size
                ));
            }
            "aliases" => {
                let snapshot = boot::snapshot();
                self.terminal.push_line(format!(
                    "root table 0x{:016x} hhdm base 0x{:016x}",
                    snapshot.root_table_phys, snapshot.hhdm_base
                ));
                self.terminal
                    .push_line(format_boot_alias("loader", snapshot.loader_alias));
                self.terminal
                    .push_line(format_boot_alias("memmap", snapshot.memory_map_alias));
                self.terminal.push_line(format!(
                    "framebuffer 0x{:016x} -> 0x{:016x}",
                    self.framebuffer.base as u64,
                    runtime_framebuffer(self.framebuffer).base as u64
                ));
            }
            "phases" => {
                let snapshot = boot::snapshot();
                self.terminal.push_line(format!(
                    "phase={} root=0x{:016x}",
                    snapshot.phase.as_str(),
                    snapshot.root_table_phys
                ));
                self.terminal.push_line(format!(
                    "hhdm base=0x{:016x} framebuffer={}",
                    snapshot.hhdm_base,
                    format_optional_hex(snapshot.framebuffer_alias)
                ));
                self.terminal.push_line(format!(
                    "loader={} memmap={}",
                    format_optional_alias(snapshot.loader_alias),
                    format_optional_alias(snapshot.memory_map_alias)
                ));
            }
            "pt-entry" => {
                let Some(raw_table) = parts.next() else {
                    self.terminal
                        .push_line("usage: pt-entry <table-phys> <index>");
                    return;
                };
                let Some(raw_index) = parts.next() else {
                    self.terminal
                        .push_line("usage: pt-entry <table-phys> <index>");
                    return;
                };

                match (
                    parse_hex_or_decimal(raw_table),
                    raw_index.parse::<usize>().ok(),
                ) {
                    (Some(table), Some(index)) => match vm::read_committed_entry(table, index) {
                        Some(entry) => self.terminal.push_line(format!(
                            "pte 0x{:016x}[{}] = 0x{:016x}",
                            table, index, entry
                        )),
                        None => self
                            .terminal
                            .push_line("entry unavailable or VM not synced"),
                    },
                    _ => self
                        .terminal
                        .push_line("pt-entry arguments must be <hex-address> <index>"),
                }
            }
            "theme" => {
                self.accent_flip = !self.accent_flip;
                self.terminal.push_line(if self.accent_flip {
                    "theme switched to violet"
                } else {
                    "theme switched to teal"
                });
            }
            "clear" => {
                self.terminal.lines.clear();
                self.terminal.push_line("screen cleared");
            }
            "focus" => {
                self.cycle_focus();
                self.terminal.push_line("focus moved");
            }
            "move-left" => {
                self.nudge_focused_window(-24, 0);
                self.terminal.push_line("focused window moved left");
            }
            "move-right" => {
                self.nudge_focused_window(24, 0);
                self.terminal.push_line("focused window moved right");
            }
            "move-up" => {
                self.nudge_focused_window(0, -24);
                self.terminal.push_line("focused window moved up");
            }
            "move-down" => {
                self.nudge_focused_window(0, 24);
                self.terminal.push_line("focused window moved down");
            }
            "about" => {
                self.terminal
                    .push_line("codexOS is a Rust GUI OS baseline with a live shell");
                self.terminal
                    .push_line("boot info now includes a compact UEFI memory map");
                self.terminal
                    .push_line("loader now inspects a standalone kernel ELF from the boot disk");
                self.terminal
                    .push_line("boot state and reserved memory ranges are visible in-kernel");
                self.terminal
                    .push_line("kernel also owns a simple physical page allocator");
                self.terminal
                    .push_line("vm manager now materializes page tables into physical memory");
            }
            "exit" => {
                self.terminal.push_line("leaving desktop");
                self.should_exit = true;
            }
            other => {
                self.terminal
                    .push_line(format!("unknown command: {}", other));
            }
        }
    }

    fn nudge_focused_window(&mut self, dx: i32, dy: i32) {
        let focused = self.focused_window;
        let x = self.windows[focused].x + dx;
        let y = self.windows[focused].y + dy;
        self.move_window_to(focused, x, y);
    }

    fn allocate_pages(&mut self, count: usize) {
        if count == 0 {
            self.terminal.push_line("alloc count must be at least 1");
            return;
        }

        let mut allocated = 0usize;
        let mut first = None;
        let mut last = None;

        for _ in 0..count {
            match memory::allocate_page() {
                Some(address) => {
                    if first.is_none() {
                        first = Some(address);
                    }
                    last = Some(address);
                    allocated += 1;
                }
                None => break,
            }
        }

        if allocated == 0 {
            self.terminal.push_line("no free physical pages available");
        } else if allocated == 1 {
            self.terminal
                .push_line(format!("allocated page at 0x{:016x}", first.unwrap_or(0)));
        } else {
            self.terminal.push_line(format!(
                "allocated {} pages from 0x{:016x} to 0x{:016x}",
                allocated,
                first.unwrap_or(0),
                last.unwrap_or(0)
            ));
        }

        self.terminal
            .push_line(format!("allocator {}", format_page_stats()));
    }

    fn move_window_to(&mut self, index: usize, x: i32, y: i32) {
        let window = &mut self.windows[index];
        let max_x = self.screen_width - window.width - 24;
        let max_y = self.screen_height - window.height - 60;
        window.x = x.clamp(24, max_x.max(24));
        window.y = y.clamp(92, max_y.max(92));
        self.dirty = true;
    }

    fn cycle_focus(&mut self) {
        self.focused_window = (self.focused_window + 1) % self.windows.len();
        self.dirty = true;
    }

    fn hit_test_title_bar(&self, x: i32, y: i32) -> Option<usize> {
        for index in (0..self.windows.len()).rev() {
            let window = self.windows[index];
            let inside_x = x >= window.x && x < window.x + window.width;
            let inside_y = y >= window.y && y < window.y + 28;
            if inside_x && inside_y {
                return Some(index);
            }
        }

        None
    }

    fn hit_test_window(&self, x: i32, y: i32) -> Option<usize> {
        for index in (0..self.windows.len()).rev() {
            let window = self.windows[index];
            let inside_x = x >= window.x && x < window.x + window.width;
            let inside_y = y >= window.y && y < window.y + window.height;
            if inside_x && inside_y {
                return Some(index);
            }
        }

        None
    }
}

fn draw_dock(canvas: &mut Canvas, screen_height: i32, focused_window: usize) {
    let y = screen_height - 100;
    canvas.fill_rect(300, y, 340, 56, Color::rgb(241, 245, 249));
    canvas.draw_rect(300, y, 340, 56, Color::rgb(148, 163, 184));

    let icons = [
        Color::rgb(59, 130, 246),
        Color::rgb(244, 114, 182),
        Color::rgb(16, 185, 129),
        Color::rgb(250, 204, 21),
        Color::rgb(249, 115, 22),
        Color::rgb(168, 85, 247),
    ];

    for (index, color) in icons.iter().enumerate() {
        let x = 320 + index as i32 * 52;
        let lift = if index == focused_window { 6 } else { 0 };
        canvas.fill_rect(x, y + 12 - lift, 32, 32, *color);
        canvas.draw_rect(x, y + 12 - lift, 32, 32, Color::rgb(15, 23, 42));
    }
}

fn dim(color: Color) -> Color {
    Color::rgb(color.r / 2 + 24, color.g / 2 + 24, color.b / 2 + 24)
}

fn format_prompt(input: &str) -> String {
    let mut prompt = String::from(PROMPT);
    prompt.push_str(input);
    prompt
}

fn format_region_line(index: usize, region: MemoryRegion) -> String {
    format!(
        "#{} {:016x}-{:016x} {} {} KiB",
        index,
        region.start,
        region.end(),
        region.kind.as_str(),
        region.size_bytes() / 1024
    )
}

fn format_reserved_line(index: usize, range: ReservedMemoryRange) -> String {
    format!(
        "#{} {:016x}-{:016x} {} {} KiB",
        index,
        range.start,
        range.end(),
        range.kind.as_str(),
        range.length / 1024
    )
}

fn format_kernel_card(info: KernelImageInfo) -> String {
    if info.is_present() {
        format!("staged {}/{}", info.loaded_segment_count, info.load_segment_total)
    } else {
        String::from("missing")
    }
}

fn format_kernel_summary(info: KernelImageInfo) -> String {
    if !info.is_present() {
        return String::from("kernel image unavailable");
    }

    format!(
        "kernel image {} bytes entry=0x{:016x} staged=0x{:016x} phdrs={} load={}/{} staged={}",
        info.image_size,
        info.entry_point,
        info.loaded_entry_point,
        info.program_header_count,
        info.load_segment_count,
        info.load_segment_total,
        info.loaded_segment_count
    )
}

fn format_kernel_segment(index: usize, segment: KernelImageSegment) -> String {
    format!(
        "seg{} virt=0x{:016x} phys=0x{:016x} load=0x{:016x} pages={} off=0x{:x} file={} mem={} flags={}",
        index,
        segment.virtual_address,
        segment.physical_address,
        segment.load_address,
        segment.load_page_count,
        segment.file_offset,
        segment.file_size,
        segment.memory_size,
        format_segment_flags(segment.flags)
    )
}

fn format_segment_flags(flags: u32) -> String {
    let read = if flags & KERNEL_SEGMENT_FLAG_READ != 0 {
        'r'
    } else {
        '-'
    };
    let write = if flags & KERNEL_SEGMENT_FLAG_WRITE != 0 {
        'w'
    } else {
        '-'
    };
    let execute = if flags & KERNEL_SEGMENT_FLAG_EXECUTE != 0 {
        'x'
    } else {
        '-'
    };
    format!("{read}{write}{execute}")
}

fn format_mib(bytes: u64) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

fn format_page_stats() -> String {
    let stats = memory::stats();
    format!(
        "{} used / {} free",
        stats.allocated_pages, stats.remaining_pages
    )
}

fn format_vm_stats() -> String {
    let stats = vm::stats();
    if !stats.initialized {
        return String::from("not initialized");
    }

    format!(
        "{} maps / {} tables @ 0x{:x} hhdm=0x{:x} {} {}",
        stats.mapped_pages,
        stats.table_pages,
        stats.root_table_phys,
        stats.high_half_base,
        if stats.committed { "synced" } else { "dirty" },
        if stats.active { "active" } else { "inactive" }
    )
}

fn runtime_framebuffer(framebuffer: FrameBufferInfo) -> FrameBufferInfo {
    if vm::stats().active {
        if let Some(mapped) = vm::physical_to_high_half(framebuffer.base as u64) {
            return FrameBufferInfo {
                base: mapped as *mut u8,
                ..framebuffer
            };
        }
    }

    framebuffer
}

fn format_boot_alias(label: &str, alias: Option<boot::ReservedAlias>) -> String {
    match alias {
        Some(alias) => format!(
            "{} 0x{:016x} -> 0x{:016x} ({} KiB)",
            label,
            alias.range.start,
            alias.virt,
            alias.range.length / 1024
        ),
        None => format!("{} not present", label),
    }
}

fn format_optional_alias(alias: Option<boot::ReservedAlias>) -> String {
    match alias {
        Some(alias) => format!("0x{:016x}", alias.virt),
        None => String::from("none"),
    }
}

fn format_optional_hex(value: Option<u64>) -> String {
    match value {
        Some(value) => format!("0x{:016x}", value),
        None => String::from("none"),
    }
}

fn format_mapping_line(index: usize, mapping: vm::Mapping) -> String {
    format!(
        "#{} virt=0x{:016x} phys=0x{:016x} pages={} {}{}",
        index,
        mapping.virt_start,
        mapping.phys_start,
        mapping.page_count,
        if mapping.writable { "W" } else { "R" },
        if mapping.executable { "X" } else { "N" }
    )
}

fn describe_vm_error(error: vm::VmError) -> &'static str {
    match error {
        vm::VmError::NotInitialized => "vm not initialized",
        vm::VmError::InvalidPageCount => "page count must be non-zero",
        vm::VmError::UnalignedVirtualAddress => "virtual address must be page-aligned",
        vm::VmError::UnalignedPhysicalAddress => "physical address must be page-aligned",
        vm::VmError::BootIdentityConflict => "boot identity map overlaps an existing mapping",
        vm::VmError::BootStackConflict => "boot stack map overlaps an existing mapping",
        vm::VmError::BootWindowConflict => "boot hhdm window overlaps an existing mapping",
        vm::VmError::ReservedIdentityConflict => {
            "reserved identity map overlaps an existing mapping"
        }
        vm::VmError::ReservedWindowConflict => {
            "reserved hhdm map overlaps an existing mapping"
        }
        vm::VmError::KernelImageConflict => "kernel image map overlaps an existing mapping",
        vm::VmError::AddressOverflow => "address arithmetic overflowed",
        vm::VmError::AlreadyMapped => "virtual range already mapped",
        vm::VmError::OutOfPhysicalPages => "out of physical pages",
    }
}

fn parse_hex_or_decimal(raw: &str) -> Option<u64> {
    if let Some(stripped) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(stripped, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}

fn format_entry(entry: Option<u64>) -> String {
    match entry {
        Some(value) => format!("0x{:016x}", value),
        None => String::from("none"),
    }
}
