use bootinfo::BootInfo;
use gfx::{Canvas, Color};

pub fn render(boot_info: &BootInfo) {
    let mut canvas = unsafe { Canvas::from_framebuffer(boot_info.framebuffer) };

    let bg_top = Color::rgb(17, 24, 39);
    let bg_bottom = Color::rgb(15, 118, 110);
    let mist = Color::rgb(255, 255, 255);
    let panel = Color::rgb(3, 7, 18);
    let panel_edge = Color::rgb(71, 85, 105);
    let glass = Color::rgb(241, 245, 249);
    let ink = Color::rgb(15, 23, 42);
    let accent = Color::rgb(59, 130, 246);
    let warm = Color::rgb(251, 191, 36);

    canvas.vertical_gradient(bg_top, bg_bottom);
    canvas.checkerboard(48, bg_bottom, mist, 1, 9);

    let width = canvas.width();
    let height = canvas.height();

    canvas.fill_rect(56, 48, width - 112, 32, Color::rgb(10, 15, 28));
    canvas.draw_rect(56, 48, width - 112, 32, Color::rgb(51, 65, 85));
    canvas.fill_rect(72, 56, 84, 16, accent);
    canvas.fill_rect(width - 196, 56, 124, 16, Color::rgb(22, 163, 74));

    draw_window(
        &mut canvas,
        72,
        108,
        width / 2,
        height / 2,
        glass,
        ink,
        accent,
    );
    draw_terminal(&mut canvas, 96, 152, width / 2 - 48, height / 2 - 84);
    draw_window(
        &mut canvas,
        width / 2 + 32,
        140,
        width / 3,
        height / 3,
        Color::rgb(255, 251, 235),
        ink,
        warm,
    );
    draw_status_cards(&mut canvas, width / 2 + 56, 192, width / 3 - 48);
    draw_dock(&mut canvas, height);
    canvas.draw_panel(44, panel, panel_edge);
    canvas.draw_cursor(width - 180, height / 2);
}

fn draw_window(
    canvas: &mut Canvas,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    body: Color,
    ink: Color,
    accent: Color,
) {
    canvas.draw_shadow(x + 8, y + 12, width, height, 6, Color::rgb(0, 0, 0));
    canvas.fill_rect(x, y, width, height, body);
    canvas.draw_rect(x, y, width, height, Color::rgb(148, 163, 184));
    canvas.fill_rect(x, y, width, 28, accent);
    canvas.fill_rect(x + 16, y + 9, 10, 10, Color::rgb(255, 255, 255));
    canvas.fill_rect(x + 34, y + 9, 10, 10, Color::rgb(219, 234, 254));
    canvas.fill_rect(x + 52, y + 9, 10, 10, Color::rgb(191, 219, 254));
    canvas.fill_rect(x + width - 56, y + 8, 16, 12, Color::rgb(255, 255, 255));
    canvas.fill_rect(x + width - 32, y + 8, 16, 12, ink);
}

fn draw_terminal(canvas: &mut Canvas, x: i32, y: i32, width: i32, height: i32) {
    let shell = Color::rgb(2, 6, 23);
    let line = Color::rgb(30, 41, 59);
    let glow = Color::rgb(34, 197, 94);

    canvas.fill_rect(x, y, width, height, shell);
    canvas.draw_rect(x, y, width, height, line);

    let row_height = 18;
    let mut row = y + 18;
    while row < y + height - 24 {
        canvas.fill_rect(x + 20, row, width - 40, 2, line);
        canvas.fill_rect(x + 20, row + 6, 96, 3, glow);
        canvas.fill_rect(x + 124, row + 6, 52, 3, Color::rgb(56, 189, 248));
        canvas.fill_rect(x + 184, row + 6, 140, 3, Color::rgb(248, 250, 252));
        row += row_height;
    }
}

fn draw_status_cards(canvas: &mut Canvas, x: i32, y: i32, width: i32) {
    let card = Color::rgb(248, 250, 252);
    let edge = Color::rgb(203, 213, 225);
    let blue = Color::rgb(37, 99, 235);
    let cyan = Color::rgb(6, 182, 212);
    let emerald = Color::rgb(16, 185, 129);

    for (index, color) in [blue, cyan, emerald].iter().enumerate() {
        let top = y + index as i32 * 60;
        canvas.fill_rect(x, top, width, 44, card);
        canvas.draw_rect(x, top, width, 44, edge);
        canvas.fill_rect(x + 14, top + 12, 20, 20, *color);
        canvas.fill_rect(x + 46, top + 12, width / 2, 6, Color::rgb(51, 65, 85));
        canvas.fill_rect(x + 46, top + 24, width / 3, 4, Color::rgb(148, 163, 184));
        canvas.fill_rect(x + width - 80, top + 14, 52, 16, *color);
    }
}

fn draw_dock(canvas: &mut Canvas, screen_height: i32) {
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
        canvas.fill_rect(x, y + 12, 32, 32, *color);
        canvas.draw_rect(x, y + 12, 32, 32, Color::rgb(15, 23, 42));
    }
}
