/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Téma-paletta: sötét / világos. A főszál minden frame elején beállítja az aktuális témát
//! (`set_dark`), a rajzoló függvények pedig innen kérik a színeket.

use eframe::egui::Color32;
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(true);

pub fn set_dark(v: bool) {
    DARK.store(v, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Fő szövegszín.
pub fn col_text() -> Color32 {
    if is_dark() {
        Color32::from_rgb(236, 238, 243)
    } else {
        Color32::from_rgb(26, 28, 34)
    }
}

/// Halvány (másodlagos) szövegszín.
pub fn col_muted() -> Color32 {
    if is_dark() {
        Color32::from_rgb(152, 158, 170)
    } else {
        Color32::from_rgb(98, 104, 118)
    }
}

/// A nem kiválasztott menüpontok szövege.
pub fn nav_text() -> Color32 {
    if is_dark() {
        Color32::from_rgb(200, 204, 212)
    } else {
        Color32::from_rgb(52, 56, 66)
    }
}

/// Finom "fátyol" a kártyákhoz, hover-hez: sötét témában fehér, világosban fekete.
pub fn overlay(a: u8) -> Color32 {
    if is_dark() {
        Color32::from_white_alpha(a)
    } else {
        Color32::from_black_alpha(((a as f32) * 0.55) as u8)
    }
}

/// A grafikonok háttere.
pub fn graph_bg() -> Color32 {
    if is_dark() {
        Color32::from_black_alpha(55)
    } else {
        Color32::from_black_alpha(12)
    }
}

/// A tartalmi rész háttere: MINDIG tömör (nincs átlátszóság).
pub fn content_fill() -> Color32 {
    if is_dark() {
        Color32::from_rgb(28, 28, 34)
    } else {
        Color32::from_rgb(246, 247, 250)
    }
}

/// A bal oldali sáv háttere. Mica módban TELJESEN ÁTLÁTSZÓ: a DWM Mica anyaga (amit a címsáv is használ) ugyanúgy
/// látszik át rajta, így a címsor és a sáv egyetlen, egybefüggő felület. (Ehhez az ablakfelületnek átlátszónak kell
/// lennie: lásd `with_transparent`, a `DxgiFromVisual` úszólánc és az átlátszó `clear_color`.)
/// Tömör módban egyszínű háttér – ez a tartalék, ha a rendszeren nem működik az átlátszóság.
pub fn nav_fill(mica: bool) -> Color32 {
    match (is_dark(), mica) {
        (_, true) => Color32::TRANSPARENT,
        (true, false) => Color32::from_rgb(32, 32, 32),
        (false, false) => Color32::from_rgb(243, 243, 243),
    }
}
