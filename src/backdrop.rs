/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Ablakháttér-effektusok: Mica (Windows 11), Acrylic (Windows 10 / 11) és Tömör.
//!
//! Minden DWM- és ablak-hívás nyers FFI-vel megy (mint a `shell.rs`-ben), így nem függ a `windows` crate feature-készletétől.
//!
//! EFFEKTUSOK ÉS MECHANIZMUSOK
//! * Mica      – Windows 11 22H2+ (build 22621): DWMWA_SYSTEMBACKDROP_TYPE = MAINWINDOW; 21H2-n (22000): DWMWA_MICA_EFFECT.
//! * Acrylic   – Windows 11 22H2+: DWMSBT_TRANSIENTWINDOW; Windows 10 1803+ (17134) és 21H2: az (undokumentált, de a
//!               rendszer által használt) SetWindowCompositionAttribute ACCENT_ENABLE_ACRYLICBLURBEHIND állapota.
//! * Tömör     – nincs átlátszóság; a címsor színét a bal sáv színére állítjuk (DWMWA_CAPTION_COLOR), hogy egybeessenek.
//!


use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::settings::Backdrop;
use crate::winreg::wide;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attr: u32, data: *const c_void, size: u32) -> i32;
    fn DwmExtendFrameIntoClientArea(hwnd: isize, margins: *const Margins) -> i32;
    fn DwmEnableBlurBehindWindow(hwnd: isize, bb: *const DwmBlurBehind) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetProcAddress(module: isize, name: *const u8) -> *const c_void;
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetClientRect(hwnd: isize, rect: *mut Rect4) -> i32;
    fn ClientToScreen(hwnd: isize, pt: *mut Point) -> i32;
    fn WindowFromPoint(pt: Point) -> isize;
    fn GetAncestor(hwnd: isize, flags: u32) -> isize;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
    fn FillRect(dc: isize, rect: *const Rect4, brush: isize) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetStockObject(index: i32) -> isize;
    fn GetPixel(dc: isize, x: i32, y: i32) -> u32;
}

#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

#[repr(C)]
struct DwmBlurBehind {
    flags: u32,
    enable: i32,
    rgn_blur: isize,
    transition_on_maximized: i32,
}

#[repr(C)]
struct Rect4 {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct OsVersionInfoW {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attrib: u32,
    data: *mut c_void,
    size: usize,
}

// DWM ablak-attribútumok
const DWMWA_TRANSITIONS_FORCEDISABLED: u32 = 3;
const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;
const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
const DWMWA_CAPTION_COLOR: u32 = 35;
const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;
const DWMWA_MICA_EFFECT: u32 = 1029;

const DWMSBT_NONE: i32 = 1;
const DWMSBT_MAINWINDOW: i32 = 2;
const DWMSBT_TRANSIENTWINDOW: i32 = 3;
const DWMWCP_ROUND: i32 = 2;
const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

// SetWindowCompositionAttribute
const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// Ki van-e terjesztve a DWM-keret a kliensterületre (ekkor a GDI-fekete = üveg; különben a fekete tényleg fekete).
static FRAME_EXTENDED: AtomicBool = AtomicBool::new(true);

/// Igaz, ha a DWM-keret ki van terjesztve a kliensterületre (ilyenkor a GDI-fekete festés üveget ad).
pub fn client_is_glass() -> bool {
    FRAME_EXTENDED.load(Ordering::Relaxed)
}

/// (főverzió, alverzió, build) – a manifest-független `RtlGetVersion`-nel, amit a rendszer nem "hazudtat".
pub fn os_version() -> (u32, u32, u32) {
    static CACHE: OnceLock<(u32, u32, u32)> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut info = OsVersionInfoW { size: std::mem::size_of::<OsVersionInfoW>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
        // SAFETY: az `info` élő, a mérete be van állítva.
        let rc = unsafe { RtlGetVersion(&mut info) };
        if rc == 0 {
            (info.major, info.minor, info.build)
        } else {
            (10, 0, 19045)
        }
    })
}

pub fn os_build() -> u32 {
    os_version().2
}

/// A kért effektusból a ténylegesen használt (a rendszer képességei szerint, lecsúszással).
pub fn resolve(requested: Backdrop) -> Backdrop {
    let build = os_build();
    let mica_ok = build >= 22000; // Windows 11
    let acrylic_ok = build >= 17134; // Windows 10 1803+
    match requested {
        Backdrop::Auto | Backdrop::Mica => {
            if mica_ok {
                Backdrop::Mica
            } else if acrylic_ok {
                Backdrop::Acrylic
            } else {
                Backdrop::Solid
            }
        }
        Backdrop::Acrylic => {
            if acrylic_ok {
                Backdrop::Acrylic
            } else {
                Backdrop::Solid
            }
        }
        other => other,
    }
}

unsafe fn set_attr<T>(hwnd: isize, attr: u32, value: &T) {
    DwmSetWindowAttribute(hwnd, attr, value as *const T as *const c_void, std::mem::size_of::<T>() as u32);
}

/// SetWindowCompositionAttribute (az user32.dll-ből futásidőben, mert nincs import-könyvtárban).
unsafe fn set_accent(hwnd: isize, state: u32, gradient_abgr: u32) {
    type Swca = unsafe extern "system" fn(isize, *mut WindowCompositionAttribData) -> i32;
    let module = wide("user32.dll");
    let h = GetModuleHandleW(module.as_ptr());
    if h == 0 {
        return;
    }
    let p = GetProcAddress(h, b"SetWindowCompositionAttribute\0".as_ptr());
    if p.is_null() {
        return;
    }
    let f: Swca = std::mem::transmute(p);
    let mut policy = AccentPolicy { accent_state: state, accent_flags: 2, gradient_color: gradient_abgr, animation_id: 0 };
    let mut data = WindowCompositionAttribData {
        attrib: WCA_ACCENT_POLICY,
        data: &mut policy as *mut AccentPolicy as *mut c_void,
        size: std::mem::size_of::<AccentPolicy>(),
    };
    f(hwnd, &mut data);
}

unsafe fn extend_frame(hwnd: isize, all: bool) {
    FRAME_EXTENDED.store(all, Ordering::Relaxed);
    let v = if all { -1 } else { 0 };
    let m = Margins { left: v, right: v, top: v, bottom: v };
    DwmExtendFrameIntoClientArea(hwnd, &m);
}

/// Az ablak háttér-effektusának beállítása. `solid_rgb`: a tömör bal sáv színe (a címsor ehhez igazodik Tömör módban).
/// Visszaadja a ténylegesen alkalmazott effektust.
pub fn apply(hwnd: isize, dark: bool, requested: Backdrop, solid_rgb: (u8, u8, u8)) -> Backdrop {
    let effect = resolve(requested);
    let build = os_build();
    // SAFETY: csak az `hwnd` ablakra vonatkozó DWM-/user32-hívások; a mutatók a hívás végéig élnek.
    unsafe {
        // Sötét / világos címsor és anyag (a régi, 19-es azonosító a Windows 10 korai kiadásaihoz kell).
        let dark_i = dark as i32;
        set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE_OLD, &dark_i);
        set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark_i);
        set_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);

        // Minden korábbi effektus lekapcsolása, hogy váltáskor ne maradjon belőle semmi.
        set_attr(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_NONE);
        if build >= 22000 {
            set_attr(hwnd, DWMWA_MICA_EFFECT, &0i32);
        }
        set_accent(hwnd, ACCENT_DISABLED, 0);
        let off = DwmBlurBehind { flags: 1, enable: 0, rgn_blur: 0, transition_on_maximized: 0 };
        DwmEnableBlurBehindWindow(hwnd, &off);
        set_attr(hwnd, DWMWA_CAPTION_COLOR, &COLOR_DEFAULT);

        // Az Acrylic tónusa: sötétben sötét, világosban világos, mérsékelten áttetsző.
        let tint: u32 = if dark { 0xA020_2020 } else { 0xA0F3_F3F3 }; // ABGR

        match effect {
            Backdrop::Mica => {
                if build >= 22621 {
                    set_attr(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_MAINWINDOW);
                } else {
                    set_attr(hwnd, DWMWA_MICA_EFFECT, &1i32);
                }
                extend_frame(hwnd, true);
            }
            Backdrop::Acrylic => {
                if build >= 22621 {
                    set_attr(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW);
                } else {
                    set_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, tint);
                }
                extend_frame(hwnd, true);
            }
            Backdrop::Solid | Backdrop::Auto => {
                extend_frame(hwnd, false);
                // A címsor színe = a tömör bal sáv színe (COLORREF: 0x00BBGGRR), így a kettő egybeolvad.
                let (r, g, b) = solid_rgb;
                let colorref = (b as u32) << 16 | (g as u32) << 8 | r as u32;
                set_attr(hwnd, DWMWA_CAPTION_COLOR, &colorref);
            }
        }
    }
    effect
}

/// A DWM-animációk ideiglenes tiltása (a javító maximalizálás–visszaállítás így nem villan).
pub fn set_transitions_enabled(hwnd: isize, enabled: bool) {
    let disabled = (!enabled) as i32;
    // SAFETY: egyetlen i32 attribútum a saját ablakra.
    unsafe { set_attr(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &disabled) };
}

/// A kliensterület GDI-s feketére festése. Kiterjesztett DWM-keretnél a fekete = üveg (átlátszó), a fehér = fehér,
/// ezért ez kitakarítja azt a "fehér" átirányítási felületet, ami az úszólánc alatt marad.
pub fn paint_client_black(hwnd: isize) {
    // SAFETY: a DC-t megszerezzük és vissza is adjuk; a téglalap élő, kimeneti struktúra.
    unsafe {
        let mut r = Rect4 { left: 0, top: 0, right: 0, bottom: 0 };
        if GetClientRect(hwnd, &mut r) == 0 {
            return;
        }
        let dc = GetDC(hwnd);
        if dc == 0 {
            return;
        }
        FillRect(dc, &r, GetStockObject(4)); // BLACK_BRUSH
        ReleaseDC(hwnd, dc);
    }
}

/// A képernyőn ténylegesen látszó pixel fényessége (0–255) a bal sávon. `None`, ha nem mérhető (takarja egy másik
/// ablak, túl kicsi az ablak stb.). Ebből ismerjük fel, hogy a Mica "fehéren indult-e".
pub fn sample_nav_luma(hwnd: isize) -> Option<u8> {
    // SAFETY: csak olvasó lekérdezések; a DC-t visszaadjuk.
    unsafe {
        let mut r = Rect4 { left: 0, top: 0, right: 0, bottom: 0 };
        if GetClientRect(hwnd, &mut r) == 0 || r.bottom < 300 {
            return None;
        }
        let mut pt = Point { x: 3, y: (r.bottom as f32 * 0.55) as i32 };
        if ClientToScreen(hwnd, &mut pt) == 0 {
            return None;
        }
        let top = WindowFromPoint(pt);
        if top == 0 || GetAncestor(top, 2) != hwnd {
            return None; // más ablak takarja
        }
        let dc = GetDC(0);
        if dc == 0 {
            return None;
        }
        let c = GetPixel(dc, pt.x, pt.y);
        ReleaseDC(0, dc);
        if c == COLOR_DEFAULT {
            return None;
        }
        let (rr, gg, bb) = (c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF);
        Some(((rr * 30 + gg * 59 + bb * 11) / 100) as u8)
    }
}

/// A mintavételezett fényesség alapján "rossz"-e az indulási állapot (fehér sötét témában, fekete világosban).
pub fn luma_looks_wrong(luma: u8, dark: bool) -> bool {
    if dark {
        luma > 215
    } else {
        luma < 40
    }
}

/// Diagnosztikai napló (`%APPDATA%\TaskManager\backdrop.log`): hogy lássuk, melyik javítási lépés segített.
pub fn log(line: &str) {
    use std::io::Write;
    let Some(dir) = crate::settings::app_data_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("backdrop.log");
    // Nem hagyjuk végtelenül nőni.
    if std::fs::metadata(&path).map(|m| m.len() > 32 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}
