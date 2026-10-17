use super::*;
impl Ui {
    pub(super) unsafe fn paint(&self) {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let screen = BeginPaint(self.hwnd, &mut ps);
        let mut rect = RECT::default();
        GetClientRect(self.hwnd, &mut rect);
        buffered(screen, &rect, |dc| self.paint_to(dc, &rect));
        EndPaint(self.hwnd, &ps);
    }
    unsafe fn paint_to(&self, dc: HDC, rect: &RECT) {
        FillRect(dc, rect, self.bg);
        match self.page {
            Page::Status => {
                DrawIconEx(
                    dc,
                    self.px(24),
                    self.px(24),
                    icons::app(self.px(44)),
                    self.px(44),
                    self.px(44),
                    0,
                    ptr::null_mut(),
                    DI_NORMAL,
                );
                for (x, y, w, h) in [
                    (20, 82, 480, 184),
                    (20, 310, 480, 222),
                    (20, 544, 480, 46),
                    (20, 608, 480, 124),
                ] {
                    self.card(dc, x, y, w, h);
                }
                for y in [152, 204, 372, 426, 480, 650, 690] {
                    self.line(dc, 36, y, 484);
                }
                DrawIconEx(
                    dc,
                    self.px(34),
                    self.px(102),
                    icons::glyph(self.state(), true, self.px(24)),
                    self.px(24),
                    self.px(24),
                    0,
                    ptr::null_mut(),
                    DI_NORMAL,
                );
            }
            Page::Done => {
                DrawIconEx(
                    dc,
                    self.px(230),
                    self.px(112),
                    icons::app(self.px(60)),
                    self.px(60),
                    self.px(60),
                    0,
                    ptr::null_mut(),
                    DI_NORMAL,
                );
            }
            _ => {}
        }
    }
    unsafe fn card(&self, dc: HDC, x: i32, y: i32, w: i32, h: i32) {
        let pen = CreatePen(PS_SOLID, 1, rgb(222, 221, 218));
        let old_pen = SelectObject(dc, pen);
        let old_brush = SelectObject(dc, self.white);
        RoundRect(
            dc,
            self.px(x),
            self.px(y),
            self.px(x + w),
            self.px(y + h),
            self.px(10),
            self.px(10),
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
    }
    unsafe fn line(&self, dc: HDC, x: i32, y: i32, right: i32) {
        let pen = CreatePen(PS_SOLID, 1, rgb(235, 234, 232));
        let old = SelectObject(dc, pen);
        MoveToEx(dc, self.px(x), self.px(y), ptr::null_mut());
        LineTo(dc, self.px(right), self.px(y));
        SelectObject(dc, old);
        DeleteObject(pen);
    }
    pub(super) unsafe fn draw_item(&self, item: &DRAWITEMSTRUCT) {
        buffered(item.hDC, &item.rcItem, |dc| self.draw_item_to(dc, item));
    }
    unsafe fn draw_item_to(&self, dc: HDC, item: &DRAWITEMSTRUCT) {
        let rect = item.rcItem;
        let id = item.CtlID as u16;
        let enabled = item.itemState & ODS_DISABLED == 0;
        FillRect(dc, &rect, self.white);
        if [ENABLE, VIDEO, AUDIO, LOGIN, MINIMIZED].contains(&id) {
            let on = match id {
                ENABLE => self.enabled,
                VIDEO => self.video_on,
                AUDIO => self.audio_on,
                LOGIN => autostart::enabled(),
                _ => self.settings.start_minimized,
            };
            let x = rect.left + self.px(1);
            let y = rect.top + (rect.bottom - rect.top - self.px(22)) / 2;
            let track = if !enabled {
                rgb(215, 215, 215)
            } else if on {
                BLUE
            } else {
                rgb(170, 170, 170)
            };
            draw_switch(dc, x, y, self.px(40), self.px(22), on, track, WHITE);
        } else {
            if item.itemState & ODS_SELECTED != 0 {
                let brush = CreateSolidBrush(rgb(240, 240, 240));
                FillRect(dc, &rect, brush);
                DeleteObject(brush);
            }
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(
                dc,
                if !enabled {
                    DIM
                } else if id == FORGET {
                    RED
                } else {
                    INK
                },
            );
            let old = SelectObject(dc, self.font);
            let mut r = rect;
            r.left += self.px(14);
            r.right -= self.px(28);
            DrawTextW(
                dc,
                wide(&text(item.hwndItem)).as_ptr(),
                -1,
                &mut r,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
            );
            if id != QUIT {
                r.left = r.right;
                r.right = rect.right;
                DrawTextW(
                    dc,
                    wide("›").as_ptr(),
                    -1,
                    &mut r,
                    DT_SINGLELINE | DT_VCENTER,
                );
            }
            SelectObject(dc, old);
        }
        if item.itemState & ODS_FOCUS != 0 {
            DrawFocusRect(dc, &rect);
        }
    }
}

/// GDI draws without antialiasing (jagged at any scale), so the switch is
/// drawn here: each pixel mixes the colours by how much of it the track
/// and the knob cover.
unsafe fn draw_switch(dc: HDC, x: i32, y: i32, w: i32, h: i32, on: bool, track: COLORREF, back: COLORREF) {
    if w <= 0 || h <= 0 {
        return;
    }
    let r = h as f32 / 2.0;
    // The knob: 16 of the track's 22 px, 3 px from its end.
    let knob = h as f32 * 8.0 / 22.0;
    let knob_x = if on { w as f32 - r } else { r };
    let channels = |c: COLORREF| [(c & 0xff) as f32, (c >> 8 & 0xff) as f32, (c >> 16 & 0xff) as f32];
    let (back, track, white) = (channels(back), channels(track), channels(WHITE));
    let coverage = |distance: f32| (0.5 - distance).clamp(0.0, 1.0);
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for py in 0..h {
        for px in 0..w {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let pill = (fx - fx.clamp(r, w as f32 - r)).hypot(fy - r) - r;
            let a = coverage(pill);
            let b = coverage((fx - knob_x).hypot(fy - r) - knob);
            let c: [f32; 3] = std::array::from_fn(|i| {
                let under = back[i] + (track[i] - back[i]) * a;
                under + (white[i] - under) * b
            });
            // BGRX.
            pixels.push((c[2].round() as u32) | (c[1].round() as u32) << 8 | (c[0].round() as u32) << 16);
        }
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };
    SetDIBitsToDevice(dc, x, y, w as u32, h as u32, 0, 0, 0, h as u32, pixels.as_ptr().cast(), &info, DIB_RGB_COLORS);
}

/// Draws off screen, then copies the result in one go: the screen never
/// shows a half-drawn window or control (the background without its text).
unsafe fn buffered(dc: HDC, rect: &RECT, draw: impl FnOnce(HDC)) {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 {
        return;
    }
    let mem = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, w, h);
    let old = SelectObject(mem, bitmap);
    SetViewportOrgEx(mem, -rect.left, -rect.top, ptr::null_mut());
    draw(mem);
    SetViewportOrgEx(mem, 0, 0, ptr::null_mut());
    BitBlt(dc, rect.left, rect.top, w, h, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old);
    DeleteObject(bitmap);
    DeleteDC(mem);
}
