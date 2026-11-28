//! Win32 双主题抗锯齿绘制与 DWM 窗口外观辅助；纯 CPU DIB 光栅化，无 GPU 依赖。
#![allow(clippy::wildcard_imports)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_arguments
)]

use std::{
    ptr::{null, null_mut},
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute},
        Gdi::*,
    },
    UI::WindowsAndMessaging::*,
};

const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1: u32 = 19;
const DWMWA_BORDER_COLOR: u32 = 34;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_TEXT_COLOR: u32 = 36;

/// Win32 `COLORREF` (`0x00BBGGRR`)。
pub(super) const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Palette {
    pub dark: bool,
    pub bg: u32,
    pub title_bg: u32,
    pub card: u32,
    pub card_subtle: u32,
    pub footer_bg: u32,
    pub content: u32,
    pub btn_bg: u32,
    pub btn_hover: u32,
    pub btn_pressed: u32,
    pub border: u32,
    pub border_subtle: u32,
    pub border_hover: u32,
    pub card_highlight: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_pressed: u32,
    pub accent_soft: u32,
    pub accent_border: u32,
    pub accent_text: u32,
    pub success: u32,
    pub success_soft: u32,
    pub success_border: u32,
    pub warning: u32,
    pub warning_soft: u32,
    pub warning_border: u32,
    pub danger: u32,
    pub danger_solid: u32,
    pub danger_solid_hover: u32,
    pub danger_solid_pressed: u32,
    pub danger_bg: u32,
    pub danger_bg_hover: u32,
    pub danger_border: u32,
    pub text: u32,
    pub secondary: u32,
    pub muted: u32,
    pub scroll_thumb: u32,
    pub scroll_hot: u32,
}

impl Palette {
    pub(super) const fn code_surface(self) -> u32 {
        if self.dark {
            rgb(34, 46, 62)
        } else {
            rgb(228, 238, 251)
        }
    }
}

pub(super) const fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark: true,
            bg: rgb(18, 22, 30),
            title_bg: rgb(18, 22, 30),
            card: rgb(26, 32, 44),
            card_subtle: rgb(22, 27, 37),
            footer_bg: rgb(22, 27, 37),
            content: rgb(13, 17, 23),
            btn_bg: rgb(33, 41, 55),
            btn_hover: rgb(43, 53, 71),
            btn_pressed: rgb(25, 31, 42),
            border: rgb(47, 57, 74),
            border_subtle: rgb(36, 44, 58),
            border_hover: rgb(72, 87, 112),
            card_highlight: rgb(44, 54, 72),
            accent: rgb(59, 130, 246),
            accent_hover: rgb(37, 99, 235),
            accent_pressed: rgb(29, 78, 216),
            accent_soft: rgb(24, 42, 72),
            accent_border: rgb(59, 130, 246),
            accent_text: rgb(147, 197, 253),
            success: rgb(52, 211, 153),
            success_soft: rgb(18, 44, 38),
            success_border: rgb(34, 118, 92),
            warning: rgb(251, 191, 36),
            warning_soft: rgb(45, 35, 19),
            warning_border: rgb(120, 86, 30),
            danger: rgb(251, 113, 133),
            danger_solid: rgb(225, 29, 72),
            danger_solid_hover: rgb(190, 18, 60),
            danger_solid_pressed: rgb(159, 18, 57),
            danger_bg: rgb(46, 24, 32),
            danger_bg_hover: rgb(62, 30, 42),
            danger_border: rgb(110, 46, 62),
            text: rgb(241, 245, 249),
            secondary: rgb(156, 170, 192),
            muted: rgb(112, 127, 150),
            scroll_thumb: rgb(82, 97, 120),
            scroll_hot: rgb(124, 144, 174),
        }
    } else {
        Palette {
            dark: false,
            bg: rgb(242, 245, 249),
            title_bg: rgb(242, 245, 249),
            card: rgb(255, 255, 255),
            card_subtle: rgb(248, 250, 252),
            footer_bg: rgb(248, 250, 253),
            content: rgb(255, 255, 255),
            btn_bg: rgb(255, 255, 255),
            btn_hover: rgb(244, 247, 252),
            btn_pressed: rgb(232, 238, 248),
            border: rgb(212, 220, 232),
            border_subtle: rgb(226, 232, 240),
            border_hover: rgb(176, 190, 210),
            card_highlight: rgb(255, 255, 255),
            accent: rgb(37, 99, 235),
            accent_hover: rgb(29, 78, 216),
            accent_pressed: rgb(30, 64, 175),
            accent_soft: rgb(235, 243, 255),
            accent_border: rgb(96, 150, 248),
            accent_text: rgb(29, 78, 216),
            success: rgb(22, 163, 74),
            success_soft: rgb(236, 253, 243),
            success_border: rgb(134, 239, 172),
            warning: rgb(217, 119, 6),
            warning_soft: rgb(255, 250, 235),
            warning_border: rgb(252, 211, 77),
            danger: rgb(220, 38, 38),
            danger_solid: rgb(220, 38, 38),
            danger_solid_hover: rgb(185, 28, 28),
            danger_solid_pressed: rgb(153, 27, 27),
            danger_bg: rgb(254, 242, 242),
            danger_bg_hover: rgb(254, 226, 226),
            danger_border: rgb(252, 165, 165),
            text: rgb(15, 23, 42),
            secondary: rgb(82, 96, 117),
            muted: rgb(124, 138, 158),
            scroll_thumb: rgb(148, 163, 184),
            scroll_hot: rgb(100, 116, 139),
        }
    }
}

/// 同步系统标题栏亮暗模式与窗口边框配色，并在已显示窗口上立即刷新非客户区。
pub(super) fn apply_window_chrome(hwnd: HWND, colors: Palette) {
    if hwnd.is_null() {
        return;
    }
    let dark = i32::from(colors.dark);
    unsafe {
        let hr = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE.cast_unsigned(),
            (&raw const dark).cast(),
            std::mem::size_of::<i32>() as u32,
        );
        if hr != 0 {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1,
                (&raw const dark).cast(),
                std::mem::size_of::<i32>() as u32,
            );
        }
        // Windows 11 22000+ 支持自定义标题栏底色、文字色和边框色；Win10 自动忽略。
        let caption = colors.title_bg;
        let text = colors.text;
        let border = colors.border;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_COLOR,
            (&raw const caption).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_TEXT_COLOR,
            (&raw const text).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&raw const border).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        if IsWindowVisible(hwnd) != 0 {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            RedrawWindow(hwnd, null(), null_mut(), RDW_FRAME | RDW_INVALIDATE);
        }
    }
}

#[inline]
const fn colorref_to_bgra(c: u32) -> u32 {
    let r = c & 0xFF;
    let g = (c >> 8) & 0xFF;
    let b = (c >> 16) & 0xFF;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

#[inline]
fn blend_pixel(dst: u32, src: u32, alpha: f32) -> u32 {
    if alpha <= 0.002 {
        return dst;
    }
    if alpha >= 0.998 {
        return src;
    }
    let a = alpha.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    let dr = ((dst >> 16) & 0xFF) as f32;
    let dg = ((dst >> 8) & 0xFF) as f32;
    let db = (dst & 0xFF) as f32;
    let sr = ((src >> 16) & 0xFF) as f32;
    let sg = ((src >> 8) & 0xFF) as f32;
    let sb = (src & 0xFF) as f32;
    let r = (dr * inv + sr * a + 0.5) as u32;
    let g = (dg * inv + sg * a + 0.5) as u32;
    let b = (db * inv + sb * a + 0.5) as u32;
    0xFF00_0000 | (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

#[inline]
fn rounded_box_sdf(px: f32, py: f32, cx: f32, cy: f32, hx: f32, hy: f32, radius: f32) -> f32 {
    let r = radius.min(hx).min(hy).max(0.0);
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

#[inline]
fn segment_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;
    let len_sq = vx * vx + vy * vy;
    let t = if len_sq <= 1e-6 {
        0.0
    } else {
        ((wx * vx + wy * vy) / len_sq).clamp(0.0, 1.0)
    };
    let dx = wx - vx * t;
    let dy = wy - vy * t;
    (dx * dx + dy * dy).sqrt()
}

/// 临时 32 位自上而下 DIB 画布；离开作用域自动释放所有 GDI 句柄。
pub(super) struct Surface {
    hdc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    width: i32,
    height: i32,
}

impl Surface {
    pub(super) fn new(reference_dc: HDC, width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        unsafe {
            let hdc = CreateCompatibleDC(reference_dc);
            if hdc.is_null() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut raw_bits: *mut core::ffi::c_void = null_mut();
            let bitmap = CreateDIBSection(
                hdc,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut raw_bits,
                null_mut(),
                0,
            );
            if bitmap.is_null() || raw_bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(hdc);
                return None;
            }
            let previous = SelectObject(hdc, bitmap);
            Some(Self {
                hdc,
                bitmap,
                previous,
                bits: raw_bits.cast::<u32>(),
                width,
                height,
            })
        }
    }

    pub(super) fn hdc(&self) -> HDC {
        self.hdc
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        let len = (self.width as usize).saturating_mul(self.height as usize);
        unsafe { std::slice::from_raw_parts_mut(self.bits, len) }
    }

    pub(super) fn clear(&mut self, color: u32) {
        let bgra = colorref_to_bgra(color);
        self.pixels_mut().fill(bgra);
    }

    pub(super) fn draw_rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        fill: u32,
        border: u32,
        border_width: f32,
        shadow_alpha: f32,
        top_highlight: Option<u32>,
    ) {
        if w <= 0.5 || h <= 0.5 {
            return;
        }
        let fill_bgra = colorref_to_bgra(fill);
        let border_bgra = colorref_to_bgra(border);
        let highlight_bgra = top_highlight.map(colorref_to_bgra);
        let shadow_bgra = 0xFF0F_172A;

        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let hx = w * 0.5;
        let hy = h * 0.5;
        let r = radius.min(hx).min(hy).max(0.0);
        let bw = border_width.max(0.0);

        let pad = if shadow_alpha > 0.001 { 4.0 } else { 1.5 };
        let min_x = ((x - pad).floor() as i32).max(0);
        let max_x = ((x + w + pad).ceil() as i32).min(self.width);
        let min_y = ((y - pad).floor() as i32).max(0);
        let max_y = ((y + h + pad + 2.0).ceil() as i32).min(self.height);
        if min_x >= max_x || min_y >= max_y {
            return;
        }

        // 内部平坦区无需逐像素计算 SDF，加快大卡片和窗口绘制。
        let fast_left = ((x + r + bw + 1.5).ceil() as i32).clamp(min_x, max_x);
        let fast_right = ((x + w - r - bw - 1.5).floor() as i32).clamp(fast_left, max_x);
        let fast_top = ((y + bw + 2.5).ceil() as i32).clamp(min_y, max_y);
        let fast_bottom = ((y + h - bw - 1.5).floor() as i32).clamp(fast_top, max_y);

        let stride = self.width as usize;
        let pixels = self.pixels_mut();
        for iy in min_y..max_y {
            let py = iy as f32 + 0.5;
            let row = (iy as usize) * stride;
            let in_fast_y = iy >= fast_top && iy < fast_bottom;
            let mut ix = min_x;
            while ix < max_x {
                if in_fast_y && ix == fast_left && fast_right > fast_left {
                    pixels[row + (fast_left as usize)..row + (fast_right as usize)].fill(fill_bgra);
                    ix = fast_right;
                    continue;
                }
                let px = ix as f32 + 0.5;
                let mut dst = pixels[row + ix as usize];

                if shadow_alpha > 0.001 {
                    let sdist = rounded_box_sdf(px, py, cx, cy + 1.6, hx, hy, r);
                    if sdist > -0.5 && sdist < 3.6 {
                        let t = (1.0 - (sdist + 0.5) / 4.1).clamp(0.0, 1.0);
                        dst = blend_pixel(dst, shadow_bgra, shadow_alpha * t * t);
                    }
                }

                let dist = rounded_box_sdf(px, py, cx, cy, hx, hy, r);
                let outer_a = (0.5 - dist).clamp(0.0, 1.0);
                if outer_a > 0.0 {
                    let mut shape_color = if bw > 0.01 {
                        let inner_a = (0.5 - (dist + bw)).clamp(0.0, 1.0);
                        blend_pixel(border_bgra, fill_bgra, inner_a)
                    } else {
                        fill_bgra
                    };
                    if let Some(hl) = highlight_bgra
                        && py >= y + bw
                        && py < y + bw + 1.15
                        && dist + bw < -0.25
                    {
                        shape_color = blend_pixel(shape_color, hl, 0.45);
                    }
                    dst = blend_pixel(dst, shape_color, outer_a);
                }
                pixels[row + ix as usize] = dst;
                ix += 1;
            }
        }
    }

    pub(super) fn draw_focus_ring(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: u32,
        thickness: f32,
    ) {
        if w <= 2.0 || h <= 2.0 {
            return;
        }
        let ring_bgra = colorref_to_bgra(color);
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let hx = w * 0.5;
        let hy = h * 0.5;
        let r = radius.min(hx).min(hy).max(0.0);
        let min_x = ((x - 1.5).floor() as i32).max(0);
        let max_x = ((x + w + 1.5).ceil() as i32).min(self.width);
        let min_y = ((y - 1.5).floor() as i32).max(0);
        let max_y = ((y + h + 1.5).ceil() as i32).min(self.height);
        let stride = self.width as usize;
        let pixels = self.pixels_mut();
        for iy in min_y..max_y {
            let py = iy as f32 + 0.5;
            let row = (iy as usize) * stride;
            for ix in min_x..max_x {
                let px = ix as f32 + 0.5;
                let d = rounded_box_sdf(px, py, cx, cy, hx, hy, r);
                let outer = (0.5 - d).clamp(0.0, 1.0);
                let inner = (0.5 - (d + thickness)).clamp(0.0, 1.0);
                let a = (outer - inner).clamp(0.0, 1.0);
                if a > 0.002 {
                    pixels[row + ix as usize] =
                        blend_pixel(pixels[row + ix as usize], ring_bgra, a);
                }
            }
        }
    }

    pub(super) fn draw_circle(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        color: u32,
        alpha_scale: f32,
    ) {
        let bgra = colorref_to_bgra(color);
        let min_x = ((cx - radius - 1.5).floor() as i32).max(0);
        let max_x = ((cx + radius + 1.5).ceil() as i32).min(self.width);
        let min_y = ((cy - radius - 1.5).floor() as i32).max(0);
        let max_y = ((cy + radius + 1.5).ceil() as i32).min(self.height);
        let stride = self.width as usize;
        let pixels = self.pixels_mut();
        for iy in min_y..max_y {
            let py = iy as f32 + 0.5;
            let row = (iy as usize) * stride;
            for ix in min_x..max_x {
                let px = ix as f32 + 0.5;
                let dist = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - radius;
                let a = (0.5 - dist).clamp(0.0, 1.0) * alpha_scale;
                if a > 0.002 {
                    pixels[row + ix as usize] = blend_pixel(pixels[row + ix as usize], bgra, a);
                }
            }
        }
    }

    pub(super) fn draw_glow_dot(
        &mut self,
        cx: f32,
        cy: f32,
        dot_r: f32,
        glow_r: f32,
        color: u32,
        glow_alpha: f32,
    ) {
        if glow_alpha > 0.002 && glow_r > dot_r {
            let bgra = colorref_to_bgra(color);
            let min_x = ((cx - glow_r - 1.0).floor() as i32).max(0);
            let max_x = ((cx + glow_r + 1.0).ceil() as i32).min(self.width);
            let min_y = ((cy - glow_r - 1.0).floor() as i32).max(0);
            let max_y = ((cy + glow_r + 1.0).ceil() as i32).min(self.height);
            let stride = self.width as usize;
            let pixels = self.pixels_mut();
            for iy in min_y..max_y {
                let py = iy as f32 + 0.5;
                let row = (iy as usize) * stride;
                for ix in min_x..max_x {
                    let px = ix as f32 + 0.5;
                    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                    if d < glow_r {
                        let t = (1.0 - d / glow_r).clamp(0.0, 1.0);
                        pixels[row + ix as usize] =
                            blend_pixel(pixels[row + ix as usize], bgra, glow_alpha * t * t);
                    }
                }
            }
        }
        self.draw_circle(cx, cy, dot_r, color, 1.0);
    }

    pub(super) fn draw_checkmark(&mut self, x: f32, y: f32, size: f32, stroke: f32, color: u32) {
        let bgra = colorref_to_bgra(color);
        let p0 = (x + size * 0.22, y + size * 0.52);
        let p1 = (x + size * 0.43, y + size * 0.74);
        let p2 = (x + size * 0.80, y + size * 0.28);
        let half = stroke * 0.5;
        let min_x = ((x - 1.0).floor() as i32).max(0);
        let max_x = ((x + size + 1.0).ceil() as i32).min(self.width);
        let min_y = ((y - 1.0).floor() as i32).max(0);
        let max_y = ((y + size + 1.0).ceil() as i32).min(self.height);
        let stride = self.width as usize;
        let pixels = self.pixels_mut();
        for iy in min_y..max_y {
            let py = iy as f32 + 0.5;
            let row = (iy as usize) * stride;
            for ix in min_x..max_x {
                let px = ix as f32 + 0.5;
                let d1 = segment_dist(px, py, p0.0, p0.1, p1.0, p1.1);
                let d2 = segment_dist(px, py, p1.0, p1.1, p2.0, p2.1);
                let dist = d1.min(d2) - half;
                let a = (0.5 - dist).clamp(0.0, 1.0);
                if a > 0.002 {
                    pixels[row + ix as usize] = blend_pixel(pixels[row + ix as usize], bgra, a);
                }
            }
        }
    }

    pub(super) fn draw_hline(&mut self, x1: i32, x2: i32, y: i32, color: u32) {
        if y < 0 || y >= self.height {
            return;
        }
        let left = x1.clamp(0, self.width);
        let right = x2.clamp(left, self.width);
        if left >= right {
            return;
        }
        let bgra = colorref_to_bgra(color);
        let row = (y as usize) * (self.width as usize);
        self.pixels_mut()[row + left as usize..row + right as usize].fill(bgra);
    }

    pub(super) fn blit(&self, target: HDC, dst_x: i32, dst_y: i32) {
        unsafe {
            BitBlt(
                target,
                dst_x,
                dst_y,
                self.width,
                self.height,
                self.hdc,
                0,
                0,
                SRCCOPY,
            );
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.hdc);
        }
    }
}

/// 在目标 DC 的矩形区域内直接绘制抗锯齿圆角卡片（用于无全屏双缓冲的局部绘制）。
pub(super) fn draw_aa_card(
    target: HDC,
    rect: RECT,
    outside_bg: u32,
    fill: u32,
    border: u32,
    radius: f32,
    border_width: f32,
    shadow_alpha: f32,
    top_highlight: Option<u32>,
) {
    let pad = if shadow_alpha > 0.001 { 4 } else { 0 };
    let sx = rect.left - pad;
    let sy = rect.top - pad;
    let sw = (rect.right - rect.left) + pad * 2;
    let sh = (rect.bottom - rect.top) + pad * 2 + if pad > 0 { 2 } else { 0 };
    let Some(mut surface) = Surface::new(target, sw, sh) else {
        return;
    };
    surface.clear(outside_bg);
    surface.draw_rounded_rect(
        pad as f32,
        pad as f32,
        (rect.right - rect.left) as f32,
        (rect.bottom - rect.top) as f32,
        radius,
        fill,
        border,
        border_width,
        shadow_alpha,
        top_highlight,
    );
    surface.blit(target, sx, sy);
}

unsafe extern "system" fn enum_font_exists(
    _lpelfe: *const LOGFONTW,
    _lpntme: *const TEXTMETRICW,
    _font_type: u32,
    lparam: LPARAM,
) -> i32 {
    if lparam != 0 {
        unsafe {
            *(lparam as *mut bool) = true;
        }
    }
    0
}

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// 优先选用 Windows 11 的 `Segoe Fluent Icons`，旧版系统自动回退 `Segoe MDL2 Assets`。
pub(super) fn icon_font_face() -> &'static str {
    static FACE: OnceLock<&'static str> = OnceLock::new();
    FACE.get_or_init(|| unsafe {
        let mut found = false;
        let dc = GetDC(null_mut());
        if !dc.is_null() {
            let mut lf = LOGFONTW {
                lfCharSet: DEFAULT_CHARSET,
                ..Default::default()
            };
            let name = wide_str("Segoe Fluent Icons");
            let count = name.len().min(lf.lfFaceName.len());
            lf.lfFaceName[..count].copy_from_slice(&name[..count]);
            EnumFontFamiliesExW(
                dc,
                &raw mut lf,
                Some(enum_font_exists),
                (&raw mut found) as isize,
                0,
            );
            ReleaseDC(null_mut(), dc);
        }
        if found {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        }
    })
}

/// 绘制系统内置矢量图标字形（ClearType 抗锯齿，随 DPI 清晰缩放）。
pub(super) fn draw_icon_glyph(hdc: HDC, rect: RECT, glyph: &str, px_size: i32, color: u32) {
    unsafe {
        let face = wide_str(icon_font_face());
        let font = CreateFontW(
            -px_size.max(8),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            0,
            0,
            CLEARTYPE_QUALITY.into(),
            DEFAULT_PITCH.into(),
            face.as_ptr(),
        );
        if font.is_null() {
            return;
        }
        let previous = SelectObject(hdc, font);
        SetBkMode(hdc, TRANSPARENT.cast_signed());
        SetTextColor(hdc, color);
        let mut bounds = rect;
        let encoded = wide_str(glyph);
        DrawTextW(
            hdc,
            encoded.as_ptr(),
            -1,
            &raw mut bounds,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(hdc, previous);
        DeleteObject(font);
    }
}
