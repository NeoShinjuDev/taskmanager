/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Folyamatonkénti hálózati forgalom: ETW (Event Tracing for Windows) valós idejű munkamenet.
//!
//! MŰKÖDÉS
//! * A program egy saját, valós idejű ETW-munkamenetet indít (`TaskManagerNetSession`), és bekapcsolja benne a
//!   `Microsoft-Windows-Kernel-Network` szolgáltatót. Ez minden TCP / UDP (IPv4 és IPv6) adatküldésről és -fogadásról
//!   eseményt ad, amelyben a TULAJDONOS folyamat PID-je és a bájtok száma szerepel.
//! * Egy külön szál (`etw-net`) dolgozza fel az eseményeket (`ProcessTrace`), és PID-enként összegzi a bájtokat.
//! * A mintavevő (`Collector`) a kumulált összegekből számol sebességet (bájt/s).
//!
//! JOGOSULTSÁG
//! Munkamenetet csak rendszergazda (vagy a "Performance Log Users" csoport tagja) indíthat. Ha ez nincs meg, az állapot
//! `NetState::NeedsAdmin`, és a felület ezt írja ki, a Hálózat oszlop pedig "—" marad.
//!
//! TAKARÍTÁS
//! Az ETW-munkamenetek a programból való kilépés után is megmaradnak, ezért a program minden induláskor leállítja az
//! azonos nevű, esetleg beragadt munkamenetet, és kilépéskor (`stop`) is leállítja a sajátját.
//!
//! Minden Win32-hívás nyers FFI-vel megy (mint a többi modulban), az adatstruktúrák elrendezését a `repr(C)` adja.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::winreg::wide;

/// A hálózati forgalom mérésének állapota.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetState {
    /// Még nem indult el (az első mintavétel előtt).
    #[default]
    NotStarted,
    /// A munkamenet fut, a számlálók valós adatot adnak.
    Active,
    /// A munkamenet indításához rendszergazdai jog kell.
    NeedsAdmin,
    /// Más hiba (a Win32 hibakóddal; 0 = belső hiba).
    Failed(u32),
}

const SESSION_NAME: &str = "TaskManagerNetSession";

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_ALREADY_EXISTS: u32 = 183;

const WNODE_FLAG_TRACED_GUID: u32 = 0x0002_0000;
const EVENT_TRACE_REAL_TIME_MODE: u32 = 0x0000_0100;
const EVENT_TRACE_CONTROL_STOP: u32 = 1;
const EVENT_CONTROL_CODE_ENABLE_PROVIDER: u32 = 1;
const TRACE_LEVEL_VERBOSE: u8 = 5;
/// KERNEL_NETWORK_KEYWORD_IPV4 (0x10) | KERNEL_NETWORK_KEYWORD_IPV6 (0x20)
const NET_KEYWORDS: u64 = 0x30;
const PROCESS_TRACE_MODE_REAL_TIME: u32 = 0x0000_0100;
const PROCESS_TRACE_MODE_EVENT_RECORD: u32 = 0x1000_0000;
const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// Microsoft-Windows-Kernel-Network: {7DD42A49-5329-4832-8DFD-43D979153A88}
const KERNEL_NETWORK_PROVIDER: Guid = Guid {
    data1: 0x7DD4_2A49,
    data2: 0x5329,
    data3: 0x4832,
    data4: [0x8D, 0xFD, 0x43, 0xD9, 0x79, 0x15, 0x3A, 0x88],
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

/// EVENT_TRACE_LOGFILEW (448 bájt x64-en). A bonyolult beágyazott részek (EVENT_TRACE: 88 bájt, TRACE_LOGFILE_HEADER:
/// 280 bájt) átlátszatlan, 8 bájtra igazított tömbök: csak a méretük és az igazításuk számít.
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

// ───────────────────────── globális állapot ─────────────────────────

static STATE_KIND: AtomicU8 = AtomicU8::new(0); // 0 nincs, 1 aktív, 2 rendszergazda kell, 3 hiba
static STATE_ERR: AtomicU32 = AtomicU32::new(0);
static TOTALS: OnceLock<Mutex<HashMap<u32, u64>>> = OnceLock::new();
static STARTED: Mutex<bool> = Mutex::new(false);

fn totals() -> &'static Mutex<HashMap<u32, u64>> {
    TOTALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_state(kind: u8, err: u32) {
    STATE_ERR.store(err, Ordering::Relaxed);
    STATE_KIND.store(kind, Ordering::Relaxed);
}

/// A mérés aktuális állapota (bármelyik szálról hívható).
pub fn state() -> NetState {
    match STATE_KIND.load(Ordering::Relaxed) {
        1 => NetState::Active,
        2 => NetState::NeedsAdmin,
        3 => NetState::Failed(STATE_ERR.load(Ordering::Relaxed)),
        _ => NetState::NotStarted,
    }
}

/// PID -> a munkamenet indulása óta összegyűjtött bájtok (küldés + fogadás). Üres, ha a mérés nem fut.
pub fn totals_snapshot() -> HashMap<u32, u64> {
    if state() != NetState::Active {
        return HashMap::new();
    }
    totals().lock().map(|m| m.clone()).unwrap_or_default()
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
            (*p).wnode.client_context = 1; // QPC időbélyeg
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

/// Elindítja a mérést (egyszer; többszöri hívásnál az aktuális állapotot adja vissza). A háttérszálról hívandó.
pub fn start() -> NetState {
    let Ok(mut started) = STARTED.lock() else { return state() };
    if *started {
        return state();
    }
    *started = true;
    drop(started);

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
        return state();
    }

    // A hálózati szolgáltató bekapcsolása a munkamenetben.
    // SAFETY: a `handle` a StartTrace által adott érvényes kezelő; a GUID élő; nincs külön paraméterblokk.
    let rc = unsafe {
        EnableTraceEx2(
            handle,
            &KERNEL_NETWORK_PROVIDER,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE,
            NET_KEYWORDS,
            0,
            0,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        stop_session_by_name();
        set_state(3, rc);
        return state();
    }

    // Az események feldolgozása külön szálon (a `ProcessTrace` a munkamenet leállításáig blokkol).
    set_state(1, 0);
    let spawned = std::thread::Builder::new().name("etw-net".into()).spawn(consumer_thread);
    if spawned.is_err() {
        stop_session_by_name();
        set_state(3, 0);
    }
    state()
}

/// Leállítja a munkamenetet (kilépéskor hívandó). A feldolgozó szál ettől magától befejeződik.
pub fn stop() {
    // Csak a saját, ténylegesen futó munkamenetünket állítjuk le (egy másik példányét nem).
    if state() == NetState::Active {
        set_state(0, 0);
        stop_session_by_name();
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

/// Az ETW-visszahívás: a Kernel-Network események (TCP/UDP küldés és fogadás, IPv4 és IPv6) adatrészének első két
/// mezője mindig a tulajdonos PID és a bájtok száma (két darab `u32`).
unsafe extern "system" fn on_event(record: *mut EventRecord) {
    // A panik nem léphet át az FFI-határon.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if record.is_null() {
            return;
        }
        // SAFETY: az ETW érvényes rekordot ad a visszahívás idejére.
        let r = unsafe { &*record };
        // 10/11: TCP IPv4 küldés/fogadás, 26/27: TCP IPv6, 42/43: UDP IPv4, 58/59: UDP IPv6.
        if !matches!(r.event_id, 10 | 11 | 26 | 27 | 42 | 43 | 58 | 59) {
            return;
        }
        if r.user_data.is_null() || (r.user_data_length as usize) < 8 {
            return;
        }
        let p = r.user_data.cast::<u8>();
        // SAFETY: legalább 8 bájt adat van; igazítatlan olvasás.
        let (pid, size) = unsafe {
            (
                std::ptr::read_unaligned(p.cast::<u32>()),
                std::ptr::read_unaligned(p.add(4).cast::<u32>()),
            )
        };
        if let Ok(mut m) = totals().lock() {
            *m.entry(pid).or_insert(0) += size as u64;
        }
    }));
}
