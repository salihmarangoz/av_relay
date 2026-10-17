use super::*;
impl Ui {
    pub(super) unsafe fn paint(&self) {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let dc = BeginPaint(self.hwnd, &mut ps);
        let mut rect = RECT::default();
        GetClientRect(self.hwnd, &mut rect);
        FillRect(dc, &rect, self.bg);
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
        EndPaint(self.hwnd, &ps);
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
        let dc = item.hDC;
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
            let brush = CreateSolidBrush(if !enabled {
                rgb(215, 215, 215)
            } else if on {
                BLUE
            } else {
                rgb(170, 170, 170)
            });
            let old = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            RoundRect(
                dc,
                x,
                y,
                x + self.px(40),
                y + self.px(22),
                self.px(22),
                self.px(22),
            );
            SelectObject(dc, self.white);
            let tx = x + self.px(if on { 21 } else { 3 });
            Ellipse(dc, tx, y + self.px(3), tx + self.px(16), y + self.px(19));
            SelectObject(dc, old);
            SelectObject(dc, old_pen);
            DeleteObject(brush);
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
