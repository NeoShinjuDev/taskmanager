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
//! Tálcaikonok az értesítési területen: kis, modern mérők (CPU, memória, GPU).
//!
//! MŰKÖDÉS
//! * Külön szál (`tray`) rejtett, csak üzenetekre figyelő ablakot hoz létre, és ezen keresztül kezeli a tálcaikonokat:
//!   másodpercenként újrarajzolja az ikonokat az utolsó értékekből, a tooltip a mért értéket mutatja. A beállítástól
//!   függően egy (CPU) vagy három (CPU, memória, GPU) ikon látszik.
//! * Bal kattintás (vagy dupla kattintás): előhozza és előre teszi a Feladatkezelő ablakát. Jobb kattintás: helyi menü
//!   (Megnyitás / Kilépés).
//! * Az ikon egy kerekített, sötét üveghatású négyzet, amelyben a terheléssel arányosan alulról töltődik egy sáv,
//!   a grafikonok színével (CPU kék, memória lila, GPU zöld). Saját háttere van, így világos és sötét tálcán is jól
//!   látszik.
//!
//! Minden Win32-hívás nyers FFI-vel megy (mint a többi modulban).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use crate::i18n::tr;
use crate::winreg::wide;
use crate::{CPU_COLOR, GPU_COLOR, MEM_COLOR};

const WM_NULL: u32 = 0x0000;
const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_COMMAND: u32 = 0x0111;
const WM_TIMER: u32 = 0x0113;
const WM_CONTEXTMENU: u32 = 0x007B;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;
/// A tálcaikonok saját visszahívási üzenete (WM_APP + 1).
const WM_TRAY: u32 = 0x8001;
/// Kérés a tálcaikonok azonnali újraszinkronizálására (WM_APP + 2), pl. a beállítás változása után.
const WM_REFRESH: u32 = 0x8002;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;

const MF_STRING: u32 = 0x0;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTBUTTON: u32 = 0x2;

const HWND_MESSAGE: isize = -3;
const SM_CXSMICON: i32 = 49;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;

const ID_OPEN: usize = 1001;
const ID_EXIT: usize = 1002;

/// Az ikonok fajtái: 0 = CPU, 1 = memória, 2 = GPU (a bitmaszk bitjei is ebben a sorrendben).
const KINDS: usize = 3;
/// Mely ikonok látszanak (bitmaszk: 1 CPU, 2 memória, 4 GPU).
static MASK: AtomicU32 = AtomicU32::new(0);
/// A mért értékek (%) ×10 fajtánként, az UI tölti.
static VALUES: [AtomicU32; KINDS] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
/// Az utoljára megjelenített (egész %-ra kerekített) érték fajtánként, hogy csak változáskor rajzoljunk újra.
static LAST: [AtomicU32; KINDS] = [AtomicU32::new(u32::MAX), AtomicU32::new(u32::MAX), AtomicU32::new(u32::MAX)];
static ADDED: [AtomicBool; KINDS] = [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];
static ICONS: [AtomicIsize; KINDS] = [AtomicIsize::new(0), AtomicIsize::new(0), AtomicIsize::new(0)];
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static QUIT: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

#[repr(C)]
#[allow(dead_code)]
struct WndClassExW {
    cb_size: u32,
    style: u32,
    wnd_proc: unsafe extern "system" fn(isize, u32, usize, isize) -> isize,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: isize,
    h_icon: isize,
    h_cursor: isize,
    hbr_background: isize,
    menu_name: *const u16,
    class_name: *const u16,
    h_icon_sm: isize,
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
    private: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct Point {
    x: i32,
    y: i32,
}

/// NOTIFYICONDATAW (976 bájt x64-en).
#[repr(C)]
#[allow(dead_code)]
struct NotifyIconData {
    cb_size: u32,
    hwnd: isize,
    id: u32,
    flags: u32,
    callback_message: u32,
    icon: isize,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid_item: [u32; 4],
    balloon_icon: isize,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct IconInfo {
    f_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    hbm_mask: isize,
    hbm_color: isize,
}

#[repr(C)]
#[allow(dead_code)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels_per_meter: i32,
    y_pels_per_meter: i32,
    clr_used: u32,
    clr_important: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 1],
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(wc: *const WndClassExW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
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
    fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(hwnd: isize) -> i32;
    fn SetTimer(hwnd: isize, id: usize, elapse_ms: u32, proc_: isize) -> usize;
    fn KillTimer(hwnd: isize, id: usize) -> i32;
    fn CreatePopupMenu() -> isize;
    fn AppendMenuW(menu: isize, flags: u32, id: usize, text: *const u16) -> i32;
    fn TrackPopupMenu(menu: isize, flags: u32, x: i32, y: i32, reserved: i32, hwnd: isize, rect: *const c_void) -> i32;
    fn DestroyMenu(menu: isize) -> i32;
    fn GetCursorPos(pt: *mut Point) -> i32;
    fn SetForegroundWindow(h: isize) -> i32;
    fn ShowWindow(h: isize, cmd: i32) -> i32;
    fn IsIconic(h: isize) -> i32;
    fn CreateIconIndirect(info: *const IconInfo) -> isize;
    fn DestroyIcon(hicon: isize) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn RegisterWindowMessageW(name: *const u16) -> u32;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateDIBSection(
        hdc: isize,
        bmi: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut c_void,
        section: isize,
        offset: u32,
    ) -> isize;
    fn CreateBitmap(w: i32, h: i32, planes: u32, bits_per_pixel: u32, bits: *const c_void) -> isize;
    fn DeleteObject(h: isize) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> isize;
}

// ───────────────────────── nyilvános felület ─────────────────────────

/// Beállítja, mely tálcaikonok látszanak (`mask`: 1 CPU, 2 memória, 4 GPU; 0 = egyik sem), és szükség szerint elindítja
/// vagy leállítja a háttérszálat. `main_hwnd`: a Feladatkezelő főablaka, amit a kattintás előhoz.
pub fn configure(main_hwnd: isize, mask: u32) {
    MAIN_HWND.store(main_hwnd, Ordering::Relaxed);
    let mask = mask & 7;
    MASK.store(mask, Ordering::Relaxed);
    if mask == 0 {
        stop();
        return;
    }
    let running = THREAD.lock().map(|t| t.is_some()).unwrap_or(false);
    let hw = TRAY_HWND.load(Ordering::Relaxed);
    if running && hw != 0 {
        // SAFETY: egyszerű üzenetküldés a saját rejtett ablakunknak.
        unsafe {
            PostMessageW(hw, WM_REFRESH, 0, 0);
        }
    } else if !running {
        start();
    }
}

fn start() {
    let Ok(mut t) = THREAD.lock() else { return };
    if t.is_some() {
        return;
    }
    QUIT.store(false, Ordering::Relaxed);
    if let Ok(h) = std::thread::Builder::new().name("tray".into()).spawn(tray_thread) {
        *t = Some(h);
    }
}

/// Eltávolítja a tálcaikonokat és leállítja a szálat (kilépéskor és a beállítás kikapcsolásakor).
pub fn stop() {
    let handle = THREAD.lock().ok().and_then(|mut t| t.take());
    let Some(h) = handle else { return };
    QUIT.store(true, Ordering::Relaxed);
    let hw = TRAY_HWND.load(Ordering::Relaxed);
    if hw != 0 {
        // SAFETY: egyszerű üzenetküldés a saját rejtett ablakunknak.
        unsafe {
            PostMessageW(hw, WM_CLOSE, 0, 0);
        }
    }
    let _ = h.join();
}

/// Az UI minden képkockán átadja az utolsó mért CPU-, memória- és GPU-használatot (%).
pub fn update(cpu: f32, mem: f32, gpu: f32) {
    for (slot, v) in VALUES.iter().zip([cpu, mem, gpu]) {
        slot.store((v.clamp(0.0, 100.0) * 10.0) as u32, Ordering::Relaxed);
    }
}

// ───────────────────────── a szál és az ablak ─────────────────────────

fn tray_thread() {
    // SAFETY: nyers Win32 hívások; minden mutató a függvény idejéig él, az ablakot és az ikont ez a szál birtokolja.
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide("TaskManagerTrayWindow");
        let wc = WndClassExW {
            cb_size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            wnd_proc,
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: hinst,
            h_icon: 0,
            h_cursor: 0,
            hbr_background: 0,
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
            h_icon_sm: 0,
        };
        // Ha az osztály korábbról már regisztrálva van, a hívás hibát ad, a létrehozás ettől még működik.
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            hinst,
            std::ptr::null(),
        );
        if hwnd == 0 {
            return;
        }
        TRAY_HWND.store(hwnd, Ordering::Relaxed);
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);
        reset_icons();
        refresh_icons(hwnd);
        SetTimer(hwnd, 1, 1000, 0);
        if QUIT.load(Ordering::Relaxed) {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        let mut msg: Msg = std::mem::zeroed();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        TRAY_HWND.store(0, Ordering::Relaxed);
    }
}

unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
    match msg {
        WM_TIMER | WM_REFRESH => {
            refresh_icons(hwnd);
            0
        }
        WM_TRAY => {
            match (lparam as u32) & 0xFFFF {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main_window(),
                WM_RBUTTONUP | WM_CONTEXTMENU => popup_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                ID_OPEN => show_main_window(),
                ID_EXIT => {
                    let main = MAIN_HWND.load(Ordering::Relaxed);
                    if main != 0 {
                        PostMessageW(main, WM_CLOSE, 0, 0);
                    }
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            KillTimer(hwnd, 1);
            for k in 0..KINDS {
                if ADDED[k].swap(false, Ordering::Relaxed) {
                    notify(NIM_DELETE, hwnd, k as u32 + 1, 0, "");
                }
                let old = ICONS[k].swap(0, Ordering::Relaxed);
                if old != 0 {
                    DestroyIcon(old);
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => {
            // Az Explorer újraindulása után az ikont újra fel kell venni.
            let created = TASKBAR_CREATED.load(Ordering::Relaxed);
            if created != 0 && msg == created {
                reset_icons();
                refresh_icons(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

/// Előhozza a főablakot: a minimalizáltat visszaállítja, a többit előre teszi.
unsafe fn show_main_window() {
    let main = MAIN_HWND.load(Ordering::Relaxed);
    if main == 0 {
        return;
    }
    ShowWindow(main, if IsIconic(main) != 0 { SW_RESTORE } else { SW_SHOW });
    SetForegroundWindow(main);
}

unsafe fn popup_menu(hwnd: isize) {
    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }
    AppendMenuW(menu, MF_STRING, ID_OPEN, wide(tr("Open Task Manager")).as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, wide(tr("Exit")).as_ptr());
    let mut pt = Point { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // A menü csak akkor záródik be megfelelően a menün kívüli kattintásra, ha az ablak előtérbe kerül.
    SetForegroundWindow(hwnd);
    TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, std::ptr::null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

/// Tálcaikon-művelet (NIM_ADD / NIM_MODIFY / NIM_DELETE) az `id` azonosítójú ikonon.
unsafe fn notify(op: u32, hwnd: isize, id: u32, icon: isize, tip: &str) {
    let mut d: NotifyIconData = std::mem::zeroed();
    d.cb_size = std::mem::size_of::<NotifyIconData>() as u32;
    d.hwnd = hwnd;
    d.id = id;
    d.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.callback_message = WM_TRAY;
    d.icon = icon;
    for (i, u) in tip.encode_utf16().take(127).enumerate() {
        d.tip[i] = u;
    }
    Shell_NotifyIconW(op, &mut d);
}

/// Az ikonok "nincs felvéve" állapotba állítása (induláskor és az Explorer újraindulása után).
fn reset_icons() {
    for k in 0..KINDS {
        ADDED[k].store(false, Ordering::Relaxed);
        LAST[k].store(u32::MAX, Ordering::Relaxed);
    }
}

/// Összehangolja a tálcaikonokat a beállítással és az értékekkel: felveszi vagy törli az ikonokat, és újrarajzolja
/// azokat, amelyek egész százalékra kerekített értéke megváltozott.
unsafe fn refresh_icons(hwnd: isize) {
    let mask = MASK.load(Ordering::Relaxed);
    let size = GetSystemMetrics(SM_CXSMICON).clamp(16, 64);
    for k in 0..KINDS {
        let id = k as u32 + 1;
        if mask & (1 << k) == 0 {
            // Kikapcsolt ikon: ha fent van, töröljük.
            if ADDED[k].swap(false, Ordering::Relaxed) {
                notify(NIM_DELETE, hwnd, id, 0, "");
                let old = ICONS[k].swap(0, Ordering::Relaxed);
                if old != 0 {
                    DestroyIcon(old);
                }
            }
            LAST[k].store(u32::MAX, Ordering::Relaxed);
            continue;
        }
        let value = VALUES[k].load(Ordering::Relaxed) as f32 / 10.0;
        let key = value.round() as u32;
        if ADDED[k].load(Ordering::Relaxed) && key == LAST[k].load(Ordering::Relaxed) {
            continue;
        }
        LAST[k].store(key, Ordering::Relaxed);
        let color = match k {
            0 => CPU_COLOR,
            1 => MEM_COLOR,
            _ => GPU_COLOR,
        };
        let icon = make_icon(size, value, (color.r() as f32, color.g() as f32, color.b() as f32));
        if icon == 0 {
            continue;
        }
        let tip = match k {
            0 if mask & 2 == 0 => {
                let mem = VALUES[1].load(Ordering::Relaxed) as f32 / 10.0;
                format!("CPU {:.0}%  ·  {} {:.0}%", value, tr("Memory"), mem)
            }
            0 => format!("CPU {:.0}%", value),
            1 => format!("{} {:.0}%", tr("Memory"), value),
            _ => format!("GPU {:.0}%", value),
        };
        let op = if ADDED[k].swap(true, Ordering::Relaxed) { NIM_MODIFY } else { NIM_ADD };
        notify(op, hwnd, id, icon, &tip);
        let old = ICONS[k].swap(icon, Ordering::Relaxed);
        if old != 0 {
            DestroyIcon(old);
        }
    }
}

// ───────────────────────── az ikon megrajzolása ─────────────────────────

/// Egy kerekített téglalap lefedettsége (0..1) a középponttól mért (dx, dy) helyen: élsimított szél.
fn rr_cov(dx: f32, dy: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = dx.abs() - (half_w - radius);
    let qy = dy.abs() - (half_h - radius);
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
    (0.5 - d).clamp(0.0, 1.0)
}

fn mix(a: (f32, f32, f32), b: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// A `size`×`size` képpontos ikon pixelei (BGRA, egyenes alfa): sötét, kerekített négyzet, finom kerettel és a
/// terheléssel arányosan alulról töltődő színes sávval.
fn render_pixels(size: usize, cpu: f32, fill_color: (f32, f32, f32)) -> Vec<u32> {
    let s = size as f32;
    let mut px = vec![0u32; size * size];
    let c = s / 2.0;
    let half = s / 2.0 - 0.25;
    let radius = (s * 0.26).max(3.0);
    let inset = (s * 0.17).max(2.5);
    let inner_top = inset;
    let inner_bottom = s - inset;
    let level = (cpu / 100.0).clamp(0.0, 1.0);
    let fill_h = (inner_bottom - inner_top) * level;
    let fill_top = inner_bottom - fill_h;
    let border_color = (112.0, 124.0, 146.0);
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let outer = rr_cov(fx - c, fy - c, half, half, radius);
            if outer <= 0.0 {
                continue;
            }
            let inner = rr_cov(fx - c, fy - c, half - 1.0, half - 1.0, (radius - 1.0).max(1.0));
            let border = (outer - inner).clamp(0.0, 1.0);
            // Sötét üveghatású háttér: felül világosabb.
            let mut col = mix((52.0, 58.0, 70.0), (24.0, 27.0, 34.0), fy / s);
            col = mix(col, border_color, border);
            // Töltősáv.
            if level > 0.0 {
                let in_x = (fx - inset + 0.5).clamp(0.0, 1.0) * ((s - inset) - fx + 0.5).clamp(0.0, 1.0);
                let in_y = (fy - fill_top + 0.5).clamp(0.0, 1.0) * (inner_bottom - fy + 0.5).clamp(0.0, 1.0);
                let cov = in_x * in_y;
                if cov > 0.0 {
                    // A sáv teteje világosabb (finom fénypont), az alja a tiszta szín.
                    let t = ((fy - fill_top) / fill_h.max(1.0)).clamp(0.0, 1.0);
                    let lit = mix((255.0, 255.0, 255.0), fill_color, 0.55 + 0.45 * t);
                    col = mix(col, lit, cov);
                }
            }
            let a = (outer * 255.0).round() as u32;
            let r = col.0.clamp(0.0, 255.0) as u32;
            let g = col.1.clamp(0.0, 255.0) as u32;
            let b = col.2.clamp(0.0, 255.0) as u32;
            px[y * size + x] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    px
}

/// Létrehozza az ikont (HICON); 0, ha nem sikerült. A hívó felelős a `DestroyIcon`-ért.
unsafe fn make_icon(size: i32, cpu: f32, color: (f32, f32, f32)) -> isize {
    let px = render_pixels(size as usize, cpu, color);
    let bmi = BitmapInfo {
        header: BitmapInfoHeader {
            size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            width: size,
            height: -size, // felülről lefelé
            planes: 1,
            bit_count: 32,
            compression: 0, // BI_RGB
            size_image: 0,
            x_pels_per_meter: 0,
            y_pels_per_meter: 0,
            clr_used: 0,
            clr_important: 0,
        },
        colors: [0],
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let hdc = GetDC(0);
    let dib = CreateDIBSection(hdc, &bmi, 0, &mut bits, 0, 0); // DIB_RGB_COLORS
    ReleaseDC(0, hdc);
    if dib == 0 || bits.is_null() {
        return 0;
    }
    std::ptr::copy_nonoverlapping(px.as_ptr(), bits.cast::<u32>(), px.len());
    // A maszk csupa nulla (az alfa csatorna dönt); a sorok 16 bitre igazítottak.
    let stride = (((size + 15) / 16) * 2) as usize;
    let zeros = vec![0u8; stride * size as usize];
    let mask = CreateBitmap(size, size, 1, 1, zeros.as_ptr().cast());
    let info = IconInfo { f_icon: 1, x_hotspot: 0, y_hotspot: 0, hbm_mask: mask, hbm_color: dib };
    let icon = CreateIconIndirect(&info);
    DeleteObject(dib);
    if mask != 0 {
        DeleteObject(mask);
    }
    icon
}
