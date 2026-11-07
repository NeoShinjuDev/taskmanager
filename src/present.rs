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
//! Képkockasebesség (FPS) és képkockaidő mérése ETW-vel (Event Tracing for Windows), INJEKTÁLÁS NÉLKÜL.
//!
//! MŰKÖDÉS
//! * A program egy saját, valós idejű ETW-munkamenetet indít (`TaskManagerPresentSession`), és bekapcsolja benne a
//!   `Microsoft-Windows-DXGI` és a `Microsoft-Windows-D3D9` szolgáltatót. Ezek minden `Present` hívásról eseményt
//!   adnak, amelynek fejlécében a hívó folyamat PID-je és a pontos (QPC) időbélyeg szerepel. Ugyanígy dolgozik a
//!   PresentMon is. Semmi nem kerül más programok memóriájába.
//! * Az események a DXGI-t használó (DirectX 10 / 11 / 12) és a D3D9-es játékokat fedik le. Vulkan- és OpenGL-játékoknál
//!   nincs ilyen esemény, ott az FPS nem mérhető (a felület kötőjelet mutat).
//! * Egy külön szál (`etw-present`) dolgozza fel az eseményeket (`ProcessTrace`), és PID-enként a legutóbbi
//!   időbélyegeket tartja (legfeljebb `MAX_STAMPS` darabot). Az FPS és a képkockaidők ezekből számolódnak (`stats`).
//!
//! JOGOSULTSÁG
//! Munkamenetet csak rendszergazda (vagy a "Performance Log Users" csoport tagja) indíthat. Ha ez nincs meg, az állapot
//! `PresentState::NeedsAdmin`.
//!
//! TAKARÍTÁS
//! Az ETW-munkamenetek a programból való kilépés után is megmaradnak, ezért a program indításkor leállítja az azonos nevű,
//! esetleg beragadt munkamenetet, és kilépéskor (`stop`) a sajátját is.
//!
//! A struktúrák és a hívások a `netmon.rs`-ben lévőkkel egyeznek (nyers FFI); itt szándékosan külön példány van belőlük,
//! hogy a két mérés egymástól teljesen független maradjon.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::winreg::wide;

/// A mérés állapota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentState {
    /// Nem fut (nincs bekapcsolva, vagy leállították).
    Stopped,
    /// A munkamenet fut, az események érkeznek.
    Active,
    /// A munkamenet indításához rendszergazdai jog kell.
    NeedsAdmin,
    /// Más hiba (a Win32 hibakóddal; 0 = belső hiba).
    Failed(u32),
}

/// Egy folyamatról legfeljebb ennyi legutóbbi `Present` időbélyeget tartunk meg.
const MAX_STAMPS: usize = 512;

const SESSION_NAME: &str = "TaskManagerPresentSession";

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_ALREADY_EXISTS: u32 = 183;

const WNODE_FLAG_TRACED_GUID: u32 = 0x0002_0000;
const EVENT_TRACE_REAL_TIME_MODE: u32 = 0x0000_0100;
const EVENT_TRACE_CONTROL_STOP: u32 = 1;
const EVENT_CONTROL_CODE_ENABLE_PROVIDER: u32 = 1;
const TRACE_LEVEL_VERBOSE: u8 = 5;
const PROCESS_TRACE_MODE_REAL_TIME: u32 = 0x0000_0100;
const PROCESS_TRACE_MODE_EVENT_RECORD: u32 = 0x1000_0000;
const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;

/// Microsoft-Windows-DXGI `Present::Start` esemény azonosítója.
const DXGI_PRESENT_START: u16 = 42;
/// Microsoft-Windows-D3D9 `PresentStart` esemény azonosítója.
const D3D9_PRESENT_START: u16 = 1;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// Microsoft-Windows-DXGI: {CA11C036-0102-4A2D-A6AD-F03CFED5D3C9}
const DXGI_PROVIDER: Guid = Guid {
    data1: 0xCA11_C036,
    data2: 0x0102,
    data3: 0x4A2D,
    data4: [0xA6, 0xAD, 0xF0, 0x3C, 0xFE, 0xD5, 0xD3, 0xC9],
};

/// Microsoft-Windows-D3D9: {783ACA0A-790E-4D7F-8451-AA850511C6B9}
const D3D9_PROVIDER: Guid = Guid {
    data1: 0x783A_CA0A,
    data2: 0x790E,
    data3: 0x4D7F,
    data4: [0x84, 0x51, 0xAA, 0x85, 0x05, 0x11, 0xC6, 0xB9],
};

/// WNODE_HEADER (48 bájt)
#[repr(C)]
#[allow(dead_code)]
struct WnodeHeader {
    buffer_size: u32,
    provider_id: u32,
    historical_context: u64,
    timestamp: i64,
    guid: Guid,
    client_context: u32,
    flags: u32,
}

/// EVENT_TRACE_PROPERTIES (120 bájt x64-en)
#[repr(C)]
#[allow(dead_code)]
struct EventTraceProperties {
    wnode: WnodeHeader,
    buffer_size: u32,
    minimum_buffers: u32,
    maximum_buffers: u32,
    maximum_file_size: u32,
    log_file_mode: u32,
    flush_timer: u32,
    enable_flags: u32,
    age_limit: i32,
    number_of_buffers: u32,
    free_buffers: u32,
    events_lost: u32,
    buffers_written: u32,
    log_buffers_lost: u32,
    real_time_buffers_lost: u32,
    logger_thread_id: usize,
    log_file_name_offset: u32,
    logger_name_offset: u32,
}

/// EVENT_RECORD (112 bájt x64-en): az EVENT_HEADER (80 bájt), az ETW_BUFFER_CONTEXT és a felhasználói adatok.
#[repr(C)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct EventRecord {
    header_size: u16,
    header_type: u16,
    header_flags: u16,
    event_property: u16,
    thread_id: u32,
    process_id: u32,
    timestamp: i64,
    provider_id: Guid,
    event_id: u16,
    version: u8,
    channel: u8,
    level: u8,
    opcode: u8,
    task: u16,
    keyword: u64,
    processor_time: u64,
    activity_id: Guid,
    processor_number: u8,
    alignment: u8,
    logger_id: u16,
    extended_data_count: u16,
    user_data_length: u16,
    extended_data: *mut c_void,
    user_data: *mut c_void,
    user_context: *mut c_void,
}

type EventRecordCallback = unsafe extern "system" fn(*mut EventRecord);

/// EVENT_TRACE_LOGFILEW (448 bájt x64-en); a beágyazott részek átlátszatlan, 8 bájtra igazított tömbök.
#[repr(C)]
#[allow(dead_code)]
struct EventTraceLogfileW {
    log_file_name: *mut u16,
    logger_name: *mut u16,
    current_time: i64,
    buffers_read: u32,
    process_trace_mode: u32,
    current_event: [u64; 11],
    logfile_header: [u64; 35],
    buffer_callback: usize,
    buffer_size: u32,
    filled: u32,
    events_lost: u32,
    event_record_callback: Option<EventRecordCallback>,
    is_kernel_trace: u32,
    context: *mut c_void,
}

#[link(name = "advapi32")]
extern "system" {
    fn StartTraceW(handle: *mut u64, name: *const u16, props: *mut EventTraceProperties) -> u32;
    fn ControlTraceW(handle: u64, name: *const u16, props: *mut EventTraceProperties, code: u32) -> u32;
    fn EnableTraceEx2(
        handle: u64,
        provider: *const Guid,
        control_code: u32,
        level: u8,
        match_any_keyword: u64,
        match_all_keyword: u64,
        timeout: u32,
        params: *const c_void,
    ) -> u32;
    fn OpenTraceW(logfile: *mut EventTraceLogfileW) -> u64;
    fn ProcessTrace(handles: *const u64, count: u32, start: *const c_void, end: *const c_void) -> u32;
    fn CloseTrace(handle: u64) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
}

// ───────────────────────── globális állapot ─────────────────────────

static STATE_KIND: AtomicU8 = AtomicU8::new(0); // 0 leállt, 1 aktív, 2 rendszergazda kell, 3 hiba
static STATE_ERR: AtomicU32 = AtomicU32::new(0);
static FRAMES: OnceLock<Mutex<HashMap<u32, VecDeque<i64>>>> = OnceLock::new();

fn frames() -> &'static Mutex<HashMap<u32, VecDeque<i64>>> {
    FRAMES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_state(kind: u8, err: u32) {
    STATE_ERR.store(err, Ordering::Relaxed);
    STATE_KIND.store(kind, Ordering::Relaxed);
}

/// A mérés aktuális állapota (bármelyik szálról hívható).
pub fn state() -> PresentState {
    match STATE_KIND.load(Ordering::Relaxed) {
        1 => PresentState::Active,
        2 => PresentState::NeedsAdmin,
        3 => PresentState::Failed(STATE_ERR.load(Ordering::Relaxed)),
        _ => PresentState::Stopped,
    }
}

fn qpc_now() -> i64 {
    let mut v = 0i64;
    // SAFETY: a `v` egy élő, kimeneti változó.
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}

fn qpc_freq() -> i64 {
    let mut v = 0i64;
    // SAFETY: a `v` egy élő, kimeneti változó.
    unsafe { QueryPerformanceFrequency(&mut v) };
    v.max(1)
}

/// Egy folyamat mért képkocka-adatai.
#[derive(Clone, Debug)]
pub struct PresentStats {
    /// Az utolsó másodperc alatt bemutatott képkockák száma.
    pub fps: f32,
    /// Az utolsó képkocka óta eltelt idő (ms); ha nagy, a folyamat már nem rajzol.
    pub age_ms: f32,
    /// A legutóbbi képkockák ideje (ms), a régebbitől az újabb felé.
    pub frametimes_ms: Vec<f32>,
}

/// A megadott folyamat FPS-e és legutóbbi `max_frames` képkockaideje; `None`, ha a mérés nem fut, vagy a folyamatról
/// nincs (friss) adat.
pub fn stats(pid: u32, max_frames: usize) -> Option<PresentStats> {
    if pid == 0 || state() != PresentState::Active {
        return None;
    }
    let freq = qpc_freq();
    let now = qpc_now();
    let mut map = frames().lock().ok()?;
    // A 10 másodperce néma folyamatok adatait eldobjuk (a térkép így nem nő a végtelenségig).
    map.retain(|_, q| q.back().is_some_and(|&t| now - t < 10 * freq));
    let q = map.get(&pid)?;
    let last = *q.back()?;
    let age_ms = (now - last) as f64 * 1000.0 / freq as f64;
    let fps = q.iter().rev().take_while(|&&t| now - t <= freq).count() as f32;
    let n = q.len();
    let take = max_frames.min(n.saturating_sub(1));
    let mut frametimes_ms = Vec::with_capacity(take);
    for i in (n - take)..n {
        frametimes_ms.push(((q[i] - q[i - 1]) as f64 * 1000.0 / freq as f64) as f32);
    }
    Some(PresentStats { fps, age_ms: age_ms as f32, frametimes_ms })
}

// ───────────────────────── a munkamenet vezérlése ─────────────────────────

/// EVENT_TRACE_PROPERTIES + a munkamenet neve egyetlen, 8 bájtra igazított memóriablokkban.
struct PropsBlock {
    buf: Vec<u64>,
}

impl PropsBlock {
    fn new() -> Self {
        let head = std::mem::size_of::<EventTraceProperties>();
        // A névnek és a (nem használt) naplófájlnévnek is hely kell: a ControlTrace visszaírja őket.
        let total = head + 2 * (SESSION_NAME.len() + 1) + 2 * 1024;
        let mut buf = vec![0u64; total.div_ceil(8)];
        let p = buf.as_mut_ptr().cast::<EventTraceProperties>();
        // SAFETY: a blokk legalább `head` bájt, nullázott és 8 bájtra igazított; csak a fejlécmezőket írjuk.
        unsafe {
            (*p).wnode.buffer_size = (buf.len() * 8) as u32;
            (*p).wnode.flags = WNODE_FLAG_TRACED_GUID;
            (*p).wnode.client_context = 1; // QPC időbélyeg (a `QueryPerformanceCounter`-rel azonos időalap)
            (*p).buffer_size = 64; // KB
            (*p).minimum_buffers = 4;
            (*p).flush_timer = 1; // másodpercenként ürítsük a puffereket, hogy az adat ne akadjon el
            (*p).maximum_buffers = 16;
            (*p).log_file_mode = EVENT_TRACE_REAL_TIME_MODE;
            (*p).log_file_name_offset = 0;
            (*p).logger_name_offset = head as u32;
        }
        Self { buf }
    }

    fn ptr(&mut self) -> *mut EventTraceProperties {
        self.buf.as_mut_ptr().cast()
    }
}

/// Leállítja a megadott nevű munkamenetet (ha nem létezik, a hívás hibakódot ad, amit figyelmen kívül hagyunk).
fn stop_session_by_name() {
    let name = wide(SESSION_NAME);
    let mut props = PropsBlock::new();
    // SAFETY: érvényes, NUL-terminált név és megfelelő méretű tulajdonságblokk; a 0 kezelő = név szerinti keresés.
    unsafe {
        ControlTraceW(0, name.as_ptr(), props.ptr(), EVENT_TRACE_CONTROL_STOP);
    }
}

/// Elindítja a mérést. Ha már fut, nem csinál semmit. Nem a felületi szálról érdemes hívni (a munkamenet indítása
/// pár ezredmásodpercig is tarthat); a hívók egymás közti kizárásáról a hívó gondoskodik.
pub fn start() {
    if state() == PresentState::Active {
        return;
    }
    // A beragadt (korábbi, rosszul leállt példány által hagyott) munkamenet eltakarítása.
    stop_session_by_name();

    let name = wide(SESSION_NAME);
    let mut handle: u64 = 0;
    let mut props = PropsBlock::new();
    // SAFETY: érvényes kimeneti kezelő, név és tulajdonságblokk.
    let mut rc = unsafe { StartTraceW(&mut handle, name.as_ptr(), props.ptr()) };
    if rc == ERROR_ALREADY_EXISTS {
        stop_session_by_name();
        props = PropsBlock::new();
        // SAFETY: mint fent.
        rc = unsafe { StartTraceW(&mut handle, name.as_ptr(), props.ptr()) };
    }
    if rc != 0 {
        if rc == ERROR_ACCESS_DENIED {
            set_state(2, rc);
        } else {
            set_state(3, rc);
        }
        return;
    }

    // A két szolgáltató bekapcsolása a munkamenetben (0 kulcsszó = minden esemény).
    for provider in [&DXGI_PROVIDER, &D3D9_PROVIDER] {
        // SAFETY: a `handle` a StartTrace által adott érvényes kezelő; a GUID élő; nincs külön paraméterblokk.
        let rc = unsafe {
            EnableTraceEx2(
                handle,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                TRACE_LEVEL_VERBOSE,
                0,
                0,
                0,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            stop_session_by_name();
            set_state(3, rc);
            return;
        }
    }

    // Az események feldolgozása külön szálon (a `ProcessTrace` a munkamenet leállításáig blokkol).
    set_state(1, 0);
    let spawned = std::thread::Builder::new().name("etw-present".into()).spawn(consumer_thread);
    if spawned.is_err() {
        stop_session_by_name();
        set_state(3, 0);
    }
}

/// Leállítja a munkamenetet (kikapcsoláskor és kilépéskor hívandó). A feldolgozó szál ettől magától befejeződik.
pub fn stop() {
    // Csak a saját, ténylegesen futó munkamenetünket állítjuk le (egy másik példányét nem).
    if state() == PresentState::Active {
        set_state(0, 0);
        stop_session_by_name();
        if let Ok(mut m) = frames().lock() {
            m.clear();
        }
    } else if state() != PresentState::Stopped {
        // Hibaállapotból (pl. rendszergazdai jog hiánya) kikapcsoláskor visszaállunk az alapállapotba.
        set_state(0, 0);
    }
}

fn consumer_thread() {
    let name = wide(SESSION_NAME);
    // SAFETY: a csupa nulla bájt érvényes ennek a struktúrának (nullmutatók, `None` visszahívás, nulla számlálók).
    let mut logfile: EventTraceLogfileW = unsafe { std::mem::zeroed() };
    logfile.logger_name = name.as_ptr() as *mut u16;
    logfile.process_trace_mode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.event_record_callback = Some(on_event);

    // SAFETY: a `logfile` és a `name` a függvény végéig élnek (az OpenTrace a nevet másolja, a ProcessTrace blokkol).
    let handle = unsafe { OpenTraceW(&mut logfile) };
    if handle == INVALID_PROCESSTRACE_HANDLE || handle == u32::MAX as u64 {
        stop_session_by_name();
        set_state(3, 0);
        return;
    }
    // SAFETY: egyetlen érvényes kezelő; nincs kezdő/záró időkorlát.
    let rc = unsafe { ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null()) };
    // SAFETY: a kezelőt az OpenTrace adta.
    unsafe { CloseTrace(handle) };
    if rc != 0 && STATE_KIND.load(Ordering::Relaxed) == 1 {
        // A feldolgozás váratlanul véget ért (nem a mi leállításunk miatt).
        set_state(3, rc);
    }
}

/// Az ETW-visszahívás: a DXGI `Present::Start` (42) és a D3D9 `PresentStart` (1) események időbélyegét a hívó folyamat
/// PID-je alá gyűjti.
unsafe extern "system" fn on_event(record: *mut EventRecord) {
    // A panik nem léphet át az FFI-határon.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if record.is_null() {
            return;
        }
        // SAFETY: az ETW érvényes rekordot ad a visszahívás idejére.
        let r = unsafe { &*record };
        let provider = r.provider_id;
        let is_present = (r.event_id == DXGI_PRESENT_START && provider == DXGI_PROVIDER)
            || (r.event_id == D3D9_PRESENT_START && provider == D3D9_PROVIDER);
        if !is_present {
            return;
        }
        if let Ok(mut m) = frames().lock() {
            let q = m.entry(r.process_id).or_default();
            if q.len() >= MAX_STAMPS {
                q.pop_front();
            }
            q.push_back(r.timestamp);
        }
    }));
}
