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
//! Task Manager / Hardware monitor – Rust, egui + eframe, Windows 11.
//!
//! ARCHITEKTÚRA RÖVIDEN
//! ─────────────────────
//! * Az UI-szál SEMMIT nem vár: a hardveres lekérdezések egy külön `hw-monitor`
//!   háttérszálon futnak, és másodpercenként egy `Snapshot`-ot tesznek egy közös
//!   "legutóbbi érték" tárolóba (Mutex<Option<Snapshot>>). Az UI ezt `try_lock`-kal
//!   olvassa, tehát akkor sem blokkolhat, ha a háttérszál egy lefagyott drivert vár.
//!   (Szándékosan nem végtelen csatorna: ha az ablak le van kicsinyítve és nem rajzolunk,
//!   a régi snapshotok nem halmozódnak fel a memóriában.)
//! * A ritka események (üdvözlés, kilövés eredménye, figyelmeztetés) egy kis korlátos
//!   csatornán mennek; az UI -> háttérszál irány (Kill parancs) szintén csatorna.
//! * A Szolgáltatások és az Indítási alkalmazások adatait egy harmadik, `sys-lists` szál frissíti (lists.rs):
//!   csak a látható fülhöz dolgozik, így a lassú SCM-/registry-hívások sosem fékezik a mintavételt.
//! * A mintavételezési idő (0,25–5 s) és a grafikon időablaka (30–120 s) a Beállításokból futás közben állítható;
//!   az értékek a settings.json-ban maradnak meg (%APPDATA%\Neo Softwares\Task Manager).
//! * Az alkalmazás-ikonok betöltése egy KÜLÖN, alacsony prioritású `icon-loader` szálon fut
//!   (lassú lehet), és csak a ténylegesen látható sorokhoz kérünk ikont.
//! * A háttérszálban minden gyűjtés `catch_unwind` alatt fut, így egy panikoló
//!   hardverlekérdező nem viszi el a programot. (Ezért nincs `panic = "abort"` a Cargo.toml-ban.)
//! * A folyamat magasabb prioritást kap (ABOVE_NORMAL), a háttérszálak alacsonyabbat.
//! * Semmi nem függ az explorer.exe-től: csak egy saját ablak, DWM, PDH, DXGI és Win32 hívások.
//!
//! GYÁRTÓFÜGGETLEN RÉTEG
//! ─────────────────────
//! * CPU: indításkor CPUID-ból felismerjük a gyártót -> `CpuBackend` trait:
//!   `AmdCpuBackend` (működő), `IntelCpuBackend` (csonk a hőmérséklethez), `GenericCpuBackend`.
//! * GPU: DXGI-n végigmegyünk az adaptereken (PCI vendor ID) -> `GpuBackend` trait:
//!   `AmdGpuBackend` (PDH + DXGI + WDDM-perf + ADL), `NvidiaGpuBackend` (NVML-csonk),
//!   `GenericGpuBackend` (Intel és bármi más). A hőfok/órajel/ventilátor alapforrása minden
//!   gyártónál a WDDM adapter-perf lekérdezés (D3DKMT) – ugyanaz, amit a Task Manager is használ.
//!
//! FONTOS, ŐSZINTE KORLÁT: a CPU-hőfok Windows alatt kernel-driver (PawnIO / WinRing0 /
//! LibreHardwareMonitor) nélkül NEM olvasható ki. Ráadásul az AMD Ryzen processzorokban
//! fizikailag nincs magonkénti hőszenzor, csak csomag-hőfok (Tctl/Tdie) és CCD-hőfok. A WMI/PDH
//! csak ACPI hőzónákat ad. Ezért a hőmérséklet-forrás egy láncolt `CpuTempSource` trait,
//! amibe a későbbi driveres olvasó beköthető (`AmdSmnTempSource`, `IntelMsrTempSource`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
compile_error!("Ez az alkalmazás kizárólag Windowsra készült (DWM, PDH, DXGI, TerminateProcess).");

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::os::raw::c_int;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{
    self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError,
};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, ColorImage, CornerRadius, FontId, Layout, Margin,
    Pos2, Rect, RichText, Sense, Shape, Stroke, TextureHandle, TextureOptions, Ui, UiBuilder,
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcess, SetPriorityClass, SetThreadPriority,
    TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS, PROCESS_TERMINATE, THREAD_PRIORITY_BELOW_NORMAL,
};

mod about;
mod apphistory;
mod backdrop;
mod i18n;
mod lists;
mod menus;
mod netmon;
mod tray;
mod services;
mod shell;
mod settings;
mod startup;
mod sysdata;
mod tabs;
mod theme;
mod winreg;

use apphistory::{AppHistory, AppHistoryTracker};
use netmon::NetState;

use i18n::{tr, trf};
use settings::{ColPrefs, Settings, ThemePref};
use sysdata::{
    cpu_topology, enable_debug_privilege, is_elevated, list_sessions, load_user_avatar, relaunch_elevated, scan_windows,
    is_metered, poll_metered, private_working_sets, session_of_pid, DiskReader, DiskStatic, DiskTelemetry, NetReader, NetTelemetry, ProcMetaCache, ProcPdh,
    SessionInfo,
};
use theme::{accent, col_muted, col_text, graph_bg, overlay};

// ═══════════════════════════════════════════════════════════════════════════
// 0. Közvetlen (nyers) Win32 FFI – ikonok, SMBIOS. Saját, C-kompatibilis struktúrákkal,
//    hogy ne függjünk a `windows` crate feature-készletétől és a szignatúráitól.
// ═══════════════════════════════════════════════════════════════════════════

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
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct BitmapObj {
    bm_type: i32,
    bm_width: i32,
    bm_height: i32,
    bm_width_bytes: i32,
    bm_planes: u16,
    bm_bits_pixel: u16,
    bm_bits: *mut c_void,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct BitmapInfo {
    header: BitmapInfoHeader,
    colors: [u32; 1],
}

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemFirmwareTable(sig: u32, id: u32, buf: *mut u8, size: u32) -> u32;
}
#[link(name = "shell32")]
extern "system" {
    fn ExtractIconExW(
        file: *const u16,
        index: i32,
        large: *mut isize,
        small: *mut isize,
        count: u32,
    ) -> u32;
}
#[link(name = "user32")]
extern "system" {
    fn GetIconInfo(hicon: isize, info: *mut IconInfo) -> i32;
    fn DestroyIcon(hicon: isize) -> i32;
}
#[link(name = "gdi32")]
extern "system" {
    fn GetObjectW(h: isize, c: i32, pv: *mut c_void) -> i32;
    fn CreateCompatibleDC(hdc: isize) -> isize;
    fn GetDIBits(
        hdc: isize,
        hbm: isize,
        start: u32,
        lines: u32,
        bits: *mut c_void,
        bmi: *mut BitmapInfo,
        usage: u32,
    ) -> i32;
    fn DeleteDC(hdc: isize) -> i32;
    fn DeleteObject(h: isize) -> i32;
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Színek, méretek, stílus
// ═══════════════════════════════════════════════════════════════════════════

// Adatszín (alacsony kihasználtság, lemez-olvasás, hálózati küldés): rögzített kék. A KIJELÖLÉSEK (aktív fül, kijelölt
// sor, csúszkák) színe NEM ez, hanem a Windows kiemelő színe: `theme::accent()`.
const DATA_BLUE: Color32 = Color32::from_rgb(0, 120, 212);
const OK_GREEN: Color32 = Color32::from_rgb(76, 194, 120);
const WARN_AMBER: Color32 = Color32::from_rgb(245, 183, 64);
const DANGER_RED: Color32 = Color32::from_rgb(232, 76, 76);
const CPU_COLOR: Color32 = Color32::from_rgb(96, 165, 250);
const MEM_COLOR: Color32 = Color32::from_rgb(167, 139, 250);
const GPU_COLOR: Color32 = Color32::from_rgb(52, 211, 153);
const DISK_COLOR: Color32 = Color32::from_rgb(251, 146, 60);
const NET_COLOR: Color32 = Color32::from_rgb(244, 114, 182);

const NAV_WIDTH: f32 = 236.0;
/// A legkisebb ablakméret (kliensterület): ennél kisebbre nem húzható. Ezen a méreten a folyamattábla vízszintes
/// görgetés nélkül, a Teljesítmény fül részletei 2 oszlopban kényelmesen elférnek, a bal menü pedig mind az
/// elemét (alsó gombokkal és állapotsorral együtt) átfedés nélkül kiírja.
const MIN_WINDOW_SIZE: [f32; 2] = [1000.0, 640.0];
/// Egy oszlop legkisebb / legnagyobb szélessége képpontban a húzásos átméretezéskor.
const COL_MIN_W: f32 = 50.0;
const COL_MAX_W: f32 = 3000.0;
/// A Név oszlop szélességének helye a `ColPrefs::widths` vektorban (az adatoszlopok indexe 0..15), és a legkisebb szélessége.
const NAME_SLOT: usize = 20;
const MIN_NAME_W: f32 = 280.0;

thread_local! {
    /// Az éppen kirajzolt sor "…"-tal rövidített celláinak (téglalap, teljes szöveg) listája: ha az egér ilyen cella fölé ér,
    /// a teljes szöveg buborékban megjelenik.
    static TRUNC: std::cell::RefCell<Vec<(Rect, String)>> = std::cell::RefCell::new(Vec::new());
}

fn trunc_clear() {
    TRUNC.with(|t| t.borrow_mut().clear());
}

fn trunc_push(rect: Rect, text: &str) {
    TRUNC.with(|t| t.borrow_mut().push((rect, text.to_string())));
}

/// Ha az egér a sor egy rövidített ("…") cellája fölött van, buborékban megjeleníti a teljes szöveget.
fn trunc_tooltip(ctx: &egui::Context, resp: &egui::Response) {
    if !resp.hovered() {
        return;
    }
    let Some(pos) = ctx.pointer_hover_pos() else { return };
    let hit = TRUNC.with(|t| t.borrow().iter().find(|(r, _)| r.contains(pos)).map(|(_, text)| text.clone()));
    if let Some(full) = hit {
        resp.clone().on_hover_text(full);
    }
}

/// A tálcaikon-beállításból a megjelenítendő ikonok bitmaszkja (1 CPU, 2 memória, 4 GPU).
fn tray_mask(mode: u8) -> u32 {
    match mode {
        1 => 1,
        2 => 7,
        _ => 0,
    }
}
/// A Folyamatok fül kéri a parancssor olvasását (csak akkor drága, ha az oszlop látszik): a UI állítja, a mintavevő olvassa.
static WANT_CMDLINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Az Indítási alkalmazások fül kéri a futó folyamatok összes lemez-I/O-jának olvasását (csak ha az oszlop látszik).
static WANT_IO_TOTAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A program ikonja (128×128 PNG, a tálcához, a címsorhoz és a bal sáv fejlécéhez). Forrás: assets/icon.png.
const APP_ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Az ikon dekódolása nyers RGBA-ra.
fn load_icon_data() -> Option<egui::IconData> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}
/// A grafikon-előzmények hossza (minták száma) és időablaka másodpercben. A Beállítások csúszkái állítják
/// (lásd `Settings::history_capacity` és `TaskApp::apply_runtime_settings`); globális, mint a téma jelzője.
static HIST_CAP: AtomicUsize = AtomicUsize::new(61);
static HIST_WINDOW_SECS: AtomicU32 = AtomicU32::new(60);

fn hist_cap() -> usize {
    HIST_CAP.load(AtomicOrdering::Relaxed).max(2)
}

fn set_hist_params(cap: usize, window_secs: f32) {
    HIST_CAP.store(cap.max(2), AtomicOrdering::Relaxed);
    HIST_WINDOW_SECS.store(window_secs.round().max(1.0) as u32, AtomicOrdering::Relaxed);
}

/// "Kihasználtság % – az utolsó N másodperc" felirat az aktuális időablakkal.
fn window_caption() -> String {
    let secs = HIST_WINDOW_SECS.load(AtomicOrdering::Relaxed);
    trf("Utilization % – last {0} seconds", &[&secs])
}
const ROW_H: f32 = 30.0;

/// Az egui "vizuális stílusa" sötét vagy világos témához: lekerekített sarkok, finom áttetsző gombok.
fn build_visuals(dark: bool) -> egui::Visuals {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let ink = theme::text_primary(dark);
    // Finom "fátyol": sötét témában fehér, világosban fekete (az érték az erősség).
    let ov = |a: u8| {
        if dark {
            Color32::from_rgba_premultiplied(a, a, a, a)
        } else {
            Color32::from_black_alpha((a as f32 * 0.55) as u8)
        }
    };
    v.panel_fill = theme::app_bg(dark);
    v.window_fill = theme::window_bg(dark);
    v.extreme_bg_color = if dark { Color32::from_black_alpha(70) } else { Color32::from_rgb(255, 255, 255) };
    v.faint_bg_color = ov(6);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = egui::Shadow::NONE;
    v.popup_shadow = egui::Shadow::NONE;
    // A kijelölés színe a Windows kiemelő színe (lásd `theme::poll_accent`: változáskor az `apply_style` újrafut).
    let acc = accent();
    v.selection.bg_fill = acc.gamma_multiply(if dark { 0.55 } else { 0.35 });
    v.selection.stroke = Stroke::new(1.0_f32, acc);

    for wv in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        wv.corner_radius = CornerRadius::same(8);
    }
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, ink);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, ov(14));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, ink);
    v.widgets.inactive.bg_fill = ov(14);
    v.widgets.inactive.weak_bg_fill = ov(14);
    v.widgets.hovered.weak_bg_fill = ov(28);
    v.widgets.hovered.bg_fill = ov(28);
    v.widgets.active.weak_bg_fill = ov(40);
    v.widgets.active.bg_fill = ov(40);
    v
}

/// Mindkét téma stílusát beállítja; hogy éppen melyik él, azt a téma-beállítás dönti el.
fn apply_style(ctx: &egui::Context) {
    for (theme, dark) in [(egui::Theme::Dark, true), (egui::Theme::Light, false)] {
        ctx.style_mut_of(theme, |s| {
            s.visuals = build_visuals(dark);
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.button_padding = vec2(14.0, 7.0);
        });
    }
}

fn apply_theme_pref(ctx: &egui::Context, pref: ThemePref) {
    ctx.set_theme(match pref {
        ThemePref::System => egui::ThemePreference::System,
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Adatmodell (ezt küldi a háttérszál az UI-nak)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CpuVendor {
    Amd,
    Intel,
    Other,
}

impl CpuVendor {
    fn label(self) -> &'static str {
        match self {
            Self::Amd => "AMD",
            Self::Intel => "Intel",
            Self::Other => tr("Unknown"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
    Other,
}

impl GpuVendor {
    /// PCI vendor ID -> gyártó (a DXGI adapter leírásából).
    fn from_pci_vendor_id(id: u32) -> Self {
        match id {
            0x1002 => Self::Amd,
            0x10DE => Self::Nvidia,
            0x8086 => Self::Intel,
            _ => Self::Other,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Amd => "AMD",
            Self::Nvidia => "NVIDIA",
            Self::Intel => "Intel",
            Self::Other => tr("Unknown"),
        }
    }
}

/// Indításkor egyszer felismert, nem változó adatok.
#[derive(Clone, Debug)]
struct CpuStatic {
    vendor: CpuVendor,
    brand: String,
    logical_cores: usize,
    /// Foglalatok, magok, gyorsítótárak, alapsebesség, virtualizáció.
    topo: sysdata::CpuTopology,
}

#[derive(Clone, Debug)]
struct GpuStatic {
    vendor: GpuVendor,
    name: String,
    vram_total_mb: f64,
    /// A DXGI LUID kisbetűs szöveges kulcsa, pl. "luid_0x00000000_0x0000e5a5".
    /// Ez köti össze a DXGI adaptert a PDH számláló-példányokkal.
    luid_key: String,
    luid_low: u32,
    luid_high: i32,
}

/// A memóriamodulok statikus adatai (SMBIOS + Win32).
#[derive(Clone, Debug, Default)]
struct MemStatic {
    installed_mb: f64,
    kind: String,       // "DDR4"
    form_factor: String, // "DIMM"
    speed_mts: Option<u32>,
    slots_used: u32,
    slots_total: u32,
    hw_reserved_mb: Option<f64>,
}

#[derive(Clone, Debug)]
struct StaticInfo {
    cpu: CpuStatic,
    mem: MemStatic,
    gpus: Vec<GpuStatic>,
    disks: Vec<DiskStatic>,
}

#[derive(Clone, Debug, Default)]
struct TempReading {
    label: String,
    celsius: f32,
}

#[derive(Clone, Debug, Default)]
struct CpuTelemetry {
    usage_total: f32,
    per_core_usage: Vec<f32>,
    freq_mhz: Option<f32>,
    temps: Vec<TempReading>,
    /// Összesített CPU-hőfok (csomag / Tctl), ha van; különben a mért értékek maximuma.
    temp_package: Option<f32>,
    /// Őszinte forrás-megjelölés (honnan jött a hőmérséklet).
    temp_source: String,
}

#[derive(Clone, Debug, Default)]
struct MemTelemetry {
    total_mb: f64,
    used_mb: f64,
    available_mb: f64,
    compressed_mb: Option<f64>,
    commit_used_mb: f64,
    commit_limit_mb: f64,
    cached_mb: f64,
    paged_pool_mb: f64,
    nonpaged_pool_mb: f64,
}

#[derive(Clone, Debug, Default)]
struct GpuTelemetry {
    utilization: Option<f32>,
    vram_used_mb: Option<f64>,
    vram_total_mb: f64,
    temp_c: Option<f32>,
    hotspot_c: Option<f32>,
    core_clock_mhz: Option<f32>,
    mem_clock_mhz: Option<f32>,
    power_w: Option<f32>,
    /// A WDDM a fogyasztást a TDP százalékában adja (nem wattban).
    power_tdp_pct: Option<f32>,
    fan_rpm: Option<f32>,
    fan_percent: Option<f32>,
    /// Állapot/korlát szöveg az UI-nak (pl. "ADL nem elérhető").
    note: String,
    /// A motorok kihasználtsága %-ban: 3D, Copy, Video Encode, Video Decode (a motoronkénti nézethez).
    engines: [f32; 4],
}

#[derive(Clone, Debug)]
struct ProcInfo {
    pid: u32,
    name: String,
    name_lc: String, // előre kisbetűsítve a gyors kereséshez és csoportosításhoz
    exe: Option<String>,
    mem_mb: f64,
    cpu: f32,
    gpu: f32,
    /// A legterheltebb GPU-motor neve (pl. "GPU 0 - 3D"); üres, ha a folyamat nem használja a GPU-t.
    gpu_engine: String,
    /// Az összes I/O (fájl, eszköz, hálózat) bájt/másodperc.
    io_bps: f64,
    /// Az összes hálózati forgalom bájt/másodperc (ETW); 0, ha a mérés nem fut.
    net_bps: f64,
    /// A hálózati forgalom feltöltött / letöltött része bájt/másodpercben.
    net_up_bps: f64,
    net_down_bps: f64,
    /// A gyártó (verzióinformáció); üres, ha nem ismert.
    publisher: String,
    /// A parancssor (csak ha a Folyamatok fül Parancssor oszlopa látszik; különben üres).
    cmd: String,
    /// Becsült energiafogyasztás-pontszám (CPU, GPU, lemez és hálózat súlyozott összege) és ennek ~2 perces mozgó átlaga.
    power: f32,
    power_trend: f32,
    /// A folyamat indulása óta végzett összes lemez-I/O bájtban (csak ha az Indítási alkalmazások fül kéri; különben 0).
    io_total: u64,
    /// Munkamenet-azonosító: 0 = szolgáltatások, 1+ = interaktív felhasználók.
    session_id: u32,
    /// A teljes munkakészlet (a megosztott lapokkal együtt) – összehasonlításhoz a privát értékkel.
    ws_mb: f64,
    /// A folyamat tulajdonosa (felhasználó, SYSTEM, helyi szolgáltatás...); üres, ha nem kérdezhető le.
    user: String,
    /// "x64" / "x86" / "ARM64"; üres, ha nem kérdezhető le.
    arch: &'static str,
    /// Az exe fájlleírása (verzióinformáció).
    desc: String,
    /// 0 = fut, 1 = nem válaszol.
    status: u8,
    /// Van-e látható, felső szintű ablaka (ez különbözteti meg az "alkalmazást" a háttérfolyamattól).
    has_window: bool,
    /// A folyamat kumulált processzorideje ezredmásodpercben (Alkalmazáselőzmények, indítási hatás becslése).
    cpu_time_ms: u64,
    /// A szülő folyamat PID-je (0, ha ismeretlen) – a "Folyamatstruktúra leállítása" menüponthoz.
    parent_pid: u32,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    cpu: CpuTelemetry,
    mem: MemTelemetry,
    gpus: Vec<GpuTelemetry>,
    disks: Vec<DiskTelemetry>,
    nets: Vec<NetTelemetry>,
    sessions: Vec<SessionInfo>,
    uptime_secs: u64,
    process_count: usize,
    processes: Vec<ProcInfo>,
    /// Alkalmazásonkénti előzmények (a háttérszál tölti ki a gyűjtés után).
    app_history: AppHistory,
    /// A folyamatonkénti hálózati mérés (ETW) állapota.
    net_state: NetState,
}

/// Háttérszál -> UI ritka események.
enum Event {
    Hello(StaticInfo),
    KillResult { label: String, killed: usize, errors: Vec<String> },
    Warning(String),
    /// A felhasználó profilképe (96×96 RGBA) – `None`, ha nem található.
    UserPicture { user: String, image: Option<(usize, usize, Vec<u8>)> },
}

/// UI -> háttérszál parancsok.
enum Command {
    /// Egy vagy több folyamat kilövése (egy csoport összes tagja).
    Kill { label: String, targets: Vec<(u32, String)> },
    /// Az Alkalmazáselőzmények számlálóinak nullázása.
    ResetAppHistory,
}

/// Az ikon-szál válasza: az exe elérési útja + RGBA kép (ha sikerült).
struct IconMsg {
    path: String,
    image: Option<(usize, usize, Vec<u8>)>,
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. PDH (Performance Data Helper) – vékony, biztonságosabb burkoló
//    A Task Manager is ugyanezekből a számlálókból dolgozik.
// ═══════════════════════════════════════════════════════════════════════════

struct PdhQuery {
    query: PDH_HQUERY,
    /// u64-szavakból áll, hogy a PDH struktúrák 8 bájtos igazítása biztosan teljesüljön.
    buffer: Vec<u64>,
}

impl PdhQuery {
    fn open() -> Option<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: a `query` érvényes kimeneti paraméter.
        let rc = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
        (rc == 0).then(|| Self { query, buffer: Vec::new() })
    }

    /// Számláló hozzáadása ANGOL névvel, így lokalizált Windowson is működik.
    fn add(&self, path: PCWSTR) -> Option<PDH_HCOUNTER> {
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: a lekérdezés él, a path statikus széles string (`w!`).
        let rc = unsafe { PdhAddEnglishCounterW(self.query, path, 0, &mut counter) };
        (rc == 0).then_some(counter)
    }

    fn collect(&self) -> bool {
        // SAFETY: a lekérdezés él.
        unsafe { PdhCollectQueryData(self.query) == 0 }
    }

    /// Egy wildcard (`(*)`) számláló összes példánya: (példánynév, érték).
    fn read_array(&mut self, counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for _attempt in 0..3 {
            let mut bytes: u32 = 0;
            let mut count: u32 = 0;
            // 1) méret lekérdezése (puffer nélkül PDH_MORE_DATA jön vissza)
            // SAFETY: nincs puffer, csak a méretet kérjük.
            let rc = unsafe {
                PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut bytes, &mut count, None)
            };
            if rc != PDH_MORE_DATA || bytes == 0 {
                return out;
            }
            self.buffer.clear();
            self.buffer.resize((bytes as usize).div_ceil(8), 0);
            // 2) tényleges olvasás
            // SAFETY: a puffer legalább `bytes` bájt, 8 bájtra igazított.
            let rc = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    Some(self.buffer.as_mut_ptr().cast()),
                )
            };
            if rc == PDH_MORE_DATA {
                continue; // közben nőtt a példányok száma, újrapróbáljuk
            }
            if rc != 0 {
                return out;
            }
            // SAFETY: a PDH `count` darab elemet írt a puffer elejére.
            let items = unsafe {
                std::slice::from_raw_parts(
                    self.buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                    count as usize,
                )
            };
            for it in items {
                let status = it.FmtValue.CStatus;
                if status != 0 && status != 1 {
                    continue; // 0 = valid, 1 = új adat; minden más érvénytelen
                }
                // SAFETY: a név a puffer farkában van, az érték `f64` (PDH_FMT_DOUBLE).
                if let Ok(name) = unsafe { it.szName.to_string() } {
                    out.push((name, unsafe { it.FmtValue.Anonymous.doubleValue }));
                }
            }
            return out;
        }
        out
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        // SAFETY: egyszer nyitottuk meg; a bezárás a számlálókat is felszabadítja.
        unsafe { PdhCloseQuery(self.query) };
    }
}

fn find_instance(v: &[(String, f64)], name: &str) -> Option<f64> {
    v.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, x)| *x)
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Gyártófelismerés
// ═══════════════════════════════════════════════════════════════════════════

/// CPUID 0. leaf: a 12 karakteres gyártó-azonosító ("AuthenticAMD" / "GenuineIntel").
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn cpuid_vendor_string() -> Option<String> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid;
    // SAFETY: a CPUID 0. leaf minden x86 processzoron létezik.
    #[allow(unused_unsafe)]
    let r = unsafe { __cpuid(0) };
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    b[4..8].copy_from_slice(&r.edx.to_le_bytes());
    b[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    Some(String::from_utf8_lossy(&b).into_owned())
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn cpuid_vendor_string() -> Option<String> {
    None
}

fn detect_cpu_vendor(sys: &System) -> CpuVendor {
    let id = cpuid_vendor_string()
        .or_else(|| sys.cpus().first().map(|c| c.vendor_id().to_string()))
        .unwrap_or_default();
    match id.as_str() {
        "AuthenticAMD" => CpuVendor::Amd,
        "GenuineIntel" => CpuVendor::Intel,
        _ => CpuVendor::Other,
    }
}

/// DXGI-val végigmegyünk a (nem szoftveres) videókártyákon.
fn enumerate_adapters() -> Vec<GpuStatic> {
    let mut out = Vec::new();
    // SAFETY: a DXGI gyári objektum létrehozása; a COM-objektumok a függvényen belül élnek.
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
        Ok(f) => f,
        Err(_) => return out,
    };
    // Ugyanaz a fizikai kártya ne szerepeljen kétszer (egyes rendszereken a DXGI két adaptert ad ugyanarra az eszközre):
    // a PCI-cím azonosítja az eszközt; ha az nem kérdezhető le, a gyártó/eszköz/név/VRAM együttes egyezése dönt.
    #[allow(clippy::type_complexity)]
    let mut seen: Vec<(Option<(u32, u32, u32)>, (u32, u32, u32, u32, String, usize))> = Vec::new();
    let mut index = 0u32;
    loop {
        // SAFETY: az index-szel való felsorolás a DXGI szokásos használata.
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(_) => break, // DXGI_ERROR_NOT_FOUND = vége a listának
        };
        index += 1;
        // SAFETY: az adapter érvényes COM-objektum.
        let desc = match unsafe { adapter.GetDesc1() } {
            Ok(d) => d,
            Err(_) => continue,
        };
        // 2 = DXGI_ADAPTER_FLAG_SOFTWARE, 0x1414 = Microsoft Basic Render Driver
        if (desc.Flags & 2) != 0 || desc.VendorId == 0x1414 {
            continue;
        }
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]).trim().to_string();
        let addr = KmtAdapter::open(desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart).and_then(|k| k.pci_address());
        let key = (desc.VendorId, desc.DeviceId, desc.SubSysId, desc.Revision, name.clone(), desc.DedicatedVideoMemory);
        let duplicate = seen.iter().any(|(a, k)| match (a, &addr) {
            (Some(x), Some(y)) => x == y,
            _ => *k == key,
        });
        if duplicate {
            continue;
        }
        seen.push((addr, key));
        out.push(GpuStatic {
            vendor: GpuVendor::from_pci_vendor_id(desc.VendorId),
            name,
            vram_total_mb: desc.DedicatedVideoMemory as f64 / 1_048_576.0,
            luid_key: format!(
                "luid_0x{:08x}_0x{:08x}",
                desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
            ),
            luid_low: desc.AdapterLuid.LowPart,
            luid_high: desc.AdapterLuid.HighPart,
        });
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Memória: SMBIOS (sebesség, foglalatok, típus) + Win32 teljesítményadatok
// ═══════════════════════════════════════════════════════════════════════════

/// SMBIOS 17-es típus (Memory Device) – a memóriamodulok leírása. A nyers táblát a
/// GetSystemFirmwareTable('RSMB') adja; a mezőoffszetek az SMBIOS specifikációból valók.
fn read_smbios_memory() -> MemStatic {
    let mut m = MemStatic::default();
    let sig = u32::from_be_bytes(*b"RSMB");
    // SAFETY: első hívás puffer nélkül: csak a szükséges méretet adja vissza.
    let size = unsafe { GetSystemFirmwareTable(sig, 0, std::ptr::null_mut(), 0) };
    if size < 8 {
        return m;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: a puffer pontosan `size` bájt.
    let got = unsafe { GetSystemFirmwareTable(sig, 0, buf.as_mut_ptr(), size) } as usize;
    if got < 8 || got > buf.len() {
        return m;
    }
    buf.truncate(got);
    // RawSMBIOSData fejléc: 1+1+1+1 bájt, majd a tábla hossza (u32), utána maguk a struktúrák.
    let data_len = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let table = &buf[8..(8 + data_len).min(buf.len())];

    let mut speed: Option<u32> = None;
    let mut configured: Option<u32> = None;
    let mut mem_type = 0u8;
    let mut form = 0u8;
    let mut i = 0usize;
    while i + 4 <= table.len() {
        let kind = table[i];
        let len = table[i + 1] as usize;
        if len < 4 || i + len > table.len() {
            break;
        }
        let f = &table[i..i + len];
        // A formázott részt a "szöveghalmaz" követi, amit két egymást követő 0 bájt zár.
        let mut j = i + len;
        while j + 1 < table.len() && !(table[j] == 0 && table[j + 1] == 0) {
            j += 1;
        }
        i = j + 2;
        if kind == 127 {
            break; // End-of-table
        }
        if kind != 17 {
            continue;
        }
        let rd16 = |off: usize| -> u16 {
            f.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
        };
        let rd32 = |off: usize| -> u32 {
            f.get(off..off + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .unwrap_or(0)
        };
        m.slots_total += 1;
        let raw = rd16(0x0C);
        let size_mb: u64 = match raw {
            0 | 0xFFFF => 0,
            0x7FFF => rd32(0x1C) as u64, // kiterjesztett méret (MB)
            r if r & 0x8000 != 0 => ((r & 0x7FFF) as u64) / 1024, // KB egységben
            r => r as u64,
        };
        if size_mb == 0 {
            continue; // üres foglalat
        }
        m.slots_used += 1;
        if form == 0 {
            form = f.get(0x0E).copied().unwrap_or(0);
        }
        if mem_type == 0 {
            mem_type = f.get(0x12).copied().unwrap_or(0);
        }
        let sp = rd16(0x15) as u32;
        let cfg = rd16(0x20) as u32;
        if speed.is_none() && sp > 0 && sp != 0xFFFF {
            speed = Some(sp);
        }
        if configured.is_none() && cfg > 0 && cfg != 0xFFFF {
            configured = Some(cfg);
        }
    }
    m.speed_mts = configured.or(speed); // a Task Manager is a beállított sebességet mutatja
    m.kind = match mem_type {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    }
    .to_string();
    m.form_factor = match form {
        0x09 => "DIMM",
        0x0D => "SODIMM",
        0x0F => "FB-DIMM",
        0x0B => "RIMM",
        _ => "",
    }
    .to_string();
    m
}

/// PERFORMANCE_INFORMATION (psapi.h) – az értékek lapokban értendők.
#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default)]
struct PerformanceInformation {
    cb: u32,
    commit_total: usize,
    commit_limit: usize,
    commit_peak: usize,
    physical_total: usize,
    physical_available: usize,
    system_cache: usize,
    kernel_total: usize,
    kernel_paged: usize,
    kernel_nonpaged: usize,
    page_size: usize,
    handle_count: u32,
    process_count: u32,
    thread_count: u32,
}

type FnPerfInfo = unsafe extern "system" fn(*mut PerformanceInformation, u32) -> i32;
type FnInstalledMem = unsafe extern "system" fn(*mut u64) -> i32;

/// A kernel32-ből futásidőben feloldott memória-függvények (nincs link-idejű függés).
struct MemReader {
    _lib: Option<libloading::Library>,
    perf: Option<FnPerfInfo>,
    installed: Option<FnInstalledMem>,
}

impl MemReader {
    fn new() -> Self {
        // SAFETY: a kernel32 mindig be van töltve; a szimbólumok típusa a Win32 fejlécekkel egyezik.
        unsafe {
            match libloading::Library::new("kernel32.dll") {
                Ok(lib) => {
                    let perf = lib.get::<FnPerfInfo>(b"K32GetPerformanceInfo\0").ok().map(|s| *s);
                    let installed = lib
                        .get::<FnInstalledMem>(b"GetPhysicallyInstalledSystemMemory\0")
                        .ok()
                        .map(|s| *s);
                    Self { _lib: Some(lib), perf, installed }
                }
                Err(_) => Self { _lib: None, perf: None, installed: None },
            }
        }
    }

    /// A fizikailag beépített RAM (a Windowsnak látható összeg + a hardverek számára fenntartott rész).
    fn installed_mb(&self) -> Option<f64> {
        let f = self.installed?;
        let mut kb: u64 = 0;
        // SAFETY: érvényes kimeneti mutató.
        let ok = unsafe { f(&mut kb) };
        (ok != 0 && kb > 0).then(|| kb as f64 / 1024.0)
    }

    fn sample(&self, sys: &System, compressed_mb: Option<f64>) -> MemTelemetry {
        let mb = |b: u64| b as f64 / 1_048_576.0;
        let total = mb(sys.total_memory());
        let avail = mb(sys.available_memory());
        let mut m = MemTelemetry {
            total_mb: total,
            available_mb: avail,
            used_mb: (total - avail).max(0.0),
            compressed_mb,
            ..Default::default()
        };
        if let Some(f) = self.perf {
            let mut pi = PerformanceInformation::default();
            pi.cb = std::mem::size_of::<PerformanceInformation>() as u32;
            let cb = pi.cb;
            // SAFETY: a struktúra mérete és elrendezése a psapi.h-val egyezik.
            if unsafe { f(&mut pi, cb) } != 0 {
                let ps = pi.page_size as f64;
                let pages = |p: usize| p as f64 * ps / 1_048_576.0;
                m.commit_used_mb = pages(pi.commit_total);
                m.commit_limit_mb = pages(pi.commit_limit);
                m.cached_mb = pages(pi.system_cache);
                m.paged_pool_mb = pages(pi.kernel_paged);
                m.nonpaged_pool_mb = pages(pi.kernel_nonpaged);
            }
        }
        m
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. CPU-backendek
// ═══════════════════════════════════════════════════════════════════════════

struct CpuPdhSample {
    total: Option<f32>,
    cores: Vec<f32>,
    freq_mhz: Option<f32>,
}

/// A Task Managerrel megegyező számlálók: "% Processor Utility", "% Processor Performance",
/// "Processor Frequency". Az effektív órajel = alapfrekvencia × teljesítmény% (boostot is mutat).
struct CpuPdh {
    q: PdhQuery,
    utility: PDH_HCOUNTER,
    perf: PDH_HCOUNTER,
    freq: PDH_HCOUNTER,
}

impl CpuPdh {
    fn new() -> Option<Self> {
        let q = PdhQuery::open()?;
        let utility = q.add(w!("\\Processor Information(*)\\% Processor Utility"))?;
        let perf = q.add(w!("\\Processor Information(*)\\% Processor Performance"))?;
        let freq = q.add(w!("\\Processor Information(*)\\Processor Frequency"))?;
        q.collect(); // alapmérés: a ráta-számlálókhoz két minta kell
        Some(Self { q, utility, perf, freq })
    }

    fn sample(&mut self) -> Option<CpuPdhSample> {
        if !self.q.collect() {
            return None;
        }
        let util = self.q.read_array(self.utility);
        let perf = self.q.read_array(self.perf);
        let freq = self.q.read_array(self.freq);

        let mut total = None;
        let mut cores: Vec<((u32, u32), f32)> = Vec::new();
        for (name, v) in &util {
            let v = (*v as f32).clamp(0.0, 100.0);
            if name.eq_ignore_ascii_case("_Total") {
                total = Some(v);
            } else if let Some(key) = parse_core_instance(name) {
                cores.push((key, v));
            }
        }
        cores.sort_by_key(|(k, _)| *k);

        let freq_mhz = match (find_instance(&freq, "_Total"), find_instance(&perf, "_Total")) {
            (Some(base), Some(p)) if base > 0.0 && p > 0.0 => Some((base * p / 100.0) as f32),
            _ => None,
        };
        Some(CpuPdhSample { total, cores: cores.into_iter().map(|(_, v)| v).collect(), freq_mhz })
    }
}

/// "0,3" -> (csoportszám 0, mag 3). A "0,_Total" és "_Total" példányokat kihagyja.
fn parse_core_instance(name: &str) -> Option<(u32, u32)> {
    let (g, c) = name.split_once(',')?;
    Some((g.parse().ok()?, c.parse().ok()?))
}

/// Gyártófüggetlen rész: kihasználtság + órajel (PDH, tartalékként sysinfo).
struct CpuCommon {
    pdh: Option<CpuPdh>,
}

impl CpuCommon {
    fn new() -> Self {
        Self { pdh: CpuPdh::new() }
    }

    fn sample(&mut self, sys: &System) -> CpuTelemetry {
        let mut t = CpuTelemetry {
            usage_total: sys.global_cpu_usage(),
            per_core_usage: sys.cpus().iter().map(|c| c.cpu_usage()).collect(),
            ..Default::default()
        };
        let freqs: Vec<f32> =
            sys.cpus().iter().map(|c| c.frequency() as f32).filter(|f| *f > 0.0).collect();
        if !freqs.is_empty() {
            t.freq_mhz = Some(freqs.iter().sum::<f32>() / freqs.len() as f32);
        }
        if let Some(p) = self.pdh.as_mut().and_then(|p| p.sample()) {
            if let Some(tot) = p.total {
                t.usage_total = tot;
            }
            if !p.cores.is_empty() {
                t.per_core_usage = p.cores;
            }
            if let Some(f) = p.freq_mhz {
                t.freq_mhz = Some(f);
            }
        }
        t
    }
}

/// Hőmérséklet-forrás. A lánc első olyan eleme nyer, amelyik adatot ad.
trait CpuTempSource {
    fn label(&self) -> &'static str;
    /// `None` = nem támogatott / nincs adat (a lánc a következő forrásra lép).
    fn read(&mut self) -> Option<Vec<TempReading>>;
}

/// ACPI hőzónák a PDH "Thermal Zone Information" számlálón át (Kelvin). Nem kell hozzá admin,
/// de NEM magonkénti, és sok alaplapon pontatlan/konstans érték. Ez a működő tartalék.
struct AcpiThermalZones {
    q: PdhQuery,
    temp: PDH_HCOUNTER,
}

impl AcpiThermalZones {
    fn new() -> Option<Self> {
        let q = PdhQuery::open()?;
        let temp = q.add(w!("\\Thermal Zone Information(*)\\Temperature"))?;
        q.collect();
        Some(Self { q, temp })
    }
}

/// Az ACPI hőzóna nyers PDH-példánynevéből (`\_TZ.TZ00`) olvasható név: "Thermal zone 1". A számozás 1-től indul;
/// a nem számozott nevek (pl. `CPUZ`) zárójelben megmaradnak.
fn friendly_zone_name(raw: &str) -> String {
    let code = raw.trim_start_matches('\\');
    let code = code.strip_prefix("_TZ.").unwrap_or(code);
    let letters = code.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &code[letters.len()..];
    if !digits.is_empty() && matches!(letters, "TZ" | "THM" | "THRM" | "TZN" | "ZONE") {
        if let Ok(n) = digits.parse::<u32>() {
            return trf("Thermal zone {0}", &[&(n + 1)]);
        }
    }
    if code.is_empty() {
        raw.to_string()
    } else {
        format!("{} ({code})", tr("Thermal zone"))
    }
}

impl CpuTempSource for AcpiThermalZones {
    fn label(&self) -> &'static str {
        tr("ACPI thermal zone (PDH – Thermal Zone Information)")
    }
    fn read(&mut self) -> Option<Vec<TempReading>> {
        if !self.q.collect() {
            return None;
        }
        let mut out = Vec::new();
        for (name, kelvin) in self.q.read_array(self.temp) {
            let c = kelvin as f32 - 273.15;
            if (1.0..=125.0).contains(&c) {
                out.push(TempReading { label: friendly_zone_name(&name), celsius: c });
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// CSONK – AMD Ryzen Tctl/Tdie és CCD-hőmérsékletek SMN-en keresztül.
///
/// Háttér (a Linux `k10temp` driver alapján, ellenőrizd a saját CPU-családodra):
/// * A root complex PCI konfigurációs terében (busz 0, eszköz 0, funkció 0) az SMN-regisztert
///   a 0xB8 indexregiszterbe írjuk, az adatot a 0xBC-n olvassuk.
/// * Tctl: SMN cím 0x0005_9800 (THM_TCON_CUR_TMP); hőfok = (érték >> 21) × 0,125 °C;
///   ha a 19. bit (CUR_TEMP_RANGE_SEL) be van állítva, további −49 °C eltolás.
/// * CCD-nként (Zen 2+): 0x0005_9954 + 4×n, alsó 12 bit × 0,125 − 49 °C, ha a 11. bit érvényes.
/// Mindez ring-0 hozzáférést igényel (aláírt driver: PawnIO / WinRing0 stb.). Amíg nincs
/// ilyen kapcsolat, `None`-t adunk, a lánc pedig az ACPI hőzónára lép.
struct AmdSmnTempSource;

impl CpuTempSource for AmdSmnTempSource {
    fn label(&self) -> &'static str {
        "AMD SMN (Tctl / CCD)"
    }
    fn read(&mut self) -> Option<Vec<TempReading>> {
        // TODO: driver-kapcsolat + SMN olvasás (lásd a fenti leírás).
        None
    }
}

/// CSONK – Intel magonkénti hőfok MSR-ből.
///
/// Háttér: logikai magonként (SetThreadAffinityMask-kal arra a magra ugorva) olvasd az
/// IA32_THERM_STATUS (0x19C) MSR-t: a 22:16 bitek a "digitális kiolvasás" (TjMax alatti fokok),
/// a 31. bit az érvényesség. A TjMax az MSR_TEMPERATURE_TARGET (0x1A2) 23:16 bitjeiből jön.
/// Hőfok = TjMax − kiolvasás. Csomag-hőfok: IA32_PACKAGE_THERM_STATUS (0x1B1).
/// Az MSR-olvasás itt is ring-0 driverhez kötött.
struct IntelMsrTempSource;

impl CpuTempSource for IntelMsrTempSource {
    fn label(&self) -> &'static str {
        "Intel MSR (IA32_THERM_STATUS)"
    }
    fn read(&mut self) -> Option<Vec<TempReading>> {
        // TODO: driver-kapcsolat + MSR olvasás (lásd a fenti leírás).
        None
    }
}

/// A CPU-backendek közös felülete.
trait CpuBackend {
    fn vendor(&self) -> CpuVendor;
    fn sample(&mut self, sys: &System) -> CpuTelemetry;
}

/// Az összesített hőfok: a csomag / Tctl / Tdie érték, ha van; különben a mért értékek maximuma.
fn pick_package_temp(temps: &[TempReading]) -> Option<f32> {
    let named = temps.iter().find(|r| {
        let l = r.label.to_lowercase();
        (l.contains("package") || l.contains("tctl") || l.contains("tdie")) && !l.contains("ccd")
    });
    named.map(|r| r.celsius).or_else(|| {
        temps.iter().map(|r| r.celsius).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |x| x.max(v))))
    })
}

fn sample_with_temps(
    common: &mut CpuCommon,
    sources: &mut [Box<dyn CpuTempSource>],
    sys: &System,
) -> CpuTelemetry {
    let mut t = common.sample(sys);
    for src in sources.iter_mut() {
        if let Some(temps) = src.read() {
            t.temp_package = pick_package_temp(&temps);
            t.temps = temps;
            t.temp_source = src.label().to_string();
            break;
        }
    }
    t
}

/// AMD – működő ág (kihasználtság, órajel, ACPI hőzónák; SMN-csonk előkészítve).
struct AmdCpuBackend {
    common: CpuCommon,
    temp_sources: Vec<Box<dyn CpuTempSource>>,
}

impl AmdCpuBackend {
    fn new() -> Self {
        let mut temp_sources: Vec<Box<dyn CpuTempSource>> = vec![Box::new(AmdSmnTempSource)];
        if let Some(z) = AcpiThermalZones::new() {
            temp_sources.push(Box::new(z));
        }
        Self { common: CpuCommon::new(), temp_sources }
    }
}

impl CpuBackend for AmdCpuBackend {
    fn vendor(&self) -> CpuVendor {
        CpuVendor::Amd
    }
    fn sample(&mut self, sys: &System) -> CpuTelemetry {
        sample_with_temps(&mut self.common, &mut self.temp_sources, sys)
    }
}

/// Intel – a kihasználtság/órajel már működik (az gyártófüggetlen), a hőfok csonk.
struct IntelCpuBackend {
    common: CpuCommon,
    temp_sources: Vec<Box<dyn CpuTempSource>>,
}

impl IntelCpuBackend {
    fn new() -> Self {
        let mut temp_sources: Vec<Box<dyn CpuTempSource>> = vec![Box::new(IntelMsrTempSource)];
        if let Some(z) = AcpiThermalZones::new() {
            temp_sources.push(Box::new(z));
        }
        Self { common: CpuCommon::new(), temp_sources }
    }
}

impl CpuBackend for IntelCpuBackend {
    fn vendor(&self) -> CpuVendor {
        CpuVendor::Intel
    }
    fn sample(&mut self, sys: &System) -> CpuTelemetry {
        sample_with_temps(&mut self.common, &mut self.temp_sources, sys)
    }
}

/// Ismeretlen gyártó (pl. ARM): csak az általános adatok.
struct GenericCpuBackend {
    common: CpuCommon,
    temp_sources: Vec<Box<dyn CpuTempSource>>,
}

impl GenericCpuBackend {
    fn new() -> Self {
        let mut temp_sources: Vec<Box<dyn CpuTempSource>> = Vec::new();
        if let Some(z) = AcpiThermalZones::new() {
            temp_sources.push(Box::new(z));
        }
        Self { common: CpuCommon::new(), temp_sources }
    }
}

impl CpuBackend for GenericCpuBackend {
    fn vendor(&self) -> CpuVendor {
        CpuVendor::Other
    }
    fn sample(&mut self, sys: &System) -> CpuTelemetry {
        sample_with_temps(&mut self.common, &mut self.temp_sources, sys)
    }
}

fn detect_cpu_backend(vendor: CpuVendor) -> Box<dyn CpuBackend> {
    match vendor {
        CpuVendor::Amd => Box::new(AmdCpuBackend::new()),
        CpuVendor::Intel => Box::new(IntelCpuBackend::new()),
        CpuVendor::Other => Box::new(GenericCpuBackend::new()),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. GPU-backendek
// ═══════════════════════════════════════════════════════════════════════════

/// A WDDM számlálók (PDH) eredménye – gyártótól függetlenül minden adapterre.
#[derive(Default)]
struct GpuCounters {
    /// LUID-kulcs -> kihasználtság % (a legterheltebb motor-típus, ahogy a Task Manager is mutatja)
    utilization: HashMap<String, f32>,
    /// LUID-kulcs -> a rendszerszinten használt dedikált VRAM bájtban
    dedicated_used_bytes: HashMap<String, f64>,
    /// PID -> a folyamat GPU-kihasználtsága % (a legterheltebb motor)
    per_pid: HashMap<u32, f32>,
    /// PID -> a folyamat legterheltebb GPU-motorja, pl. "GPU 0 - 3D"
    engine_label: HashMap<u32, String>,
    /// LUID-kulcs -> motor-típus (kisbetűs: "3d", "copy", "videoencode", "videodecode"...) -> kihasználtság %
    engines: HashMap<String, HashMap<String, f32>>,
}

struct GpuPdh {
    q: PdhQuery,
    engine: Option<PDH_HCOUNTER>,
    mem_dedicated: Option<PDH_HCOUNTER>,
}

/// "…luid_0x00000000_0x0000e5a5_phys_0…" -> "luid_0x00000000_0x0000e5a5" (a bemenet kisbetűs).
fn luid_key(instance_lc: &str) -> Option<String> {
    let start = instance_lc.find("luid_0x")?;
    let rest = &instance_lc[start..];
    let end = rest.find("_phys").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// "pid_1234_luid_…" -> 1234
fn parse_pid(instance_lc: &str) -> Option<u32> {
    instance_lc.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

impl GpuPdh {
    fn new() -> Option<Self> {
        let q = PdhQuery::open()?;
        let engine = q.add(w!("\\GPU Engine(*)\\Utilization Percentage"));
        let mem_dedicated = q.add(w!("\\GPU Adapter Memory(*)\\Dedicated Usage"));
        if engine.is_none() && mem_dedicated.is_none() {
            return None; // régi Windows (WDDM < 2.0): nincs GPU-számláló
        }
        q.collect();
        Some(Self { q, engine, mem_dedicated })
    }

    fn sample(&mut self) -> GpuCounters {
        let mut out = GpuCounters::default();
        if !self.q.collect() {
            return out;
        }
        if let Some(c) = self.engine {
            // Példánynév: pid_1234_luid_0x…_0x…_phys_0_eng_0_engtype_3D
            // Egy motor-típuson belül összeadunk, típusok közt a maximumot vesszük.
            let mut per_engine: HashMap<(String, String), f64> = HashMap::new();
            let mut per_pid_engine: HashMap<(u32, String), f64> = HashMap::new();
            let mut per_pid_label: HashMap<(u32, String), f64> = HashMap::new();
            // (LUID, motor-típus, motor sorszáma) -> összeg a folyamatokra; típusonként a legterheltebb motort vesszük.
            let mut per_eng_idx: HashMap<(String, String, String), f64> = HashMap::new();
            for (name, v) in self.q.read_array(c) {
                let n = name.to_ascii_lowercase();
                let (Some(luid), Some(eng)) =
                    (luid_key(&n), n.rsplit_once("engtype_").map(|(_, e)| e.to_string()))
                else {
                    continue;
                };
                if let Some(pid) = parse_pid(&n) {
                    *per_pid_engine.entry((pid, format!("{luid}/{eng}"))).or_default() += v;
                    // Megjelenített motornév (az eredeti kis-/nagybetűkkel): "GPU <fizikai sorszám> - <motortípus>".
                    if let Some((_, et)) = name.rsplit_once("engtype_") {
                        let phys = name
                            .split("_phys_")
                            .nth(1)
                            .and_then(|r| r.split('_').next())
                            .and_then(|p| p.parse::<u32>().ok())
                            .unwrap_or(0);
                        *per_pid_label.entry((pid, format!("GPU {phys} - {et}"))).or_default() += v;
                    }
                }
                let eng_idx = n.split("_eng_").nth(1).and_then(|r| r.split('_').next()).unwrap_or("0").to_string();
                *per_eng_idx.entry((luid.clone(), eng.clone(), eng_idx)).or_default() += v;
                *per_engine.entry((luid, eng)).or_default() += v;
            }
            for ((luid, eng, _), v) in per_eng_idx {
                let e = out.engines.entry(luid).or_default().entry(eng).or_insert(0.0);
                *e = e.max(v as f32).min(100.0);
            }
            for ((luid, _), v) in per_engine {
                let e = out.utilization.entry(luid).or_insert(0.0);
                *e = e.max(v as f32).min(100.0);
            }
            for ((pid, _), v) in per_pid_engine {
                let e = out.per_pid.entry(pid).or_insert(0.0);
                *e = e.max(v as f32).min(100.0);
            }
            let mut best: HashMap<u32, (f64, String)> = HashMap::new();
            for ((pid, label), v) in per_pid_label {
                if v > 0.0 && best.get(&pid).is_none_or(|(b, _)| v > *b) {
                    best.insert(pid, (v, label));
                }
            }
            for (pid, (_, label)) in best {
                out.engine_label.insert(pid, label);
            }
        }
        if let Some(c) = self.mem_dedicated {
            for (name, v) in self.q.read_array(c) {
                if let Some(luid) = luid_key(&name.to_ascii_lowercase()) {
                    out.dedicated_used_bytes.insert(luid, v);
                }
            }
        }
        out
    }
}

// ───────────── WDDM adapter-perf (D3DKMT) – hőfok, ventilátor, órajelek ─────────────
// A Windows 11 Task Manager is innen veszi a GPU-hőfokot, és gyártótól függetlenül működik,
// ha a driver támogatja (AMD és NVIDIA is). A gdi32.dll-ből futásidőben oldjuk fel a függvényeket.
// Típusazonosítók (d3dkmthk.h): KMTQAITYPE_NODEPERFDATA = 61, KMTQAITYPE_ADAPTERPERFDATA = 62.

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct KmtOpenAdapterFromLuid {
    luid_low: u32,
    luid_high: i32,
    h_adapter: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct KmtCloseAdapter {
    h_adapter: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct KmtQueryAdapterInfo {
    h_adapter: u32,
    info_type: u32,
    private_data: *mut c_void,
    private_size: u32,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default)]
struct KmtAdapterPerfData {
    physical_adapter_index: u32,
    memory_frequency: u64,
    max_memory_frequency: u64,
    max_memory_frequency_oc: u64,
    memory_bandwidth: u64,
    pcie_bandwidth: u64,
    fan_rpm: u32,
    power: u32,       // a TDP tized százalékában
    temperature: u32, // tized °C
    power_state_override: u8,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default)]
struct KmtNodePerfData {
    node_ordinal: u32,
    physical_adapter_index: u32,
    frequency: u64, // Hz
    max_frequency: u64,
    max_frequency_oc: u64,
    voltage: u32,
    voltage_max: u32,
    voltage_max_oc: u32,
    max_transition_latency: u64,
}

type FnKmtOpen = unsafe extern "system" fn(*mut KmtOpenAdapterFromLuid) -> i32;
type FnKmtClose = unsafe extern "system" fn(*mut KmtCloseAdapter) -> i32;
type FnKmtQuery = unsafe extern "system" fn(*mut KmtQueryAdapterInfo) -> i32;

struct KmtAdapter {
    _lib: libloading::Library,
    close: FnKmtClose,
    query_fn: FnKmtQuery,
    handle: u32,
}

impl KmtAdapter {
    fn open(luid_low: u32, luid_high: i32) -> Option<Self> {
        // SAFETY: a gdi32.dll exportjai a d3dkmthk.h szignatúráival egyeznek.
        unsafe {
            let lib = libloading::Library::new("gdi32.dll").ok()?;
            let open = *lib.get::<FnKmtOpen>(b"D3DKMTOpenAdapterFromLuid\0").ok()?;
            let close = *lib.get::<FnKmtClose>(b"D3DKMTCloseAdapter\0").ok()?;
            let query_fn = *lib.get::<FnKmtQuery>(b"D3DKMTQueryAdapterInfo\0").ok()?;
            let mut a = KmtOpenAdapterFromLuid { luid_low, luid_high, h_adapter: 0 };
            if open(&mut a) != 0 || a.h_adapter == 0 {
                return None;
            }
            Some(Self { _lib: lib, close, query_fn, handle: a.h_adapter })
        }
    }

    /// Típusos lekérdezés. A kernel a méretet ellenőrzi, így rossz struktúra nem ír túl a pufferen.
    fn query<T>(&self, info_type: u32, data: &mut T) -> bool {
        let mut q = KmtQueryAdapterInfo {
            h_adapter: self.handle,
            info_type,
            private_data: (data as *mut T).cast(),
            private_size: std::mem::size_of::<T>() as u32,
        };
        // SAFETY: a `q` és a `data` az egész hívás alatt él.
        unsafe { (self.query_fn)(&mut q) == 0 }
    }

    /// Az adapter PCI-címe (busz, eszköz, funkció): ugyanazt a fizikai kártyát azonosítja, bármennyi adapterként látszik.
    fn pci_address(&self) -> Option<(u32, u32, u32)> {
        // KMTQAITYPE_ADAPTERADDRESS = 6. A kernel a méretet ellenőrzi: előbb 3, majd 4 mezős szerkezettel próbáljuk.
        let mut a = [0u32; 3];
        if self.query(6, &mut a) {
            return Some((a[0], a[1], a[2]));
        }
        let mut b = [0u32; 4];
        if self.query(6, &mut b) {
            return Some((b[0], b[1], b[2]));
        }
        None
    }

    /// Kitölti a hőfokot, ventilátort, órajeleket, fogyasztást. `true`, ha bármi érkezett.
    fn fill(&self, t: &mut GpuTelemetry) -> bool {
        let mut any = false;
        let mut perf = KmtAdapterPerfData::default();
        if self.query(62, &mut perf) {
            if perf.temperature > 0 && perf.temperature < 1500 {
                t.temp_c = Some(perf.temperature as f32 / 10.0);
                any = true;
            }
            if perf.fan_rpm > 0 {
                t.fan_rpm = Some(perf.fan_rpm as f32);
                any = true;
            }
            if perf.memory_frequency > 0 {
                t.mem_clock_mhz = Some((perf.memory_frequency / 1_000_000) as f32);
                any = true;
            }
            if perf.power > 0 {
                t.power_tdp_pct = Some(perf.power as f32 / 10.0);
                any = true;
            }
        }
        let mut node = KmtNodePerfData::default(); // 0. csomópont = 3D motor
        if self.query(61, &mut node) && node.frequency > 0 {
            t.core_clock_mhz = Some((node.frequency / 1_000_000) as f32);
            any = true;
        }
        any
    }
}

impl Drop for KmtAdapter {
    fn drop(&mut self) {
        let mut c = KmtCloseAdapter { h_adapter: self.handle };
        // SAFETY: a handle-t mi nyitottuk, egyszer zárjuk.
        unsafe {
            (self.close)(&mut c);
        }
    }
}

/// A GPU-backendek közös felülete.
trait GpuBackend {
    fn info(&self) -> &GpuStatic;
    fn sample(&mut self, counters: &GpuCounters) -> GpuTelemetry;
}

/// Gyártófüggetlen alap: kihasználtság + VRAM a WDDM számlálókból.
fn base_gpu_telemetry(info: &GpuStatic, c: &GpuCounters) -> GpuTelemetry {
    let eng = |k: &str| c.engines.get(&info.luid_key).and_then(|m| m.get(k)).copied().unwrap_or(0.0);
    GpuTelemetry {
        engines: [eng("3d"), eng("copy"), eng("videoencode"), eng("videodecode")],
        utilization: c.utilization.get(&info.luid_key).copied(),
        vram_used_mb: c.dedicated_used_bytes.get(&info.luid_key).map(|b| b / 1_048_576.0),
        vram_total_mb: info.vram_total_mb,
        ..Default::default()
    }
}

fn kmt_status(ok: bool) -> &'static str {
    if ok {
        tr("WDDM perf active")
    } else {
        tr("WDDM perf unavailable")
    }
}

// ───────────── AMD ADL (atiadlxx.dll) – futásidejű betöltéssel ─────────────
// A struktúrák és szenzor-indexek az AMD ADL SDK adl_structures.h / adl_defines.h
// fejléceivel egyeznek (ADL_PMLOG_SENSORS, ADL_PMLOG_MAX_SENSORS = 256).

const ADL_PMLOG_MAX_SENSORS: usize = 256;
const PMLOG_CLK_GFXCLK: usize = 1;
const PMLOG_CLK_MEMCLK: usize = 2;
const PMLOG_TEMPERATURE_EDGE: usize = 8;
const PMLOG_FAN_RPM: usize = 14;
const PMLOG_FAN_PERCENTAGE: usize = 15;
const PMLOG_SOC_POWER: usize = 17;
const PMLOG_INFO_ACTIVITY_GFX: usize = 19;
const PMLOG_ASIC_POWER: usize = 23;
const PMLOG_TEMPERATURE_HOTSPOT: usize = 27;

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Clone, Copy)]
struct AdlSensor {
    supported: c_int,
    value: c_int,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct AdlPmLogOutput {
    size: c_int,
    sensors: [AdlSensor; ADL_PMLOG_MAX_SENSORS],
}

/// Windows-os AdapterInfo (ADL_MAX_PATH = 256).
#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Clone, Copy)]
struct AdlAdapterInfo {
    size: c_int,
    adapter_index: c_int,
    udid: [u8; 256],
    bus: c_int,
    device: c_int,
    function: c_int,
    vendor: c_int,
    name: [u8; 256],
    display_name: [u8; 256],
    present: c_int,
    exist: c_int,
    driver_path: [u8; 256],
    driver_path_ext: [u8; 256],
    pnp: [u8; 256],
    os_display_index: c_int,
}

type AdlMallocCb = unsafe extern "system" fn(c_int) -> *mut c_void;
type FnCreate = unsafe extern "system" fn(AdlMallocCb, c_int, *mut *mut c_void) -> c_int;
type FnDestroy = unsafe extern "system" fn(*mut c_void) -> c_int;
type FnCount = unsafe extern "system" fn(*mut c_void, *mut c_int) -> c_int;
type FnInfo = unsafe extern "system" fn(*mut c_void, *mut AdlAdapterInfo, c_int) -> c_int;
type FnPmLog = unsafe extern "system" fn(*mut c_void, c_int, *mut AdlPmLogOutput) -> c_int;

/// Az ADL kéri, hogy mi foglaljunk neki memóriát. Nem szabadítjuk fel (egyszeri, apró szivárgás).
unsafe extern "system" fn adl_malloc(size: c_int) -> *mut c_void {
    let n = size.max(1) as usize;
    Box::leak(vec![0u8; n].into_boxed_slice()).as_mut_ptr().cast()
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[derive(Default)]
struct AdlReading {
    temp_edge: Option<f32>,
    temp_hotspot: Option<f32>,
    gfx_clock: Option<f32>,
    mem_clock: Option<f32>,
    power_w: Option<f32>,
    fan_rpm: Option<f32>,
    fan_percent: Option<f32>,
    activity: Option<f32>,
}

/// Az ADL-lekérdezés eredménye: az adatok, a forrás neve és a PMLog hibakódja (−8 = nem támogatott).
struct AdlOutcome {
    reading: AdlReading,
    source: &'static str,
    pmlog_rc: c_int,
}

// Overdrive 5 / 6 struktúrák (adl_structures.h) – a régebbi, PMLog-ot nem támogató GPU-khoz.
#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default, Clone, Copy)]
struct AdlTemperature {
    size: c_int,
    temperature: c_int, // ezredfok Celsius
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default, Clone, Copy)]
struct AdlPmActivity {
    size: c_int,
    engine_clock: c_int, // 10 kHz egységben
    memory_clock: c_int, // 10 kHz egységben
    vddc: c_int,
    activity_percent: c_int,
    current_performance_level: c_int,
    current_bus_speed: c_int,
    current_bus_lanes: c_int,
    maximum_bus_lanes: c_int,
    reserved: c_int,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default, Clone, Copy)]
struct AdlFanSpeedValue {
    size: c_int,
    speed_type: c_int, // 1 = százalék, 2 = fordulat/perc
    fan_speed: c_int,
    flags: c_int,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default, Clone, Copy)]
struct AdlOd6CurrentStatus {
    engine_clock: c_int, // 10 kHz egységben
    memory_clock: c_int, // 10 kHz egységben
    activity_percent: c_int,
    current_performance_level: c_int,
    current_bus_speed: c_int,
    current_bus_lanes: c_int,
    maximum_bus_lanes: c_int,
    ext_value: c_int,
    ext_mask: c_int,
}

#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
#[derive(Default, Clone, Copy)]
struct AdlOd6FanSpeedInfo {
    speed_type: c_int,
    fan_speed_percent: c_int,
    fan_speed_rpm: c_int,
    ext_value: c_int,
    ext_mask: c_int,
}

// A globális (nem ADL2_) függvények az ADL_Main_Control_Create-tel létrehozott globális kontextust használják.
type FnGlobalCreate = unsafe extern "system" fn(AdlMallocCb, c_int) -> c_int;
type FnGlobalDestroy = unsafe extern "system" fn() -> c_int;
type FnOdCaps2 = unsafe extern "system" fn(*mut c_void, c_int, *mut c_int, *mut c_int, *mut c_int) -> c_int;
type FnOd5Temp = unsafe extern "system" fn(c_int, c_int, *mut AdlTemperature) -> c_int;
type FnOd5Activity = unsafe extern "system" fn(c_int, *mut AdlPmActivity) -> c_int;
type FnOd5Fan = unsafe extern "system" fn(c_int, c_int, *mut AdlFanSpeedValue) -> c_int;
type FnOd6Temp = unsafe extern "system" fn(c_int, *mut c_int) -> c_int;
type FnOd6Status = unsafe extern "system" fn(c_int, *mut AdlOd6CurrentStatus) -> c_int;
type FnOd6Fan = unsafe extern "system" fn(c_int, *mut AdlOd6FanSpeedInfo) -> c_int;

/// ADL-hőmérséklet -> °C. A régi API ezredfokot ad, de józansági szűrőt is alkalmazunk.
fn adl_temp(v: c_int) -> Option<f32> {
    let c = if v > 300 { v as f32 / 1000.0 } else { v as f32 };
    (1.0..=150.0).contains(&c).then_some(c)
}

struct Adl {
    _lib: libloading::Library, // életben kell tartani, amíg a függvénymutatók élnek
    ctx: *mut c_void,
    destroy: FnDestroy,
    adapter_count: FnCount,
    adapter_info: FnInfo,
    pmlog: FnPmLog,
    // Opcionális: Overdrive 5/6 tartalék és verzió-lekérdezés.
    global_destroy: Option<FnGlobalDestroy>,
    global_ok: bool,
    od_caps: Option<FnOdCaps2>,
    od5_temp: Option<FnOd5Temp>,
    od5_activity: Option<FnOd5Activity>,
    od5_fan: Option<FnOd5Fan>,
    od6_temp: Option<FnOd6Temp>,
    od6_status: Option<FnOd6Status>,
    od6_fan: Option<FnOd6Fan>,
}

impl Adl {
    fn init() -> Result<Self, String> {
        // SAFETY: ismert nevű, a driver által telepített DLL betöltése; a szimbólumok típusa
        // az ADL SDK fejlécével egyezik. 64 bites build esetén atiadlxx.dll a neve.
        unsafe {
            let lib = libloading::Library::new("atiadlxx.dll")
                .map_err(|e| format!("atiadlxx.dll: {e}"))?;
            let create = *lib
                .get::<FnCreate>(b"ADL2_Main_Control_Create\0")
                .map_err(|e| e.to_string())?;
            let destroy = *lib
                .get::<FnDestroy>(b"ADL2_Main_Control_Destroy\0")
                .map_err(|e| e.to_string())?;
            let adapter_count = *lib
                .get::<FnCount>(b"ADL2_Adapter_NumberOfAdapters_Get\0")
                .map_err(|e| e.to_string())?;
            let adapter_info = *lib
                .get::<FnInfo>(b"ADL2_Adapter_AdapterInfo_Get\0")
                .map_err(|e| e.to_string())?;
            let pmlog = *lib
                .get::<FnPmLog>(b"ADL2_New_QueryPMLogData_Get\0")
                .map_err(|e| e.to_string())?;

            let mut ctx: *mut c_void = std::ptr::null_mut();
            let rc = create(adl_malloc, 1, &mut ctx);
            if rc != 0 || ctx.is_null() {
                return Err(format!("ADL2_Main_Control_Create hibakód: {rc}"));
            }

            // Opcionális függvények (hiányuk nem hiba).
            let global_create = lib.get::<FnGlobalCreate>(b"ADL_Main_Control_Create\0").ok().map(|s| *s);
            let global_destroy = lib.get::<FnGlobalDestroy>(b"ADL_Main_Control_Destroy\0").ok().map(|s| *s);
            let global_ok = match global_create {
                Some(gc) => gc(adl_malloc, 1) >= 0,
                None => false,
            };
            let od_caps = lib.get::<FnOdCaps2>(b"ADL2_Overdrive_Caps\0").ok().map(|s| *s);
            let od5_temp = lib.get::<FnOd5Temp>(b"ADL_Overdrive5_Temperature_Get\0").ok().map(|s| *s);
            let od5_activity =
                lib.get::<FnOd5Activity>(b"ADL_Overdrive5_CurrentActivity_Get\0").ok().map(|s| *s);
            let od5_fan = lib.get::<FnOd5Fan>(b"ADL_Overdrive5_FanSpeed_Get\0").ok().map(|s| *s);
            let od6_temp = lib.get::<FnOd6Temp>(b"ADL_Overdrive6_Temperature_Get\0").ok().map(|s| *s);
            let od6_status =
                lib.get::<FnOd6Status>(b"ADL_Overdrive6_CurrentStatus_Get\0").ok().map(|s| *s);
            let od6_fan = lib.get::<FnOd6Fan>(b"ADL_Overdrive6_FanSpeed_Get\0").ok().map(|s| *s);

            Ok(Self {
                _lib: lib,
                ctx,
                destroy,
                adapter_count,
                adapter_info,
                pmlog,
                global_destroy,
                global_ok,
                od_caps,
                od5_temp,
                od5_activity,
                od5_fan,
                od6_temp,
                od6_status,
                od6_fan,
            })
        }
    }

    /// Megkeresi a DXGI-adapterhez tartozó ADL adapter-indexet (név alapján, tartalékként az első AMD).
    fn find_adapter_index(&self, dxgi_name: &str) -> Option<c_int> {
        let mut n: c_int = 0;
        // SAFETY: a kontextus él, `n` érvényes kimenet.
        if unsafe { (self.adapter_count)(self.ctx, &mut n) } != 0 || n <= 0 {
            return None;
        }
        // SAFETY: a csupa nulla bitminta ezekre a repr(C) struktúrákra érvényes érték.
        let zero: AdlAdapterInfo = unsafe { std::mem::zeroed() };
        let mut list = vec![zero; n as usize];
        let bytes = (n as usize * std::mem::size_of::<AdlAdapterInfo>()) as c_int;
        // SAFETY: a puffer pontosan `bytes` bájt.
        if unsafe { (self.adapter_info)(self.ctx, list.as_mut_ptr(), bytes) } != 0 {
            return None;
        }
        // Az ADL ugyanazt a GPU-t több indexen is listázhatja (kijelzőnként) – szűrünk.
        let mut seen: Vec<(c_int, c_int, c_int)> = Vec::new();
        let mut amd: Vec<&AdlAdapterInfo> = Vec::new();
        for a in &list {
            let is_amd = a.vendor == 1002 || a.vendor == 0x1002;
            if !is_amd || a.present == 0 {
                continue;
            }
            let key = (a.bus, a.device, a.function);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            amd.push(a);
        }
        let want = dxgi_name.trim().to_ascii_lowercase();
        amd.iter()
            .find(|a| cstr(&a.name).trim().to_ascii_lowercase() == want)
            .or_else(|| amd.first())
            .map(|a| a.adapter_index)
    }

    /// Az Overdrive-verzió (5, 6, 7, 8) – csak tájékoztatásnak, az UI-n megjelenítjük.
    fn od_version(&self, idx: c_int) -> Option<c_int> {
        let f = self.od_caps?;
        let (mut supported, mut enabled, mut version) = (0, 0, 0);
        // SAFETY: a kontextus él, a kimenetek érvényesek.
        let rc = unsafe { f(self.ctx, idx, &mut supported, &mut enabled, &mut version) };
        (rc == 0).then_some(version)
    }

    /// `Err(rc)`: az ADL hibakódja (pl. −8 = ADL_ERR_NOT_SUPPORTED).
    fn read_pmlog(&self, idx: c_int) -> Result<AdlReading, c_int> {
        // SAFETY: csupa nulla bitminta érvényes AdlPmLogOutput.
        let mut out: AdlPmLogOutput = unsafe { std::mem::zeroed() };
        out.size = std::mem::size_of::<AdlPmLogOutput>() as c_int;
        // SAFETY: a kontextus él, `out` pontosan az SDK-beli struktúra méretével egyezik.
        let rc = unsafe { (self.pmlog)(self.ctx, idx, &mut out) };
        if rc != 0 {
            return Err(rc);
        }
        let get = |id: usize| -> Option<f32> {
            let s = out.sensors.get(id)?;
            (s.supported != 0).then_some(s.value as f32)
        };
        let temp_ok = |v: f32| (1.0..=150.0).contains(&v); // józansági szűrő
        Ok(AdlReading {
            temp_edge: get(PMLOG_TEMPERATURE_EDGE).filter(|v| temp_ok(*v)),
            temp_hotspot: get(PMLOG_TEMPERATURE_HOTSPOT).filter(|v| temp_ok(*v)),
            gfx_clock: get(PMLOG_CLK_GFXCLK).filter(|v| *v > 0.0),
            mem_clock: get(PMLOG_CLK_MEMCLK).filter(|v| *v > 0.0),
            power_w: get(PMLOG_ASIC_POWER).or_else(|| get(PMLOG_SOC_POWER)).filter(|v| *v > 0.0),
            fan_rpm: get(PMLOG_FAN_RPM),
            fan_percent: get(PMLOG_FAN_PERCENTAGE),
            activity: get(PMLOG_INFO_ACTIVITY_GFX),
        })
    }

    /// Tartalék a PMLog-ot nem támogató GPU-khoz: először Overdrive 6, majd Overdrive 5.
    /// (Overdrive 7 / OverdriveN GPU-knál – pl. Polaris/Vega – ezek is "nem támogatott"-at adhatnak.)
    fn read_overdrive(&self, idx: c_int) -> (AdlReading, &'static str) {
        let mut r = AdlReading::default();
        if !self.global_ok {
            return (r, "");
        }
        // SAFETY (az összes alábbi hívásra): a globális ADL-kontextus él, a struktúrák az SDK-val egyeznek.
        // — Overdrive 6 —
        let mut got6 = false;
        if let Some(f) = self.od6_temp {
            let mut t: c_int = 0;
            if unsafe { f(idx, &mut t) } == 0 {
                if let Some(c) = adl_temp(t) {
                    r.temp_edge = Some(c);
                    got6 = true;
                }
            }
        }
        if let Some(f) = self.od6_status {
            let mut st = AdlOd6CurrentStatus::default();
            if unsafe { f(idx, &mut st) } == 0 {
                if st.engine_clock > 0 {
                    r.gfx_clock = Some(st.engine_clock as f32 / 100.0);
                    got6 = true;
                }
                if st.memory_clock > 0 {
                    r.mem_clock = Some(st.memory_clock as f32 / 100.0);
                    got6 = true;
                }
                r.activity = Some(st.activity_percent as f32);
            }
        }
        if let Some(f) = self.od6_fan {
            let mut fi = AdlOd6FanSpeedInfo::default();
            if unsafe { f(idx, &mut fi) } == 0 {
                if fi.fan_speed_rpm > 0 {
                    r.fan_rpm = Some(fi.fan_speed_rpm as f32);
                    got6 = true;
                }
                if fi.fan_speed_percent > 0 {
                    r.fan_percent = Some(fi.fan_speed_percent as f32);
                    got6 = true;
                }
            }
        }
        if got6 {
            return (r, "Overdrive 6");
        }
        // — Overdrive 5 —
        r = AdlReading::default();
        let mut got5 = false;
        if let Some(f) = self.od5_temp {
            let mut t = AdlTemperature { size: std::mem::size_of::<AdlTemperature>() as c_int, temperature: 0 };
            if unsafe { f(idx, 0, &mut t) } == 0 {
                if let Some(c) = adl_temp(t.temperature) {
                    r.temp_edge = Some(c);
                    got5 = true;
                }
            }
        }
        if let Some(f) = self.od5_activity {
            let mut a = AdlPmActivity { size: std::mem::size_of::<AdlPmActivity>() as c_int, ..Default::default() };
            if unsafe { f(idx, &mut a) } == 0 {
                if a.engine_clock > 0 {
                    r.gfx_clock = Some(a.engine_clock as f32 / 100.0);
                    got5 = true;
                }
                if a.memory_clock > 0 {
                    r.mem_clock = Some(a.memory_clock as f32 / 100.0);
                    got5 = true;
                }
                r.activity = Some(a.activity_percent as f32);
            }
        }
        if let Some(f) = self.od5_fan {
            for (kind, is_rpm) in [(2, true), (1, false)] {
                let mut fv = AdlFanSpeedValue {
                    size: std::mem::size_of::<AdlFanSpeedValue>() as c_int,
                    speed_type: kind,
                    ..Default::default()
                };
                if unsafe { f(idx, 0, &mut fv) } == 0 && fv.fan_speed > 0 {
                    if is_rpm {
                        r.fan_rpm = Some(fv.fan_speed as f32);
                    } else {
                        r.fan_percent = Some(fv.fan_speed as f32);
                    }
                    got5 = true;
                }
            }
        }
        if got5 {
            return (r, "Overdrive 5");
        }
        (AdlReading::default(), "")
    }

    /// PMLog, ennek hiányában Overdrive 6 / 5.
    fn read(&self, idx: c_int) -> AdlOutcome {
        match self.read_pmlog(idx) {
            Ok(reading) => AdlOutcome { reading, source: "PMLog", pmlog_rc: 0 },
            Err(rc) => {
                let (reading, source) = self.read_overdrive(idx);
                AdlOutcome { reading, source, pmlog_rc: rc }
            }
        }
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        // SAFETY: a kontextusokat mi hoztuk létre, egyszer szabadítjuk fel.
        unsafe {
            (self.destroy)(self.ctx);
            if self.global_ok {
                if let Some(gd) = self.global_destroy {
                    gd();
                }
            }
        }
    }
}

enum AdlState {
    Active,
    NoAdapter,
    Unavailable(String),
}

/// AMD GPU – működő ág: PDH (kihasználtság, VRAM) + DXGI (név, VRAM-méret) + WDDM-perf
/// (hőfok, órajel, ventilátor) + ADL (PMLog, ennek hiányában Overdrive 6/5).
/// Bármelyik forrás kiesése esetén a többi tovább működik.
struct AmdGpuBackend {
    info: GpuStatic,
    kmt: Option<KmtAdapter>,
    adl: Option<(Adl, c_int)>,
    state: AdlState,
    od_version: Option<c_int>,
}

impl AmdGpuBackend {
    fn new(info: GpuStatic) -> Self {
        let kmt = KmtAdapter::open(info.luid_low, info.luid_high);
        match Adl::init() {
            Ok(adl) => match adl.find_adapter_index(&info.name) {
                Some(idx) => {
                    let od_version = adl.od_version(idx);
                    Self { info, kmt, adl: Some((adl, idx)), state: AdlState::Active, od_version }
                }
                None => Self { info, kmt, adl: None, state: AdlState::NoAdapter, od_version: None },
            },
            Err(e) => Self { info, kmt, adl: None, state: AdlState::Unavailable(e), od_version: None },
        }
    }
}

impl GpuBackend for AmdGpuBackend {
    fn info(&self) -> &GpuStatic {
        &self.info
    }
    fn sample(&mut self, c: &GpuCounters) -> GpuTelemetry {
        let mut t = base_gpu_telemetry(&self.info, c);
        let kmt_ok = self.kmt.as_ref().map_or(false, |k| k.fill(&mut t));
        let adl_msg = match (&self.state, &self.adl) {
            (AdlState::Unavailable(e), _) => format!("{} {e}", tr("ADL unavailable:")),
            (AdlState::NoAdapter, _) => tr("ADL: no AMD adapter found").to_string(),
            (_, Some((adl, idx))) => {
                let o = adl.read(*idx);
                let r = &o.reading;
                // Az ADL adatai felülírják a WDDM-értékeket, ha van adat.
                if let Some(v) = r.temp_edge {
                    t.temp_c = Some(v);
                }
                if let Some(v) = r.temp_hotspot {
                    t.hotspot_c = Some(v);
                }
                if let Some(v) = r.gfx_clock {
                    t.core_clock_mhz = Some(v);
                }
                if let Some(v) = r.mem_clock {
                    t.mem_clock_mhz = Some(v);
                }
                if let Some(v) = r.power_w {
                    t.power_w = Some(v);
                }
                if let Some(v) = r.fan_rpm {
                    t.fan_rpm = Some(v);
                }
                if let Some(v) = r.fan_percent {
                    t.fan_percent = Some(v);
                }
                if t.utilization.is_none() {
                    t.utilization = r.activity;
                }
                let ver = self.od_version.map(|v| format!(", Overdrive v{v}")).unwrap_or_default();
                if o.source == "PMLog" {
                    format!("ADL: PMLog{ver}")
                } else if !o.source.is_empty() {
                    format!("ADL: {} (PMLog: {}{ver})", o.source, o.pmlog_rc)
                } else {
                    format!("{} (PMLog: {}{ver})", tr("ADL: not supported on this GPU"), o.pmlog_rc)
                }
            }
            _ => String::new(),
        };
        t.note = format!("{adl_msg} · {}", kmt_status(kmt_ok));
        t
    }
}

/// NVIDIA GPU – CSONK (előkészítve). A kihasználtság, VRAM és (ha a driver támogatja) a hőfok már
/// most működik a WDDM-forrásokból. A pontosabb értékeket az NVML adja: fordítsd
/// `--features nvidia` kapcsolóval, és kövesd a `fill_from_nvml` TODO-ját. (NVIDIA-hardver híján
/// ezt itt nem tudtam kipróbálni.)
struct NvidiaGpuBackend {
    info: GpuStatic,
    kmt: Option<KmtAdapter>,
    /// A hányadik NVIDIA-adapter a DXGI-sorrendben = NVML eszközindex (hibrid gépeken ellenőrizd).
    /// Csak az `nvidia` feature-rel olvassuk.
    #[cfg_attr(not(feature = "nvidia"), allow(dead_code))]
    nvidia_index: u32,
    #[cfg(feature = "nvidia")]
    nvml: Option<nvml_wrapper::Nvml>,
    note: String,
}

#[cfg(feature = "nvidia")]
fn nvml_init() -> (Option<nvml_wrapper::Nvml>, String) {
    match nvml_wrapper::Nvml::init() {
        Ok(n) => (Some(n), "NVML aktív".to_string()),
        Err(e) => (None, format!("NVML nem inicializálható: {e}")),
    }
}

impl NvidiaGpuBackend {
    fn new(info: GpuStatic, nvidia_index: u32) -> Self {
        #[cfg(feature = "nvidia")]
        let (nvml, note) = nvml_init();
        #[cfg(not(feature = "nvidia"))]
        let note = tr("NVIDIA: NVML branch is a stub (build with --features nvidia)").to_string();
        let kmt = KmtAdapter::open(info.luid_low, info.luid_high);
        Self {
            info,
            kmt,
            nvidia_index,
            #[cfg(feature = "nvidia")]
            nvml,
            note,
        }
    }

    /// TODO (NVML): hőfok, órajelek, fogyasztás, VRAM, ventilátor. A `nvidia` feature-rel él.
    #[cfg(feature = "nvidia")]
    fn fill_from_nvml(&self, t: &mut GpuTelemetry) {
        use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
        let Some(nvml) = self.nvml.as_ref() else { return };
        let Ok(dev) = nvml.device_by_index(self.nvidia_index) else { return };
        if let Ok(v) = dev.temperature(TemperatureSensor::Gpu) {
            t.temp_c = Some(v as f32);
        }
        if let Ok(v) = dev.clock_info(Clock::Graphics) {
            t.core_clock_mhz = Some(v as f32);
        }
        if let Ok(v) = dev.clock_info(Clock::Memory) {
            t.mem_clock_mhz = Some(v as f32);
        }
        if let Ok(mw) = dev.power_usage() {
            t.power_w = Some(mw as f32 / 1000.0); // milliwatt -> watt
        }
        if let Ok(m) = dev.memory_info() {
            t.vram_used_mb = Some(m.used as f64 / 1_048_576.0);
            t.vram_total_mb = m.total as f64 / 1_048_576.0;
        }
        if let Ok(u) = dev.utilization_rates() {
            t.utilization = Some(u.gpu as f32);
        }
        if let Ok(f) = dev.fan_speed(0) {
            t.fan_percent = Some(f as f32);
        }
    }

    #[cfg(not(feature = "nvidia"))]
    fn fill_from_nvml(&self, _t: &mut GpuTelemetry) {
        // Csonk: az NVML nincs bekapcsolva.
    }
}

impl GpuBackend for NvidiaGpuBackend {
    fn info(&self) -> &GpuStatic {
        &self.info
    }
    fn sample(&mut self, c: &GpuCounters) -> GpuTelemetry {
        let mut t = base_gpu_telemetry(&self.info, c);
        let kmt_ok = self.kmt.as_ref().map_or(false, |k| k.fill(&mut t));
        self.fill_from_nvml(&mut t);
        t.note = format!("{} · {}", self.note, kmt_status(kmt_ok));
        t
    }
}

/// Intel GPU és minden egyéb: WDDM számlálók + WDDM-perf.
/// TODO (Intel): a pontosabb hőfok/órajel Intel Arc esetén az IGCL (Intel Graphics Control Library) API-n át érhető el.
struct GenericGpuBackend {
    info: GpuStatic,
    kmt: Option<KmtAdapter>,
}

impl GenericGpuBackend {
    fn new(info: GpuStatic) -> Self {
        let kmt = KmtAdapter::open(info.luid_low, info.luid_high);
        Self { info, kmt }
    }
}

impl GpuBackend for GenericGpuBackend {
    fn info(&self) -> &GpuStatic {
        &self.info
    }
    fn sample(&mut self, c: &GpuCounters) -> GpuTelemetry {
        let mut t = base_gpu_telemetry(&self.info, c);
        let kmt_ok = self.kmt.as_ref().map_or(false, |k| k.fill(&mut t));
        t.note = format!("{} · {}", tr("No vendor-specific backend yet"), kmt_status(kmt_ok));
        t
    }
}

fn build_gpu_backends() -> Vec<Box<dyn GpuBackend>> {
    let mut nvidia_idx = 0u32;
    let mut v: Vec<Box<dyn GpuBackend>> = Vec::new();
    for info in enumerate_adapters() {
        match info.vendor {
            GpuVendor::Amd => v.push(Box::new(AmdGpuBackend::new(info))),
            GpuVendor::Nvidia => {
                v.push(Box::new(NvidiaGpuBackend::new(info, nvidia_idx)));
                nvidia_idx += 1;
            }
            _ => v.push(Box::new(GenericGpuBackend::new(info))),
        }
    }
    v
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Folyamat kilövése (TerminateProcess)
// ═══════════════════════════════════════════════════════════════════════════

/// Ezek leállítása azonnali kék halált / kényszerített újraindítást okoz.
const PROTECTED: &[&str] = &[
    "system",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "memory compression",
    "secure system",
];

fn kill_process(pid: u32, name: &str) -> Result<(), String> {
    if pid == 0 || pid == 4 || pid == std::process::id() {
        return Err(tr("This process cannot be ended (system / own process).").to_string());
    }
    if PROTECTED.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(format!("{} {name}", tr("Critical system process, cannot be ended:")));
    }
    // SAFETY: a handle-t nyitjuk, használjuk, és mindenképp lezárjuk.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
            .map_err(|e| format!("OpenProcess {}: {e}", tr("failed")))?;
        let result = TerminateProcess(handle, 1);
        let _ = CloseHandle(handle);
        result.map_err(|e| format!("TerminateProcess {}: {e}", tr("failed")))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. Alkalmazás-ikonok (külön szálon): exe -> HICON -> RGBA
// ═══════════════════════════════════════════════════════════════════════════

/// Az exe első (nagy) ikonja RGBA formában. `None`, ha nincs ikon vagy nem olvasható.
fn load_icon_rgba(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    if path.to_ascii_lowercase().ends_with(".png") {
        return load_png_rgba(path);
    }
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut large: isize = 0;
    // SAFETY: a `wide` NUL-terminált, a `large` egyetlen elemű kimeneti tömb (count = 1).
    let n = unsafe { ExtractIconExW(wide.as_ptr(), 0, &mut large, std::ptr::null_mut(), 1) };
    if n == 0 || large == 0 {
        return None;
    }
    let result = icon_to_rgba(large);
    // SAFETY: az ikont az ExtractIconEx hozta létre, nekünk kell megsemmisíteni.
    unsafe {
        DestroyIcon(large);
    }
    result
}

/// Egy PNG-kép (a csomagolt alkalmazások logója) RGBA formában, legfeljebb 64×64 képpontra kicsinyítve.
fn load_png_rgba(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    let img = image::open(path).ok()?;
    let img = if img.width() > 64 || img.height() > 64 {
        img.resize(64, 64, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some((rgba.width() as usize, rgba.height() as usize, rgba.into_raw()))
}

fn icon_to_rgba(hicon: isize) -> Option<(usize, usize, Vec<u8>)> {
    // SAFETY: a GDI-hívások a dokumentált szignatúrákkal; minden létrehozott objektumot törlünk.
    unsafe {
        let mut info: IconInfo = std::mem::zeroed();
        if GetIconInfo(hicon, &mut info) == 0 {
            return None;
        }
        let mut result = None;
        if info.hbm_color != 0 {
            let mut bm: BitmapObj = std::mem::zeroed();
            let got = GetObjectW(
                info.hbm_color,
                std::mem::size_of::<BitmapObj>() as i32,
                (&mut bm as *mut BitmapObj).cast(),
            );
            if got != 0 && bm.bm_width > 0 && bm.bm_height > 0 {
                let (w, h) = (bm.bm_width as usize, bm.bm_height as usize);
                let hdc = CreateCompatibleDC(0);
                let mut bmi: BitmapInfo = std::mem::zeroed();
                bmi.header.bi_size = std::mem::size_of::<BitmapInfoHeader>() as u32;
                bmi.header.bi_width = w as i32;
                bmi.header.bi_height = -(h as i32); // negatív: fentről lefelé haladó sorok
                bmi.header.bi_planes = 1;
                bmi.header.bi_bit_count = 32;
                bmi.header.bi_compression = 0; // BI_RGB
                let mut px = vec![0u8; w * h * 4];
                let lines = GetDIBits(
                    hdc,
                    info.hbm_color,
                    0,
                    h as u32,
                    px.as_mut_ptr().cast(),
                    &mut bmi,
                    0, // DIB_RGB_COLORS
                );
                // A maszk az olyan ikonokhoz kell, amelyeknek nincs alfa-csatornájuk.
                let mut mask = vec![0u8; w * h * 4];
                let have_mask = info.hbm_mask != 0
                    && GetDIBits(hdc, info.hbm_mask, 0, h as u32, mask.as_mut_ptr().cast(), &mut bmi, 0) != 0;
                DeleteDC(hdc);
                if lines != 0 {
                    let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
                    let mut rgba = vec![0u8; w * h * 4];
                    for i in 0..w * h {
                        let (b, g, r, a) = (px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]);
                        let alpha = if has_alpha {
                            a
                        } else if have_mask && mask[i * 4] != 0 {
                            0 // a maszkban a fehér = átlátszó
                        } else {
                            255
                        };
                        rgba[i * 4] = r;
                        rgba[i * 4 + 1] = g;
                        rgba[i * 4 + 2] = b;
                        rgba[i * 4 + 3] = alpha;
                    }
                    result = Some((w, h, rgba));
                }
            }
        }
        if info.hbm_color != 0 {
            DeleteObject(info.hbm_color);
        }
        if info.hbm_mask != 0 {
            DeleteObject(info.hbm_mask);
        }
        result
    }
}

fn icon_thread(requests: Receiver<String>, results: Sender<IconMsg>, ctx: egui::Context) {
    lower_current_thread_priority();
    while let Ok(path) = requests.recv() {
        let image = catch_unwind(|| load_icon_rgba(&path)).ok().flatten();
        if results.send(IconMsg { path, image }).is_err() {
            break; // az UI bezárult
        }
        ctx.request_repaint();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. Windows-specifikus ablak- és prioritás-beállítások
// ═══════════════════════════════════════════════════════════════════════════

/// Az eframe-es ablak HWND-je a CreationContext-ből.
fn hwnd_from_creation_context(cc: &eframe::CreationContext<'_>) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = cc.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut c_void)),
        _ => None,
    }
}

/// A folyamat magasabb prioritást kap, hogy teljes CPU-terhelés alatt is rajzolni tudjon.
fn boost_process_priority() {
    // SAFETY: a saját folyamat pszeudo-handle-jén dolgozunk.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS);
    }
}

/// A háttérszálak alacsonyabb prioritásúak, mint az UI-szál.
fn lower_current_thread_priority() {
    // SAFETY: a hívó szál pszeudo-handle-jén dolgozunk.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. A háttérszál: gyűjtés + kommunikáció
// ═══════════════════════════════════════════════════════════════════════════

fn process_refresh_kind() -> ProcessRefreshKind {
    let mut kind = ProcessRefreshKind::nothing().with_cpu().with_memory().with_exe(UpdateKind::OnlyIfNotSet);
    if WANT_CMDLINE.load(AtomicOrdering::Relaxed) {
        kind = kind.with_cmd(UpdateKind::OnlyIfNotSet);
    }
    if WANT_IO_TOTAL.load(AtomicOrdering::Relaxed) {
        kind = kind.with_disk_usage();
    }
    kind
}

/// A szöveget a megadott szélességre rövidíti: ami nem fér el, annak a végét "…"-ra cseréli ("valami/hosszú/ut…").
fn fit_text(painter: &egui::Painter, text: &str, font: &FontId, max_w: f32) -> String {
    if max_w <= 6.0 {
        return String::new();
    }
    if text.is_empty() {
        return String::new();
    }
    let width = |t: String| painter.layout_no_wrap(t, font.clone(), Color32::WHITE).size().x;
    if width(text.to_string()) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    // A legnagyobb olyan előtag, amelyhez a "…" hozzáfűzve még elfér.
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut cand: String = chars[..mid].iter().collect();
        cand.push('…');
        if width(cand) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
}

/// A becsült energiafogyasztás szöveges besorolása (a Windows Feladatkezelő skálájához hasonlóan).
fn power_label(score: f32) -> (&'static str, Color32) {
    if score < 0.5 {
        (tr("Very low"), col_muted())
    } else if score < 2.5 {
        (tr("Low"), col_muted())
    } else if score < 8.0 {
        (tr("Moderate"), col_text())
    } else if score < 20.0 {
        (tr("High"), WARN_AMBER)
    } else {
        (tr("Very high"), DANGER_RED)
    }
}

struct Collector {
    sys: System,
    cpu: Box<dyn CpuBackend>,
    gpus: Vec<Box<dyn GpuBackend>>,
    gpu_pdh: Option<GpuPdh>,
    mem: MemReader,
    disk_static: Vec<DiskStatic>,
    disks: DiskReader,
    nets: NetReader,
    proc_pdh: Option<ProcPdh>,
    /// PID -> munkamenet-azonosító (a PID élete alatt nem változik).
    session_cache: HashMap<u32, u32>,
    meta: ProcMetaCache,
    /// Hálózati ETW: az előző mintavétel kumulált bájtjai (PID -> bájt) és időpontja.
    net_prev: HashMap<u32, (u64, u64)>,
    net_prev_at: Option<Instant>,
    net_state: NetState,
    /// Folyamatonkénti mozgó átlag az energiafogyasztás-becsléshez (PID -> pontszám), és az utolsó számítás ideje.
    power_ema: HashMap<u32, f32>,
    power_at: Option<Instant>,
}

impl Collector {
    /// Hardverfelismerés: gyártók, backendek kiválasztása. Csak a háttérszálon hívjuk.
    fn new() -> (Self, StaticInfo) {
        let mut sys = System::new();
        sys.refresh_cpu_all();
        sys.refresh_memory();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());

        let vendor = detect_cpu_vendor(&sys);
        let brand = sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default();
        let cpu = detect_cpu_backend(vendor);
        let gpus = build_gpu_backends();
        let mem = MemReader::new();
        let disk_static = DiskReader::enumerate_static();
        // Folyamatonkénti hálózati forgalom: ETW-munkamenet (rendszergazdai jog kell hozzá; ha nincs, az állapot jelzi).
        let net_state = netmon::start();

        let mut mem_static = read_smbios_memory();
        let total_mb = sys.total_memory() as f64 / 1_048_576.0;
        mem_static.installed_mb = mem.installed_mb().unwrap_or(total_mb);
        if mem_static.installed_mb > total_mb {
            mem_static.hw_reserved_mb = Some(mem_static.installed_mb - total_mb);
        }

        let info = StaticInfo {
            cpu: CpuStatic { vendor: cpu.vendor(), brand, logical_cores: sys.cpus().len(), topo: cpu_topology() },
            mem: mem_static,
            gpus: gpus.iter().map(|g| g.info().clone()).collect(),
            disks: disk_static.clone(),
        };
        (
            Self {
                sys,
                cpu,
                gpus,
                gpu_pdh: GpuPdh::new(),
                mem,
                disk_static,
                disks: DiskReader::new(),
                nets: NetReader::new(),
                proc_pdh: ProcPdh::new(),
                session_cache: HashMap::new(),
                meta: ProcMetaCache::default(),
                net_prev: HashMap::new(),
                net_prev_at: None,
                net_state,
                power_ema: HashMap::new(),
                power_at: None,
            },
            info,
        )
    }

    fn collect(&mut self) -> Snapshot {
        self.sys.refresh_cpu_all();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());

        // GPU-számlálók először, mert a folyamatonkénti GPU-% is belőlük jön.
        let counters = self.gpu_pdh.as_mut().map(|g| g.sample()).unwrap_or_default();

        // Folyamatonkénti privát munkakészlet és I/O (PDH), valamint a munkamenet-azonosítók.
        let extras = self.proc_pdh.as_mut().map(|x| x.sample()).unwrap_or_default();
        // Privát munkakészlet közvetlenül a rendszertől, PID szerint (névillesztés nélkül).
        let private_ws = private_working_sets();
        // Hálózati forgalom (ETW): a kumulált bájtokból sebesség (bájt/s).
        self.net_state = netmon::state();
        let net_now = Instant::now();
        let net_totals = netmon::totals_snapshot();
        let net_dt = self.net_prev_at.map(|t| net_now.duration_since(t).as_secs_f64()).filter(|d| *d > 0.05);
        // PID -> (feltöltés bájt/s, letöltés bájt/s)
        let net_rates: HashMap<u32, (f64, f64)> = match net_dt {
            Some(dt) => net_totals
                .iter()
                .map(|(pid, &(sent, recv))| {
                    let (prev_sent, prev_recv) = self.net_prev.get(pid).copied().unwrap_or((0, 0));
                    (*pid, (sent.saturating_sub(prev_sent) as f64 / dt, recv.saturating_sub(prev_recv) as f64 / dt))
                })
                .collect(),
            None => HashMap::new(),
        };
        self.net_prev = net_totals;
        self.net_prev_at = Some(net_now);
        let mut session_cache = std::mem::take(&mut self.session_cache);
        if session_cache.len() > 4096 {
            session_cache.clear();
        }

        // Látható ablakok (alkalmazás vs. háttérfolyamat), tulajdonos / architektúra / leírás (gyorsítótárral).
        let (app_pids, hung_pids) = scan_windows();
        let mut meta = std::mem::take(&mut self.meta);
        let mut meta_budget = 80u32;
        let mut desc_budget = 120u32; // fájlleírás (megjelenített alkalmazásnév) olvasása ciklusonként
        let mut pub_budget = 60u32; // gyártó (verzióinformáció) olvasása ciklusonként
        let want_cmd = WANT_CMDLINE.load(AtomicOrdering::Relaxed);
        let want_io = WANT_IO_TOTAL.load(AtomicOrdering::Relaxed);
        poll_metered(); // a forgalmi díjas kapcsolat állapotának időnkénti, háttérben történő frissítése
        // Energiafogyasztás-becslés: a trend egy ~120 másodperces időállandójú mozgó átlag.
        let power_now = Instant::now();
        let power_dt = self.power_at.map_or(1.0, |t| power_now.duration_since(t).as_secs_f32()).clamp(0.05, 30.0);
        self.power_at = Some(power_now);
        let power_alpha = 1.0 - (-power_dt / 120.0).exp();
        let mut power_ema = std::mem::take(&mut self.power_ema);

        let cores = self.sys.cpus().len().max(1) as f32;
        let processes: Vec<ProcInfo> = self
            .sys
            .processes()
            .values()
            .map(|p| {
                let name = p.name().to_string_lossy().into_owned();
                let name_lc = name.to_lowercase();
                let pid = p.pid().as_u32();
                let working_set_mb = p.memory() as f64 / 1_048_576.0;
                let exe = p.exe().map(|e| e.to_string_lossy().into_owned());
                let (user, arch) = meta.meta(pid, &mut meta_budget);
                let desc = match &exe {
                    Some(e) => meta.description(e, &mut desc_budget),
                    None => String::new(),
                };
                let publisher = match &exe {
                    Some(e) => meta.publisher(e, &mut pub_budget),
                    None => String::new(),
                };
                let cmd = if want_cmd {
                    p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
                } else {
                    String::new()
                };
                let (net_up, net_down) = net_rates.get(&pid).copied().unwrap_or((0.0, 0.0));
                let extra = extras.get(&pid).copied();
                // A Task Manager tr("Memory") oszlopa a PRIVÁT munkakészlet (nem a teljes munkakészlet, amit a
                // sysinfo ad). A "Memory Compression" folyamatnál a teljes munkakészlet a tömörített tár mérete.
                let mem_mb = if name_lc == "memory compression" {
                    working_set_mb
                } else if let Some(&b) = private_ws.get(&pid).filter(|b| **b > 0) {
                    b as f64 / 1_048_576.0
                } else {
                    match extra {
                        Some(e) if e.private_bytes > 0.0 => e.private_bytes / 1_048_576.0,
                        _ => working_set_mb,
                    }
                };
                let session_id =
                    *session_cache.entry(pid).or_insert_with(|| session_of_pid(pid).unwrap_or(0));
                // Becsült energiafogyasztás: a CPU- és GPU-terhelés, valamint a lemez- és hálózati forgalom súlyozott összege.
                let cpu_share = (p.cpu_usage() / cores).clamp(0.0, 100.0);
                let gpu_share = counters.per_pid.get(&pid).copied().unwrap_or(0.0);
                let io_now = extra.map_or(0.0, |e| e.io_bps);
                let power = cpu_share
                    + 0.6 * gpu_share
                    + (io_now / 1_048_576.0) as f32 * 2.0
                    + ((net_up + net_down) / 1_048_576.0) as f32;
                let ema = power_ema.entry(pid).or_insert(power);
                *ema += power_alpha * (power - *ema);
                let power_trend = *ema;
                let io_total = if want_io {
                    let d = p.disk_usage();
                    d.total_read_bytes + d.total_written_bytes
                } else {
                    0
                };
                ProcInfo {
                    pid,
                    name_lc,
                    name,
                    exe,
                    mem_mb,
                    cpu: (p.cpu_usage() / cores).clamp(0.0, 100.0),
                    gpu: counters.per_pid.get(&pid).copied().unwrap_or(0.0),
                    gpu_engine: counters.engine_label.get(&pid).cloned().unwrap_or_default(),
                    io_bps: extra.map_or(0.0, |e| e.io_bps),
                    net_bps: net_up + net_down,
                    net_up_bps: net_up,
                    net_down_bps: net_down,
                    publisher,
                    cmd,
                    power,
                    power_trend,
                    io_total,
                    session_id,
                    ws_mb: working_set_mb,
                    user,
                    arch,
                    desc,
                    status: if hung_pids.contains(&pid) { 1 } else { 0 },
                    has_window: app_pids.contains(&pid),
                    cpu_time_ms: p.accumulated_cpu_time(),
                    parent_pid: p.parent().map(|pp| pp.as_u32()).unwrap_or(0),
                }
            })
            .collect();
        self.session_cache = session_cache;
        let live: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
        power_ema.retain(|pid, _| live.contains(pid));
        self.power_ema = power_ema;
        meta.prune(&live);
        self.meta = meta;

        // A tömörített memória a "Memory Compression" rendszerfolyamat munkakészlete.
        let compressed = processes
            .iter()
            .find(|p| p.name_lc == "memory compression")
            .map(|p| p.mem_mb);

        let cpu = self.cpu.sample(&self.sys);
        let mem = self.mem.sample(&self.sys, compressed);
        let gpus = self.gpus.iter_mut().map(|b| b.sample(&counters)).collect();
        let disks = self.disks.sample(&self.disk_static);
        let nets = self.nets.sample();
        let sessions = list_sessions();

        Snapshot {
            cpu,
            mem,
            gpus,
            disks,
            nets,
            sessions,
            // SAFETY: egyszerű, paraméter nélküli Win32 hívás.
            uptime_secs: unsafe { GetTickCount64() } / 1000,
            process_count: processes.len(),
            processes,
            app_history: AppHistory::default(),
            net_state: self.net_state,
        }
    }
}

/// A "legutóbbi snapshot" tároló. Az UI try_lock-kal olvassa, így sosem blokkol.
struct Shared {
    latest: Mutex<Option<Snapshot>>,
}

struct MonitorHandle {
    shared: Arc<Shared>,
    events: Receiver<Event>,
    cmd: Sender<Command>,
    icon_tx: Sender<String>,
    icon_rx: Receiver<IconMsg>,
}

fn start_monitor(ctx: egui::Context, interval_ms: Arc<AtomicU32>) -> Option<MonitorHandle> {
    let shared = Arc::new(Shared { latest: Mutex::new(None) });
    let (ev_tx, ev_rx) = mpsc::sync_channel::<Event>(64);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (icon_req_tx, icon_req_rx) = mpsc::channel::<String>();
    let (icon_res_tx, icon_res_rx) = mpsc::channel::<IconMsg>();

    let worker_shared = Arc::clone(&shared);
    let worker_ctx = ctx.clone();
    thread::Builder::new()
        .name("hw-monitor".into())
        .spawn(move || monitor_thread(worker_ctx, worker_shared, ev_tx, cmd_rx, interval_ms))
        .ok()?;
    // Az ikon-szál hibája nem végzetes: ikonok nélkül is működik minden.
    let _ = thread::Builder::new()
        .name("icon-loader".into())
        .spawn(move || icon_thread(icon_req_rx, icon_res_tx, ctx));
    Some(MonitorHandle {
        shared,
        events: ev_rx,
        cmd: cmd_tx,
        icon_tx: icon_req_tx,
        icon_rx: icon_res_rx,
    })
}

fn monitor_thread(
    ctx: egui::Context,
    shared: Arc<Shared>,
    events: SyncSender<Event>,
    commands: Receiver<Command>,
    interval_ms: Arc<AtomicU32>,
) {
    lower_current_thread_priority();

    let mut collector = match catch_unwind(AssertUnwindSafe(Collector::new)) {
        Ok((c, info)) => {
            let _ = events.try_send(Event::Hello(info));
            c
        }
        Err(_) => {
            let _ = events.try_send(Event::Warning(tr("Hardware detection crashed.").to_string()));
            ctx.request_repaint();
            return; // a szál véget ér; az UI ezt észleli és felkínálja az újraindítást
        }
    };

    // A mintavételezési idő menet közben is változhat (Beállítások csúszka): minden körben újraolvassuk.
    let current_interval =
        || Duration::from_millis(interval_ms.load(AtomicOrdering::Relaxed).clamp(250, 5000) as u64);
    let mut last_sample = Instant::now();
    let mut next_tick = Instant::now(); // az első mérés azonnal
    // Lassú gépen (pl. régi, egymagos processzoron) maga a mérés lenne a fő terhelés: a következő mérés előtti szünet
    // legalább a mérési idő kétszerese, így a mintavevő legfeljebb ~1/3 processzoridőt használ.
    let mut backoff = Duration::ZERO;
    let mut avatars_sent: HashSet<String> = HashSet::new();
    let mut history = AppHistoryTracker::new();
    loop {
        // Rövidebb intervallumra váltásnál a következő mérést előrehozzuk, nem várjuk ki a régi (akár 5 s-os) időt.
        next_tick = next_tick.min(last_sample + current_interval().max(backoff));
        // Várakozás a következő tickig, de a parancs azonnal felébreszt. Legfeljebb 250 ms-ot várunk egyben,
        // hogy az új intervallum gyorsan érvényesüljön.
        let wait = next_tick.saturating_duration_since(Instant::now()).min(Duration::from_millis(250));
        match commands.recv_timeout(wait) {
            Ok(Command::Kill { label, targets }) => {
                let mut killed = 0usize;
                let mut errors = Vec::new();
                for (pid, name) in &targets {
                    match kill_process(*pid, name) {
                        Ok(()) => killed += 1,
                        Err(e) => errors.push(format!("{name} (PID {pid}): {e}")),
                    }
                }
                if let Err(TrySendError::Disconnected(_)) =
                    events.try_send(Event::KillResult { label, killed, errors })
                {
                    break;
                }
                ctx.request_repaint();
                next_tick = Instant::now() + Duration::from_millis(250); // gyors frissítés
                continue;
            }
            Ok(Command::ResetAppHistory) => {
                history.reset();
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() < next_tick {
                    continue; // még nem járt le az intervallum
                }
            }
            Err(RecvTimeoutError::Disconnected) => break, // az UI bezárult
        }

        let collect_started = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| collector.collect())) {
            Ok(mut snapshot) => {
                // Alkalmazásonkénti előzmények (a tracker hibája nem viheti magával a mintavételt).
                snapshot.app_history = catch_unwind(AssertUnwindSafe(|| {
                    history.update(&snapshot.processes, snapshot.net_state == NetState::Active, is_metered());
                    history.history()
                }))
                .unwrap_or_default();
                let users: Vec<String> = snapshot.sessions.iter().map(|s| s.user.clone()).collect();
                {
                    // Mérgezett mutex esetén is továbbmegyünk (az adat csak egy Option<Snapshot>).
                    let mut guard = shared.latest.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = Some(snapshot);
                }
                // Az új felhasználók profilképét egyszer töltjük be és küldjük el.
                for user in users {
                    if !user.is_empty() && avatars_sent.insert(user.clone()) {
                        let image = catch_unwind(AssertUnwindSafe(|| load_user_avatar(&user))).ok().flatten();
                        let _ = events.try_send(Event::UserPicture { user, image });
                    }
                }
            }
            Err(_) => {
                let _ = events.try_send(Event::Warning(
                    tr("A hardware query failed; retrying in the next cycle.").to_string(),
                ));
            }
        }
        ctx.request_repaint(); // az egui Context szálbiztos
        backoff = (collect_started.elapsed() * 2).min(Duration::from_secs(8));
        last_sample = Instant::now();
        next_tick = last_sample + current_interval().max(backoff);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. UI – segédrajzolók
// ═══════════════════════════════════════════════════════════════════════════

fn comma(s: String) -> String {
    s.replace('.', ",")
}

fn fmt_opt(v: Option<f32>, unit: &str, dec: usize) -> String {
    match v {
        Some(x) => comma(format!("{x:.dec$} {unit}")),
        None => "—".to_string(),
    }
}

/// MB-ból "123,4 MB" vagy (1 GB fölött) "1,2 GB".
fn fmt_mb(mb: f64) -> String {
    if mb >= 1024.0 {
        comma(format!("{:.1} GB", mb / 1024.0))
    } else {
        comma(format!("{mb:.1} MB"))
    }
}

fn fmt_gb(mb: f64) -> String {
    comma(format!("{:.1} GB", mb / 1024.0))
}

fn fmt_uptime(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    format!("{d}:{h:02}:{m:02}:{s:02}")
}

fn temp_color(c: f32) -> Color32 {
    if c < 60.0 {
        OK_GREEN
    } else if c < 80.0 {
        WARN_AMBER
    } else {
        DANGER_RED
    }
}

fn usage_color(pct: f32) -> Color32 {
    if pct < 70.0 {
        DATA_BLUE
    } else if pct < 90.0 {
        WARN_AMBER
    } else {
        DANGER_RED
    }
}

fn push_hist(h: &mut VecDeque<f32>, v: f32) {
    let cap = hist_cap();
    while h.len() >= cap {
        h.pop_front();
    }
    h.push_back(v);
}

/// A háttéreffekt felirata a Beállításokban.
fn backdrop_label(b: settings::Backdrop) -> &'static str {
    match b {
        settings::Backdrop::Auto => tr("Automatic (matches your system)"),
        settings::Backdrop::Mica => tr("Mica (Windows 11)"),
        settings::Backdrop::Acrylic => tr("Acrylic (Windows 10 / 11)"),
        settings::Backdrop::Solid => tr("Solid"),
    }
}

fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(26.0).strong());
    ui.label(RichText::new(subtitle).size(13.0).color(col_muted()));
    ui.add_space(14.0);
}

/// Részletező oldal fejléce: bal oldalt a cím, jobb oldalt a (halvány) alcím.
fn detail_header(ui: &mut Ui, title: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(24.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(right).size(14.0).color(col_muted()));
        });
    });
    ui.add_space(6.0);
}

/// Lekerekített, áttetsző "kártya" (a Beállítások és az Információ oldalhoz).
fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::col_card())
        .stroke(Stroke::new(1.0_f32, overlay(16)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(12.0);
}

fn stat(ui: &mut Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(12.0).color(col_muted()));
        ui.label(RichText::new(value).size(18.0).strong());
    });
}

/// Kulcs–érték sor (statikus információk): halvány címke + érték.
fn kv(ui: &mut Ui, k: &str, v: &str) {
    ui.label(RichText::new(k).size(12.5).color(col_muted()));
    ui.label(RichText::new(v).size(13.0));
    ui.end_row();
}

/// A grafikon háttere és (opcionális) rácsvonalai.
/// A téglalap szélei a fizikai képpontrácsra igazítva. Tört koordinátáknál (pl. 125 / 150 %-os méretezés) a háttér és a
/// vonal másképp kerekítődik, és a vonal "kicsúszik" a háttérből; így mindkettő ugyanarra a rácsra esik.
fn snap_rect(painter: &egui::Painter, rect: Rect) -> Rect {
    let ppp = painter.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(snap(rect.left()), snap(rect.top())), pos2(snap(rect.right()), snap(rect.bottom())))
}

fn draw_graph_bg(painter: &egui::Painter, rect: Rect, grid: bool) {
    let rect = snap_rect(painter, rect);
    painter.rect_filled(rect, CornerRadius::same(6), graph_bg());
    if grid {
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.line_segment(
                [pos2(rect.left(), y), pos2(rect.right(), y)],
                Stroke::new(1.0_f32, overlay(9)),
            );
        }
    }
}

/// Egy idősor (0..`max` skálán) vonallal és (opcionálisan) kitöltéssel.
fn draw_series(painter: &egui::Painter, rect: Rect, data: &VecDeque<f32>, color: Color32, max: f32, fill: bool) {
    if data.len() < 2 || max <= 0.0 {
        return;
    }
    // A rajz a háttérrel azonos pixelrácsra esik, és SEMMI nem lóghat ki a grafikon téglalapjából: a festő vágása a
    // téglalapra áll (a tile többi része – margók, szöveg – érintetlen marad).
    let rect = snap_rect(painter, rect);
    let painter = &painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    // A belső margó legalább akkora, mint a háttér sarok-lekerekítése, így a vonal nem lóghat ki a sarkoknál.
    let inner = rect.shrink(3.0);
    let cap = hist_cap();
    let step = inner.width() / (cap as f32 - 1.0);
    // Csak az utolsó `cap` mintát rajzoljuk: ha több lenne (pl. az időablak csökkentése után), a régebbiek nem
    // csúszhatnak ki balra a téglalapból.
    let skip = data.len().saturating_sub(cap);
    let n = data.len() - skip;
    if n < 2 {
        return;
    }
    let vals: Vec<f32> =
        data.iter().skip(skip).map(|v| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 }).collect();
    let pts: Vec<Pos2> = vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = inner.right() - (n - 1 - i) as f32 * step;
            pos2(x, inner.bottom() - (v / max) * inner.height())
        })
        .collect();
    if fill {
        // A kitöltést szegmensenként trapézokból építjük (mindegyik konvex).
        for w in pts.windows(2) {
            painter.add(Shape::convex_polygon(
                vec![w[0], w[1], pos2(w[1].x, inner.bottom()), pos2(w[0].x, inner.bottom())],
                color.gamma_multiply(0.22),
                Stroke::NONE,
            ));
        }
    }
    // A vonalat szegmensenként rajzoljuk: az egybefüggő poliline éles csúcsoknál (pl. lemez-forgalom ugrásai)
    // hosszú, kilógó "tüskéket" (miter) képezhet, a külön szakaszok nem.
    let stroke = Stroke::new(1.5_f32, color);
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
}

/// Háttér + egy kitöltött idősor.
fn draw_graph(painter: &egui::Painter, rect: Rect, data: &VecDeque<f32>, color: Color32, grid: bool, max: f32) {
    draw_graph_bg(painter, rect, grid);
    draw_series(painter, rect, data, color, max, true);
}

/// Teljes szélességű százalékos (0–100) grafikon a rendelkezésre álló helyen.
fn usage_graph(ui: &mut Ui, data: &VecDeque<f32>, color: Color32, height: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let painter = ui.painter_at(rect);
    draw_graph(&painter, rect, data, color, true, 100.0);
    resp
}

/// Több idősor egy grafikonon, közös, megadott skálával (átviteli sebességekhez).
fn scaled_graph(ui: &mut Ui, series: &[(&VecDeque<f32>, Color32)], max: f32, height: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let painter = ui.painter_at(rect);
    draw_graph_bg(&painter, rect, true);
    for (i, (data, color)) in series.iter().enumerate() {
        draw_series(&painter, rect, data, *color, max, i == 0);
    }
    resp
}

/// Grafikon-menü csak "Másolás" sorral (ahol nincs választható nézet).
fn copy_menu(resp: &egui::Response, copy: &str) {
    resp.context_menu(|ui| {
        menus::graph_menu(ui, &[], copy);
    });
}

fn cpu_graph_menu(ui: &mut Ui, perf: &mut settings::PerfPrefs, copy: &str) {
    let items = [(tr("Overall utilization"), Some(!perf.cpu_numa)), (tr("NUMA nodes"), Some(perf.cpu_numa))];
    if let Some(i) = menus::graph_menu(ui, &items, copy) {
        perf.cpu_numa = i == 1;
    }
}

fn disk_graph_menu(ui: &mut Ui, perf: &mut settings::PerfPrefs, copy: &str) {
    let items = [
        (tr("Separate read and write graphs"), Some(!perf.disk_combined)),
        (tr("Combined read and write graph"), Some(perf.disk_combined)),
    ];
    if let Some(i) = menus::graph_menu(ui, &items, copy) {
        perf.disk_combined = i == 1;
    }
}

fn gpu_graph_menu(ui: &mut Ui, perf: &mut settings::PerfPrefs, copy: &str) {
    let items = [(tr("Summary view"), Some(!perf.gpu_engines)), (tr("Engine view"), Some(perf.gpu_engines))];
    if let Some(i) = menus::graph_menu(ui, &items, copy) {
        perf.gpu_engines = i == 1;
    }
}

/// Szóközzel tagolt egész szám ("178 685").
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// NUMA-csomópontonként egy grafikon (a csomópont logikai processzorainak átlagos terhelése).
fn numa_graphs(ui: &mut Ui, nodes: &[VecDeque<f32>], perf: &mut settings::PerfPrefs, copy: &str) {
    let gap = 12.0;
    let cols = if nodes.len() > 1 { 2usize } else { 1 };
    let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
    for (row, chunk) in nodes.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (j, h) in chunk.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(w);
                    let last = h.back().copied().unwrap_or(0.0);
                    graph_caption(ui, &format!("{} {}", tr("NUMA node"), row * cols + j), &format!("{last:.0}%"));
                    let resp = usage_graph(ui, h, CPU_COLOR, 110.0);
                    resp.context_menu(|ui| cpu_graph_menu(ui, perf, copy));
                });
            }
        });
        ui.add_space(8.0);
    }
}

/// A videókártya motoronkénti nézete (3D, Copy, Video Encode, Video Decode): 2×2 grafikon, mint a Feladatkezelőben.
fn gpu_engine_graphs(
    ui: &mut Ui,
    hist: &[VecDeque<f32>; 4],
    t: &GpuTelemetry,
    perf: &mut settings::PerfPrefs,
    copy: &str,
) {
    const NAMES: [&str; 4] = ["3D", "Copy", "Video Encode", "Video Decode"];
    let gap = 12.0;
    let w = (ui.available_width() - gap) / 2.0;
    let h = (w * 0.3).clamp(80.0, 220.0);
    for row in 0..2 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for col in 0..2 {
                let k = row * 2 + col;
                ui.vertical(|ui| {
                    ui.set_width(w);
                    graph_caption(ui, NAMES[k], &format!("{:.0}%", t.engines[k]));
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
                    let painter = ui.painter_at(rect);
                    draw_graph_bg(&painter, rect, true);
                    // A Feladatkezelőhöz hasonlóan függőleges rács is van.
                    let snapped = snap_rect(&painter, rect);
                    for i in 1..10 {
                        let x = snapped.left() + snapped.width() * i as f32 / 10.0;
                        painter.line_segment(
                            [pos2(x, snapped.top()), pos2(x, snapped.bottom())],
                            Stroke::new(1.0_f32, overlay(9)),
                        );
                    }
                    draw_series(&painter, rect, &hist[k], GPU_COLOR, 100.0, true);
                    resp.context_menu(|ui| gpu_graph_menu(ui, perf, copy));
                });
            }
        });
        ui.add_space(6.0);
    }
}

fn series_max(series: &[&VecDeque<f32>]) -> f32 {
    series.iter().flat_map(|d| d.iter().copied()).fold(0.0_f32, f32::max)
}

/// A következő "szép" skálahatár (1, 2, 5 × 10^k), legalább `min`.
fn nice_max(m: f32, min: f32) -> f32 {
    let m = m.max(min);
    let exp = 10f32.powf(m.log10().floor());
    let f = m / exp;
    let n = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    n * exp
}

/// Grafikon-felirat: bal oldalt a név, jobb oldalt a skála maximuma.
fn graph_caption(ui: &mut Ui, left: &str, right: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(left).size(12.0).color(col_muted()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(right).size(12.0).color(col_muted()));
        });
    });
}

/// Bájtban megadott méret (kapacitás): "931 GB", "1,82 TB".
fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_099_511_627_776.0 {
        comma(format!("{:.2} TB", b / 1_099_511_627_776.0))
    } else if b >= 1_073_741_824.0 {
        comma(format!("{:.0} GB", b / 1_073_741_824.0))
    } else if b >= 1_048_576.0 {
        comma(format!("{:.0} MB", b / 1_048_576.0))
    } else {
        format!("{bytes} B")
    }
}

/// Bájt/másodperc -> "12,3 MB/s".
fn fmt_bytes_per_s(bps: f64) -> String {
    if bps >= 1_048_576.0 {
        comma(format!("{:.1} MB/s", bps / 1_048_576.0))
    } else if bps >= 1024.0 {
        comma(format!("{:.0} KB/s", bps / 1024.0))
    } else {
        format!("{bps:.0} B/s")
    }
}

/// Bájt/másodperc -> bit/másodperc a hálózati szokás szerint: "0 Kb/s", "12,3 Mb/s".
fn fmt_bits_per_s(bytes_per_s: f64) -> String {
    let bits = bytes_per_s * 8.0;
    if bits >= 1e9 {
        comma(format!("{:.2} Gb/s", bits / 1e9))
    } else if bits >= 1e6 {
        comma(format!("{:.1} Mb/s", bits / 1e6))
    } else {
        format!("{:.0} Kb/s", bits / 1e3)
    }
}

/// A kapcsolat névleges sebessége: "1 Gb/s", "866,7 Mb/s".
fn fmt_link_speed(bps: u64) -> String {
    if bps == 0 {
        "—".to_string()
    } else if bps >= 1_000_000_000 {
        comma(format!("{:.1} Gb/s", bps as f64 / 1e9))
    } else {
        comma(format!("{:.0} Mb/s", bps as f64 / 1e6))
    }
}

/// "K: 0 F: 0 Kb/s" – a küldés és fogadás egy sorban (a csempéhez).
fn net_pair(send: f64, recv: f64) -> String {
    let (s, r) = (send * 8.0, recv * 8.0);
    let (a, b) = (tr("S"), tr("R")); // küldés / fogadás rövidítése
    if s.max(r) >= 1e6 {
        comma(format!("{a}: {:.1} {b}: {:.1} Mb/s", s / 1e6, r / 1e6))
    } else {
        format!("{a}: {:.0} {b}: {:.0} Kb/s", s / 1e3, r / 1e3)
    }
}

/// "0. lemez (C:)" / "Disk 0 (C:)"
fn disk_title(index: u32, letters: &str) -> String {
    let l = if letters.is_empty() { String::new() } else { format!(" ({letters})") };
    trf("Disk {0}{1}", &[&index, &l])
}

/// Magonkénti vízszintes sáv: "Mag 3 ▮▮▮▯▯ 42 %".
fn bar_cell(ui: &mut Ui, label: &str, pct: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 22.0), Sense::hover());
    let p = ui.painter_at(rect);
    let cy = rect.center().y;
    p.text(pos2(rect.left(), cy), Align2::LEFT_CENTER, label, FontId::proportional(12.0), col_muted());
    let bar = Rect::from_min_max(pos2(rect.left() + 52.0, cy - 4.0), pos2(rect.right() - 46.0, cy + 4.0));
    p.rect_filled(bar, CornerRadius::same(4), overlay(14));
    let frac = (pct / 100.0).clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height()));
        p.rect_filled(fill, CornerRadius::same(4), usage_color(pct));
    }
    p.text(
        pos2(rect.right(), cy),
        Align2::RIGHT_CENTER,
        format!("{pct:.0} %"),
        FontId::proportional(12.0),
        col_text(),
    );
}

fn core_bars(ui: &mut Ui, usage: &[f32]) {
    let gap = 14.0;
    let total_w = ui.available_width();
    // Soronként legfeljebb 2 mag (keskeny ablakban 1), hogy a sávok szélesek és olvashatók maradjanak.
    let cols = (((total_w + gap) / (190.0 + gap)).floor() as usize).clamp(1, 2);
    let cell_w = (total_w - gap * (cols as f32 - 1.0)) / cols as f32;
    for (row, chunk) in usage.chunks(cols).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (j, u) in chunk.iter().enumerate() {
                bar_cell(ui, &format!("{} {}", tr("Core"), row * cols + j), *u, cell_w);
            }
        });
    }
}

/// Háromszög alakú nyíl (rendezés irányához / csoport lenyitásához) – tisztán vektoros, nem font.
fn tri(painter: &egui::Painter, c: Pos2, dir: u8, color: Color32) {
    let s = 3.5;
    let pts = match dir {
        0 => vec![pos2(c.x - s, c.y - s * 0.6), pos2(c.x + s, c.y - s * 0.6), pos2(c.x, c.y + s * 0.8)], // le
        1 => vec![pos2(c.x - s, c.y + s * 0.6), pos2(c.x + s, c.y + s * 0.6), pos2(c.x, c.y - s * 0.8)], // fel
        _ => vec![pos2(c.x - s * 0.6, c.y - s), pos2(c.x - s * 0.6, c.y + s), pos2(c.x + s * 0.8, c.y)], // jobbra
    };
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A menüpont ikonja.
#[derive(Clone, Copy)]
enum NavIcon {
    /// Szöveges szimbólum (az egui betűkészlete rajzolja).
    Glyph(&'static str),
    /// Óra körkörös nyíllal (Alkalmazáselőzmények).
    History,
    /// Pontozott lista (Részletek).
    List,
    /// Kirakós darab (Szolgáltatások).
    Puzzle,
}

/// Körív pontjai a 16×16-os tervezési dobozban. A szögek fokban, képernyő-irányban (0° = jobbra, 90° = lefelé,
/// 270° = felfelé); `a1` lehet kisebb is, mint `a0`.
fn arc_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32, n: usize) -> Vec<(f32, f32)> {
    (0..=n)
        .map(|i| {
            let a = (a0 + (a1 - a0) * i as f32 / n as f32).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Kirajzolja a menüikont a `center` köré, `size` méretben, `color` színnel. A vektoros ikonok vonalrajzok (mint a
/// Windows 11 Feladatkezelő ikonjai), ezért élesek minden felbontáson, és követik a szöveg színét.
fn paint_nav_icon(p: &egui::Painter, icon: NavIcon, center: Pos2, size: f32, color: Color32) {
    if let NavIcon::Glyph(g) = icon {
        p.text(center, Align2::CENTER_CENTER, g, FontId::proportional(size), color);
        return;
    }
    let k = size / 16.0; // a rajz 16×16-os dobozban készült
    let origin = center - vec2(8.0, 8.0) * k;
    let pt = |x: f32, y: f32| pos2(origin.x + x * k, origin.y + y * k);
    let pts = |v: &[(f32, f32)]| -> Vec<Pos2> { v.iter().map(|&(x, y)| pt(x, y)).collect() };
    let stroke = Stroke::new(1.3 * k, color);
    match icon {
        NavIcon::History => {
            // Óralap: a bal oldalon nyitott körív, a nyitott végen nyílhegy; benne a mutatók.
            let arc = arc_pts(8.6, 8.0, 5.2, 205.0, 515.0, 28);
            let (ax, ay) = arc[0];
            p.add(Shape::line(pts(&arc), stroke));
            p.add(Shape::line(vec![pt(8.6, 5.0), pt(8.6, 8.0), pt(10.6, 9.4)], stroke));
            p.add(Shape::line(vec![pt(ax - 0.2, ay - 2.3), pt(ax, ay), pt(ax + 2.4, ay + 0.2)], stroke));
        }
        NavIcon::List => {
            for y in [4.0_f32, 8.0, 12.0] {
                p.circle_filled(pt(2.9, y), 0.95 * k, color);
                p.line_segment([pt(6.2, y), pt(13.8, y)], stroke);
            }
        }
        NavIcon::Puzzle => {
            // Kirakós darab: tetején és jobb oldalán kidudorodás, bal oldalán bemélyedés.
            let mut v: Vec<(f32, f32)> = vec![(3.0, 5.0), (6.2, 5.0)];
            v.extend(arc_pts(8.0, 5.0, 1.8, 180.0, 360.0, 16));
            v.extend([(9.8, 5.0), (13.0, 5.0), (13.0, 7.2)]);
            v.extend(arc_pts(13.0, 9.0, 1.8, 270.0, 450.0, 16));
            v.extend([(13.0, 10.8), (13.0, 13.0), (3.0, 13.0), (3.0, 10.8)]);
            v.extend(arc_pts(3.0, 9.0, 1.8, 90.0, -90.0, 16));
            v.extend([(3.0, 7.2), (3.0, 5.0)]);
            p.add(Shape::line(pts(&v), stroke));
        }
        NavIcon::Glyph(_) => {}
    }
}

/// Bal oldali menüpont: ikon + felirat; az aktív elem bal szélén vékony, függőleges csík (a Windows kiemelő színével).
fn nav_item(ui: &mut Ui, icon: NavIcon, title: &str, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let cy = rect.center().y;
        let pill = rect.shrink2(vec2(4.0, 2.0));
        if selected {
            p.rect_filled(pill, CornerRadius::same(8), overlay(22));
        } else if resp.hovered() {
            p.rect_filled(pill, CornerRadius::same(8), overlay(11));
        }
        if selected {
            let bar = Rect::from_min_size(pos2(rect.left() + 2.0, cy - 10.0), vec2(3.0, 20.0));
            p.rect_filled(bar, CornerRadius::same(2), accent());
        }
        let col = if selected { col_text() } else { theme::nav_text() };
        paint_nav_icon(p, icon, pos2(rect.left() + 24.0, cy), 16.0, col);
        p.text(pos2(rect.left() + 46.0, cy), Align2::LEFT_CENTER, title, FontId::proportional(14.0), col);
    }
    resp
}

/// A Teljesítmény fül bal oldali "csempéje": mini grafikon + cím + két sor adat (mint a Task Managerben).
fn perf_tile(
    ui: &mut Ui,
    selected: bool,
    title: &str,
    line1: &str,
    line2: &str,
    hist: &VecDeque<f32>,
    color: Color32,
    max: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 68.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        if selected {
            // A kijelölt csempe a Windows kiemelő színével van megjelölve.
            painter.rect_filled(rect, CornerRadius::same(8), accent().gamma_multiply(0.28));
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(8), overlay(12));
        }
        let graph = Rect::from_min_size(rect.left_top() + vec2(8.0, 9.0), vec2(80.0, 50.0));
        draw_graph(&painter, graph, hist, color, false, max);
        let text_clip = Rect::from_min_max(pos2(graph.right() + 8.0, rect.top()), rect.right_bottom());
        let tp = painter.with_clip_rect(text_clip);
        let x = graph.right() + 12.0;
        tp.text(pos2(x, rect.top() + 9.0), Align2::LEFT_TOP, title, FontId::proportional(14.5), col_text());
        tp.text(pos2(x, rect.top() + 29.0), Align2::LEFT_TOP, line1, FontId::proportional(12.0), col_muted());
        tp.text(pos2(x, rect.top() + 45.0), Align2::LEFT_TOP, line2, FontId::proportional(12.0), col_muted());
    }
    resp
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. UI – a Teljesítmény fül részletező oldalai
// ═══════════════════════════════════════════════════════════════════════════

/// Reszponzív statisztika-rács a nagy grafikonok alatt. Egy sorban legfeljebb `max_cols` adat áll (alapesetben 3);
/// ha az ablak összehúzásától ennyi már nem fér el egymás mellett, akkor csak 2, a többi pedig a következő sorokba,
/// egymás alá kerül.
fn stat_flow(ui: &mut Ui, id: impl std::hash::Hash, max_cols: usize, items: &[(&str, String)]) {
    const GAP: f32 = 36.0; // oszlopköz
    const MIN_COL_W: f32 = 170.0; // ennél keskenyebb oszlopba az adat már nem fér el kényelmesen
    const MAX_COL_W: f32 = 190.0;
    let avail = ui.available_width();
    // Annyi oszlop, ahány MIN_COL_W szélességgel elfér; legalább 2, legfeljebb `max_cols`.
    let fit = ((avail + GAP) / (MIN_COL_W + GAP)).floor() as usize;
    let cols = fit.clamp(2, max_cols.max(2));
    let col_w = ((avail - GAP * (cols as f32 - 1.0)) / cols as f32).clamp(120.0, MAX_COL_W);
    egui::Grid::new((id, cols)).num_columns(cols).min_col_width(col_w).spacing([GAP, 26.0]).show(ui, |ui| {
        for (i, (label, value)) in items.iter().enumerate() {
            stat(ui, label, value);
            if (i + 1) % cols == 0 {
                ui.end_row();
            }
        }
    });
}

/// Az adatblokkok kerete a Teljesítmény fülön (ugyanaz a stílus, mint a processzor kártyáján): halvány, lekerekített
/// háttér, az oldal teljes szélességében.
fn data_frame(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(overlay(8))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// Kulcs–érték rács (statikus információk).
fn kv_grid(id: impl std::hash::Hash) -> egui::Grid {
    egui::Grid::new(id).num_columns(2).min_col_width(210.0).spacing([32.0, 14.0])
}

fn dot(ui: &mut Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    ui.painter().circle_filled(r.center(), 5.0, color);
}

fn cpu_detail(
    ui: &mut Ui,
    s: &CpuStatic,
    t: &CpuTelemetry,
    hist: &VecDeque<f32>,
    numa_hist: &[VecDeque<f32>],
    process_count: usize,
    uptime: u64,
    perf: &mut settings::PerfPrefs,
) {
    detail_header(ui, tr("Processor"), &s.brand);
    ui.label(RichText::new(window_caption()).size(12.0).color(col_muted()));
    let copy = format!("{}: {:.0} %", tr("Processor"), t.usage_total);
    if perf.cpu_numa && !numa_hist.is_empty() {
        numa_graphs(ui, numa_hist, perf, &copy);
    } else {
        let resp = usage_graph(ui, hist, CPU_COLOR, 150.0);
        resp.context_menu(|ui| cpu_graph_menu(ui, perf, &copy));
    }
    ui.add_space(18.0);

    data_frame(ui, |ui| {
        stat_flow(
            ui,
            "cpu_stats",
            3,
            &[
                (tr("Utilization"), format!("{:.0} %", t.usage_total)),
                (tr("Speed"), fmt_opt(t.freq_mhz.map(|f| f / 1000.0), "GHz", 2)),
                (tr("Process count"), process_count.to_string()),
                (tr("Logical processors"), s.logical_cores.to_string()),
                (tr("Vendor"), s.vendor.label().to_string()),
                (tr("Uptime"), fmt_uptime(uptime)),
            ],
        );
    });

    ui.add_space(18.0);
    ui.label(RichText::new(tr("Per-core utilization")).size(15.0).strong());
    ui.add_space(6.0);
    core_bars(ui, &t.per_core_usage);
    ui.add_space(18.0);
    cpu_info_box(ui, s);

    // Hőmérséklet: csak akkor jelenítjük meg, ha natívan (kernel-driver és külső program nélkül) lekérhető.
    if let Some(pkg) = t.temp_package {
        ui.add_space(20.0);
        ui.label(RichText::new(tr("Temperature")).size(15.0).strong());
        ui.add_space(6.0);
        data_frame(ui, |ui| {
            // Összesített hőfok + a részletek (magonként / CCD-nként), ha vannak.
            ui.horizontal(|ui| {
                dot(ui, temp_color(pkg));
                ui.label(RichText::new(tr("Overall")).color(col_muted()));
                ui.label(RichText::new(comma(format!("{pkg:.1} °C"))).size(20.0).strong());
            });
            if t.temps.len() > 1 {
                ui.add_space(10.0);
                kv_grid("cpu_temps").show(ui, |ui| {
                    for r in &t.temps {
                        ui.label(&r.label);
                        ui.label(
                            RichText::new(comma(format!("{:.1} °C", r.celsius)))
                                .color(temp_color(r.celsius))
                                .strong(),
                        );
                        ui.end_row();
                    }
                });
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{}: {}", tr("Source"), t.temp_source)).size(11.5).color(col_muted()));
    }
}

/// A processzor állandó adatai egy kis kártyán: alapsebesség, foglalatok, magok, virtualizáció, gyorsítótárak.
fn cpu_info_box(ui: &mut Ui, s: &CpuStatic) {
    let t = &s.topo;
    let cache = |bytes: u64| -> String {
        if bytes == 0 {
            "—".to_string()
        } else if bytes >= 1_048_576 {
            comma(format!("{:.1} MB", bytes as f64 / 1_048_576.0))
        } else {
            comma(format!("{} KB", bytes / 1024))
        }
    };
    let count = |n: u32| if n > 0 { n.to_string() } else { "—".to_string() };
    let virt_txt: &str = if t.virtualization { tr("Enabled") } else { tr("Disabled") };
    let hyperv_txt: &str = if t.hyperv { tr("Yes") } else { tr("No") };
    let rows: Vec<(&str, String)> = vec![
        (tr("Base speed"), fmt_opt(t.base_mhz.map(|m| m as f32 / 1000.0), "GHz", 2)),
        (tr("Sockets"), count(t.sockets)),
        (tr("Cores"), count(t.cores)),
        (tr("Logical processors"), s.logical_cores.to_string()),
        (tr("Virtualization"), virt_txt.to_string()),
        (tr("Hyper-V support"), hyperv_txt.to_string()),
        (tr("L1 cache"), cache(t.l1_bytes)),
        (tr("L2 cache"), cache(t.l2_bytes)),
        (tr("L3 cache"), cache(t.l3_bytes)),
    ];
    data_frame(ui, |ui| {
        egui::Grid::new("cpu_info_box").num_columns(2).spacing([56.0, 9.0]).show(ui, |ui| {
            for (label, value) in &rows {
                ui.label(RichText::new(*label).color(col_muted()));
                ui.label(RichText::new(value).strong());
                ui.end_row();
            }
        });
    });
}

fn mem_detail(ui: &mut Ui, st: &MemStatic, m: &MemTelemetry, hist: &VecDeque<f32>) {
    let right = if st.kind.is_empty() {
        fmt_gb(st.installed_mb)
    } else {
        format!("{} {}", fmt_gb(st.installed_mb), st.kind)
    };
    detail_header(ui, tr("Memory"), &right);
    graph_caption(ui, tr("Memory usage"), &fmt_gb(m.total_mb));
    let resp = usage_graph(ui, hist, MEM_COLOR, 150.0);
    copy_menu(&resp, &format!("{}: {} / {}", tr("Memory"), fmt_gb(m.used_mb), fmt_gb(m.total_mb)));
    ui.add_space(18.0);

    let in_use = match m.compressed_mb {
        Some(c) => format!("{} ({})", fmt_gb(m.used_mb), fmt_gb(c)),
        None => fmt_gb(m.used_mb),
    };
    let committed = format!(
        "{}/{}",
        comma(format!("{:.1}", m.commit_used_mb / 1024.0)),
        fmt_gb(m.commit_limit_mb)
    );
    data_frame(ui, |ui| {
        stat_flow(
            ui,
            "mem_stats",
            3,
            &[
                (tr("In use (compressed)"), in_use),
                (tr("Available"), fmt_gb(m.available_mb)),
                (tr("Committed"), committed),
                (tr("Cached"), fmt_gb(m.cached_mb)),
                (tr("Paged pool"), fmt_mb(m.paged_pool_mb)),
                (tr("Non-paged pool"), fmt_mb(m.nonpaged_pool_mb)),
            ],
        );
    });

    ui.add_space(10.0);
    data_frame(ui, |ui| {
        kv_grid("mem_static").show(ui, |ui| {
            kv(ui, tr("Speed:"), &st.speed_mts.map_or("—".to_string(), |s| format!("{s} MT/s")));
            kv(ui, tr("Slots used:"), &format!("{}/{}", st.slots_used, st.slots_total));
            kv(ui, tr("Form factor:"), if st.form_factor.is_empty() { "—" } else { &st.form_factor });
            kv(ui, tr("Hardware reserved:"), &st.hw_reserved_mb.map_or("—".to_string(), fmt_mb));
        });
    });
}

fn gpu_detail(
    ui: &mut Ui,
    idx: usize,
    info: &GpuStatic,
    t: &GpuTelemetry,
    hist: &VecDeque<f32>,
    vram_hist: &VecDeque<f32>,
    eng_hist: &[VecDeque<f32>; 4],
    perf: &mut settings::PerfPrefs,
) {
    detail_header(ui, &format!("GPU {idx}"), &format!("{} · {}", info.vendor.label(), info.name));
    ui.label(RichText::new(window_caption()).size(12.0).color(col_muted()));
    let copy = format!("GPU {idx}: {:.0} %", t.utilization.unwrap_or(0.0));
    if perf.gpu_engines {
        gpu_engine_graphs(ui, eng_hist, t, perf, &copy);
    } else {
        let resp = usage_graph(ui, hist, GPU_COLOR, 130.0);
        resp.context_menu(|ui| gpu_graph_menu(ui, perf, &copy));
    }
    ui.add_space(10.0);
    graph_caption(ui, tr("Dedicated GPU memory usage"), &fmt_mb(t.vram_total_mb));
    let resp = usage_graph(ui, vram_hist, MEM_COLOR, 90.0);
    resp.context_menu(|ui| gpu_graph_menu(ui, perf, &copy));
    ui.add_space(18.0);

    let vram = match t.vram_used_mb {
        Some(u) => format!("{} / {}", fmt_mb(u), fmt_mb(t.vram_total_mb)),
        None => format!("— / {}", fmt_mb(t.vram_total_mb)),
    };
    let fan = if t.fan_rpm.is_some() {
        fmt_opt(t.fan_rpm, "rpm", 0)
    } else {
        fmt_opt(t.fan_percent, "%", 0)
    };
    let power = if t.power_w.is_some() {
        fmt_opt(t.power_w, "W", 0)
    } else if t.power_tdp_pct.is_some() {
        fmt_opt(t.power_tdp_pct, "% TDP", 0)
    } else {
        "—".to_string()
    };
    data_frame(ui, |ui| {
        stat_flow(
            ui,
            ("gpu_stats", idx),
            3,
            &[
                (tr("Utilization"), fmt_opt(t.utilization, "%", 0)),
                (tr("Dedicated memory"), vram),
                (tr("Temp."), fmt_opt(t.temp_c, "°C", 0)),
                ("Hotspot", fmt_opt(t.hotspot_c, "°C", 0)),
                (tr("GPU clock"), fmt_opt(t.core_clock_mhz, "MHz", 0)),
                (tr("VRAM clock"), fmt_opt(t.mem_clock_mhz, "MHz", 0)),
                (tr("Power"), power),
                (tr("Fan"), fan),
            ],
        );
    });
    if !t.note.is_empty() {
        ui.add_space(10.0);
        ui.label(RichText::new(format!("{}: {}", tr("Data sources"), t.note)).size(11.5).color(col_muted()));
    }
}

fn disk_detail(
    ui: &mut Ui,
    st: Option<&DiskStatic>,
    t: &DiskTelemetry,
    active: &VecDeque<f32>,
    read: &VecDeque<f32>,
    write: &VecDeque<f32>,
    perf: &mut settings::PerfPrefs,
) {
    let model = st.map_or(String::new(), |s| s.model.clone());
    detail_header(ui, &disk_title(t.index, &t.letters), &model);

    graph_caption(ui, tr("Active time"), "100%");
    let resp = usage_graph(ui, active, DISK_COLOR, 110.0);
    resp.context_menu(|ui| disk_graph_menu(ui, perf, &format!("{}: {:.0} %", tr("Active time"), t.active_pct)));
    ui.add_space(10.0);

    let read_copy = format!("{}: {}", tr("Read speed"), fmt_bytes_per_s(t.read_bps));
    let write_copy = format!("{}: {}", tr("Write speed"), fmt_bytes_per_s(t.write_bps));
    if perf.disk_combined {
        // Közös grafikon: az olvasás és az írás egy skálán.
        let max = nice_max(series_max(&[read, write]), 1_048_576.0);
        graph_caption(ui, tr("Disk transfer rate"), &fmt_bytes_per_s(max as f64));
        let resp = scaled_graph(ui, &[(read, DATA_BLUE), (write, WARN_AMBER)], max, 64.0);
        resp.context_menu(|ui| disk_graph_menu(ui, perf, &format!("{read_copy}, {write_copy}")));
    } else {
        // Külön grafikon, saját skálával (egy közös skálán a kisebb forgalom eltűnne).
        let read_max = nice_max(series_max(&[read]), 1_048_576.0);
        graph_caption(ui, tr("Read speed"), &fmt_bytes_per_s(read_max as f64));
        let resp = scaled_graph(ui, &[(read, DATA_BLUE)], read_max, 64.0);
        resp.context_menu(|ui| disk_graph_menu(ui, perf, &read_copy));
        ui.add_space(10.0);

        let write_max = nice_max(series_max(&[write]), 1_048_576.0);
        graph_caption(ui, tr("Write speed"), &fmt_bytes_per_s(write_max as f64));
        let resp = scaled_graph(ui, &[(write, WARN_AMBER)], write_max, 64.0);
        resp.context_menu(|ui| disk_graph_menu(ui, perf, &write_copy));
    }
    ui.add_space(18.0);

    // A lemeznek 4 adata van: 2×2-es elrendezésben marad (a 3+1 csúnya lenne).
    data_frame(ui, |ui| {
        stat_flow(
            ui,
            ("disk_stats", t.index),
            2,
            &[
                (tr("Active time"), format!("{:.0} %", t.active_pct)),
                (tr("Average response time"), comma(format!("{:.1} ms", t.resp_ms))),
                (tr("Read speed"), fmt_bytes_per_s(t.read_bps)),
                (tr("Write speed"), fmt_bytes_per_s(t.write_bps)),
            ],
        );
    });

    ui.add_space(10.0);
    data_frame(ui, |ui| {
        kv_grid(("disk_info", t.index)).show(ui, |ui| {
            kv(ui, tr("Model:"), if model.is_empty() { "—" } else { &model });
            kv(ui, tr("Capacity:"), &st.map_or("—".to_string(), |s| fmt_size(s.capacity_bytes)));
            kv(ui, tr("Formatted:"), &fmt_size(t.formatted_bytes));
            kv(ui, tr("System disk:"), if t.is_system { tr("Yes") } else { tr("No") });
            kv(ui, tr("Page file:"), if t.has_pagefile { tr("Yes") } else { tr("No") });
            kv(
                ui,
                tr("Type:"),
                match st {
                    Some(s) if !s.kind.is_empty() => &s.kind,
                    _ => "—",
                },
            );
        });
    });
}

fn net_detail(
    ui: &mut Ui,
    n: &NetTelemetry,
    send: &VecDeque<f32>,
    recv: &VecDeque<f32>,
    open_details: &mut Option<String>,
) {
    detail_header(ui, &n.name, &n.desc);

    let max = nice_max(series_max(&[send, recv]), 125_000.0);
    graph_caption(ui, tr("Throughput"), &fmt_bits_per_s(max as f64));
    let resp = scaled_graph(ui, &[(recv, NET_COLOR), (send, DATA_BLUE)], max, 150.0);
    let copy = format!("{}: {}", n.name, net_pair(n.send_bps, n.recv_bps));
    resp.context_menu(|ui| {
        let items: [(&str, Option<bool>); 1] = [(tr("View network details"), None)];
        if menus::graph_menu(ui, &items, &copy).is_some() {
            *open_details = Some(n.name.clone());
        }
    });
    ui.add_space(18.0);

    data_frame(ui, |ui| {
        stat_flow(
            ui,
            ("net_stats", n.name.as_str()),
            2,
            &[(tr("Send"), fmt_bits_per_s(n.send_bps)), (tr("Receive"), fmt_bits_per_s(n.recv_bps))],
        );
    });

    ui.add_space(10.0);
    data_frame(ui, |ui| {
        kv_grid(("net_info", n.name.as_str())).show(ui, |ui| {
            kv(ui, tr("Adapter name:"), &n.desc);
            kv(ui, tr("Connection type:"), n.kind_label());
            kv(ui, tr("IPv4 address:"), n.ipv4.as_deref().unwrap_or("—"));
            kv(ui, tr("IPv6 address:"), n.ipv6.as_deref().unwrap_or("—"));
            kv(ui, tr("Link speed:"), &fmt_link_speed(n.link_bps));
        });
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. Az alkalmazás állapota és a főciklus
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Processes,
    Performance,
    AppHistory,
    Startup,
    Users,
    Details,
    Services,
    Settings,
    About,
}

impl Tab {
    /// A felső (fő) menüpontok.
    const ALL: [Tab; 7] = [
        Tab::Processes,
        Tab::Performance,
        Tab::AppHistory,
        Tab::Startup,
        Tab::Users,
        Tab::Details,
        Tab::Services,
    ];
    /// Az alsó menüpontok (fentről lefelé).
    const BOTTOM: [Tab; 2] = [Tab::Settings, Tab::About];

    fn title(self) -> &'static str {
        match self {
            Tab::Processes => tr("Processes"),
            Tab::Performance => tr("Performance"),
            Tab::AppHistory => tr("App history"),
            Tab::Startup => tr("Startup apps"),
            Tab::Users => tr("Users"),
            Tab::Details => tr("Details"),
            Tab::Services => tr("Services"),
            Tab::Settings => tr("Settings"),
            Tab::About => tr("About"),
        }
    }
    /// A menüpont ikonja: egyszerű szöveges szimbólum (az egui beépített emoji-fontja rajzolja), vagy a Windows 11
    /// ikonjaihoz hasonló, saját vektoros vonalrajz (`NavIcon`). Ha valamelyik szimbólum doboznak látszik, cseréld itt.
    fn icon(self) -> NavIcon {
        match self {
            Tab::Processes => NavIcon::Glyph("☰"),
            Tab::Performance => NavIcon::Glyph("📈"),
            Tab::AppHistory => NavIcon::History,
            Tab::Startup => NavIcon::Glyph("🚀"),
            Tab::Users => NavIcon::Glyph("👥"),
            Tab::Details => NavIcon::List,
            Tab::Services => NavIcon::Puzzle,
            Tab::Settings => NavIcon::Glyph("⚙"),
            Tab::About => NavIcon::Glyph("ℹ"),
        }
    }
}

/// Melyik "csempe" van kiválasztva a Teljesítmény fülön.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PerfSel {
    Cpu,
    Memory,
    Disk(usize),
    Net(usize),
    Gpu(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Type,
    Status,
    Publisher,
    Pid,
    ProcName,
    Cmd,
    Cpu,
    Mem,
    Disk,
    Net,
    Gpu,
    GpuEngine,
    Power,
    PowerTrend,
}

impl SortKey {
    /// Szöveges oszlop (alapból növekvő rendezés); a számoknál a legnagyobb legyen elöl.
    fn is_text(self) -> bool {
        matches!(
            self,
            SortKey::Name
                | SortKey::Type
                | SortKey::Status
                | SortKey::Publisher
                | SortKey::ProcName
                | SortKey::Cmd
                | SortKey::GpuEngine
        )
    }
}

#[derive(Default)]
struct Histories {
    cpu: VecDeque<f32>,
    mem: VecDeque<f32>,
    gpu: Vec<VecDeque<f32>>,
    gpu_vram: Vec<VecDeque<f32>>,
    disk_active: Vec<VecDeque<f32>>,
    disk_read: Vec<VecDeque<f32>>,
    disk_write: Vec<VecDeque<f32>>,
    /// NUMA-csomópontonként a logikai processzorok átlagos terhelése.
    numa: Vec<VecDeque<f32>>,
    /// Videókártyánként a 4 motor (3D, Copy, Video Encode, Video Decode) kihasználtsága.
    gpu_eng: Vec<[VecDeque<f32>; 4]>,
    /// Adapternév -> (küldés, fogadás) bájt/s idősor.
    net: HashMap<String, (VecDeque<f32>, VecDeque<f32>)>,
}

/// Azonos nevű folyamatok csoportja (pl. a böngésző több tucat folyamata).
struct Group {
    key: String, // kisbetűs folyamatnév
    name: String,
    exe: Option<String>,
    members: Vec<ProcInfo>,
    mem_mb: f64,
    cpu: f32,
    gpu: f32,
    /// Alkalmazás (van látható ablaka legalább egy tagnak), különben háttérfolyamat.
    is_app: bool,
}

impl Group {
    fn min_pid(&self) -> u32 {
        self.members.iter().map(|m| m.pid).min().unwrap_or(0)
    }

    /// A csoport tagjainak összes I/O-ja, illetve hálózati forgalma (bájt/s).
    fn io_bps(&self) -> f64 {
        self.members.iter().map(|m| m.io_bps).sum()
    }

    fn net_bps(&self) -> f64 {
        self.members.iter().map(|m| m.net_bps).sum()
    }

    /// Van-e a csoportban nem válaszoló folyamat.
    fn hung(&self) -> bool {
        self.members.iter().any(|m| m.status == 1)
    }

    fn first(&self) -> Option<&ProcInfo> {
        self.members.first()
    }

    /// A csoport becsült energiafogyasztás-pontszáma (összeg) és ennek mozgó átlaga.
    fn power(&self) -> f32 {
        self.members.iter().map(|m| m.power).sum()
    }

    fn power_trend(&self) -> f32 {
        self.members.iter().map(|m| m.power_trend).sum()
    }
}

/// Mi van kijelölve a folyamatlistában.
#[derive(Clone, PartialEq, Eq)]
enum Sel {
    Pid(u32),
    Group(String),
}

#[derive(Clone, Copy)]
enum RowRef {
    /// Szakaszfejléc: (alkalmazások-e, a szakasz csoportjainak száma).
    Section(bool, usize),
    Group(usize),
    Child(usize, usize),
}

enum RowAction {
    Select(Sel),
    Toggle(String),
}

enum IconSlot {
    Pending,
    Missing,
    Ready(TextureHandle),
}

/// A Folyamatok tábla választható oszlopai (a Név oszlop mindig látszik). A sorrend egyben a megjelenítési sorrend és a
/// bitmaszk bitjeinek sorrendje (`settings::PROC_COLUMNS_DEFAULT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PCol {
    Type,
    Status,
    Publisher,
    Pid,
    ProcName,
    Cmd,
    Cpu,
    Mem,
    Disk,
    Net,
    Gpu,
    GpuEngine,
    Power,
    PowerTrend,
}

impl PCol {
    const ALL: [PCol; 14] = [
        PCol::Type,
        PCol::Status,
        PCol::Publisher,
        PCol::Pid,
        PCol::ProcName,
        PCol::Cmd,
        PCol::Cpu,
        PCol::Mem,
        PCol::Disk,
        PCol::Net,
        PCol::Gpu,
        PCol::GpuEngine,
        PCol::Power,
        PCol::PowerTrend,
    ];

    fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Alapértelmezett szélesség (a szöveg két oldalán 10-10 képpont térközzel számolva).
    fn width(self) -> f32 {
        match self {
            PCol::Type => 130.0,
            PCol::Status => 130.0,
            PCol::Publisher => 170.0,
            PCol::Pid => 80.0,
            PCol::ProcName => 170.0,
            PCol::Cmd => 460.0,
            PCol::Cpu => 90.0,
            PCol::Mem => 116.0,
            PCol::Disk => 110.0,
            PCol::Net => 110.0,
            PCol::Gpu => 90.0,
            PCol::GpuEngine => 140.0,
            PCol::Power => 150.0,
            PCol::PowerTrend => 170.0,
        }
    }

    fn right_aligned(self) -> bool {
        matches!(self, PCol::Pid | PCol::Cpu | PCol::Mem | PCol::Disk | PCol::Net | PCol::Gpu)
    }

    fn title(self) -> &'static str {
        match self {
            PCol::Type => tr("Type"),
            PCol::Status => tr("Status"),
            PCol::Publisher => tr("Vendor"),
            PCol::Pid => "PID",
            PCol::ProcName => tr("Process name"),
            PCol::Cmd => tr("Command line"),
            PCol::Cpu => "CPU",
            PCol::Mem => tr("Memory"),
            PCol::Disk => tr("Disk"),
            PCol::Net => tr("Network"),
            PCol::Gpu => "GPU",
            PCol::GpuEngine => tr("GPU engine"),
            PCol::Power => tr("Power usage"),
            PCol::PowerTrend => tr("Power usage trend"),
        }
    }

    fn sort_key(self) -> SortKey {
        match self {
            PCol::Type => SortKey::Type,
            PCol::Status => SortKey::Status,
            PCol::Publisher => SortKey::Publisher,
            PCol::Pid => SortKey::Pid,
            PCol::ProcName => SortKey::ProcName,
            PCol::Cmd => SortKey::Cmd,
            PCol::Cpu => SortKey::Cpu,
            PCol::Mem => SortKey::Mem,
            PCol::Disk => SortKey::Disk,
            PCol::Net => SortKey::Net,
            PCol::Gpu => SortKey::Gpu,
            PCol::GpuEngine => SortKey::GpuEngine,
            PCol::Power => SortKey::Power,
            PCol::PowerTrend => SortKey::PowerTrend,
        }
    }
}

/// A Folyamatok tábla látható oszlopainak helye egy sorban (jobbról balra számolva; fejlécben és adatsorban ugyanaz).
struct ProcCols {
    name_x: f32,
    /// A név-oszlop jobb széle.
    name_right: f32,
    /// A Név oszlop és az első adatoszlop határa.
    name_edge: f32,
    name_clip: Rect,
    /// A látható oszlopok balról jobbra: (oszlop, bal szél, jobb szél).
    cols: Vec<(PCol, f32, f32)>,
}

impl ProcCols {
    /// Az oszlopok a Név oszloptól jobbra, balról jobbra követik egymást. A Név oszlop szélessége alapból kitölti a
    /// maradék helyet; ha a felhasználó állította, az a szélesség marad.
    fn new(rect: Rect, mask: u32, prefs: &ColPrefs) -> Self {
        let cols_w: f32 =
            PCol::ALL.iter().filter(|c| mask & c.bit() != 0).map(|c| prefs.width(*c as usize, c.width())).sum();
        let custom = prefs.width(NAME_SLOT, 0.0);
        let name_w = if custom >= 30.0 { custom } else { (rect.width() - cols_w - 6.0).max(MIN_NAME_W) };
        let name_edge = rect.left() + name_w;
        let mut x = name_edge;
        let mut cols = Vec::new();
        for c in PCol::ALL.iter() {
            if mask & c.bit() == 0 {
                continue;
            }
            let w = prefs.width(*c as usize, c.width());
            cols.push((*c, x, x + w));
            x += w;
        }
        let name_right = name_edge - 4.0;
        let name_clip = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(name_right, rect.bottom()));
        Self { name_x: rect.left() + 62.0, name_right, name_edge, name_clip, cols }
    }

    /// A tábla legkisebb szélessége a látható oszlopokkal: ennél keskenyebb ablakban vízszintes görgetősáv jelenik meg.
    fn min_width(mask: u32, prefs: &ColPrefs) -> f32 {
        let cols: f32 =
            PCol::ALL.iter().filter(|c| mask & c.bit() != 0).map(|c| prefs.width(*c as usize, c.width())).sum();
        let custom = prefs.width(NAME_SLOT, 0.0);
        (if custom >= 30.0 { custom } else { MIN_NAME_W }) + cols + 6.0
    }
}

/// Kör alakú profilkép: a textúrát körlegyezőből (háromszög-legyező) rajzoljuk, megfelelő UV-koordinátákkal.
fn draw_avatar(painter: &egui::Painter, center: Pos2, radius: f32, tex: egui::TextureId) {
    use egui::epaint::{Mesh, Vertex};
    let mut mesh = Mesh::with_texture(tex);
    let segs = 40u32;
    mesh.vertices.push(Vertex { pos: center, uv: pos2(0.5, 0.5), color: Color32::WHITE });
    for i in 0..=segs {
        let a = i as f32 / segs as f32 * std::f32::consts::TAU;
        let (sn, cs) = a.sin_cos();
        mesh.vertices.push(Vertex {
            pos: pos2(center.x + cs * radius, center.y + sn * radius),
            uv: pos2(0.5 + cs * 0.5, 0.5 + sn * 0.5),
            color: Color32::WHITE,
        });
    }
    for i in 0..segs {
        mesh.indices.extend_from_slice(&[0, i + 1, i + 2]);
    }
    painter.add(Shape::mesh(mesh));
}

/// Egy bejelentkezett felhasználó (munkamenet) összesített adatai.
struct UserEntry {
    id: u32,
    user: String,
    /// "TARTOMÁNY\\felhasználó"
    full: String,
    state: u32,
    cpu: f32,
    mem_mb: f64,
    io_bps: f64,
    net_bps: f64,
    gpu: f32,
    /// A munkamenet neve ("Console", "RDP-Tcp#0"…) és a távoli ügyfél gépének neve.
    station: String,
    client: String,
    /// A munkamenet legterheltebb GPU-motorja.
    gpu_engine: String,
}

/// Egy alkalmazás (azonos nevű folyamatok csoportja) egy felhasználó munkamenetében.
struct UserApp {
    name: String,
    /// Kisbetűs folyamatnév (a csoport kulcsa; a kijelöléshez).
    key: String,
    count: usize,
    exe: Option<String>,
    /// A munkamenet ilyen nevű folyamatai: (PID, név) – a bezáráshoz és a helyi menühöz.
    targets: Vec<(u32, String)>,
    has_window: bool,
    cpu: f32,
    mem_mb: f64,
    io_bps: f64,
    net_bps: f64,
    gpu: f32,
}

/// A munkamenet folyamatai alkalmazásonként csoportosítva, memória szerint csökkenően.
fn user_apps(groups: &[Group], session: u32) -> Vec<UserApp> {
    let mut out = Vec::new();
    for g in groups {
        let members: Vec<&ProcInfo> = g.members.iter().filter(|m| m.session_id == session).collect();
        if members.is_empty() {
            continue;
        }
        out.push(UserApp {
            name: g.name.clone(),
            key: g.key.clone(),
            targets: members.iter().map(|m| (m.pid, m.name.clone())).collect(),
            has_window: members.iter().any(|m| m.has_window),
            count: members.len(),
            exe: members.iter().find_map(|m| m.exe.clone()),
            cpu: members.iter().map(|m| m.cpu).sum::<f32>().min(100.0),
            mem_mb: members.iter().map(|m| m.mem_mb).sum(),
            io_bps: members.iter().map(|m| m.io_bps).sum(),
            net_bps: members.iter().map(|m| m.net_bps).sum(),
            gpu: members.iter().map(|m| m.gpu).sum::<f32>().min(100.0),
        });
    }
    out.sort_by(|a, b| b.mem_mb.total_cmp(&a.mem_mb));
    out
}

/// A munkamenet állapota: felirat + szín.
fn session_state(code: u32) -> (&'static str, Color32) {
    match code {
        0 => (tr("Running"), OK_GREEN),
        1 => (tr("Connected"), OK_GREEN),
        4 => (tr("Disconnected"), WARN_AMBER),
        5 => (tr("Idle"), WARN_AMBER),
        _ => (tr("Unknown"), col_muted()),
    }
}

/// A Felhasználók tábla választható oszlopai. A sorrend egyben a megjelenítési sorrend és a bitmaszk bitjeinek
/// sorrendje (`settings::USERS_COLUMNS_DEFAULT`); a Név oszlop mindig látszik.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UCol {
    Id,
    Session,
    Client,
    Status,
    Cpu,
    Mem,
    Disk,
    Net,
    Gpu,
    GpuEngine,
}

impl UCol {
    const ALL: [UCol; 10] = [
        UCol::Id,
        UCol::Session,
        UCol::Client,
        UCol::Status,
        UCol::Cpu,
        UCol::Mem,
        UCol::Disk,
        UCol::Net,
        UCol::Gpu,
        UCol::GpuEngine,
    ];

    fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Az oszlop szélessége képpontban (az alapértelmezett oszlopoké a korábbi elrendezésé).
    fn width(self) -> f32 {
        match self {
            UCol::Id => 90.0,
            UCol::Session => 140.0,
            UCol::Client => 150.0,
            UCol::Status => 130.0,
            UCol::Cpu => 90.0,
            UCol::Mem => 116.0,
            UCol::Disk => 110.0,
            UCol::Net => 110.0,
            UCol::Gpu => 90.0,
            UCol::GpuEngine => 140.0,
        }
    }

    /// A számértékek jobbra, a szövegek balra igazítva jelennek meg.
    fn right_aligned(self) -> bool {
        matches!(self, UCol::Id | UCol::Cpu | UCol::Mem | UCol::Disk | UCol::Net | UCol::Gpu)
    }

    fn title(self) -> &'static str {
        match self {
            UCol::Id => tr("Session ID"),
            UCol::Session => tr("Session"),
            UCol::Client => tr("Client name"),
            UCol::Status => tr("Status"),
            UCol::Cpu => "CPU",
            UCol::Mem => tr("Memory"),
            UCol::Disk => tr("Disk"),
            UCol::Net => tr("Network"),
            UCol::Gpu => "GPU",
            UCol::GpuEngine => tr("GPU engine"),
        }
    }
}

/// A Felhasználók tábla látható oszlopainak helye egy sorban (jobbról balra számolva).
struct UserCols {
    /// A név (avatár mellett) kezdőpontja.
    name_x: f32,
    /// A név-oszlop jobb széle: a hosszú név / teljes név itt vágódik le.
    name_right: f32,
    /// A Név oszlop és az első adatoszlop határa.
    name_edge: f32,
    /// A látható oszlopok balról jobbra: (oszlop, bal szél, jobb szél).
    cols: Vec<(UCol, f32, f32)>,
}

impl UserCols {
    /// Az oszlopok a Név oszloptól jobbra, balról jobbra követik egymást. A Név oszlop szélessége alapból kitölti a
    /// maradék helyet; ha a felhasználó állította, az a szélesség marad.
    fn new(rect: Rect, mask: u32, prefs: &ColPrefs) -> Self {
        let cols_w: f32 =
            UCol::ALL.iter().filter(|c| mask & c.bit() != 0).map(|c| prefs.width(*c as usize, c.width())).sum();
        let custom = prefs.width(NAME_SLOT, 0.0);
        let name_w = if custom >= 30.0 { custom } else { (rect.width() - cols_w - 6.0).max(MIN_NAME_W) };
        let name_edge = rect.left() + name_w;
        let mut x = name_edge;
        let mut cols = Vec::new();
        for c in UCol::ALL.iter() {
            if mask & c.bit() == 0 {
                continue;
            }
            let w = prefs.width(*c as usize, c.width());
            cols.push((*c, x, x + w));
            x += w;
        }
        Self { name_x: rect.left() + 78.0, name_right: name_edge - 8.0, name_edge, cols }
    }

    /// A tábla legkisebb szélessége a látható oszlopokkal: ennél keskenyebb ablakban vízszintes görgetősáv jelenik meg.
    fn min_width(mask: u32, prefs: &ColPrefs) -> f32 {
        let cols: f32 =
            UCol::ALL.iter().filter(|c| mask & c.bit() != 0).map(|c| prefs.width(*c as usize, c.width())).sum();
        let custom = prefs.width(NAME_SLOT, 0.0);
        (if custom >= 30.0 { custom } else { MIN_NAME_W }) + cols + 6.0
    }
}

/// Az "Erőforrásértékek" százalékos megjelenítéséhez szükséges összehasonlító értékek (egy képkockára számolva).
struct PctCtx {
    /// A telepített memória MB-ban.
    total_mem_mb: f64,
    /// A legterheltebb lemez aktív ideje %-ban.
    disk_busy: f64,
    /// Az összes folyamat I/O-ja bájt/másodpercben.
    io_total: f64,
    /// A leggyorsabb hálózati adapter kapcsolati sebessége bit/másodpercben.
    link_bps: f64,
}

impl PctCtx {
    /// A folyamat memóriája a teljes memória százalékában.
    fn mem(&self, mb: f64) -> String {
        if self.total_mem_mb > 0.0 {
            comma(format!("{:.1} %", (mb / self.total_mem_mb * 100.0).min(100.0)))
        } else {
            "—".to_string()
        }
    }

    /// A lemez aktív idejéből a folyamat I/O-forgalmával arányos rész.
    fn disk(&self, bps: f64) -> String {
        if self.io_total > 0.0 {
            comma(format!("{:.1} %", (self.disk_busy * bps / self.io_total).min(100.0)))
        } else {
            comma("0.0 %".to_string())
        }
    }

    /// A folyamat hálózati forgalma a kapcsolat sebességének százalékában.
    fn net(&self, bytes_per_s: f64) -> String {
        if self.link_bps > 0.0 {
            comma(format!("{:.1} %", (bytes_per_s * 8.0 / self.link_bps * 100.0).min(100.0)))
        } else {
            "—".to_string()
        }
    }
}

impl TaskApp {
    fn pct_ctx(&self) -> PctCtx {
        match &self.snap {
            Some(sn) => PctCtx {
                total_mem_mb: sn.mem.total_mb,
                disk_busy: sn.disks.iter().map(|d| d.active_pct as f64).fold(0.0, f64::max),
                io_total: sn.processes.iter().map(|p| p.io_bps).sum(),
                link_bps: sn.nets.iter().map(|n| n.link_bps as f64).fold(0.0, f64::max),
            },
            None => PctCtx { total_mem_mb: 0.0, disk_busy: 0.0, io_total: 0.0, link_bps: 0.0 },
        }
    }
}

/// Üres szöveg helyett gondolatjel.
fn dash_text(s: &str) -> String {
    if s.is_empty() {
        "—".to_string()
    } else {
        s.to_string()
    }
}

/// A "Hálózat adatai" ablak állapota: melyik adapter, az előző mérés (a "/időköz" sorokhoz) és az aktuális számlálók.
#[derive(Default)]
struct NetDetails {
    name: String,
    prev: Option<(Instant, sysdata::NetCounters)>,
    delta: sysdata::NetCounters,
    cur: Option<sysdata::NetCounters>,
}

struct TaskApp {
    monitor: Option<MonitorHandle>,
    monitor_alive: bool,
    info: Option<StaticInfo>,
    snap: Option<Snapshot>,
    groups: Vec<Group>,
    expanded: HashSet<String>,
    icons: HashMap<String, IconSlot>,
    hist: Histories,
    last_update: Instant,
    tab: Tab,
    perf_sel: PerfSel,
    /// A nyitott "Hálózat adatai" ablak (ha van).
    net_details: Option<NetDetails>,
    selected: Option<Sel>,
    confirm: Option<(String, Vec<(u32, String)>)>,
    filter: String,
    sort: SortKey,
    sort_desc: bool,
    status: Option<(String, bool, Instant)>,
    hwnd: Option<HWND>,
    settings: Settings,
    /// Melyik témára (sötét/világos) állítottuk utoljára a DWM-címsort.
    dark_applied: Option<bool>,
    users: Vec<UserEntry>,
    expanded_users: HashSet<u32>,
    /// Felhasználónév -> profilkép-textúra.
    avatars: HashMap<String, TextureHandle>,
    avatar_queue: Vec<(String, usize, usize, Vec<u8>)>,
    /// Részletek fül: szűrő, rendezési oszlop, irány, vízszintes görgetés.
    det_filter: String,
    /// Rendszergazdai (emelt) jogosultsággal fut-e a program.
    elevated: bool,
    /// A mintavételezési idő ezredmásodpercben; a `hw-monitor` szál minden körben innen olvassa.
    interval_ms: Arc<AtomicU32>,
    /// A Szolgáltatások / Indítási alkalmazások háttérszála és annak legutóbbi adatai.
    lists: Option<lists::ListsHandle>,
    lists_data: lists::ListsData,
    lists_version: u64,
    /// Alkalmazáselőzmények (a Snapshotból átvéve).
    app_history: AppHistory,
    /// Futtatható-fájlnév (kisbetűs) -> (összes processzoridő ms, memória MB): az indítási hatás becsléséhez.
    run_stats: HashMap<String, (u64, f64)>,
    /// Futtatható-fájlnév (kisbetűs) -> a futó folyamatok indulásuk óta végzett összes lemez-I/O-ja bájtban.
    run_io: HashMap<String, u64>,
    /// A három listás fül UI-állapota (rendezés, szűrők).
    tabs: tabs::TabState,
    /// Hány frame-et rajzoltunk eddig (az ablak megjelenése utáni DWM-frissítés időzítéséhez).
    frames: u32,
    /// Ennyi frame múlva kell a DWM-keretet újraszámoltatni (a Mica csak így lesz azonnal érvényes).
    refresh_in: Option<u32>,
    /// A DWM-frissítés 1 képpontos "bökése" után vissza kell állítani az ablak szélességét.
    nudge_back: bool,
    /// A program ikonja: a natív ablaknak (tálca) és a bal sáv fejlécének.
    window_icon: Option<Arc<egui::IconData>>,
    app_icon: Option<TextureHandle>,
    /// Az Információ fül adatai (külső about.json), és hogy újra kell-e olvasni.
    about: about::AboutInfo,
    about_stale: bool,
    /// A háttéreffekt indulás utáni önellenőrző javításának állapota (0 = nincs folyamatban).
    fix_stage: u8,
    fix_at: Instant,
    /// Hány egymást követő mérésnél volt tiszta fekete a bal sáv (az átlátszó felület nem működik).
    black_hits: u8,
    /// Futás közbeni őrzés (lásd `watch_backdrop`): a következő ellenőrzés ideje, a gyanús (fehér) mérések száma, a
    /// javítások közti szünet vége, a sikertelen javítások száma, hogy a most futó javítást az őrző indította-e,
    /// az utolsó `update` ideje és hogy az ablak előtte fókuszban volt-e.
    watch_next: Instant,
    watch_bad: u8,
    watch_cooldown_until: Instant,
    watch_strikes: u32,
    fix_from_watch: bool,
    last_update_at: Instant,
    was_focused: bool,
}

impl TaskApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 1) Beállítások, nyelv, téma (a mentett értékek; hiányuknál az alapértékek).
        let settings = Settings::load();
        i18n::apply(&settings.lang);
        theme::poll_accent(); // a Windows kiemelő színe, MIELŐTT a stílusok felépülnek
        apply_style(&cc.egui_ctx);
        apply_theme_pref(&cc.egui_ctx, settings.theme);

        // 2) Natív Windows 11 Mica a címsávban (a DWM-en át, hardveresen). A címsor sötét/világos
        //    módját az első frame-ben a feloldott témához igazítjuk (lásd `update`).
        // Ha a megjelenítés szoftveres (nincs használható videókártya-gyorsítás), az átlátszó felület feketén jelenne
        // meg, ezért ilyenkor eleve tömör háttérrel indulunk (a mentett beállítás nem változik).
        #[cfg(feature = "gpu-wgpu")]
        if let Some(rs) = &cc.wgpu_render_state {
            if rs.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
                backdrop::force_solid();
            }
        }
        let hwnd = hwnd_from_creation_context(cc);
        if let Some(h) = hwnd {
            let c = theme::nav_fill(false);
            backdrop::apply(h.0 as isize, true, settings.backdrop, (c.r(), c.g(), c.b()));
            // A natív címsor szövegét és ikonját elrejtjük: a saját (nagyobb) ikonunkat és címünket a bal sáv tetején
            // rajzoljuk, így nem jelenik meg duplán.
            shell::hide_caption_text_and_icon(h.0 as isize);
            if settings.always_on_top {
                shell::set_topmost(h.0 as isize, true);
            }
            tray::configure(h.0 as isize, tray_mask(settings.tray_mode));
        }
        let window_icon = load_icon_data().map(Arc::new);
        let app_icon = load_icon_data().map(|d| {
            cc.egui_ctx.load_texture(
                "app_icon",
                ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.rgba),
                TextureOptions::LINEAR,
            )
        });
        let about = about::AboutInfo::load(is_elevated());

        // 3) Időzítés (mentett értékekből) és háttérszálak indítása.
        let interval_ms = Arc::new(AtomicU32::new((settings.sample_interval_secs * 1000.0).round() as u32));
        set_hist_params(settings.history_capacity(), settings.chart_window_secs);
        let monitor = start_monitor(cc.egui_ctx.clone(), Arc::clone(&interval_ms));
        let lists = lists::start(cc.egui_ctx.clone());
        Self {
            monitor_alive: monitor.is_some(),
            monitor,
            info: None,
            snap: None,
            groups: Vec::new(),
            expanded: HashSet::new(),
            icons: HashMap::new(),
            hist: Histories::default(),
            last_update: Instant::now(),
            tab: Tab::Processes,
            perf_sel: PerfSel::Cpu,
            net_details: None,
            selected: None,
            confirm: None,
            filter: String::new(),
            sort: SortKey::Mem,
            sort_desc: true,
            status: None,
            hwnd,
            settings,
            dark_applied: None,
            users: Vec::new(),
            expanded_users: HashSet::new(),
            avatars: HashMap::new(),
            avatar_queue: Vec::new(),
            det_filter: String::new(),
            elevated: is_elevated(),
            interval_ms,
            lists,
            lists_data: lists::ListsData::default(),
            lists_version: 0,
            app_history: AppHistory::default(),
            run_stats: HashMap::new(),
            run_io: HashMap::new(),
            tabs: tabs::TabState::default(),
            frames: 0,
            refresh_in: Some(10), // az ablak megjelenése után néhány frame-mel: DWM-keret frissítése
            nudge_back: false,
            window_icon,
            app_icon,
            about,
            about_stale: false,
            fix_stage: 0,
            fix_at: Instant::now(),
            black_hits: 0,
            watch_next: Instant::now() + Duration::from_secs(5),
            watch_bad: 0,
            watch_cooldown_until: Instant::now(),
            watch_strikes: 0,
            fix_from_watch: false,
            last_update_at: Instant::now(),
            was_focused: true,
        }
    }

    fn set_status(&mut self, msg: String, ok: bool) {
        self.status = Some((msg, ok, Instant::now()));
    }

    fn restart_monitor(&mut self, ctx: &egui::Context) {
        self.monitor = start_monitor(ctx.clone(), Arc::clone(&self.interval_ms));
        self.monitor_alive = self.monitor.is_some();
        self.last_update = Instant::now();
        // Az új ikon-szál nem ismeri a korábbi kéréseket: a függőben lévőket újra kell kérni.
        self.icons.retain(|_, v| !matches!(v, IconSlot::Pending));
        self.set_status(tr("Background thread restarted.").to_string(), true);
    }

    /// A háttérszálból érkező adatok átvétele – SOSEM blokkol.
    fn pump(&mut self) {
        let Some(m) = &self.monitor else { return };
        let mut events = Vec::new();
        let mut disconnected = false;
        loop {
            match m.events.try_recv() {
                Ok(e) => events.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let latest = match m.shared.latest.try_lock() {
            Ok(mut g) => g.take(),
            Err(TryLockError::Poisoned(p)) => p.into_inner().take(),
            Err(TryLockError::WouldBlock) => None, // a háttérszál éppen ír: a következő frame-ben jön
        };

        for e in events {
            self.handle_event(e);
        }
        if let Some(s) = latest {
            self.on_snapshot(s);
        }
        if disconnected {
            self.monitor_alive = false;
        }
    }

    /// A kész ikonok átvétele és textúrává alakítása (frame-enként legfeljebb 24 darab).
    fn pump_icons(&mut self, ctx: &egui::Context) {
        // A beérkezett profilképekből textúra lesz.
        for (user, w, h, rgba) in std::mem::take(&mut self.avatar_queue) {
            let img = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            let tex = ctx.load_texture(format!("avatar:{user}"), img, TextureOptions::LINEAR);
            self.avatars.insert(user, tex);
        }
        let Some(m) = &self.monitor else { return };
        let mut got = Vec::new();
        for _ in 0..24 {
            match m.icon_rx.try_recv() {
                Ok(msg) => got.push(msg),
                Err(_) => break,
            }
        }
        let more = got.len() == 24;
        for msg in got {
            let slot = match msg.image {
                Some((w, h, rgba)) => {
                    let img = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                    IconSlot::Ready(ctx.load_texture(
                        format!("icon:{}", msg.path),
                        img,
                        TextureOptions::LINEAR,
                    ))
                }
                None => IconSlot::Missing,
            };
            self.icons.insert(msg.path, slot);
        }
        if more {
            ctx.request_repaint();
        }
    }

    fn request_icon(&mut self, path: &str) {
        if self.icons.contains_key(path) {
            return;
        }
        self.icons.insert(path.to_string(), IconSlot::Pending);
        if let Some(m) = &self.monitor {
            let _ = m.icon_tx.send(path.to_string());
        }
    }

    fn handle_event(&mut self, e: Event) {
        match e {
            Event::Hello(info) => self.info = Some(info),
            Event::KillResult { label, killed, errors } => {
                if errors.is_empty() {
                    self.selected = None;
                    let msg = trf("“{0}” ended ({1} process(es)).", &[&label, &killed]);
                    self.set_status(msg, true);
                } else {
                    let msg = trf("“{0}”: {1} error(s) – {2}", &[&label, &errors.len(), &errors[0]]);
                    self.set_status(msg, false);
                }
            }
            Event::Warning(w) => self.set_status(w, false),
            Event::UserPicture { user, image } => {
                if let Some((w, h, rgba)) = image {
                    self.avatar_queue.push((user, w, h, rgba));
                }
            }
        }
    }

    fn on_snapshot(&mut self, mut s: Snapshot) {
        push_hist(&mut self.hist.cpu, s.cpu.usage_total);
        if let Some(info) = &self.info {
            let nodes = &info.cpu.topo.numa_nodes;
            while self.hist.numa.len() < nodes.len() {
                self.hist.numa.push(VecDeque::new());
            }
            for (h, procs) in self.hist.numa.iter_mut().zip(nodes) {
                let vals: Vec<f32> = procs.iter().filter_map(|&i| s.cpu.per_core_usage.get(i).copied()).collect();
                let avg = if vals.is_empty() { 0.0 } else { vals.iter().sum::<f32>() / vals.len() as f32 };
                push_hist(h, avg);
            }
        }
        let mem_pct = if s.mem.total_mb > 0.0 { (s.mem.used_mb / s.mem.total_mb * 100.0) as f32 } else { 0.0 };
        push_hist(&mut self.hist.mem, mem_pct);
        while self.hist.gpu.len() < s.gpus.len() {
            self.hist.gpu.push(VecDeque::new());
            self.hist.gpu_vram.push(VecDeque::new());
        }
        for ((h, hv), g) in self.hist.gpu.iter_mut().zip(self.hist.gpu_vram.iter_mut()).zip(&s.gpus) {
            push_hist(h, g.utilization.unwrap_or(0.0));
            let vram_pct = match g.vram_used_mb {
                Some(u) if g.vram_total_mb > 0.0 => (u / g.vram_total_mb * 100.0) as f32,
                _ => 0.0,
            };
            push_hist(hv, vram_pct);
        }
        while self.hist.gpu_eng.len() < s.gpus.len() {
            self.hist.gpu_eng.push(Default::default());
        }
        for (h, g) in self.hist.gpu_eng.iter_mut().zip(&s.gpus) {
            for (k, hh) in h.iter_mut().enumerate() {
                push_hist(hh, g.engines[k]);
            }
        }
        while self.hist.disk_active.len() < s.disks.len() {
            self.hist.disk_active.push(VecDeque::new());
            self.hist.disk_read.push(VecDeque::new());
            self.hist.disk_write.push(VecDeque::new());
        }
        for (i, d) in s.disks.iter().enumerate() {
            push_hist(&mut self.hist.disk_active[i], d.active_pct);
            push_hist(&mut self.hist.disk_read[i], d.read_bps as f32);
            push_hist(&mut self.hist.disk_write[i], d.write_bps as f32);
        }
        for n in &s.nets {
            let e = self.hist.net.entry(n.name.clone()).or_default();
            push_hist(&mut e.0, n.send_bps as f32);
            push_hist(&mut e.1, n.recv_bps as f32);
        }
        self.app_history = std::mem::take(&mut s.app_history);
        let procs = std::mem::take(&mut s.processes);
        self.update_run_stats(&procs);
        self.rebuild_groups(procs);
        self.rebuild_users(&s.sessions);
        self.last_update = Instant::now();
        self.snap = Some(s);
    }

    /// A felhasználónkénti (munkamenet-szintű) összegzések: CPU, memória, I/O, GPU a munkamenet folyamataiból.
    fn rebuild_users(&mut self, sessions: &[SessionInfo]) {
        let mut out = Vec::new();
        for sess in sessions {
            let (mut cpu, mut mem, mut io, mut gpu) = (0.0_f32, 0.0_f64, 0.0_f64, 0.0_f32);
            let mut net = 0.0_f64;
            let (mut best_gpu, mut engine) = (0.0_f32, String::new());
            for g in &self.groups {
                for m in &g.members {
                    if m.session_id == sess.id {
                        if m.gpu > best_gpu && !m.gpu_engine.is_empty() {
                            best_gpu = m.gpu;
                            engine = m.gpu_engine.clone();
                        }
                        cpu += m.cpu;
                        mem += m.mem_mb;
                        io += m.io_bps;
                        net += m.net_bps;
                        gpu += m.gpu;
                    }
                }
            }
            let full = if sess.domain.is_empty() {
                sess.user.clone()
            } else {
                format!("{}\\{}", sess.domain, sess.user)
            };
            out.push(UserEntry {
                id: sess.id,
                user: sess.user.clone(),
                full,
                state: sess.state,
                cpu: cpu.min(100.0),
                mem_mb: mem,
                io_bps: io,
                net_bps: net,
                gpu: gpu.min(100.0),
                station: sess.station.clone(),
                client: sess.client.clone(),
                gpu_engine: engine,
            });
        }
        self.users = out;
    }

    /// A folyamatok csoportosítása név szerint (mint a Task Manager tr("Apps") nézete).
    fn rebuild_groups(&mut self, procs: Vec<ProcInfo>) {
        let mut map: HashMap<String, Group> = HashMap::new();
        for p in procs {
            let g = map.entry(p.name_lc.clone()).or_insert_with(|| Group {
                key: p.name_lc.clone(),
                name: p.name.clone(),
                exe: None,
                members: Vec::new(),
                mem_mb: 0.0,
                cpu: 0.0,
                gpu: 0.0,
                is_app: false,
            });
            g.mem_mb += p.mem_mb;
            g.cpu += p.cpu;
            g.gpu += p.gpu;
            if g.exe.is_none() {
                g.exe = p.exe.clone();
            }
            g.members.push(p);
        }
        self.groups = map.into_values().collect();
        for g in &mut self.groups {
            g.cpu = g.cpu.min(100.0);
            g.gpu = g.gpu.min(100.0);
            g.is_app = g.members.iter().any(|m| m.has_window);
            // Megjelenített név: az alkalmazás fájlleírása (pl. "Google Chrome"); ha nincs, a programfájl neve
            // (pl. "chrome.exe"). A csoport kulcsa (`key`) továbbra is a kisbetűs programfájlnév.
            if let Some(d) = g.members.iter().map(|m| m.desc.trim()).find(|d| !d.is_empty()) {
                g.name = d.to_string();
            }
        }
        self.resort();
    }

    fn resort(&mut self) {
        let key = self.sort;
        let desc = self.sort_desc;
        let cmp_proc = |a: &ProcInfo, b: &ProcInfo| match key {
            SortKey::Name => a.name_lc.cmp(&b.name_lc).then(a.pid.cmp(&b.pid)),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
            SortKey::Mem => a.mem_mb.total_cmp(&b.mem_mb),
            SortKey::Gpu => a.gpu.total_cmp(&b.gpu),
            SortKey::Type => a.has_window.cmp(&b.has_window),
            SortKey::Status => a.status.cmp(&b.status),
            SortKey::Publisher => a.publisher.to_lowercase().cmp(&b.publisher.to_lowercase()),
            SortKey::ProcName => a.name_lc.cmp(&b.name_lc),
            SortKey::Cmd => a.cmd.to_lowercase().cmp(&b.cmd.to_lowercase()),
            SortKey::Disk => a.io_bps.total_cmp(&b.io_bps),
            SortKey::Net => a.net_bps.total_cmp(&b.net_bps),
            SortKey::GpuEngine => a.gpu_engine.cmp(&b.gpu_engine),
            SortKey::Power => a.power.total_cmp(&b.power),
            SortKey::PowerTrend => a.power_trend.total_cmp(&b.power_trend),
        };
        for g in &mut self.groups {
            g.members.sort_by(|a, b| cmp_proc(a, b));
            if desc {
                g.members.reverse();
            }
        }
        self.groups.sort_by(|a, b| match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Pid => a.min_pid().cmp(&b.min_pid()),
            SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
            SortKey::Mem => a.mem_mb.total_cmp(&b.mem_mb),
            SortKey::Gpu => a.gpu.total_cmp(&b.gpu),
            SortKey::Type => a.is_app.cmp(&b.is_app),
            SortKey::Status => a.hung().cmp(&b.hung()),
            SortKey::Publisher => a
                .first()
                .map(|m| m.publisher.to_lowercase())
                .cmp(&b.first().map(|m| m.publisher.to_lowercase())),
            SortKey::ProcName => a.key.cmp(&b.key),
            SortKey::Cmd => a
                .first()
                .map(|m| m.cmd.to_lowercase())
                .cmp(&b.first().map(|m| m.cmd.to_lowercase())),
            SortKey::Disk => a.io_bps().total_cmp(&b.io_bps()),
            SortKey::Net => a.net_bps().total_cmp(&b.net_bps()),
            SortKey::GpuEngine => a
                .first()
                .map(|m| m.gpu_engine.clone())
                .cmp(&b.first().map(|m| m.gpu_engine.clone())),
            SortKey::Power => a.power().total_cmp(&b.power()),
            SortKey::PowerTrend => a.power_trend().total_cmp(&b.power_trend()),
        });
        if desc {
            self.groups.reverse();
        }
    }

    /// A kijelölés alapján a kilövendő folyamatok: (megjelenített név, [(pid, név)]).
    fn selection_targets(&self) -> (String, Vec<(u32, String)>) {
        match &self.selected {
            Some(Sel::Pid(pid)) => {
                for g in &self.groups {
                    for m in &g.members {
                        if m.pid == *pid {
                            return (m.name.clone(), vec![(m.pid, m.name.clone())]);
                        }
                    }
                }
                (String::new(), Vec::new())
            }
            Some(Sel::Group(key)) => self
                .groups
                .iter()
                .find(|g| &g.key == key)
                .map(|g| {
                    (g.name.clone(), g.members.iter().map(|m| (m.pid, m.name.clone())).collect())
                })
                .unwrap_or_default(),
            None => (String::new(), Vec::new()),
        }
    }

    fn send_kill(&self, label: String, targets: Vec<(u32, String)>) {
        if let Some(m) = &self.monitor {
            let _ = m.cmd.send(Command::Kill { label, targets });
        }
    }

    fn status_label(&self) -> (Color32, &'static str) {
        if !self.monitor_alive {
            (DANGER_RED, tr("Background thread stopped"))
        } else if self.snap.is_none() {
            (WARN_AMBER, tr("Loading data…"))
        } else if self.last_update.elapsed()
            > Duration::from_secs_f32((self.settings.sample_interval_secs * 3.0).max(4.0))
        {
            (WARN_AMBER, tr("No fresh data"))
        } else {
            (OK_GREEN, tr("Live data"))
        }
    }

    // ───────────────────────── bal oldali navigáció ─────────────────────────
    fn nav_panel(&mut self, ctx: &egui::Context) {
        // A bal sáv az egyetlen "áttetsző" rész: Mica módban teljesen átlátszó, így a DWM anyaga a címsorral
        // egybefüggően látszik át rajta (a lekerekített tartalmi kártya a jobb oldalon kezdődik).
        let nav_fill = theme::nav_fill(backdrop::resolve(self.settings.backdrop).is_transparent());
        egui::SidePanel::left("nav")
            .exact_width(NAV_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            // A felső margó kicsi: a fejléc (ikon + cím) feljebb kerül, közelebb a címsorhoz.
            .frame(egui::Frame::new().fill(nav_fill).inner_margin(Margin { left: 8, right: 8, top: 4, bottom: 14 }))
            .show(ctx, |ui| {
                // Fejléc: a program saját, nagyobb ikonja és címe (a natív címsor szövege/ikonja el van rejtve).
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let (icon_rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                    if let Some(tex) = &self.app_icon {
                        ui.painter().image(
                            tex.id(),
                            icon_rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(tr("Task Manager")).size(22.0).strong());
                });
                ui.add_space(14.0);

                for tab in Tab::ALL {
                    if nav_item(ui, tab.icon(), tab.title(), self.tab == tab).clicked() {
                        self.tab = tab;
                    }
                }

                // Alul (lentről felfelé): állapotjelző, (szükség esetén) újraindítás, Információ, Beállítások.
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.add_space(4.0);
                    let (dot_color, text) = self.status_label();
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, dot_color);
                        ui.label(RichText::new(text).size(12.0).color(col_muted()));
                    });
                    ui.add_space(6.0);
                    if !self.monitor_alive {
                        let btn = egui::Button::new(tr("Restart background thread"))
                            .min_size(vec2(ui.available_width(), 30.0));
                        if ui.add(btn).clicked() {
                            self.restart_monitor(ctx);
                        }
                        ui.add_space(6.0);
                    }
                    for tab in Tab::BOTTOM.iter().rev() {
                        if nav_item(ui, tab.icon(), tab.title(), self.tab == *tab).clicked() {
                            self.tab = *tab;
                        }
                    }
                });
            });
    }

    // ───────────────────────────── Folyamatok fül ─────────────────────────────
    fn ui_processes(&mut self, ui: &mut Ui) {
        let total = self.snap.as_ref().map_or(0, |s| s.process_count);
        page_header(
            ui,
            tr("Processes"),
            &trf("{0} processes · {1} apps / groups", &[&total, &self.groups.len()]),
        );

        let (sel_label, sel_targets) = self.selection_targets();
        let mut kill_clicked = false;
        let mut new_task_clicked = false;

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(tr("Search by name or PID…"))
                    .desired_width(280.0),
            );
            let (k, n) = menus::task_buttons(ui, &sel_label, sel_targets.len());
            kill_clicked = k;
            new_task_clicked = n;
        });
        if new_task_clicked {
            self.run_menu_action(menus::MenuAction::NewTask);
        }
        if kill_clicked {
            if sel_targets.len() > 1 {
                // Csoport kilövése előtt megerősítést kérünk (egy gombnyomással sok folyamat halna meg).
                self.confirm = Some((sel_label.clone(), sel_targets.clone()));
            } else {
                self.send_kill(sel_label.clone(), sel_targets.clone());
            }
        }

        if let Some((msg, ok, at)) = &self.status {
            if at.elapsed() < Duration::from_secs(8) {
                ui.add_space(4.0);
                ui.label(RichText::new(msg).size(12.5).color(if *ok { OK_GREEN } else { DANGER_RED }));
            }
        }
        ui.add_space(8.0);

        // Oszlopok: a beállított láthatóság és szélességek szerint.
        let mut pprefs = self.settings.tables.get("proc").cloned().unwrap_or_default();
        let pmask = pprefs.vis.unwrap_or(settings::PROC_COLUMNS_DEFAULT);
        // A parancssort a mintavevő csak akkor olvassa, ha az oszlop látszik (egyébként fölösleges és drága).
        WANT_CMDLINE.store(pmask & PCol::Cmd.bit() != 0, AtomicOrdering::Relaxed);
        let net_ok = self.snap.as_ref().map_or(false, |sn| sn.net_state == NetState::Active);

        // Fejléc: kattintható oszlopcímek (rendezés), az oszlopok bal szélét húzva a szélesség állítható, jobb klikk:
        // oszlopválasztó.
        let mut new_sort: Option<SortKey> = None;
        let (hrect, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        // A tábla legalább a látható oszlopokhoz tartozó szélességű: keskenyebb ablakban vízszintes görgetősáv jelenik
        // meg, és a fejléc a tartalommal együtt mozog.
        let content_w = hrect.width().max(ProcCols::min_width(pmask, &pprefs));
        let virt = Rect::from_min_size(
            pos2(hrect.left() - self.tabs.proc_scroll_x, hrect.top()),
            vec2(content_w, hrect.height()),
        );
        let cols = ProcCols::new(virt, pmask, &pprefs);
        let hp = ui.painter_at(hrect);
        let mut menu_act = menus::ColumnMenuAction::None;
        let menu_items: Vec<(usize, &str, bool)> =
            PCol::ALL.iter().map(|c| (*c as usize, c.title(), pmask & c.bit() != 0)).collect();
        let can_reset = pprefs.is_customized();
        // (rendezési kulcs, felirat, balra igazított-e, bal szél, jobb szél, oszlop)
        let mut cells: Vec<(SortKey, &str, bool, f32, f32, Option<PCol>)> =
            vec![(SortKey::Name, tr("Name"), true, cols.name_x - 6.0, cols.name_right, None)];
        for (col, l, r) in &cols.cols {
            cells.push((col.sort_key(), col.title(), !col.right_aligned(), *l, *r, Some(*col)));
        }
        for (key, title, left_aligned, l, r, col) in cells {
            let cy = hrect.center().y;
            let click = Rect::from_min_max(
                pos2(l, hrect.top()),
                pos2(if col.is_some() { r - 5.0 } else { r }, hrect.bottom()),
            )
            .intersect(hrect);
            if click.width() > 1.0 {
                let resp = ui
                    .interact(click, ui.id().with(("sort_hdr", title)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let active = self.sort == key;
                let color = if active { col_text() } else { col_muted() };
                let clipped = hp.with_clip_rect(click);
                let (anchor, x) =
                    if left_aligned { (Align2::LEFT_CENTER, l + 10.0) } else { (Align2::RIGHT_CENTER, r - 10.0) };
                let title_font = FontId::proportional(12.5);
                let title_shown = fit_text(&clipped, title, &title_font, (r - l) - 34.0);
                let title_rect = clipped.text(pos2(x, cy), anchor, title_shown, title_font, color);
                if active {
                    let ax = if left_aligned { title_rect.right() + 10.0 } else { title_rect.left() - 10.0 };
                    let dir = if self.sort_desc { 0 } else { 1 };
                    tri(&clipped, pos2(ax, cy), dir, color);
                }
                if resp.clicked() {
                    new_sort = Some(key);
                }
                resp.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(pprefs.pct)));
            }
            // Oszlopelválasztó az oszlop bal szélén, és átméretező fogantyú a jobb szélén (az oszlopok határa mindkét
            // oldalról húzható). A Név oszlopé a név-oszlop jobb széle.
            if col.is_some() {
                hp.line_segment(
                    [pos2(l, hrect.top() + 8.0), pos2(l, hrect.bottom() - 8.0)],
                    Stroke::new(1.0_f32, overlay(24)),
                );
            }
            let edge = if col.is_some() { r } else { cols.name_edge };
            let slot = col.map_or(NAME_SLOT, |c| c as usize);
            let grip = Rect::from_min_max(pos2(edge - 4.0, hrect.top()), pos2(edge + 4.0, hrect.bottom()))
                .intersect(hrect);
            if grip.width() > 1.0 {
                let g = ui
                    .interact(grip, ui.id().with(("proc_grip", slot)), Sense::click_and_drag())
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                if g.hovered() || g.dragged() {
                    hp.line_segment(
                        [pos2(edge, hrect.top() + 6.0), pos2(edge, hrect.bottom() - 6.0)],
                        Stroke::new(1.0_f32, overlay(70)),
                    );
                }
                if g.double_clicked() {
                    pprefs.set_width(slot, 0.0);
                } else if g.dragged() {
                    let (cur, lo) = if col.is_some() { (r - l, COL_MIN_W) } else { (cols.name_edge - virt.left(), 140.0) };
                    pprefs.set_width(slot, (cur + g.drag_delta().x).clamp(lo, COL_MAX_W));
                }
                g.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(pprefs.pct)));
            }
        }
        hp.line_segment([hrect.left_bottom(), hrect.right_bottom()], Stroke::new(1.0_f32, overlay(18)));
        hresp.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(pprefs.pct)));
        match menu_act {
            menus::ColumnMenuAction::Toggle(i) => {
                pprefs.vis = Some(pmask ^ (1 << i));
                pprefs.changed = true;
            }
            menus::ColumnMenuAction::Reset => pprefs.reset(),
            menus::ColumnMenuAction::TogglePct(b) => {
                pprefs.pct ^= b;
                pprefs.changed = true;
            }
            menus::ColumnMenuAction::None => {}
        }
        let rows_prefs = pprefs.clone();
        let rmask = rows_prefs.vis.unwrap_or(settings::PROC_COLUMNS_DEFAULT);
        let pctx = self.pct_ctx();
        let pbits = rows_prefs.pct;
        if pprefs.changed {
            self.settings.tables.insert("proc".to_string(), pprefs);
        }

        // A megjelenítendő sorok kilapítása: két szakasz (Alkalmazások, Háttérfolyamatok), bennük a csoportsorok
        // és a lenyitott gyerekek, a szűrő figyelembevételével.
        let q = self.filter.trim().to_lowercase();
        let mut view: Vec<RowRef> = Vec::new();
        for want_app in [true, false] {
            let mut rows: Vec<RowRef> = Vec::new();
            let mut count = 0usize;
            for (gi, g) in self.groups.iter().enumerate() {
                if g.is_app != want_app {
                    continue;
                }
                let name_hit = q.is_empty() || g.key.contains(&q) || g.name.to_lowercase().contains(&q);
                let pid_hit =
                    !q.is_empty() && g.members.iter().any(|m| m.pid.to_string().contains(&q));
                if !(name_hit || pid_hit) {
                    continue;
                }
                count += 1;
                rows.push(RowRef::Group(gi));
                let show_children = g.members.len() > 1
                    && (self.expanded.contains(&g.key) || (pid_hit && !name_hit));
                if show_children {
                    for mi in 0..g.members.len() {
                        rows.push(RowRef::Child(gi, mi));
                    }
                }
            }
            if count > 0 {
                view.push(RowRef::Section(want_app, count));
                view.extend(rows);
            }
        }

        let selected = self.selected.clone();
        let groups = &self.groups;
        let icons = &self.icons;
        let expanded = &self.expanded;
        let mut action: Option<RowAction> = None;
        let mut want_icons: Vec<String> = Vec::new();
        let mut menu_action: Option<menus::MenuAction> = None;

        // Lista: virtualizált (csak a látható sorokat rajzoljuk) – ezer folyamatnál is könnyű.
        let proc_out = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0; // a show_rows pontos sormagasságot vár
            egui::ScrollArea::both()
                .id_salt("proc_list")
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, view.len(), |ui, range| {
                    for rr in &view[range] {
                        let (g, child) = match *rr {
                            RowRef::Section(is_app, count) => {
                                let (rect, _) = ui.allocate_exact_size(vec2(content_w, ROW_H), Sense::hover());
                                if ui.is_rect_visible(rect) {
                                    let painter = ui.painter();
                                    let title = if is_app { tr("Apps") } else { tr("Background processes") };
                                    painter.text(
                                        pos2(rect.left() + 16.0, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        format!("{title} ({count})"),
                                        FontId::proportional(14.0),
                                        col_text(),
                                    );
                                    painter.line_segment(
                                        [rect.left_bottom(), rect.right_bottom()],
                                        Stroke::new(1.0_f32, overlay(14)),
                                    );
                                }
                                continue;
                            }
                            RowRef::Group(gi) => (&groups[gi], None),
                            RowRef::Child(gi, mi) => (&groups[gi], Some(&groups[gi].members[mi])),
                        };
                        let multi = g.members.len() > 1;
                        let (rect, resp) = ui.allocate_exact_size(vec2(content_w, ROW_H), Sense::click());
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                        // Mit jelent a sor kijelölése / milyen adatot mutat.
                        let sel_target = match child {
                            Some(m) => Sel::Pid(m.pid),
                            None if multi => Sel::Group(g.key.clone()),
                            None => Sel::Pid(g.members[0].pid),
                        };
                        let (cpu, mem, gpu) = match child {
                            Some(m) => (m.cpu, m.mem_mb, m.gpu),
                            None => (g.cpu, g.mem_mb, g.gpu),
                        };
                        let exe: Option<&String> = match child {
                            Some(m) => m.exe.as_ref(),
                            None => g.exe.as_ref(),
                        };
                        let pid_text = match child {
                            Some(m) => m.pid.to_string(),
                            None if multi => String::new(),
                            None => g.members[0].pid.to_string(),
                        };
                        let name_text = match child {
                            Some(m) => m.name.clone(),
                            None if multi => format!("{} ({})", g.name, g.members.len()),
                            None => g.name.clone(),
                        };
                        let indent = if child.is_some() { 22.0 } else { 0.0 };
                        // A további oszlopok adatai (a csoportsor az összegzett / első tag értékeit mutatja).
                        let m0: &ProcInfo = child.unwrap_or(&g.members[0]);
                        let group_row = child.is_none() && multi;
                        let hung = match child {
                            Some(m) => m.status == 1,
                            None => g.hung(),
                        };
                        let (io, net) = match child {
                            Some(m) => (m.io_bps, m.net_bps),
                            None => (g.io_bps(), g.net_bps()),
                        };
                        let (pw, pw_trend) = match child {
                            Some(m) => (m.power, m.power_trend),
                            None => (g.power(), g.power_trend()),
                        };
                        let engine: String = match child {
                            Some(m) => m.gpu_engine.clone(),
                            None => g
                                .members
                                .iter()
                                .filter(|m| !m.gpu_engine.is_empty())
                                .max_by(|a, b| a.gpu.total_cmp(&b.gpu))
                                .map(|m| m.gpu_engine.clone())
                                .unwrap_or_default(),
                        };
                        let cmd_text = if group_row { String::new() } else { m0.cmd.clone() };
                        let type_label: &str = if g.is_app { tr("App") } else { tr("Background process") };

                        if ui.is_rect_visible(rect) {
                            let painter = ui.painter();
                            let row = rect.shrink2(vec2(0.0, 1.0));
                            if selected.as_ref() == Some(&sel_target) {
                                painter.rect_filled(row, CornerRadius::same(6), accent().gamma_multiply(0.35));
                            } else if resp.hovered() {
                                painter.rect_filled(row, CornerRadius::same(6), overlay(10));
                            }
                            let cols = ProcCols::new(rect, rmask, &rows_prefs);
                            let cy = rect.center().y;
                            // Lenyitó nyíl (csak a többtagú csoportoknál).
                            if multi && child.is_none() {
                                let dir = if expanded.contains(&g.key) { 0 } else { 2 };
                                tri(painter, pos2(rect.left() + 20.0, cy), dir, col_muted());
                            }
                            // Alkalmazás-ikon (a még nem ismerteket az ikon-szál tölti be).
                            let icon_rect =
                                Rect::from_center_size(pos2(rect.left() + 44.0 + indent, cy), vec2(18.0, 18.0));
                            menus::paint_icon_slot(painter, exe.and_then(|e| icons.get(e)), exe.is_some(), icon_rect);
                            if let Some(e) = exe {
                                if !icons.contains_key(e) {
                                    want_icons.push(e.clone());
                                }
                            }
                            let font = FontId::proportional(13.5);
                            trunc_clear();
                            let name_shown =
                                fit_text(painter, &name_text, &font, cols.name_right - (cols.name_x + indent) - 8.0);
                            if name_shown != name_text {
                                trunc_push(
                                    Rect::from_min_max(pos2(cols.name_x, rect.top()), pos2(cols.name_right, rect.bottom())),
                                    &name_text,
                                );
                            }
                            painter.with_clip_rect(cols.name_clip).text(
                                pos2(cols.name_x + indent, cy),
                                Align2::LEFT_CENTER,
                                &name_shown,
                                font.clone(),
                                col_text(),
                            );
                            let dim = |v: f32| if v < 0.05 { col_muted() } else { col_text() };
                            let dim_f = |v: f64| if v < 1.0 { col_muted() } else { col_text() };
                            for (col, l, r) in &cols.cols {
                                painter.line_segment(
                                    [pos2(*l, rect.top()), pos2(*l, rect.bottom())],
                                    Stroke::new(1.0_f32, overlay(9)),
                                );
                                let (text, color): (String, Color32) = match col {
                                    PCol::Type => (type_label.to_string(), col_muted()),
                                    PCol::Status => {
                                        if hung {
                                            (tr("Not responding").to_string(), DANGER_RED)
                                        } else {
                                            (tr("Running").to_string(), col_muted())
                                        }
                                    }
                                    PCol::Publisher => (dash_text(&m0.publisher), col_muted()),
                                    PCol::Pid => (pid_text.clone(), col_muted()),
                                    PCol::ProcName => (m0.name.clone(), col_muted()),
                                    PCol::Cmd => (cmd_text.clone(), col_muted()),
                                    PCol::Cpu => (comma(format!("{cpu:.1} %")), dim(cpu)),
                                    PCol::Mem => (if pbits & 1 != 0 { pctx.mem(mem) } else { fmt_mb(mem) }, col_text()),
                                    PCol::Disk => (if pbits & 2 != 0 { pctx.disk(io) } else { fmt_bytes_per_s(io) }, dim_f(io)),
                                    PCol::Net => {
                                        if net_ok {
                                            (if pbits & 4 != 0 { pctx.net(net) } else { fmt_bits_per_s(net) }, dim_f(net))
                                        } else {
                                            ("—".to_string(), col_muted())
                                        }
                                    }
                                    PCol::Gpu => (comma(format!("{gpu:.1} %")), dim(gpu)),
                                    PCol::GpuEngine => (dash_text(&engine), col_muted()),
                                    PCol::Power => {
                                        let (t, c) = power_label(pw);
                                        (t.to_string(), c)
                                    }
                                    PCol::PowerTrend => {
                                        let (t, c) = power_label(pw_trend);
                                        (t.to_string(), c)
                                    }
                                };
                                // Ami nem fér el az oszlopban, azt "…"-tal rövidítjük.
                                let shown = fit_text(painter, &text, &font, (*r - *l) - 20.0);
                                if shown != text {
                                    trunc_push(Rect::from_min_max(pos2(*l, rect.top()), pos2(*r, rect.bottom())), &text);
                                }
                                if col.right_aligned() {
                                    painter.text(pos2(*r - 10.0, cy), Align2::RIGHT_CENTER, shown, font.clone(), color);
                                } else {
                                    painter.text(pos2(*l + 10.0, cy), Align2::LEFT_CENTER, shown, font.clone(), color);
                                }
                            }
                        }

                        trunc_tooltip(ui.ctx(), &resp);
                        if resp.clicked() {
                            let on_arrow = multi
                                && child.is_none()
                                && resp
                                    .interact_pointer_pos()
                                    .map_or(false, |p| p.x < rect.left() + 34.0);
                            action = Some(if on_arrow {
                                RowAction::Toggle(g.key.clone())
                            } else {
                                RowAction::Select(sel_target.clone())
                            });
                        }
                        if resp.double_clicked() && multi && child.is_none() {
                            action = Some(RowAction::Toggle(g.key.clone()));
                        }
                        // Jobb klikk: a sor kijelölődik, és megnyílik a helyi menü (mint a gyári Task Managerben).
                        if resp.secondary_clicked() {
                            action = Some(RowAction::Select(sel_target));
                        }
                        resp.context_menu(|ui| {
                            let spec = menus::ProcMenuSpec::for_row(g, child, expanded.contains(&g.key));
                            menus::process_menu(ui, &spec, &mut menu_action);
                        });
                    }
                })
        });
        self.tabs.proc_scroll_x = proc_out.inner.state.offset.x;

        for path in want_icons {
            self.request_icon(&path);
        }
        match action {
            Some(RowAction::Select(s)) => self.selected = Some(s),
            Some(RowAction::Toggle(k)) => {
                if !self.expanded.remove(&k) {
                    self.expanded.insert(k);
                }
            }
            None => {}
        }
        if let Some(a) = menu_action {
            self.run_menu_action(a);
        }
        if let Some(k) = new_sort {
            if self.sort == k {
                self.sort_desc = !self.sort_desc;
            } else {
                self.sort = k;
                self.sort_desc = !k.is_text(); // számoknál a legnagyobb legyen elöl
            }
            self.resort();
        }
    }

    // ───────────────────────────── Teljesítmény fül ─────────────────────────────
    fn ui_performance(&mut self, ui: &mut Ui) {
        let header_rect = ui.available_rect_before_wrap();
        page_header(ui, tr("Performance"), tr("Real-time hardware diagnostics"));
        // Jobb felül: a Windows Erőforrás-figyelő megnyitása (a fejléc elrendezését nem érinti).
        if menus::header_corner_button(
            ui,
            header_rect,
            "resmon_button",
            menus::SvcIcon::ResMon,
            tr("Resource Monitor"),
            tr("Open the Windows Resource Monitor"),
        ) {
            self.run_menu_action(menus::MenuAction::OpenResourceMonitor);
        }
        self.status_line(ui);
        let (Some(snap), Some(info)) = (&self.snap, &self.info) else {
            ui.label(RichText::new(tr("Loading data…")).color(col_muted()));
            return;
        };
        let sel = self.perf_sel;
        let mut new_sel: Option<PerfSel> = None;
        // A grafikonok jobb gombos menüje által módosítható nézetek, és a "Hálózat adatai" kérés.
        let mut perf = self.settings.perf;
        let mut open_net: Option<String> = None;

        // Bal oldal: a "csempék" (mini grafikon + cím + adatok); jobb oldal: a részletek.
        let full = ui.available_rect_before_wrap();
        let left_w = 232.0;
        let left = Rect::from_min_size(full.min, vec2(left_w, full.height()));
        let right = Rect::from_min_max(pos2(full.min.x + left_w + 18.0, full.min.y), full.max);

        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("perf_tiles")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    // Processzor
                    let cpu_line = format!(
                        "{:.0}% {}",
                        snap.cpu.usage_total,
                        fmt_opt(snap.cpu.freq_mhz.map(|f| f / 1000.0), "GHz", 2)
                    );
                    if perf_tile(ui, sel == PerfSel::Cpu, tr("Processor"), &cpu_line, "", &self.hist.cpu, CPU_COLOR, 100.0)
                        .clicked()
                    {
                        new_sel = Some(PerfSel::Cpu);
                    }
                    // Memória
                    let pct = if snap.mem.total_mb > 0.0 { snap.mem.used_mb / snap.mem.total_mb * 100.0 } else { 0.0 };
                    let mem_line = format!(
                        "{}/{} GB ({pct:.0}%)",
                        comma(format!("{:.1}", snap.mem.used_mb / 1024.0)),
                        comma(format!("{:.1}", snap.mem.total_mb / 1024.0))
                    );
                    if perf_tile(ui, sel == PerfSel::Memory, tr("Memory"), &mem_line, "", &self.hist.mem, MEM_COLOR, 100.0)
                        .clicked()
                    {
                        new_sel = Some(PerfSel::Memory);
                    }
                    // Lemezek
                    for (i, d) in snap.disks.iter().enumerate() {
                        let Some(h) = self.hist.disk_active.get(i) else { continue };
                        let kind = info.disks.iter().find(|s| s.index == d.index).map_or("", |s| s.kind.as_str());
                        let title = disk_title(d.index, &d.letters);
                        let line2 = format!("{:.0}%", d.active_pct);
                        if perf_tile(ui, sel == PerfSel::Disk(i), &title, kind, &line2, h, DISK_COLOR, 100.0).clicked() {
                            new_sel = Some(PerfSel::Disk(i));
                        }
                    }
                    // Hálózati adapterek
                    for (i, n) in snap.nets.iter().enumerate() {
                        let Some((hs, hr)) = self.hist.net.get(&n.name) else { continue };
                        let max = nice_max(series_max(&[hs, hr]), 125_000.0);
                        let line2 = net_pair(n.send_bps, n.recv_bps);
                        if perf_tile(ui, sel == PerfSel::Net(i), &n.name, n.kind_label(), &line2, hr, NET_COLOR, max)
                            .clicked()
                        {
                            new_sel = Some(PerfSel::Net(i));
                        }
                    }
                    // Videókártyák
                    for (i, g) in info.gpus.iter().enumerate() {
                        let (Some(t), Some(h)) = (snap.gpus.get(i), self.hist.gpu.get(i)) else { continue };
                        let line2 = match t.temp_c {
                            Some(c) => format!("{:.0}% ({:.0} °C)", t.utilization.unwrap_or(0.0), c),
                            None => format!("{:.0}%", t.utilization.unwrap_or(0.0)),
                        };
                        if perf_tile(ui, sel == PerfSel::Gpu(i), &format!("GPU {i}"), &g.name, &line2, h, GPU_COLOR, 100.0)
                            .clicked()
                        {
                            new_sel = Some(PerfSel::Gpu(i));
                        }
                    }
                });
        });

        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("perf_detail")
                .auto_shrink([false, false])
                .show(ui, |ui| match sel {
                    PerfSel::Cpu => {
                        cpu_detail(
                            ui,
                            &info.cpu,
                            &snap.cpu,
                            &self.hist.cpu,
                            &self.hist.numa,
                            snap.process_count,
                            snap.uptime_secs,
                            &mut perf,
                        )
                    }
                    PerfSel::Memory => mem_detail(ui, &info.mem, &snap.mem, &self.hist.mem),
                    PerfSel::Disk(i) => {
                        if let (Some(d), Some(a), Some(r), Some(w)) = (
                            snap.disks.get(i),
                            self.hist.disk_active.get(i),
                            self.hist.disk_read.get(i),
                            self.hist.disk_write.get(i),
                        ) {
                            disk_detail(ui, info.disks.iter().find(|s| s.index == d.index), d, a, r, w, &mut perf);
                        }
                    }
                    PerfSel::Net(i) => {
                        if let Some(n) = snap.nets.get(i) {
                            if let Some((hs, hr)) = self.hist.net.get(&n.name) {
                                net_detail(ui, n, hs, hr, &mut open_net);
                            }
                        }
                    }
                    PerfSel::Gpu(i) => {
                        if let (Some(g), Some(t), Some(h), Some(hv), Some(he)) = (
                            info.gpus.get(i),
                            snap.gpus.get(i),
                            self.hist.gpu.get(i),
                            self.hist.gpu_vram.get(i),
                            self.hist.gpu_eng.get(i),
                        ) {
                            gpu_detail(ui, i, g, t, h, hv, he, &mut perf);
                        } else {
                            ui.label(RichText::new(tr("GPU not found.")).color(col_muted()));
                        }
                    }
                });
        });

        if let Some(s) = new_sel {
            self.perf_sel = s;
        }
        if perf != self.settings.perf {
            self.settings.perf = perf;
            self.settings.save();
        }
        if let Some(name) = open_net {
            if self.net_details.as_ref().map_or(true, |d| d.name != name) {
                self.net_details = Some(NetDetails { name, ..NetDetails::default() });
            }
        }
    }

    /// A "Hálózat adatai" ablak (mint a Feladatkezelőé): egy adapter részletes számlálói, lebegő, mozgatható ablakban.
    fn show_net_details(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.net_details.take() else { return };
        let Some(net) = self.snap.as_ref().and_then(|s| s.nets.iter().find(|n| n.name == st.name)).cloned() else {
            return; // az adapter eltűnt: az ablak bezárul
        };
        let interval = self.settings.sample_interval_secs.max(0.25);
        if let Some(c) = sysdata::read_if_counters(net.if_index) {
            let now = Instant::now();
            match st.prev {
                None => st.prev = Some((now, c)),
                Some((t0, p)) if now.duration_since(t0).as_secs_f32() >= interval => {
                    st.delta = c.since(&p);
                    st.prev = Some((now, c));
                }
                _ => {}
            }
            st.cur = Some(c);
        }
        let cur = st.cur.unwrap_or_default();
        let d = st.delta;
        let link = net.link_bps.max(cur.link_bps) as f64;
        let pct = |bytes_per_s: f64| if link > 0.0 { (bytes_per_s * 8.0 / link * 100.0).clamp(0.0, 100.0) } else { 0.0 };
        let p1 = |v: f64| comma(format!("{v:.1}%"));
        let g = group_digits;
        let state = if cur.up { tr("Connected") } else { tr("Disconnected") };
        let rows: Vec<(&str, String)> = vec![
            (tr("Network utilization"), p1(pct(net.send_bps + net.recv_bps))),
            (tr("Link speed"), fmt_link_speed(link as u64)),
            (tr("State"), state.to_string()),
            (tr("Bytes sent throughput"), p1(pct(net.send_bps))),
            (tr("Bytes received throughput"), p1(pct(net.recv_bps))),
            (tr("Bytes throughput"), p1(pct(net.send_bps + net.recv_bps))),
            (tr("Bytes sent"), g(cur.out_octets)),
            (tr("Bytes received"), g(cur.in_octets)),
            (tr("Bytes"), g(cur.out_octets + cur.in_octets)),
            (tr("Bytes sent/interval"), g(d.out_octets)),
            (tr("Bytes received/interval"), g(d.in_octets)),
            (tr("Bytes/interval"), g(d.out_octets + d.in_octets)),
            (tr("Unicasts sent"), g(cur.out_ucast)),
            (tr("Unicasts received"), g(cur.in_ucast)),
            (tr("Unicasts"), g(cur.out_ucast + cur.in_ucast)),
            (tr("Unicasts sent/interval"), g(d.out_ucast)),
            (tr("Unicasts received/interval"), g(d.in_ucast)),
            (tr("Unicasts/interval"), g(d.out_ucast + d.in_ucast)),
            (tr("Non-unicasts sent"), g(cur.out_nucast)),
            (tr("Non-unicasts received"), g(cur.in_nucast)),
            (tr("Non-unicasts"), g(cur.out_nucast + cur.in_nucast)),
            (tr("Non-unicasts sent/interval"), g(d.out_nucast)),
            (tr("Non-unicasts received/interval"), g(d.in_nucast)),
            (tr("Non-unicasts/interval"), g(d.out_nucast + d.in_nucast)),
        ];
        let mut open = true;
        egui::Window::new(tr("Network details"))
            .id(egui::Id::new("net_details_window"))
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("net_details_grid").striped(true).num_columns(2).min_col_width(190.0).spacing([28.0, 5.0]).show(
                        ui,
                        |ui| {
                            ui.label(RichText::new(tr("Property")).strong());
                            ui.label(RichText::new(&net.name).strong());
                            ui.end_row();
                            for (k, v) in &rows {
                                ui.label(RichText::new(*k).color(col_muted()));
                                ui.label(v.as_str());
                                ui.end_row();
                            }
                        },
                    );
                });
            });
        if open {
            self.net_details = Some(st);
        }
    }

    fn content_panel(&mut self, ctx: &egui::Context) {
        let tab = self.tab;
        // Tömör bal sávnál a kártya lekerekített sarka mögött a sáv színe látszik (különben "lyuk" maradna).
        let nav_fill = theme::nav_fill(backdrop::resolve(self.settings.backdrop).is_transparent());
        if nav_fill.a() > 0 {
            let r = ctx.available_rect();
            ctx.layer_painter(egui::LayerId::background()).rect_filled(
                Rect::from_min_size(r.min, vec2(14.0, 14.0)),
                CornerRadius::ZERO,
                nav_fill,
            );
        }
        // A tartalmi rész háttere MINDIG tömör (nincs átlátszóság).
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::content_fill())
                    // Mint a Windows 11 Feladatkezelőben: csak a bal felső sarok lekerekített, a címsorral egybefüggő
                    // bal sáv (Mica) felé.
                    .corner_radius(CornerRadius { nw: 10, ne: 0, sw: 0, se: 0 })
                    .inner_margin(Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| match tab {
                Tab::Processes => self.ui_processes(ui),
                Tab::Performance => self.ui_performance(ui),
                Tab::Settings => self.ui_settings(ui, ctx),
                Tab::About => self.ui_about(ui),
                Tab::Users => self.ui_users(ui),
                Tab::Details => self.ui_details(ui),
                Tab::AppHistory => self.ui_app_history(ui),
                Tab::Startup => self.ui_startup(ui),
                Tab::Services => self.ui_services(ui),
            });
    }

    /// Megerősítő ablak csoport-kilövés előtt.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some((label, targets)) = self.confirm.clone() else { return };
        let mut decision: Option<bool> = None;
        egui::Window::new(tr("Confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf("Do you really want to end all {1} processes of “{0}”?", &[&label, &targets.len()]));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Yes, end")).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                self.send_kill(label, targets);
                self.confirm = None;
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

impl TaskApp {
    // ───────────────────────────── Beállítások fül ─────────────────────────────
    fn ui_settings(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        page_header(ui, tr("Settings"), tr("Language, theme, appearance and timing"));
        let mut changed = false;
        // A csúszkák értékét csak elengedéskor érvényesítjük és mentjük (húzás közben nem írunk lemezre).
        let mut runtime_changed = false;

        card(ui, |ui| {
            egui::Grid::new("settings_grid").num_columns(2).spacing([48.0, 22.0]).show(ui, |ui| {
                // Nyelv: a két beágyazott nyelv + a Langs mappa nyelvfájljai
                ui.label(RichText::new(tr("Language")).size(14.0));
                ui.vertical(|ui| {
                    let langs = i18n::available();
                    let lang_text = if self.settings.lang.eq_ignore_ascii_case("auto") {
                        tr("Automatic (system language)").to_string()
                    } else {
                        langs
                            .iter()
                            .find(|l| l.code.eq_ignore_ascii_case(&self.settings.lang))
                            .map_or_else(|| self.settings.lang.clone(), |l| l.name.clone())
                    };
                    egui::ComboBox::from_id_salt("lang_combo")
                        .width(280.0)
                        .selected_text(lang_text)
                        .show_ui(ui, |ui| {
                            changed |= ui
                                .selectable_value(&mut self.settings.lang, "auto".to_string(), tr("Automatic (system language)"))
                                .changed();
                            for l in &langs {
                                changed |= ui
                                    .selectable_value(&mut self.settings.lang, l.code.clone(), l.name.as_str())
                                    .changed();
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button(tr("Reload language files")).clicked() {
                            i18n::rescan();
                            changed = true; // az aktuális nyelv szótárát is újratölti
                        }
                        if ui.button(tr("Open the language files folder")).clicked() {
                            i18n::open_langs_dir();
                        }
                    });
                    ui.label(
                        RichText::new(tr(
                            "More languages: put a JSON language file (e.g. de.json) into the Langs folder, then reload the language files.",
                        ))
                        .size(12.0)
                        .color(col_muted()),
                    );
                });
                ui.end_row();

                // Téma
                ui.label(RichText::new(tr("Theme")).size(14.0));
                let theme_text = match self.settings.theme {
                    ThemePref::System => tr("Follow system"),
                    ThemePref::Dark => tr("Dark"),
                    ThemePref::Light => tr("Light"),
                };
                egui::ComboBox::from_id_salt("theme_combo")
                    .width(280.0)
                    .selected_text(theme_text)
                    .show_ui(ui, |ui| {
                        changed |= ui
                            .selectable_value(&mut self.settings.theme, ThemePref::System, tr("Follow system"))
                            .changed();
                        changed |= ui.selectable_value(&mut self.settings.theme, ThemePref::Dark, tr("Dark")).changed();
                        changed |= ui.selectable_value(&mut self.settings.theme, ThemePref::Light, tr("Light")).changed();
                    });
                ui.end_row();

                // Háttéreffekt (a bal sáv és a címsor anyaga)
                ui.label(RichText::new(tr("Background effect")).size(14.0));
                egui::ComboBox::from_id_salt("backdrop_combo")
                    .width(280.0)
                    .selected_text(backdrop_label(self.settings.backdrop))
                    .show_ui(ui, |ui| {
                        for b in settings::Backdrop::available() {
                            changed |= ui
                                .selectable_value(&mut self.settings.backdrop, b, backdrop_label(b))
                                .changed();
                        }
                    });
                ui.end_row();

                // Rendszergazdai jog kérése induláskor
                ui.label(RichText::new(tr("Ask for administrator rights at startup")).size(14.0));
                egui::ComboBox::from_id_salt("elevate_combo")
                    .width(280.0)
                    .selected_text(if self.settings.elevate { tr("Yes") } else { tr("No") })
                    .show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut self.settings.elevate, true, tr("Yes")).changed();
                        changed |= ui.selectable_value(&mut self.settings.elevate, false, tr("No")).changed();
                    });
                ui.end_row();

                // Az ablak mindig a többi ablak felett (induláskor is); alapértelmezetten kikapcsolva
                ui.label(RichText::new(tr("Always on top of other windows")).size(14.0));
                egui::ComboBox::from_id_salt("topmost_combo")
                    .width(280.0)
                    .selected_text(if self.settings.always_on_top { tr("Yes") } else { tr("No") })
                    .show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut self.settings.always_on_top, true, tr("Yes")).changed();
                        changed |= ui.selectable_value(&mut self.settings.always_on_top, false, tr("No")).changed();
                    });
                ui.end_row();

                // Kis mérő ikonok a tálca értesítési területén (kattintásra előhozzák az ablakot): nincs / CPU / CPU + memória + GPU
                ui.label(RichText::new(tr("Meters in the notification area")).size(14.0));
                let tray_label = |m: u8| match m {
                    1 => tr("Yes (CPU only)"),
                    2 => tr("Multiple (CPU, memory, GPU)"),
                    _ => tr("No"),
                };
                egui::ComboBox::from_id_salt("tray_combo")
                    .width(280.0)
                    .selected_text(tray_label(self.settings.tray_mode))
                    .show_ui(ui, |ui| {
                        for m in [0u8, 1, 2] {
                            changed |=
                                ui.selectable_value(&mut self.settings.tray_mode, m, tray_label(m)).changed();
                        }
                    });
                ui.end_row();

                // Grafikonok időintervalluma (30–120 s, alapérték 60 s)
                ui.label(RichText::new(tr("Chart time window")).size(14.0));
                ui.spacing_mut().slider_width = 220.0;
                let r = ui.add(
                    egui::Slider::new(
                        &mut self.settings.chart_window_secs,
                        settings::CHART_WINDOW_MIN..=settings::CHART_WINDOW_MAX,
                    )
                    .step_by(5.0)
                    .fixed_decimals(0)
                    .suffix(" s"),
                );
                runtime_changed |= r.drag_stopped() || (r.changed() && !r.dragged());
                ui.end_row();

                // Mintavételezési idő (0,25–5 s, alapérték 1 s)
                ui.label(RichText::new(tr("Sampling interval")).size(14.0));
                let r = ui.add(
                    egui::Slider::new(
                        &mut self.settings.sample_interval_secs,
                        settings::SAMPLE_INTERVAL_MIN..=settings::SAMPLE_INTERVAL_MAX,
                    )
                    .step_by(0.25)
                    .fixed_decimals(2)
                    .suffix(" s"),
                );
                runtime_changed |= r.drag_stopped() || (r.changed() && !r.dragged());
                ui.end_row();
            });
            ui.add_space(12.0);
            ui.label(
                RichText::new(tr("Asking for administrator rights takes effect on the next start; the chart time window and the sampling interval apply immediately."))
                    .size(12.0)
                    .color(col_muted()),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr("The sidebar and the title bar use the same material (Mica or Acrylic), so they form one continuous surface; the content area is always solid. If the sidebar stays white or black, choose “Solid”."))
                    .size(12.0)
                    .color(col_muted()),
            );
            ui.label(
                RichText::new(format!(
                    "{}: {}",
                    tr("Effect currently in use"),
                    backdrop_label(backdrop::resolve(self.settings.backdrop))
                ))
                .size(12.0)
                .color(col_muted()),
            );
        });

        ui.label(RichText::new(tr("Settings are saved automatically.")).size(12.0).color(col_muted()));

        if changed {
            i18n::apply(&self.settings.lang);
            apply_theme_pref(ctx, self.settings.theme);
            // Az effektus (és a címsor színe) azonnali alkalmazása, majd a DWM-keret újraszámoltatása + ellenőrzés.
            if let Some(h) = self.hwnd {
                let dark = self.dark_applied.unwrap_or(true);
                backdrop::apply(h.0 as isize, dark, self.settings.backdrop, menus::nav_solid_rgb());
                shell::set_topmost(h.0 as isize, self.settings.always_on_top);
                tray::configure(h.0 as isize, tray_mask(self.settings.tray_mode));
            }
            self.refresh_in = Some(2);
        }
        if runtime_changed {
            self.apply_runtime_settings();
        }
        if changed || runtime_changed {
            self.settings.save();
        }
    }

    // ───────────────────────────── Felhasználók fül ─────────────────────────────
    fn ui_users(&mut self, ui: &mut Ui) {
        let n = self.users.len();
        let subtitle = trf("{0} signed-in user(s)", &[&n]);
        page_header(ui, tr("Users"), &subtitle);
        let (sel_label, sel_targets) = self.users_selection();
        let mut kill_clicked = false;
        let mut new_task_clicked = false;
        ui.horizontal(|ui| {
            let (k, n) = menus::task_buttons(ui, &sel_label, sel_targets.len());
            kill_clicked = k;
            new_task_clicked = n;
        });
        if new_task_clicked {
            self.run_menu_action(menus::MenuAction::NewTask);
        }
        if kill_clicked {
            self.run_menu_action(menus::MenuAction::EndTask { label: sel_label.clone(), targets: sel_targets.clone() });
        }
        self.status_line(ui);
        ui.add_space(4.0);
        let net_state = self.snap.as_ref().map(|sn| sn.net_state).unwrap_or_default();
        let net_ok = net_state == NetState::Active;
        let mut restart_admin = false;
        match net_state {
            NetState::NeedsAdmin => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(tr("Per-process network traffic is measured with an ETW session, which needs administrator rights. The Network column stays empty until the program runs as administrator."))
                            .size(11.5)
                            .color(col_muted()),
                    );
                    if ui.small_button(tr("Restart as administrator")).clicked() {
                        restart_admin = true;
                    }
                });
            }
            NetState::Failed(code) => {
                ui.label(
                    RichText::new(trf("The network measurement session could not be started (error code {0}), so the Network column is not available.", &[&code]))
                        .size(11.5)
                        .color(col_muted()),
                );
            }
            _ => {
                ui.label(
                    RichText::new(tr("The Network value is measured per process with an ETW session (all TCP and UDP traffic, including local traffic). The Disk value is the sum of the processes' I/O traffic (file, device and network operations together)."))
                        .size(11.5)
                        .color(col_muted()),
                );
            }
        }
        if restart_admin && relaunch_elevated() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.add_space(8.0);

        // Fejléc: jobb klikk = oszlopválasztó, az oszlopok bal szélét húzva a szélességük állítható.
        let (hrect, hresp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        let mut uprefs = self.settings.tables.get("users").cloned().unwrap_or_default();
        let mask = uprefs.vis.unwrap_or(settings::USERS_COLUMNS_DEFAULT);
        // Legalább a látható oszlopokhoz tartozó szélességű tábla: keskenyebb ablakban vízszintes görgetősáv, a fejléc
        // együtt mozog.
        let content_w = hrect.width().max(UserCols::min_width(mask, &uprefs));
        let virt = Rect::from_min_size(
            pos2(hrect.left() - self.tabs.usr_scroll_x, hrect.top()),
            vec2(content_w, hrect.height()),
        );
        let mut menu_act = menus::ColumnMenuAction::None;
        let menu_items: Vec<(usize, &str, bool)> =
            UCol::ALL.iter().map(|c| (*c as usize, c.title(), mask & c.bit() != 0)).collect();
        let can_reset = uprefs.is_customized();
        {
            let c = UserCols::new(virt, mask, &uprefs);
            let p = ui.painter_at(hrect);
            let cy = hrect.center().y;
            let f = FontId::proportional(12.5);
            p.text(pos2(c.name_x, cy), Align2::LEFT_CENTER, tr("User"), f.clone(), col_muted());
            for (col, l, r) in &c.cols {
                let (x, al) =
                    if col.right_aligned() { (*r - 10.0, Align2::RIGHT_CENTER) } else { (*l + 10.0, Align2::LEFT_CENTER) };
                let title = fit_text(&p, col.title(), &f, (*r - *l) - 20.0);
                p.text(pos2(x, cy), al, title, f.clone(), col_muted());
                // Oszlopelválasztó
                p.line_segment(
                    [pos2(*l, hrect.top() + 8.0), pos2(*l, hrect.bottom() - 8.0)],
                    Stroke::new(1.0_f32, overlay(24)),
                );
                // Átméretező fogantyú az oszlop jobb szélén (az oszlopok határa mindkét oldalról húzható).
                let grip = Rect::from_min_max(pos2(*r - 4.0, hrect.top()), pos2(*r + 4.0, hrect.bottom()))
                    .intersect(hrect);
                if grip.width() > 1.0 {
                    let g = ui
                        .interact(grip, ui.id().with(("usr_grip", *col as usize)), Sense::click_and_drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                    if g.hovered() || g.dragged() {
                        p.line_segment(
                            [pos2(*r, hrect.top() + 6.0), pos2(*r, hrect.bottom() - 6.0)],
                            Stroke::new(1.0_f32, overlay(70)),
                        );
                    }
                    if g.double_clicked() {
                        uprefs.set_width(*col as usize, 0.0);
                    } else if g.dragged() {
                        uprefs.set_width(*col as usize, (*r - *l + g.drag_delta().x).clamp(COL_MIN_W, COL_MAX_W));
                    }
                    g.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(uprefs.pct)));
                }
            }
            // A Név oszlop jobb széle (az első adatoszlop bal széle) is húzható.
            {
                let x = c.name_edge;
                let grip =
                    Rect::from_min_max(pos2(x - 4.0, hrect.top()), pos2(x + 4.0, hrect.bottom())).intersect(hrect);
                if grip.width() > 1.0 {
                    let g = ui
                        .interact(grip, ui.id().with("usr_grip_name"), Sense::click_and_drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                    if g.hovered() || g.dragged() {
                        p.line_segment(
                            [pos2(x, hrect.top() + 6.0), pos2(x, hrect.bottom() - 6.0)],
                            Stroke::new(1.0_f32, overlay(70)),
                        );
                    }
                    if g.double_clicked() {
                        uprefs.set_width(NAME_SLOT, 0.0);
                    } else if g.dragged() {
                        uprefs.set_width(NAME_SLOT, (x - virt.left() + g.drag_delta().x).clamp(140.0, COL_MAX_W));
                    }
                    g.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(uprefs.pct)));
                }
            }
            p.line_segment(
                [hrect.left_bottom(), hrect.right_bottom()],
                Stroke::new(1.0_f32, overlay(18)),
            );
        }
        hresp.context_menu(|ui| menus::column_menu(ui, &menu_items, can_reset, &mut menu_act, Some(uprefs.pct)));
        match menu_act {
            menus::ColumnMenuAction::Toggle(i) => {
                uprefs.vis = Some(mask ^ (1 << i));
                uprefs.changed = true;
            }
            menus::ColumnMenuAction::Reset => uprefs.reset(),
            menus::ColumnMenuAction::TogglePct(b) => {
                uprefs.pct ^= b;
                uprefs.changed = true;
            }
            menus::ColumnMenuAction::None => {}
        }
        let rows_prefs = uprefs.clone();
        let mask = rows_prefs.vis.unwrap_or(settings::USERS_COLUMNS_DEFAULT);
        let pctx = self.pct_ctx();
        let pbits = rows_prefs.pct;
        if uprefs.changed {
            self.settings.tables.insert("users".to_string(), uprefs);
        }
        if self.users.is_empty() {
            ui.label(RichText::new(tr("No signed-in user.")).color(col_muted()));
            return;
        }

        let users = &self.users;
        let groups = &self.groups;
        let icons = &self.icons;
        let open = &self.expanded_users;
        let avatars = &self.avatars;
        let mut toggle: Option<u32> = None;
        let mut want_icons: Vec<String> = Vec::new();
        let usr_sel = self.tabs.usr_sel.clone();
        let mut sel_click: Option<(u32, String)> = None;
        let mut menu_action: Option<menus::MenuAction> = None;

        let users_out = egui::ScrollArea::both().id_salt("users_list").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for u in users {
                let expanded = open.contains(&u.id);
                let (rect, resp) = ui.allocate_exact_size(vec2(content_w, 44.0), Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                if ui.is_rect_visible(rect) {
                    let painter = ui.painter();
                    let c = UserCols::new(rect, mask, &rows_prefs);
                    let cy = rect.center().y;
                    if resp.hovered() {
                        painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(6), overlay(10));
                    }
                    tri(painter, pos2(rect.left() + 20.0, cy), if expanded { 0 } else { 2 }, col_muted());
                    // Avatár: a felhasználónév kezdőbetűje.
                    let center = pos2(rect.left() + 50.0, cy);
                    if let Some(tex) = avatars.get(&u.user) {
                        draw_avatar(painter, center, 16.0, tex.id());
                    } else {
                        painter.circle_filled(center, 15.0, DATA_BLUE.gamma_multiply(0.85));
                        let initial = u
                            .user
                            .chars()
                            .next()
                            .map(|ch| ch.to_uppercase().to_string())
                            .unwrap_or_default();
                        painter.text(
                            center,
                            Align2::CENTER_CENTER,
                            initial,
                            FontId::proportional(15.0),
                            Color32::WHITE,
                        );
                    }
                    let name_rect = painter.text(
                        pos2(rect.left() + 76.0, cy),
                        Align2::LEFT_CENTER,
                        &u.user,
                        FontId::proportional(14.5),
                        col_text(),
                    );
                    if u.full != u.user {
                        let clip = Rect::from_min_max(
                            pos2(name_rect.right() + 8.0, rect.top()),
                            pos2(c.name_right, rect.bottom()),
                        );
                        painter.with_clip_rect(clip).text(
                            pos2(name_rect.right() + 8.0, cy),
                            Align2::LEFT_CENTER,
                            &u.full,
                            FontId::proportional(12.0),
                            col_muted(),
                        );
                    }
                    let (state_label, state_color) = session_state(u.state);
                    let font = FontId::proportional(13.5);
                    trunc_clear();
                    for (col, l, r) in &c.cols {
                        painter.line_segment(
                            [pos2(*l, rect.top()), pos2(*l, rect.bottom())],
                            Stroke::new(1.0_f32, overlay(9)),
                        );
                        let (text, color): (String, Color32) = match col {
                            UCol::Id => (u.id.to_string(), col_text()),
                            UCol::Session => (dash_text(&u.station), col_text()),
                            UCol::Client => (dash_text(&u.client), col_text()),
                            UCol::Status => (state_label.to_string(), state_color),
                            UCol::Cpu => (comma(format!("{:.1} %", u.cpu)), col_text()),
                            UCol::Mem => (if pbits & 1 != 0 { pctx.mem(u.mem_mb) } else { fmt_mb(u.mem_mb) }, col_text()),
                            UCol::Disk => (if pbits & 2 != 0 { pctx.disk(u.io_bps) } else { fmt_bytes_per_s(u.io_bps) }, col_text()),
                            UCol::Net => {
                                if net_ok {
                                    (if pbits & 4 != 0 { pctx.net(u.net_bps) } else { fmt_bits_per_s(u.net_bps) }, col_text())
                                } else {
                                    ("—".to_string(), col_muted())
                                }
                            }
                            UCol::Gpu => (comma(format!("{:.1} %", u.gpu)), col_text()),
                            UCol::GpuEngine => (dash_text(&u.gpu_engine), col_text()),
                        };
                        // Ami nem fér el az oszlopban, azt "…"-tal rövidítjük.
                        let shown = fit_text(painter, &text, &font, (*r - *l) - 20.0);
                        if shown != text {
                            trunc_push(Rect::from_min_max(pos2(*l, rect.top()), pos2(*r, rect.bottom())), &text);
                        }
                        if col.right_aligned() {
                            painter.text(pos2(*r - 10.0, cy), Align2::RIGHT_CENTER, shown, font.clone(), color);
                        } else {
                            painter.text(pos2(*l + 10.0, cy), Align2::LEFT_CENTER, shown, font.clone(), color);
                        }
                    }
                }
                trunc_tooltip(ui.ctx(), &resp);
                if resp.clicked() {
                    toggle = Some(u.id);
                }
                resp.context_menu(|ui| {
                    let text = if expanded { tr("Collapse") } else { tr("Expand") };
                    menus::fit_menu(ui, &[text], false);
                    if menus::item(ui, text, true, true) {
                        menu_action = Some(menus::MenuAction::ToggleUserExpand(u.id));
                    }
                });

                // Lenyitva: a munkamenet összes alkalmazása (azonos nevűek összevonva).
                if expanded {
                    for a in user_apps(groups, u.id) {
                        let (r2, resp2) = ui.allocate_exact_size(vec2(content_w, 30.0), Sense::click());
                        if !ui.is_rect_visible(r2) {
                            continue;
                        }
                        let sel_here = usr_sel.as_ref().is_some_and(|(s, k)| *s == u.id && *k == a.key);
                        if sel_here {
                            ui.painter().rect_filled(
                                r2.shrink2(vec2(0.0, 1.0)),
                                CornerRadius::same(6),
                                accent().gamma_multiply(0.35),
                            );
                        } else if resp2.hovered() {
                            ui.painter().rect_filled(r2.shrink2(vec2(0.0, 1.0)), CornerRadius::same(4), overlay(10));
                        }
                        let painter = ui.painter();
                        let c2 = UserCols::new(r2, mask, &rows_prefs);
                        let cy2 = r2.center().y;
                        let icon_rect = Rect::from_center_size(pos2(r2.left() + 90.0, cy2), vec2(18.0, 18.0));
                        menus::paint_icon_slot(painter, a.exe.as_ref().and_then(|e| icons.get(e)), a.exe.is_some(), icon_rect);
                        if let Some(e) = &a.exe {
                            if !icons.contains_key(e) {
                                want_icons.push(e.clone());
                            }
                        }
                        let label = if a.count > 1 { format!("{} ({})", a.name, a.count) } else { a.name.clone() };
                        let clip = Rect::from_min_max(pos2(r2.left(), r2.top()), pos2(c2.name_right, r2.bottom()));
                        painter.with_clip_rect(clip).text(
                            pos2(r2.left() + 112.0, cy2),
                            Align2::LEFT_CENTER,
                            label,
                            FontId::proportional(13.0),
                            col_text(),
                        );
                        let font = FontId::proportional(13.0);
                        for (col, l, r) in &c2.cols {
                            painter.line_segment(
                                [pos2(*l, r2.top()), pos2(*l, r2.bottom())],
                                Stroke::new(1.0_f32, overlay(9)),
                            );
                            let (text, color): (String, Color32) = match col {
                                UCol::Cpu => (comma(format!("{:.1} %", a.cpu)), col_text()),
                                UCol::Mem => (if pbits & 1 != 0 { pctx.mem(a.mem_mb) } else { fmt_mb(a.mem_mb) }, col_text()),
                                UCol::Disk => (if pbits & 2 != 0 { pctx.disk(a.io_bps) } else { fmt_bytes_per_s(a.io_bps) }, col_text()),
                                UCol::Net => {
                                    if net_ok {
                                        (if pbits & 4 != 0 { pctx.net(a.net_bps) } else { fmt_bits_per_s(a.net_bps) }, col_text())
                                    } else {
                                        ("—".to_string(), col_muted())
                                    }
                                }
                                UCol::Gpu => (comma(format!("{:.1} %", a.gpu)), col_text()),
                                // Az azonosító-, a munkamenet-, az állapot- és a GPU-motor oszlop csak a felhasználó sorában látszik.
                                _ => continue,
                            };
                            let shown = fit_text(painter, &text, &font, (*r - *l) - 20.0);
                            painter.text(pos2(*r - 10.0, cy2), Align2::RIGHT_CENTER, shown, font.clone(), color);
                        }
                        // Bal és jobb kattintásra kijelöljük a sort; a jobb klikk a Folyamatok fülével azonos menüt nyit.
                        if resp2.clicked() || resp2.secondary_clicked() {
                            sel_click = Some((u.id, a.key.clone()));
                        }
                        resp2.context_menu(|ui| {
                            let spec = menus::ProcMenuSpec::for_user_app(&a);
                            menus::process_menu(ui, &spec, &mut menu_action);
                        });
                    }
                }
            }
        });
        self.tabs.usr_scroll_x = users_out.state.offset.x;

        for path in want_icons {
            self.request_icon(&path);
        }
        if let Some(id) = toggle {
            if !self.expanded_users.remove(&id) {
                self.expanded_users.insert(id);
            }
        }
        if let Some(k) = sel_click {
            self.tabs.usr_sel = Some(k);
        }
        if let Some(a) = menu_action {
            self.run_menu_action(a);
        }
    }

    // ───────────────────────────── Részletek fül ─────────────────────────────
    // ───────────────────────────── Információ fül ─────────────────────────────
    fn ui_about(&self, ui: &mut Ui) {
        page_header(ui, tr("About"), tr("About this program"));
        // Az ADATOK (fejlesztő, Copyright-tulajdonos stb.) a külső about.json fájlból jönnek, a feliratok a programból.
        card(ui, |ui| {
            ui.label(RichText::new(&self.about.title).size(22.0).strong());
            if !self.about.subtitle.is_empty() {
                ui.label(RichText::new(&self.about.subtitle).color(col_muted()));
            }
            ui.add_space(16.0);
            kv_grid("about_grid").show(ui, |ui| {
                for row in &self.about.rows {
                    if row.value.starts_with("https://") || row.value.starts_with("http://") {
                        // Webcím: kattintható hivatkozás (a Windows kiemelő színével), megnyílik a böngészőben.
                        ui.label(RichText::new(&row.label).size(12.5).color(col_muted()));
                        let link = ui.add(
                            egui::Label::new(RichText::new(&row.value).size(13.0).color(accent()).underline())
                                .sense(Sense::click()),
                        );
                        if link.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            let _ = shell::open_url(&row.value);
                        }
                        ui.end_row();
                    } else {
                        kv(ui, &row.label, &row.value);
                    }
                }
            });
        });
        // Copyright-sor az oldal alján (az év automatikus).
        let rest = ui.available_height();
        if rest.is_finite() && rest > 40.0 {
            ui.add_space(rest - 26.0);
        }
        ui.label(RichText::new(self.about.copyright()).size(12.0).color(col_muted()));
    }
}

impl eframe::App for TaskApp {
    /// Átlátszó törlőszín: így látszhat át a Mica a bal sávon (a tartalmi részt tömören festjük).
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // A feloldott téma (sötét/világos): a rajzolók innen kérik a színeket, a DWM-címsort is hozzáigazítjuk.
        let dark = ctx.style().visuals.dark_mode;
        theme::set_dark(dark);
        // A Windows kiemelő színének követése: ha a felhasználó átállítja, a kijelölések azonnal igazodnak.
        if theme::poll_accent() {
            apply_style(ctx);
            ctx.request_repaint();
        }
        if self.dark_applied != Some(dark) {
            if let Some(h) = self.hwnd {
                let c = theme::nav_fill(false);
                backdrop::apply(h.0 as isize, dark, self.settings.backdrop, (c.r(), c.g(), c.b()));
                shell::hide_caption_text_and_icon(h.0 as isize);
            }
            self.dark_applied = Some(dark);
            self.refresh_in = Some(2); // témaváltás után a keretet újra kell számoltatni
        }

        // Háttéreffekt: induláskor, témaváltáskor és effektusváltáskor a DWM-et újra kell "lökni", és ellenőrizni, hogy
        // tényleg átlátszó-e a bal sáv (lásd `drive_backdrop_fix`).
        self.frames = self.frames.saturating_add(1);
        if self.frames < 14 {
            ctx.request_repaint(); // induláskor biztosan fussanak le a frame-ek
        }
        // Futás közbeni őrzés: ha a program hosszabb ideig nem frissült (alvó állapot, kikapcsolt kijelző, takarékos mód),
        // vagy újra előtérbe került, a Mica / Acrylic "kifehéredhet": ilyenkor azonnal ellenőrizzük és javítjuk.
        let now = Instant::now();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if now.duration_since(self.last_update_at) > Duration::from_secs(4) || (focused && !self.was_focused) {
            self.watch_bad = 1;
            self.watch_next = now;
        }
        self.last_update_at = now;
        self.was_focused = focused;
        self.drive_backdrop_fix(ctx, dark);

        // Az Információ fül adatait minden megnyitáskor újraolvassuk (külső fájl: szerkesztés után azonnal látszik).
        if self.tab == Tab::About {
            if self.about_stale {
                self.about = about::AboutInfo::load(self.elevated);
                self.about_stale = false;
            }
        } else {
            self.about_stale = true;
        }

        // A táblák oszlop-beállításait az egérgomb felengedése után mentjük (húzás közben nem írunk lemezre).
        if self.settings.tables.values().any(|p| p.changed) && !ctx.input(|i| i.pointer.any_down()) {
            for p in self.settings.tables.values_mut() {
                p.changed = false;
            }
            self.settings.save();
        }
        // A parancssor olvasását a Folyamatok fül kéri minden képkockán, amíg az oszlop látszik.
        WANT_CMDLINE.store(false, AtomicOrdering::Relaxed);

        self.pump();
        self.pump_icons(ctx);
        self.pump_lists();
        // A tálcaikon (kis CPU-mérő) az utolsó mintából frissül.
        if let Some(sn) = &self.snap {
            let mem_pct = if sn.mem.total_mb > 0.0 { (sn.mem.used_mb / sn.mem.total_mb * 100.0) as f32 } else { 0.0 };
            let gpu_pct = sn.gpus.first().and_then(|g| g.utilization).unwrap_or(0.0);
            tray::update(sn.cpu.usage_total, mem_pct, gpu_pct);
        }
        self.affinity_dialog(ctx);
        self.new_task_dialog(ctx);
        // Ha a háttérszál elakad, akkor is frissítünk legalább másodpercenként,
        // hogy az tr("No fresh data") jelzés megjelenjen.
        ctx.request_repaint_after(Duration::from_secs(1));
        self.nav_panel(ctx);
        self.content_panel(ctx);
        self.show_net_details(ctx);
        self.confirm_dialog(ctx);
    }
}

/// Hibanapló a futtatható fájl mellett (taskmanager-error.log). Azért kell, mert a release
/// build "windows" alrendszerű, tehát nincs konzol, ahol a panik szövege látszana.
fn log_line(text: &str) {
    use std::io::Write;
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("taskmanager-error.log")))
        .unwrap_or_else(|| "taskmanager-error.log".into());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{text}");
    }
}

/// Minden panikot (a háttérszálon elkapottakat is) naplóz: szál neve, hely, üzenet, backtrace.
fn install_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let text = format!(
            "[PANIK] szál: {} | {info}\n{}",
            thread.name().unwrap_or("?"),
            std::backtrace::Backtrace::force_capture()
        );
        log_line(&text);
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{text}");
    }));
}

/// A wgpu-s ágon: DX12 + DirectComposition ("DxgiFromVisual") úszólánc, mert csak ez támogat átlátszó
/// (alfás) ablakfelületet Windowson – ez kell ahhoz, hogy a Mica átlásson a bal sávon.
/// A beállítást a wgpu a `WGPU_DX12_PRESENTATION_SYSTEM` környezeti változóból veszi át, amit a
/// `WgpuSetupCreateNew::default()` hívás előtt állítunk be. Ha ezzel gond van, a
/// `TASKMANAGER_NO_VISUAL=1` környezeti változóval a régi (HWND-s) mód kapcsolható vissza.
#[cfg(feature = "gpu-wgpu")]
fn apply_wgpu_options(options: &mut eframe::NativeOptions) {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    if std::env::var_os("TASKMANAGER_NO_VISUAL").is_none()
        && std::env::var_os("WGPU_DX12_PRESENTATION_SYSTEM").is_none()
    {
        // SAFETY: a program legelején, még a szálak indítása előtt hívjuk.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("WGPU_DX12_PRESENTATION_SYSTEM", "DxgiFromVisual");
        }
    }
    let mut setup = WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = wgpu::Backends::DX12;
    options.wgpu_options =
        WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() };
}

#[cfg(not(feature = "gpu-wgpu"))]
fn apply_wgpu_options(_options: &mut eframe::NativeOptions) {}

fn main() -> eframe::Result {
    install_panic_logger();

    // Induláskor rendszergazdai jogot kérünk (UAC), hogy minden folyamat adata olvasható legyen. A `--elevated`
    // kapcsoló jelzi, hogy már az emelt példány fut; a `TASKMANAGER_NO_ELEVATE=1` kikapcsolja a kérést.
    let settings_at_start = Settings::load();
    i18n::apply(&settings_at_start.lang); // az ablak címe is a választott nyelven jelenjen meg
    let already_elevated_launch = std::env::args().any(|a| a == "--elevated");
    if settings_at_start.elevate
        && !already_elevated_launch
        && std::env::var_os("TASKMANAGER_NO_ELEVATE").is_none()
        && !is_elevated()
        && relaunch_elevated()
    {
        return Ok(()); // az emelt példány elindult, ez a példány kilép
    }
    if is_elevated() {
        let _ = enable_debug_privilege();
    }

    boost_process_priority();
    let mut options = eframe::NativeOptions {
        viewport: {
            let vb = egui::ViewportBuilder::default()
                .with_title(tr("Task Manager"))
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size(MIN_WINDOW_SIZE)
                .with_transparent(true); // kell a Mica áttetszéséhez
            let vb = if settings_at_start.always_on_top { vb.with_always_on_top() } else { vb };
            match load_icon_data() {
                Some(icon) => vb.with_icon(Arc::new(icon)), // a tálcán és az Alt+Tab-ban is ez látszik
                None => vb,
            }
        },
        ..Default::default()
    };
    apply_wgpu_options(&mut options);
    let result = eframe::run_native(
        tr("Task Manager"),
        options,
        Box::new(|cc| Ok(Box::new(TaskApp::new(cc)))),
    );
    // Az ETW-munkamenetek kilépés után is megmaradnának: a sajátunkat leállítjuk.
    netmon::stop();
    tray::stop();
    if let Err(e) = &result {
        log_line(&format!("[HIBA] eframe: {e}"));
    }
    result
}
