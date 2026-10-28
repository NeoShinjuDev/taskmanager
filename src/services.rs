/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Szolgáltatások: a Service Control Manager (SCM) lekérdezése nyers Win32 FFI-vel (advapi32).
//!
//! * `EnumServicesStatusExW` (SC_ENUM_PROCESS_INFO) adja a nevet, a megjelenített nevet (a Task Manager ezt írja ki
//!   "Leírás" oszlopban), az állapotot és a PID-et.
//! * A "Csoport" oszlop: svchost-szolgáltatásoknál a `-k <csoport>` kapcsoló a parancssorból, egyébként a
//!   betöltési sorrend csoportja (`lpLoadOrderGroup`). Ehhez szolgáltatásonként egy `QueryServiceConfigW` kell, ezért
//!   az eredményt gyorsítótárazzuk (a konfiguráció menet közben gyakorlatilag nem változik).
//!
//! Minden hívás csak olvasó jogot kér (SC_MANAGER_ENUMERATE_SERVICE, SERVICE_QUERY_CONFIG), így normál
//! felhasználóként is működik. A függvények a háttérszálról hívandók.

use std::collections::HashMap;

use crate::i18n::tr;
use crate::winreg::wide;

#[link(name = "advapi32")]
extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> isize;
    fn EnumServicesStatusExW(
        scm: isize,
        info_level: u32,
        service_type: u32,
        service_state: u32,
        services: *mut u8,
        buf_size: u32,
        bytes_needed: *mut u32,
        services_returned: *mut u32,
        resume_handle: *mut u32,
        group_name: *const u16,
    ) -> i32;
    fn OpenServiceW(scm: isize, name: *const u16, access: u32) -> isize;
    fn QueryServiceConfigW(service: isize, config: *mut u8, buf_size: u32, bytes_needed: *mut u32) -> i32;
    fn CloseServiceHandle(h: isize) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn StartServiceW(service: isize, argc: u32, argv: *const *const u16) -> i32;
    fn ControlService(service: isize, control: u32, status: *mut ServiceStatus) -> i32;
    fn QueryServiceStatus(service: isize, status: *mut ServiceStatus) -> i32;
}

/// SERVICE_STATUS.
#[repr(C)]
#[derive(Default)]
struct ServiceStatus {
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit: u32,
    service_exit: u32,
    check_point: u32,
    wait_hint: u32,
}

const SERVICE_QUERY_STATUS: u32 = 0x0004;
const SERVICE_START: u32 = 0x0010;
const SERVICE_STOP: u32 = 0x0020;
const SERVICE_CONTROL_STOP: u32 = 1;
const SERVICE_STOPPED: u32 = 1;
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;

const SC_MANAGER_CONNECT: u32 = 0x0001;
const SC_MANAGER_ENUMERATE_SERVICE: u32 = 0x0004;
const SERVICE_QUERY_CONFIG: u32 = 0x0001;
const SC_ENUM_PROCESS_INFO: u32 = 0;
const SERVICE_WIN32: u32 = 0x0000_0030;
/// A felhasználói szolgáltatás-példányok (pl. CDPUserSvc_xxxxx). Régebbi rendszeren nem ismert -> tartalék a nélküle.
const SERVICE_USERSERVICE_INSTANCE: u32 = 0x0000_0080;
const SERVICE_STATE_ALL: u32 = 0x0000_0003;
const ERROR_MORE_DATA: u32 = 234;

/// ENUM_SERVICE_STATUS_PROCESSW (a SERVICE_STATUS_PROCESS mezői kibontva).
#[repr(C)]
struct EnumSvc {
    name: *const u16,
    display: *const u16,
    service_type: u32,
    current_state: u32,
    controls_accepted: u32,
    win32_exit: u32,
    service_exit: u32,
    check_point: u32,
    wait_hint: u32,
    process_id: u32,
    service_flags: u32,
}

/// QUERY_SERVICE_CONFIGW.
#[repr(C)]
struct QueryServiceConfig {
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary_path: *const u16,
    load_order_group: *const u16,
    tag_id: u32,
    dependencies: *const u16,
    start_name: *const u16,
    display_name: *const u16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Unknown,
}

impl ServiceState {
    fn from_raw(v: u32) -> Self {
        match v {
            1 => ServiceState::Stopped,
            2 => ServiceState::StartPending,
            3 => ServiceState::StopPending,
            4 => ServiceState::Running,
            5 => ServiceState::ContinuePending,
            6 => ServiceState::PausePending,
            7 => ServiceState::Paused,
            _ => ServiceState::Unknown,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, ServiceState::Running | ServiceState::ContinuePending)
    }

    pub fn label(self) -> &'static str {
        match self {
            ServiceState::Running | ServiceState::ContinuePending => tr("Fut"),
            ServiceState::Stopped => tr("Leállítva"),
            ServiceState::StartPending => tr("Indítás…"),
            ServiceState::StopPending => tr("Leállítás…"),
            ServiceState::PausePending | ServiceState::Paused => tr("Szüneteltetve"),
            ServiceState::Unknown => "—",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceEntry {
    /// A szolgáltatás neve (pl. "Dnscache").
    pub name: String,
    /// A megjelenített név (a Task Manager "Leírás" oszlopa).
    pub display: String,
    /// A futó szolgáltatás folyamatazonosítója; 0, ha nem fut.
    pub pid: u32,
    pub state: ServiceState,
    pub group: String,
}

/// SC_HANDLE (RAII).
struct ScHandle(isize);

impl Drop for ScHandle {
    fn drop(&mut self) {
        // SAFETY: a handle a `ScHandle` sajátja, csak itt záródik.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}

/// Szolgáltatásnév -> csoport. Az üres csoport is érvényes (nem kérdezzük le újra).
#[derive(Default)]
pub struct ServiceCache {
    groups: HashMap<String, String>,
}

/// NUL-lezárt UTF-16 mutatóból String (NULL mutatóra üres szöveg).
unsafe fn wstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    while *p.add(n) != 0 {
        n += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
}

/// A svchost `-k <csoport>` kapcsolója a parancssorból.
fn svchost_group(path: &str) -> Option<String> {
    let lc = path.to_ascii_lowercase();
    let i = lc.find(" -k ")?;
    let g = path[i + 4..].split_whitespace().next()?;
    let g = g.trim_matches('"');
    if g.is_empty() {
        None
    } else {
        Some(g.to_string())
    }
}

/// Egy szolgáltatás csoportja (svchost -k csoport, vagy betöltési sorrend csoport). Hiba esetén üres.
unsafe fn query_group(scm: &ScHandle, name: &str) -> String {
    let n = wide(name);
    let h = OpenServiceW(scm.0, n.as_ptr(), SERVICE_QUERY_CONFIG);
    if h == 0 {
        return String::new();
    }
    let h = ScHandle(h);
    let mut needed = 0u32;
    // Az első hívás szándékosan hibázik (ERROR_INSUFFICIENT_BUFFER), csak a szükséges méretet adja.
    QueryServiceConfigW(h.0, std::ptr::null_mut(), 0, &mut needed);
    if needed == 0 || needed > 65_536 {
        return String::new();
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8) + 1]; // 8 bájtra igazított puffer
    let mut needed2 = 0u32;
    if QueryServiceConfigW(h.0, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut needed2) == 0 {
        return String::new();
    }
    let cfg = &*(buf.as_ptr() as *const QueryServiceConfig);
    let path = wstr(cfg.binary_path);
    if let Some(g) = svchost_group(&path) {
        return g;
    }
    wstr(cfg.load_order_group)
}

/// Egy teljes EnumServicesStatusEx-kör az adott szolgáltatástípus-maszkkal. Hiba esetén a Win32 hibakód.
unsafe fn enum_all(scm: &ScHandle, service_type: u32) -> Result<Vec<(String, String, u32, u32)>, u32> {
    let mut out = Vec::new();
    let mut resume = 0u32;
    let mut buf: Vec<u64> = vec![0; 8192]; // 64 KB, 8 bájtra igazítva (a struktúrában mutatók vannak)
    for _ in 0..64 {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let ok = EnumServicesStatusExW(
            scm.0,
            SC_ENUM_PROCESS_INFO,
            service_type,
            SERVICE_STATE_ALL,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut needed,
            &mut returned,
            &mut resume,
            std::ptr::null(),
        );
        let err = if ok == 0 { GetLastError() } else { 0 };
        if ok == 0 && err != ERROR_MORE_DATA {
            return Err(err);
        }
        let items = buf.as_ptr() as *const EnumSvc;
        for i in 0..returned as usize {
            let s = &*items.add(i);
            out.push((wstr(s.name), wstr(s.display), s.process_id, s.current_state));
        }
        if ok != 0 {
            return Ok(out);
        }
        // ERROR_MORE_DATA: a maradékhoz (resume handle) folytatjuk; ha a puffer kicsi volt, nagyobbat adunk.
        let need_u64 = (needed as usize).div_ceil(8) + 1;
        if need_u64 > buf.len() {
            buf.resize(need_u64, 0);
        } else if returned == 0 {
            return Err(err); // nincs előrelépés: ne pörögjünk a végtelenségig
        }
    }
    Err(ERROR_MORE_DATA)
}

/// A szolgáltatások teljes listája (név szerint rendezve).
pub fn enumerate(cache: &mut ServiceCache) -> Result<Vec<ServiceEntry>, String> {
    // SAFETY: nyers SCM-hívások; minden mutató élő, a pufferek 8 bájtra igazítottak és elég nagyok, a handle-ök RAII-ak.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE);
        if scm == 0 {
            return Err(format!("OpenSCManager: {}", GetLastError()));
        }
        let scm = ScHandle(scm);

        let mut last_err = 0u32;
        let mut raw = None;
        for ty in [SERVICE_WIN32 | SERVICE_USERSERVICE_INSTANCE, SERVICE_WIN32] {
            match enum_all(&scm, ty) {
                Ok(v) => {
                    raw = Some(v);
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        let Some(raw) = raw else {
            return Err(format!("EnumServicesStatusEx: {last_err}"));
        };

        let mut out = Vec::with_capacity(raw.len());
        for (name, display, pid, state) in raw {
            let group = match cache.groups.get(&name) {
                Some(g) => g.clone(),
                None => {
                    let g = query_group(&scm, &name);
                    cache.groups.insert(name.clone(), g.clone());
                    g
                }
            };
            out.push(ServiceEntry { name, display, pid, state: ServiceState::from_raw(state), group });
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Indítás / leállítás / újraindítás (a helyi menühöz)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

unsafe fn stop_service(h: &ScHandle) -> Result<(), u32> {
    let mut st = ServiceStatus::default();
    if ControlService(h.0, SERVICE_CONTROL_STOP, &mut st) == 0 {
        return Err(GetLastError());
    }
    Ok(())
}

unsafe fn start_service(h: &ScHandle) -> Result<(), u32> {
    if StartServiceW(h.0, 0, std::ptr::null()) == 0 {
        return Err(GetLastError());
    }
    Ok(())
}

/// Megvárja, hogy a szolgáltatás leálljon (legfeljebb `timeout`).
unsafe fn wait_stopped(h: &ScHandle, timeout: std::time::Duration) -> Result<(), u32> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut st = ServiceStatus::default();
        if QueryServiceStatus(h.0, &mut st) == 0 {
            return Err(GetLastError());
        }
        if st.current_state == SERVICE_STOPPED {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(1053); // ERROR_SERVICE_REQUEST_TIMEOUT-szerű: a szolgáltatás nem állt le időben
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Egy szolgáltatás indítása / leállítása / újraindítása. Rendszergazdai jog kell hozzá; hiba esetén érthető szöveget ad.
/// Blokkolhat (újraindításnál akár 20 s-ig), ezért külön szálról hívandó.
pub fn control(name: &str, action: ServiceAction) -> Result<(), String> {
    // SAFETY: nyers SCM-hívások; a handle-ök RAII-ak, a mutatók élők.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm == 0 {
            return Err(crate::shell::win_error(GetLastError()));
        }
        let scm = ScHandle(scm);
        let n = wide(name);
        let h = OpenServiceW(scm.0, n.as_ptr(), SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS);
        if h == 0 {
            return Err(crate::shell::win_error(GetLastError()));
        }
        let h = ScHandle(h);
        let res = match action {
            ServiceAction::Start => start_service(&h),
            ServiceAction::Stop => stop_service(&h),
            ServiceAction::Restart => {
                // Ha már nem fut, a leállítás hibáját (1062) figyelmen kívül hagyjuk, és simán elindítjuk.
                let stopped = match stop_service(&h) {
                    Ok(()) => wait_stopped(&h, std::time::Duration::from_secs(20)),
                    Err(ERROR_SERVICE_NOT_ACTIVE) => Ok(()),
                    Err(e) => Err(e),
                };
                stopped.and_then(|_| start_service(&h))
            }
        };
        res.map_err(crate::shell::win_error)
    }
}
