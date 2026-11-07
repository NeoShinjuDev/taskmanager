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
//! Átfedő sáv (HUD) a játékok fölött: egy sorban FPS, CPU, GPU, RAM és egy kis képkockaidő-grafikon.
//!
//! MŰKÖDÉS (injektálás nélkül)
//! * Külön, saját üzenetciklusú szálon (`hud-window`) fut egy mindig felül lévő, áttetsző, átkattintható és nem
//!   aktiválható ablak (`WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`). Az ablakot GDI-vel
//!   rajzoljuk. Mivel nem az egui rajzolja, a főablak állapotától (kicsinyítve, tálcára rejtve) független.
//! * A CPU / GPU / RAM adatokat a hardver-háttérszál adja át (`publish`), az FPS-t és a képkockaidőket a `present`
//!   modul méri ETW-vel az éppen előtérben lévő ablak folyamatára.
//! * Negyedmásodpercenként (`TIMER_MS`) a szál eldönti, látszódjon-e a sáv (be van-e kapcsolva, és ha kell, teljes
//!   képernyős-e az előtérben lévő ablak), kiszámolja a tartalmat és a helyét, majd újrarajzol.
//!
//! KORLÁTOK
//! * Ablakos és szegély nélküli (borderless) játékok fölött látszik; valódi, kizárólagos teljes képernyős módban a
//!   játék takarja.
//! * Az FPS csak DXGI-t (DirectX 10 / 11 / 12) vagy D3D9-et használó játéknál mérhető; egyébként kötőjel látszik.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;

use crate::present;
use crate::settings::HudPrefs;
use crate::winreg::wide;

// ───────────────────────── a hardver-háttérszál adatai ─────────────────────────

/// A kiválasztott videókártya adatai az átfedő sávhoz.
#[derive(Clone, Debug)]
pub struct HudGpu {
    pub util: Option<f32>,
    pub vram_used_mb: Option<f64>,
    pub vram_total_mb: f64,
    pub temp: Option<f32>,
}

/// Az átfedő sáv hardveradatai (a háttérszál minden mintavételnél frissíti).
#[derive(Clone, Debug, Default)]
pub struct HudData {
    pub cpu_pct: f32,
    pub cpu_temp: Option<f32>,
    pub ram_used_mb: f64,
    pub ram_total_mb: f64,
    pub gpu: Option<HudGpu>,
}

static PREFS: Mutex<HudPrefs> = Mutex::new(HudPrefs {
    enabled: false,
    corner: 0,
    size: 1,
    fullscreen_only: true,
    graph: true,
    cpu: true,
    gpu: true,
    ram: true,
});
static DATA: Mutex<Option<HudData>> = Mutex::new(None);
static FRAME: Mutex<Option<Frame>> = Mutex::new(None);
static WINDOW_STARTED: AtomicBool = AtomicBool::new(false);
/// A mérés vezérlése: egymás közti kizárás, hogy a be- és kikapcsolás sorrendje ne keveredjen össze.
static CTRL: Mutex<()> = Mutex::new(());
/// A legutóbb alkalmazott "be van-e kapcsolva": 0 még nem volt alkalmazva, 1 ki, 2 be.
static LAST_ENABLED: AtomicU8 = AtomicU8::new(0);

/// A háttérszál hívja minden új mintánál.
pub fn publish(data: HudData) {
    if let Ok(mut g) = DATA.lock() {
        *g = Some(data);
    }
}

/// Az átfedő sáv beállításainak alkalmazása (induláskor és minden beállításváltozáskor hívandó).
pub fn apply(prefs: HudPrefs) {
    if let Ok(mut g) = PREFS.lock() {
        *g = prefs;
    }
    if prefs.enabled {
        ensure_window_thread();
    }
    let now = if prefs.enabled { 2 } else { 1 };
    if LAST_ENABLED.swap(now, Ordering::Relaxed) == now {
        return; // az ETW-mérés állapota nem változott
    }
    // Az ETW-munkamenet indítása / leállítása nem a felületi szálon fut (az indítás pár ezredmásodpercig is tarthat).
    let _ = std::thread::Builder::new().name("hud-etw-ctl".into()).spawn(|| {
        let _guard = CTRL.lock().unwrap_or_else(|e| e.into_inner());
        let want = PREFS.lock().map(|g| g.enabled).unwrap_or(false);
        if want {
            present::start();
        } else {
            present::stop();
        }
    });
}

/// Kilépéskor hívandó: az ETW-munkamenet nem maradhat meg a program után.
pub fn stop() {
    present::stop();
}

fn ensure_window_thread() {
    if WINDOW_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new().name("hud-window".into()).spawn(|| {
        // A panik nem vihet magával semmit: legfeljebb nincs átfedő sáv.
        let _ = std::panic::catch_unwind(window_thread);
    });
}

// ───────────────────────── Win32 FFI ─────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[allow(dead_code)]
struct MonitorInfo {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}

#[repr(C)]
#[allow(dead_code)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
}

#[repr(C)]
#[allow(dead_code)]
struct PaintStruct {
    hdc: isize,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: Option<WndProc>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    icon_small: isize,
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *const c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn SetTimer(hwnd: isize, id: usize, ms: u32, func: *const c_void) -> usize;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn SetLayeredWindowAttributes(hwnd: isize, key: u32, alpha: u8, flags: u32) -> i32;
    fn InvalidateRect(hwnd: isize, rect: *const Rect, erase: i32) -> i32;
    fn BeginPaint(hwnd: isize, ps: *mut PaintStruct) -> isize;
    fn EndPaint(hwnd: isize, ps: *const PaintStruct) -> i32;
    fn GetForegroundWindow() -> isize;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn GetWindowRect(hwnd: isize, rect: *mut Rect) -> i32;
    fn GetClassNameW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
    fn FillRect(dc: isize, rect: *const Rect, brush: isize) -> i32;
    fn PostQuitMessage(code: i32);
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(dc: isize) -> isize;
    fn CreateCompatibleBitmap(dc: isize, w: i32, h: i32) -> isize;
    fn SelectObject(dc: isize, obj: isize) -> isize;
    fn DeleteObject(obj: isize) -> i32;
    fn DeleteDC(dc: isize) -> i32;
    fn CreateSolidBrush(color: u32) -> isize;
    fn SetBkMode(dc: isize, mode: i32) -> i32;
    fn SetTextColor(dc: isize, color: u32) -> u32;
    fn TextOutW(dc: isize, x: i32, y: i32, text: *const u16, count: i32) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike_out: u32,
        charset: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face: *const u16,
    ) -> isize;
    fn GetTextExtentPoint32W(dc: isize, text: *const u16, count: i32, size: *mut Size) -> i32;
    fn BitBlt(dst: isize, x: i32, y: i32, w: i32, h: i32, src: isize, sx: i32, sy: i32, rop: u32) -> i32;
}

#[link(name = "shcore")]
extern "system" {
    fn GetDpiForMonitor(monitor: isize, kind: i32, dpi_x: *mut u32, dpi_y: *mut u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

const WS_POPUP: u32 = 0x8000_0000;
const WS_EX_TOPMOST: u32 = 0x0000_0008;
const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const WS_EX_LAYERED: u32 = 0x0008_0000;
const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const LWA_ALPHA: u32 = 0x0000_0002;
const SW_HIDE: i32 = 0;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_SHOWWINDOW: u32 = 0x0040;
const HWND_TOPMOST: isize = -1;
const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
const MONITOR_DEFAULTTONEAREST: u32 = 2;
const WM_DESTROY: u32 = 0x0002;
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_MOUSEACTIVATE: u32 = 0x0021;
const WM_NCHITTEST: u32 = 0x0084;
const WM_TIMER: u32 = 0x0113;
const HTTRANSPARENT: isize = -1;
const MA_NOACTIVATE: isize = 3;
const TRANSPARENT: i32 = 1;
const SRCCOPY: u32 = 0x00CC_0020;
const FW_SEMIBOLD: i32 = 600;
const DEFAULT_CHARSET: u32 = 1;
const CLEARTYPE_QUALITY: u32 = 5;

/// A sáv hátterének áttetszetlensége (0–255).
const BAR_ALPHA: u8 = 225;
/// Az átfedő sáv frissítési ideje.
const TIMER_MS: u32 = 250;

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

// ───────────────────────── a megjelenítendő tartalom ─────────────────────────

#[derive(Clone)]
struct Seg {
    label: String,
    value: String,
    label_w: i32,
    value_w: i32,
}

/// A kirajzolásra kész sáv: a tartalom és a kiszámolt méretek (képpontban).
#[derive(Clone)]
struct Frame {
    segs: Vec<Seg>,
    /// A legutóbbi képkockaidők (ms); `None`, ha a grafikon ki van kapcsolva.
    graph: Option<Vec<f32>>,
    w: i32,
    h: i32,
    font_px: i32,
    pad: i32,
    gap: i32,
    sp: i32,
    graph_w: i32,
    cy: i32,
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn text_width(dc: isize, s: &str) -> i32 {
    let w = utf16(s);
    let mut size = Size { cx: 0, cy: 0 };
    // SAFETY: érvényes DC, a `w` élő puffer, a `size` élő kimeneti struktúra.
    unsafe { GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32, &mut size) };
    size.cx
}

fn text_out(dc: isize, x: i32, y: i32, s: &str) {
    let w = utf16(s);
    // SAFETY: érvényes DC és élő puffer.
    unsafe { TextOutW(dc, x, y, w.as_ptr(), w.len() as i32) };
}

fn make_font(px: i32) -> isize {
    let face = wide("Segoe UI");
    // SAFETY: a `face` NUL-terminált és a hívás idejére él.
    unsafe {
        CreateFontW(-px, 0, 0, 0, FW_SEMIBOLD, 0, 0, 0, DEFAULT_CHARSET, 0, 0, CLEARTYPE_QUALITY, 0, face.as_ptr())
    }
}

fn gb(mb: f64) -> f64 {
    mb / 1024.0
}

/// A sor szegmensei az aktuális adatokból: (címke, érték).
fn build_segments(prefs: &HudPrefs, fps: Option<f32>) -> Vec<(String, String)> {
    let data = DATA.lock().ok().and_then(|g| g.clone());
    let mut out: Vec<(String, String)> = Vec::new();
    out.push(("FPS".to_string(), fps.map_or_else(|| "–".to_string(), |f| format!("{f:.0}"))));
    let Some(d) = data else { return out };
    if prefs.cpu {
        let mut v = format!("{:.0}%", d.cpu_pct);
        if let Some(t) = d.cpu_temp {
            v.push_str(&format!(" {t:.0}°C"));
        }
        out.push(("CPU".to_string(), v));
    }
    if prefs.gpu {
        if let Some(g) = &d.gpu {
            let mut parts: Vec<String> = Vec::new();
            if let Some(u) = g.util {
                parts.push(format!("{u:.0}%"));
            }
            if let Some(used) = g.vram_used_mb {
                if g.vram_total_mb > 0.0 {
                    parts.push(format!("{:.1}/{:.1} GB", gb(used), gb(g.vram_total_mb)));
                }
            }
            if let Some(t) = g.temp {
                parts.push(format!("{t:.0}°C"));
            }
            if !parts.is_empty() {
                out.push(("GPU".to_string(), parts.join(" ")));
            }
        }
    }
    if prefs.ram && d.ram_total_mb > 0.0 {
        out.push(("RAM".to_string(), format!("{:.1}/{:.1} GB", gb(d.ram_used_mb), gb(d.ram_total_mb))));
    }
    out
}

// ───────────────────────── a figyelő (időzítő) ─────────────────────────

fn hide(hwnd: isize) {
    // SAFETY: a `hwnd` a saját ablakunk.
    unsafe { ShowWindow(hwnd, SW_HIDE) };
}

/// Az előtérben lévő ablak és folyamata.
fn foreground() -> (isize, u32) {
    // SAFETY: egyszerű lekérdezések; a `pid` élő kimeneti változó.
    unsafe {
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        if fg != 0 {
            GetWindowThreadProcessId(fg, &mut pid);
        }
        (fg, pid)
    }
}

/// Kitölti-e az ablak a teljes monitort (játékok teljes képernyős vagy szegély nélküli módja)? Az asztal és a tálca
/// ablakát nem számítjuk annak.
fn fullscreen_like(hwnd: isize, monitor: &Rect) -> bool {
    if hwnd == 0 {
        return false;
    }
    let mut cls = [0u16; 64];
    // SAFETY: a puffer 64 elemű, és ennyit adunk meg maximumnak.
    let n = unsafe { GetClassNameW(hwnd, cls.as_mut_ptr(), 64) };
    let name = String::from_utf16_lossy(&cls[..n.clamp(0, 64) as usize]);
    if matches!(name.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
        return false;
    }
    let mut r = Rect::default();
    // SAFETY: élő kimeneti struktúra.
    if unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
        return false;
    }
    r.left <= monitor.left && r.top <= monitor.top && r.right >= monitor.right && r.bottom >= monitor.bottom
}

fn monitor_dpi(monitor: isize) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: élő kimeneti változók; 0 = MDT_EFFECTIVE_DPI.
    let hr = unsafe { GetDpiForMonitor(monitor, 0, &mut x, &mut y) };
    if hr == 0 && x >= 48 {
        x
    } else {
        96
    }
}

/// Negyedmásodpercenként: döntés a láthatóságról, a tartalom és a méretek kiszámolása, a helyzet beállítása.
fn tick(hwnd: isize) {
    let prefs = PREFS.lock().map(|g| *g).unwrap_or_default();
    if !prefs.enabled {
        hide(hwnd);
        return;
    }
    let (fg, pid) = foreground();
    // SAFETY: egyszerű lekérdezések érvényes ablakkezelőkkel.
    let monitor = unsafe {
        if fg != 0 {
            MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY)
        }
    };
    if monitor == 0 {
        hide(hwnd);
        return;
    }
    let mut mi = MonitorInfo {
        size: std::mem::size_of::<MonitorInfo>() as u32,
        monitor: Rect::default(),
        work: Rect::default(),
        flags: 0,
    };
    // SAFETY: élő kimeneti struktúra, a `size` mező kitöltve.
    if unsafe { GetMonitorInfoW(monitor, &mut mi) } == 0 {
        hide(hwnd);
        return;
    }
    if prefs.fullscreen_only && !fullscreen_like(fg, &mi.monitor) {
        hide(hwnd);
        return;
    }

    // Méretezés: a monitor DPI-je és a választott méret szorzata.
    let factor = [1.0_f32, 1.25, 1.5][prefs.size.min(2) as usize];
    let scale = monitor_dpi(monitor) as f32 / 96.0 * factor;
    let px = |v: f32| (v * scale).round().max(1.0) as i32;
    let font_px = px(14.0);
    let pad = px(8.0);
    let gap = px(16.0);
    let sp = px(5.0);
    let graph_w = px(110.0);

    // FPS és képkockaidők az előtérben lévő folyamatról.
    let stats = if pid != 0 { present::stats(pid, graph_w as usize) } else { None };
    let fresh = stats.as_ref().filter(|s| s.age_ms < 2000.0);
    let fps = fresh.map(|s| s.fps);
    let graph = if prefs.graph {
        Some(fresh.map(|s| s.frametimes_ms.clone()).unwrap_or_default())
    } else {
        None
    };
    let parts = build_segments(&prefs, fps);

    // Méretek mérése a kiválasztott betűtípussal.
    // SAFETY: a DC-t és a betűtípust lefoglaljuk és a végén felszabadítjuk.
    let (segs, cy) = unsafe {
        let dc = GetDC(hwnd);
        let font = make_font(font_px);
        let old = SelectObject(dc, font);
        let segs: Vec<Seg> = parts
            .into_iter()
            .map(|(label, value)| {
                let label_w = text_width(dc, &label);
                let value_w = text_width(dc, &value);
                Seg { label, value, label_w, value_w }
            })
            .collect();
        let mut size = Size { cx: 0, cy: 0 };
        let probe = utf16("Ag");
        GetTextExtentPoint32W(dc, probe.as_ptr(), probe.len() as i32, &mut size);
        SelectObject(dc, old);
        DeleteObject(font);
        ReleaseDC(hwnd, dc);
        (segs, size.cy.max(font_px))
    };

    let mut w = pad * 2;
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            w += gap;
        }
        w += s.label_w + sp + s.value_w;
    }
    if graph.is_some() {
        w += gap + graph_w;
    }
    let h = cy + pad * 2;

    // Helyzet: sima csík a monitor választott sarkában, közvetlenül a szélhez illesztve (nincs margó, nincs kerekítés).
    let (left, right) = (mi.monitor.left, mi.monitor.right);
    let (top, bottom) = (mi.monitor.top, mi.monitor.bottom);
    let x = if prefs.corner == 1 || prefs.corner == 3 { right - w } else { left };
    let y = if prefs.corner >= 2 { bottom - h } else { top };

    if let Ok(mut f) = FRAME.lock() {
        *f = Some(Frame { segs, graph, w, h, font_px, pad, gap, sp, graph_w, cy });
    }
    // SAFETY: a `hwnd` a saját ablakunk.
    unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

// ───────────────────────── rajzolás ─────────────────────────

fn fill(dc: isize, left: i32, top: i32, right: i32, bottom: i32, brush: isize) {
    let r = Rect { left, top, right, bottom };
    // SAFETY: érvényes DC és ecset, élő téglalap.
    unsafe { FillRect(dc, &r, brush) };
}

/// A képkockaidő-grafikon: oszloponként egy képkocka; a feltűnően hosszú képkockák (microstutter) pirosak.
fn draw_graph(dc: isize, x: i32, y: i32, w: i32, h: i32, times: &[f32]) {
    // SAFETY: az ecseteket létrehozzuk és a végén töröljük.
    unsafe {
        let back = CreateSolidBrush(rgb(36, 36, 44));
        let good = CreateSolidBrush(rgb(90, 200, 120));
        let bad = CreateSolidBrush(rgb(235, 90, 80));
        fill(dc, x, y, x + w, y + h, back);
        if !times.is_empty() {
            let avg = times.iter().sum::<f32>() / times.len() as f32;
            // A skála a képkockaidő kétszeresét (de legalább 16,7 ms-ot) mutatja, hogy a kiugrások jól látsszanak.
            let top = (avg * 2.5).max(16.7);
            let n = times.len().min(w.max(0) as usize);
            let start = times.len() - n;
            for (i, &dt) in times[start..].iter().enumerate() {
                let bar = ((dt / top).clamp(0.0, 1.0) * h as f32).round().max(1.0) as i32;
                let bx = x + w - n as i32 + i as i32;
                let stutter = dt > avg * 1.5 + 2.0;
                fill(dc, bx, y + h - bar, bx + 1, y + h, if stutter { bad } else { good });
            }
        }
        DeleteObject(back);
        DeleteObject(good);
        DeleteObject(bad);
    }
}

fn paint(hwnd: isize) {
    let frame = FRAME.lock().ok().and_then(|g| g.clone());
    // SAFETY: a csupa nulla bájt érvényes a PaintStruct-nak (csak egész számok).
    let mut ps: PaintStruct = unsafe { std::mem::zeroed() };
    // SAFETY: élő PaintStruct; a BeginPaint párja az EndPaint.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc != 0 {
        if let Some(f) = frame {
            draw(hdc, &f);
        }
    }
    // SAFETY: mint fent.
    unsafe { EndPaint(hwnd, &ps) };
}

/// A sáv kirajzolása memória-DC-n keresztül (villogásmentesen).
fn draw(hdc: isize, f: &Frame) {
    // SAFETY: minden lefoglalt GDI-objektumot a függvény végén felszabadítunk; a DC-k érvényesek.
    unsafe {
        let mdc = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, f.w, f.h);
        let old_bmp = SelectObject(mdc, bmp);

        let back = CreateSolidBrush(rgb(18, 18, 22));
        fill(mdc, 0, 0, f.w, f.h, back);
        DeleteObject(back);

        let font = make_font(f.font_px);
        let old_font = SelectObject(mdc, font);
        SetBkMode(mdc, TRANSPARENT);
        let mut x = f.pad;
        let y = f.pad;
        for (i, s) in f.segs.iter().enumerate() {
            if i > 0 {
                x += f.gap;
            }
            SetTextColor(mdc, rgb(150, 150, 165));
            text_out(mdc, x, y, &s.label);
            x += s.label_w + f.sp;
            SetTextColor(mdc, rgb(240, 240, 245));
            text_out(mdc, x, y, &s.value);
            x += s.value_w;
        }
        if let Some(times) = &f.graph {
            x += f.gap;
            draw_graph(mdc, x, y, f.graph_w, f.cy, times);
        }
        SelectObject(mdc, old_font);
        DeleteObject(font);

        BitBlt(hdc, 0, 0, f.w, f.h, mdc, 0, 0, SRCCOPY);
        SelectObject(mdc, old_bmp);
        DeleteObject(bmp);
        DeleteDC(mdc);
    }
}

// ───────────────────────── az ablak és szála ─────────────────────────

unsafe extern "system" fn wndproc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    // A panik nem léphet át az FFI-határon.
    let handled = std::panic::catch_unwind(|| match msg {
        WM_TIMER => {
            tick(hwnd);
            Some(0)
        }
        WM_PAINT => {
            paint(hwnd);
            Some(0)
        }
        WM_ERASEBKGND => Some(1),
        // Átkattintható: az egér mindent a mögötte lévő ablaknak ad át, és az ablak sosem kap fókuszt.
        WM_NCHITTEST => Some(HTTRANSPARENT),
        WM_MOUSEACTIVATE => Some(MA_NOACTIVATE),
        WM_DESTROY => {
            // SAFETY: egyszerű hívás a saját szálunkon.
            unsafe { PostQuitMessage(0) };
            Some(0)
        }
        _ => None,
    });
    match handled {
        Ok(Some(v)) => v,
        // SAFETY: a rendszer által átadott paraméterek változatlanul továbbadva.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn window_thread() {
    let class = wide("TaskManagerHudWindow");
    // SAFETY: a Win32 hívások érvényes paraméterekkel; az osztálynév és az ablak a szál élettartama alatt él.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WndClassExW {
            size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            wnd_proc: Some(wndproc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
            icon_small: 0,
        };
        if RegisterClassExW(&wc) == 0 {
            return;
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            0,
            0,
            instance,
            std::ptr::null(),
        );
        if hwnd == 0 {
            return;
        }
        SetLayeredWindowAttributes(hwnd, 0, BAR_ALPHA, LWA_ALPHA);
        SetTimer(hwnd, 1, TIMER_MS, std::ptr::null());

        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
