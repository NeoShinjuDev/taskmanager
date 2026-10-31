/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Téma-paletta: sötét / világos. A főszál minden frame elején beállítja az aktuális témát
//! (`set_dark`), a rajzoló függvények pedig innen kérik a színeket.

use eframe::egui::Color32;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static DARK: AtomicBool = AtomicBool::new(true);

/// A Windows-kék: ez az alapérték, ha a rendszer kiemelő színét nem sikerül kiolvasni.
const DEFAULT_ACCENT: Color32 = Color32::from_rgb(0, 120, 212);

/// A Windowsban beállított kiemelő (accent) szín, 0x00RRGGBB alakban.
static ACCENT_RGB: AtomicU32 = AtomicU32::new(0x0000_78D4);
/// Az utolsó registry-ellenőrzés ideje (ezredmásodperc a program indulásától); `u64::MAX` = még nem volt.
static ACCENT_CHECKED_MS: AtomicU64 = AtomicU64::new(u64::MAX);
static ACCENT_CLOCK: OnceLock<Instant> = OnceLock::new();

/// Hány ezredmásodpercenként olvassuk újra a Windows kiemelő színét (olcsó registry-olvasás).
const ACCENT_POLL_MS: u64 = 1500;

fn pack_rgb(c: Color32) -> u32 {
    ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32
}

/// A Windows kiemelő színe (Beállítások › Személyre szabás › Színek). A fülek aktív jelölése, a kijelölt sorok és a
/// kijelölés-jellegű elemek ezt használják.
pub fn accent() -> Color32 {
    let v = ACCENT_RGB.load(Ordering::Relaxed);
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// A kiemelő szín a registryből: előbb `AccentColor` (0xAABBGGRR), tartaléknak `ColorizationColor` (0xAARRGGBB).
fn read_windows_accent() -> Option<Color32> {
    use crate::winreg::{RegKey, RegValue, HKEY_CURRENT_USER, KEY_QUERY_VALUE};
    let key = RegKey::open(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\DWM", KEY_QUERY_VALUE).ok()?;
    if let Some(RegValue::Dword(v)) = key.value("AccentColor") {
        return Some(Color32::from_rgb(v as u8, (v >> 8) as u8, (v >> 16) as u8));
    }
    if let Some(RegValue::Dword(v)) = key.value("ColorizationColor") {
        return Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    None
}

/// Időnként (legfeljebb `ACCENT_POLL_MS`-onként) újraolvassa a Windows kiemelő színét. Igazat ad vissza, ha a szín
/// megváltozott – ilyenkor a stílust (kijelölés színe) újra kell alkalmazni.
pub fn poll_accent() -> bool {
    let now = ACCENT_CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64;
    let last = ACCENT_CHECKED_MS.load(Ordering::Relaxed);
    if last != u64::MAX && now.saturating_sub(last) < ACCENT_POLL_MS {
        return false;
    }
    ACCENT_CHECKED_MS.store(now, Ordering::Relaxed);
    let packed = pack_rgb(read_windows_accent().unwrap_or(DEFAULT_ACCENT));
    ACCENT_RGB.swap(packed, Ordering::Relaxed) != packed
}

pub fn set_dark(v: bool) {
    DARK.store(v, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

// ───────────────────────── Alapértelmezett témaszínek ─────────────────────────
//
//  Felületi elem                      Világos                    Sötét
//  Ablak háttér (Window)              #FFFFFF                    #202020
//  Alkalmazás háttér (App)            #F3F3F3                    #1C1C1C
//  Elsődleges szöveg                  #000000, ~90 % (0xE4)      #FFFFFF, 100 %
//  Másodlagos szöveg                  #000000, 60 %              #FFFFFF, 78 %
//  Letiltott szöveg                   #000000, 36 %              #FFFFFF, 36 %
//  Panelek / kártyák                  #FFFFFF                    #2D2D2D
//
// A szövegek áttetsző színek (a háttéren át keverednek), ezért mindenhol jól olvashatók. A `dark` paraméteres
// változatok a stílusépítéshez kellenek (az nem a globális jelzőt használja), a paraméter nélküliek a rajzolókhoz.

/// Elsődleges szöveg.
pub fn text_primary(dark: bool) -> Color32 {
    if dark {
        Color32::WHITE
    } else {
        Color32::from_black_alpha(0xE4)
    }
}

/// Másodlagos szöveg (feliratok, leírások).
pub fn text_secondary(dark: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(199) // 78 %
    } else {
        Color32::from_black_alpha(153) // 60 %
    }
}

/// Letiltott szöveg.
pub fn text_disabled(dark: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(92) // 36 %
    } else {
        Color32::from_black_alpha(92)
    }
}

/// Az ablak háttere (felugró ablakok, párbeszédpanelek, a bal sáv tömör változata).
pub fn window_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x20, 0x20, 0x20)
    } else {
        Color32::from_rgb(0xFF, 0xFF, 0xFF)
    }
}

/// Az alkalmazás (tartalmi rész) háttere.
pub fn app_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x1C, 0x1C, 0x1C)
    } else {
        Color32::from_rgb(0xF3, 0xF3, 0xF3)
    }
}

/// Panelek, kártyák háttere.
pub fn card_bg(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x2D, 0x2D, 0x2D)
    } else {
        Color32::from_rgb(0xFF, 0xFF, 0xFF)
    }
}

/// Fő szövegszín.
pub fn col_text() -> Color32 {
    text_primary(is_dark())
}

/// Halvány (másodlagos) szövegszín.
pub fn col_muted() -> Color32 {
    text_secondary(is_dark())
}

/// Letiltott elemek szövege.
pub fn col_disabled() -> Color32 {
    text_disabled(is_dark())
}

/// A kártyák háttere.
pub fn col_card() -> Color32 {
    card_bg(is_dark())
}

/// A nem kiválasztott menüpontok szövege (a Windows 11-hez hasonlóan ugyanaz, mint az elsődleges szöveg; a kijelölést
/// a háttérpilula és a kiemelő színű csík mutatja).
pub fn nav_text() -> Color32 {
    text_primary(is_dark())
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
    app_bg(is_dark())
}

/// A bal oldali sáv háttere. Mica módban TELJESEN ÁTLÁTSZÓ: a DWM Mica anyaga (amit a címsáv is használ) ugyanúgy
/// látszik át rajta, így a címsor és a sáv egyetlen, egybefüggő felület. (Ehhez az ablakfelületnek átlátszónak kell
/// lennie: lásd `with_transparent`, a `DxgiFromVisual` úszólánc és az átlátszó `clear_color`.)
/// Tömör módban egyszínű háttér – ez a tartalék, ha a rendszeren nem működik az átlátszóság.
pub fn nav_fill(mica: bool) -> Color32 {
    match (is_dark(), mica) {
        (_, true) => Color32::TRANSPARENT,
        (dark, false) => window_bg(dark),
    }
}
