#![no_std]

use bootinfo::{FrameBufferInfo, PixelFormat};
use core::cmp::{max, min};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

pub struct Canvas {
    info: FrameBufferInfo,
}

impl Canvas {
    pub unsafe fn from_framebuffer(info: FrameBufferInfo) -> Self {
        Self { info }
    }

    pub fn width(&self) -> i32 {
        self.info.width as i32
    }

    pub fn height(&self) -> i32 {
        self.info.height as i32
    }

    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width(), self.height(), color);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Color) {
        let x0 = max(0, x);
        let y0 = max(0, y);
        let x1 = min(self.width(), x.saturating_add(width));
        let y1 = min(self.height(), y.saturating_add(height));

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        for py in y0..y1 {
            for px in x0..x1 {
                self.put_pixel(px, py, color);
            }
        }
    }

    pub fn draw_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Color) {
        if width <= 0 || height <= 0 {
            return;
        }

        self.fill_rect(x, y, width, 1, color);
        self.fill_rect(x, y + height - 1, width, 1, color);
        self.fill_rect(x, y, 1, height, color);
        self.fill_rect(x + width - 1, y, 1, height, color);
    }

    pub fn vertical_gradient(&mut self, top: Color, bottom: Color) {
        let height = self.height().max(1);
        for y in 0..height {
            let t = y as u32;
            let h = (height - 1) as u32;
            let color = if h == 0 {
                top
            } else {
                blend(top, bottom, t, h)
            };
            self.fill_rect(0, y, self.width(), 1, color);
        }
    }

    pub fn checkerboard(
        &mut self,
        cell: i32,
        base: Color,
        accent: Color,
        alpha_num: u32,
        alpha_den: u32,
    ) {
        if cell <= 0 || alpha_den == 0 {
            return;
        }

        let h_cells = (self.width() + cell - 1) / cell;
        let v_cells = (self.height() + cell - 1) / cell;

        for cy in 0..v_cells {
            for cx in 0..h_cells {
                if (cx + cy) % 2 == 0 {
                    let mixed = blend(base, accent, alpha_num, alpha_den);
                    self.fill_rect(cx * cell, cy * cell, cell, cell, mixed);
                }
            }
        }
    }

    pub fn draw_panel(&mut self, height: i32, color: Color, edge: Color) {
        let top = self.height() - height;
        self.fill_rect(0, top, self.width(), height, color);
        self.fill_rect(0, top, self.width(), 1, edge);
    }

    pub fn draw_shadow(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        layers: i32,
        color: Color,
    ) {
        for layer in 0..layers {
            let alpha = 4 + (layers - layer) as u32;
            let shadow = blend(color, Color::rgb(0, 0, 0), alpha, 18);
            self.draw_rect(
                x - layer,
                y - layer,
                width + layer * 2,
                height + layer * 2,
                shadow,
            );
        }
    }

    pub fn draw_cursor(&mut self, x: i32, y: i32) {
        let white = Color::rgb(245, 247, 250);
        let edge = Color::rgb(17, 24, 39);
        for row in 0..18 {
            for col in 0..=row.min(9) {
                self.put_pixel(x + col, y + row, white);
                if col == row.min(9) || col == 0 || row == 17 {
                    self.put_pixel(x + col, y + row, edge);
                }
            }
        }
    }

    fn put_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width() || y >= self.height() {
            return;
        }

        let x = x as usize;
        let y = y as usize;
        let pixel_index = y
            .saturating_mul(self.info.stride)
            .saturating_add(x)
            .saturating_mul(self.info.bytes_per_pixel);

        if pixel_index + self.info.bytes_per_pixel > self.info.size {
            return;
        }

        let buffer = unsafe { core::slice::from_raw_parts_mut(self.info.base, self.info.size) };
        match self.info.pixel_format {
            PixelFormat::Rgb => {
                buffer[pixel_index] = color.r;
                buffer[pixel_index + 1] = color.g;
                buffer[pixel_index + 2] = color.b;
            }
            PixelFormat::Bgr | PixelFormat::Unknown => {
                buffer[pixel_index] = color.b;
                buffer[pixel_index + 1] = color.g;
                buffer[pixel_index + 2] = color.r;
            }
        }

        if self.info.bytes_per_pixel > 3 {
            buffer[pixel_index + 3] = 0;
        }
    }
}

fn blend(from: Color, to: Color, numerator: u32, denominator: u32) -> Color {
    fn component(a: u8, b: u8, n: u32, d: u32) -> u8 {
        let a = a as i32;
        let b = b as i32;
        let delta = b - a;
        let value = a + ((delta * n as i32) / d as i32);
        value.clamp(0, 255) as u8
    }

    Color::rgb(
        component(from.r, to.r, numerator, denominator),
        component(from.g, to.g, numerator, denominator),
        component(from.b, to.b, numerator, denominator),
    )
}
