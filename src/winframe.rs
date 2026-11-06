/*
 * Copyright (c) 2026 Neo Shinju.
 *
 * This file is part of TaskManager.
 *
 * TaskManager is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * TaskManager is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with TaskManager. If not, see <https://gnu.org>.
 */
//! Saját ablakkeret Windows 10-en (lásd `backdrop::custom_frame`): címsor ablakgombokkal, vékony körvonal és az
//! ablakszélek egérrel húzható átméretezése.
//!
//! MIÉRT KELL
//! * A natív címsort a Windows 10 mindig tömören festi, így nem látszhat rajta át az Acrylic.
//! * A natív keret láthatatlan (kb. 7 képpontos) átméretező szegélyére is kiterjed az Acrylic, ami az ablakon kívül
//!   áttetsző "szellemkeretként" látszott. Keret nélküli ablaknál (az eframe `with_decorations(false)`) a kliensterület
//!   az egész ablak, nincs láthatatlan szegély.
//!
//! HOGYAN MŰKÖDIK
//! * A címsor egy felső egui-panel: ugyanazt a színt kapja, mint a bal sáv (átlátszó = átüt az Acrylic; tömör módban a
//!   bal sáv színe), így egy összefüggő felületet alkotnak, mint Windows 11-en.
//! * Az ablak mozgatása / dupla kattintásra maximalizálás: az egui `StartDrag` / `Maximized` parancsa.
//! * Az átméretezés: a szélek közelében az egérkurzor átméretező alakú, lenyomáskor a `shell::begin_resize` indítja a
//!   rendszer saját átméretezési ciklusát (a minimális ablakméretet így a rendszer is betartatja).
//! * A maximalizált ablak határait az eframe / winit kezeli (a munkaterületre vágja, nem lóg ki a képernyőről).

use super::*;
use crate::{backdrop, shell};

/// A címsor magassága (egui-pontban).
const TITLE_BAR_H: f32 = 32.0;
/// Egy ablakgomb szélessége.
const BTN_W: f32 = 46.0;
/// Az egyenes élek átméretező sávjának vastagsága, illetve a sarkoké.
const EDGE: f32 = 6.0;
const CORNER: f32 = 14.0;

/// Az átméretezés találati kódja (HT*) és a hozzá tartozó egérkurzor a `p` pontnál; `None`, ha a pont nem a szélen van.
fn edge_hit(win: Rect, p: Pos2) -> Option<(u32, egui::CursorIcon)> {
    let (dl, dr, dt, db) = (p.x - win.left(), win.right() - p.x, p.y - win.top(), win.bottom() - p.y);
    let (left, right, top, bottom) = (dl < EDGE, dr < EDGE, dt < EDGE, db < EDGE);
    // A sarkokban nagyobb a megfogható terület.
    let hl = left || (dl < CORNER && (top || bottom));
    let hr = right || (dr < CORNER && (top || bottom));
    let ht = top || (dt < CORNER && (left || right));
    let hb = bottom || (db < CORNER && (left || right));
    use egui::CursorIcon::{ResizeHorizontal, ResizeNeSw, ResizeNwSe, ResizeVertical};
    match (hl, hr, ht, hb) {
        (true, false, true, false) => Some((13, ResizeNwSe)),   // HTTOPLEFT
        (false, true, true, false) => Some((14, ResizeNeSw)),   // HTTOPRIGHT
        (true, false, false, true) => Some((16, ResizeNeSw)),   // HTBOTTOMLEFT
        (false, true, false, true) => Some((17, ResizeNwSe)),   // HTBOTTOMRIGHT
        (true, false, false, false) => Some((10, ResizeHorizontal)), // HTLEFT
        (false, true, false, false) => Some((11, ResizeHorizontal)), // HTRIGHT
        (false, false, true, false) => Some((12, ResizeVertical)),   // HTTOP
        (false, false, false, true) => Some((15, ResizeVertical)),   // HTBOTTOM
        _ => None,
    }
}

/// Összefüggő vonallánc rajzolása (a gombok jelképeihez).
fn polyline(p: &egui::Painter, pts: &[Pos2], stroke: Stroke) {
    for w in pts.windows(2) {
        p.line_segment([w[0], w[1]], stroke);
    }
}

impl TaskApp {
    /// A saját címsor (minimalizálás / maximalizálás / bezárás gombokkal). Csak saját keretnél (`custom_frame`) rajzol.
    /// A bal sáv és a tartalom előtt kell meghívni, hogy a felső panel foglalja el a legfelső sávot.
    pub(crate) fn title_bar(&mut self, ctx: &egui::Context) {
        if !self.custom_frame {
            return;
        }
        let transparent = backdrop::resolve(self.settings.backdrop).is_transparent();
        let fill = theme::nav_fill(transparent);
        let maximized = self.hwnd.is_some_and(|h| shell::is_maximized(h.0 as isize));
        let mut cmd: Option<egui::ViewportCommand> = None;

        egui::TopBottomPanel::top("titlebar")
            .exact_height(TITLE_BAR_H)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(fill))
            .show(ctx, |ui| {
                let bar = ui.max_rect();
                let btns_left = bar.right() - 3.0 * BTN_W;

                // Húzható sáv: mozgatás, dupla kattintásra maximalizálás / visszaállítás.
                let drag_rect = Rect::from_min_max(bar.min, pos2(btns_left, bar.bottom()));
                let drag = ui.interact(drag_rect, ui.id().with("titlebar_drag"), Sense::click_and_drag());
                if drag.double_clicked() {
                    cmd = Some(egui::ViewportCommand::Maximized(!maximized));
                } else if drag.drag_started_by(egui::PointerButton::Primary) {
                    cmd = Some(egui::ViewportCommand::StartDrag);
                }

                // Ablakgombok.
                let dark = ui.visuals().dark_mode;
                let fg = ui.visuals().text_color();
                for i in 0..3 {
                    let r = Rect::from_min_size(
                        pos2(btns_left + i as f32 * BTN_W, bar.top()),
                        vec2(BTN_W, bar.height()),
                    );
                    let resp = ui.interact(r, ui.id().with(("titlebar_btn", i)), Sense::click());
                    let hovered = resp.hovered();
                    let is_close = i == 2;
                    if hovered {
                        let bg = if is_close {
                            Color32::from_rgb(196, 43, 28)
                        } else if dark {
                            Color32::from_white_alpha(22)
                        } else {
                            Color32::from_black_alpha(18)
                        };
                        ui.painter().rect_filled(r, CornerRadius::ZERO, bg);
                    }
                    let color = if hovered && is_close { Color32::WHITE } else { fg };
                    let stroke = Stroke::new(1.0_f32, color);
                    // Fél képpontos eltolás: élesebb 1 képpontos vonalak.
                    let c = pos2(r.center().x.floor() + 0.5, r.center().y.floor() + 0.5);
                    let p = ui.painter();
                    match i {
                        0 => {
                            p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
                        }
                        1 if maximized => {
                            // Visszaállítás: két egymásba csúsztatott négyzet.
                            polyline(
                                p,
                                &[
                                    pos2(c.x - 5.0, c.y - 3.0),
                                    pos2(c.x + 3.0, c.y - 3.0),
                                    pos2(c.x + 3.0, c.y + 5.0),
                                    pos2(c.x - 5.0, c.y + 5.0),
                                    pos2(c.x - 5.0, c.y - 3.0),
                                ],
                                stroke,
                            );
                            polyline(
                                p,
                                &[
                                    pos2(c.x - 3.0, c.y - 3.0),
                                    pos2(c.x - 3.0, c.y - 5.0),
                                    pos2(c.x + 5.0, c.y - 5.0),
                                    pos2(c.x + 5.0, c.y + 3.0),
                                    pos2(c.x + 3.0, c.y + 3.0),
                                ],
                                stroke,
                            );
                        }
                        1 => polyline(
                            p,
                            &[
                                pos2(c.x - 5.0, c.y - 5.0),
                                pos2(c.x + 5.0, c.y - 5.0),
                                pos2(c.x + 5.0, c.y + 5.0),
                                pos2(c.x - 5.0, c.y + 5.0),
                                pos2(c.x - 5.0, c.y - 5.0),
                            ],
                            stroke,
                        ),
                        _ => {
                            p.line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], stroke);
                            p.line_segment([pos2(c.x - 5.0, c.y + 5.0), pos2(c.x + 5.0, c.y - 5.0)], stroke);
                        }
                    }
                    if resp.clicked() {
                        cmd = Some(match i {
                            0 => egui::ViewportCommand::Minimized(true),
                            1 => egui::ViewportCommand::Maximized(!maximized),
                            _ => egui::ViewportCommand::Close,
                        });
                    }
                }
            });

        if let Some(c) = cmd {
            ctx.send_viewport_cmd(c);
        }
    }

    /// Saját keretnél: vékony körvonal az ablak szélén, és az ablakszélek egérrel húzható átméretezése. A panelek után
    /// kell meghívni (a kurzoralak így felülírja a vezérlők alakját). `win`: az ablak teljes területe az `update`
    /// elején (még a panelek előtt) lekérve.
    pub(crate) fn frame_edges(&mut self, ctx: &egui::Context, win: Rect) {
        if !self.custom_frame {
            return;
        }
        let Some(h) = self.hwnd else { return };
        let hw = h.0 as isize;
        if shell::is_maximized(hw) {
            return; // maximalizálva nincs szegély és nincs átméretezés
        }

        // Vékony, halvány körvonal (a natív keret nélkül az ablak széle egyébként beleolvadna a háttérbe).
        let dark = ctx.style().visuals.dark_mode;
        let col = if dark { Color32::from_white_alpha(34) } else { Color32::from_black_alpha(48) };
        let stroke = Stroke::new(1.0_f32, col);
        let painter =
            ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("win_border")));
        let (l, r, t, b) = (win.left() + 0.5, win.right() - 0.5, win.top() + 0.5, win.bottom() - 0.5);
        painter.line_segment([pos2(l, t), pos2(r, t)], stroke);
        painter.line_segment([pos2(r, t), pos2(r, b)], stroke);
        painter.line_segment([pos2(r, b), pos2(l, b)], stroke);
        painter.line_segment([pos2(l, b), pos2(l, t)], stroke);

        // Átméretezés a szélekről. A címsor gombjai fölött nincs átméretező sáv (ott a gombok kapják a kattintást).
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let buttons = Rect::from_min_max(
            pos2(win.right() - 3.0 * BTN_W, win.top()),
            pos2(win.right(), win.top() + TITLE_BAR_H),
        );
        if buttons.contains(p) {
            return;
        }
        let Some((hit_test, icon)) = edge_hit(win, p) else { return };
        ctx.set_cursor_icon(icon);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            shell::begin_resize(hw, hit_test);
        }
    }
}
