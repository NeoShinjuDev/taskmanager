/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Rendszerszintű segédek a helyi (jobb klikkes) menükhöz: ablak előtérbe hozása, Fájlkezelő, Tulajdonságok ablak,
//! online keresés, szolgáltatáskezelő indítása, folyamat-prioritás és -affinitás.
//!
//! Nyers Win32 FFI saját szignatúrákkal – ugyanúgy, mint a `sysdata.rs` és a `winreg.rs`, hogy ne függjön a
//! `windows` crate feature-készletétől. Minden függvény hibát ad vissza (`Err(szöveg)`), sosem panikol.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Once;

use crate::i18n::tr;
use crate::winreg::wide;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
    fn IsWindowVisible(h: isize) -> i32;
    fn GetWindowTextLengthW(h: isize) -> i32;
    fn GetWindow(h: isize, cmd: u32) -> isize;
    fn GetWindowLongPtrW(h: isize, index: i32) -> isize;
    fn GetWindowThreadProcessId(h: isize, pid: *mut u32) -> u32;
    fn IsIconic(h: isize) -> i32;
    fn IsZoomed(h: isize) -> i32;
    fn ShowWindow(h: isize, cmd: i32) -> i32;
    fn SetForegroundWindow(h: isize) -> i32;
    fn GetWindowTextW(h: isize, buf: *mut u16, max: i32) -> i32;
    fn GetWindowRect(h: isize, rect: *mut WinRect) -> i32;
    fn SetWindowPos(h: isize, insert_after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
}

#[link(name = "uxtheme")]
extern "system" {
    fn SetWindowThemeAttribute(h: isize, attribute: i32, data: *const WtaOptions, size: u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn CloseHandle(h: isize) -> i32;
    fn SetPriorityClass(h: isize, class: u32) -> i32;
    fn GetProcessAffinityMask(h: isize, process_mask: *mut usize, system_mask: *mut usize) -> i32;
    fn SetProcessAffinityMask(h: isize, mask: usize) -> i32;
    fn GetLastError() -> u32;
    fn GetCurrentProcessId() -> u32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "comdlg32")]
extern "system" {
    fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
}

const PROCESS_SET_INFORMATION: u32 = 0x0200;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// A Windows prioritási osztályai (SetPriorityClass). A "valós idejű" szándékosan nincs a menüben:
/// rossz folyamaton lefagyaszthatja a gépet.
pub const PRIORITY_HIGH: u32 = 0x0000_0080;
pub const PRIORITY_ABOVE_NORMAL: u32 = 0x0000_8000;
pub const PRIORITY_NORMAL: u32 = 0x0000_0020;
pub const PRIORITY_BELOW_NORMAL: u32 = 0x0000_4000;
pub const PRIORITY_IDLE: u32 = 0x0000_0040;

/// SHELLEXECUTEINFOW (x64 és x86 elrendezés is a C-szabály szerint).
#[repr(C)]
struct ShellExecuteInfoW {
    cb_size: u32,
    mask: u32,
    hwnd: isize,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    inst_app: isize,
    id_list: *mut c_void,
    class: *const u16,
    hkey_class: isize,
    hot_key: u32,
    icon_or_monitor: isize,
    process: isize,
}

/// OPENFILENAMEW (C-elrendezés; x64-en és x86-on is a Windows fejléke szerinti).
#[repr(C)]
struct OpenFileNameW {
    struct_size: u32,
    hwnd_owner: isize,
    instance: isize,
    filter: *const u16,
    custom_filter: *mut u16,
    max_custom_filter: u32,
    filter_index: u32,
    file: *mut u16,
    max_file: u32,
    file_title: *mut u16,
    max_file_title: u32,
    initial_dir: *const u16,
    title: *const u16,
    flags: u32,
    file_offset: u16,
    file_extension: u16,
    def_ext: *const u16,
    cust_data: isize,
    hook: *const c_void,
    template_name: *const u16,
    reserved_ptr: *mut c_void,
    reserved_dw: u32,
    flags_ex: u32,
}

#[repr(C)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// WTA_OPTIONS (SetWindowThemeAttribute).
#[repr(C)]
struct WtaOptions {
    flags: u32,
    mask: u32,
}

/// Win32 hibakód -> érthető szöveg (a gyakoribbak lefordítva).
pub fn win_error(code: u32) -> String {
    match code {
        5 => tr("Hozzáférés megtagadva – rendszergazdai jog szükséges.").to_string(),
        2 | 3 => tr("A rendszer nem találja a megadott fájlt.").to_string(),
        87 => tr("Érvénytelen paraméter.").to_string(),
        1223 => tr("A kérést megszakítottad.").to_string(),
        1051 => tr("Más, futó szolgáltatások függnek ettől a szolgáltatástól.").to_string(),
        1053 => tr("A szolgáltatás nem válaszolt időben.").to_string(),
        1056 => tr("A szolgáltatás már fut.").to_string(),
        1062 => tr("A szolgáltatás nem fut.").to_string(),
        _ => format!("{} {code}", tr("Win32-hiba")),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Ablak előtérbe hozása ("Átváltás")
// ═══════════════════════════════════════════════════════════════════════════

struct FindWin<'a> {
    pids: &'a [u32],
    found: isize,
}

unsafe extern "system" fn find_cb(hwnd: isize, lparam: isize) -> i32 {
    // SAFETY: az lparam a hívó (`focus_window_of_pids`) élő `FindWin` példányára mutat.
    let data = &mut *(lparam as *mut FindWin);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    if GetWindow(hwnd, 4) != 0 {
        return 1; // GW_OWNER: csak a nem tulajdonolt, felső szintű ablakok
    }
    if GetWindowLongPtrW(hwnd, -20) & 0x80 != 0 {
        return 1; // WS_EX_TOOLWINDOW
    }
    if GetWindowTextLengthW(hwnd) == 0 {
        return 1;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if data.pids.contains(&pid) {
        data.found = hwnd;
        return 0; // megvan, a felsorolás leáll
    }
    1
}

/// A megadott folyamatok valamelyikének látható főablakát előtérbe hozza (kicsinyítettből visszaállítja).
pub fn focus_window_of_pids(pids: &[u32]) -> bool {
    let mut data = FindWin { pids, found: 0 };
    // SAFETY: a callback csak a `data`-t éri el, ami a hívás végéig él.
    unsafe {
        EnumWindows(find_cb, &mut data as *mut FindWin as isize);
        if data.found == 0 {
            return false;
        }
        if IsIconic(data.found) != 0 {
            ShowWindow(data.found, 9); // SW_RESTORE
        }
        SetForegroundWindow(data.found) != 0
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Fájlkezelő, Tulajdonságok, online keresés, szolgáltatáskezelő
// ═══════════════════════════════════════════════════════════════════════════

/// Megnyitja a Fájlkezelőt, és kijelöli a fájlt.
pub fn reveal_in_explorer(path: &str) -> Result<(), String> {
    // A raw_arg-ra azért van szükség, mert az explorer `/select,"útvonal"` alakot vár (idézőjel csak az útvonal körül).
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A fájl Tulajdonságok ablaka (ugyanaz, mint a Fájlkezelő jobb klikk -> Tulajdonságok).
pub fn show_properties(path: &str) -> Result<(), String> {
    static COM_INIT: Once = Once::new();
    COM_INIT.call_once(|| {
        // SAFETY: egyszeri COM-inicializálás (STA, DDE nélkül); az eredmény (S_FALSE / már inicializált) nem érdekes.
        unsafe {
            CoInitializeEx(std::ptr::null_mut(), 0x2 | 0x4);
        }
    });
    let verb = wide("properties");
    let file = wide(path);
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        mask: 0x0000_000C, // SEE_MASK_INVOKEIDLIST
        hwnd: 0,
        verb: verb.as_ptr(),
        file: file.as_ptr(),
        parameters: std::ptr::null(),
        directory: std::ptr::null(),
        show: 5, // SW_SHOW
        inst_app: 0,
        id_list: std::ptr::null_mut(),
        class: std::ptr::null(),
        hkey_class: 0,
        hot_key: 0,
        icon_or_monitor: 0,
        process: 0,
    };
    // SAFETY: az `info` és a benne lévő mutatók (verb, file) a hívás végéig élnek.
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err(win_error(GetLastError()));
        }
    }
    Ok(())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Online keresés az alapértelmezett böngészőben (a Task Manager is Bing-et használ).
pub fn search_online(query: &str) -> Result<(), String> {
    let url = format!("https://www.bing.com/search?q={}", percent_encode(query));
    Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A natív szolgáltatáskezelő megnyitása. A `services.msc` MMC-modul, nem közvetlenül futtatható fájl
/// (a CreateProcess nem tudja elindítani), ezért az `mmc.exe` kapja argumentumként – ugyanez történik a Futtatás ablakból.
pub fn open_services_console() -> Result<(), String> {
    Command::new("mmc.exe").arg("services.msc").spawn().map(|_| ()).map_err(|e| e.to_string())
}

// ═══════════════════════════════════════════════════════════════════════════
// Prioritás és affinitás
// ═══════════════════════════════════════════════════════════════════════════

/// Egy folyamat prioritási osztályának beállítása.
pub fn set_priority(pid: u32, class: u32) -> Result<(), String> {
    // SAFETY: a handle-t nyitjuk, használjuk, és mindenképp lezárjuk.
    unsafe {
        let h = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if h == 0 {
            return Err(win_error(GetLastError()));
        }
        let ok = SetPriorityClass(h, class);
        let err = GetLastError();
        CloseHandle(h);
        if ok == 0 {
            Err(win_error(err))
        } else {
            Ok(())
        }
    }
}

/// (a folyamat affinitás-maszkja, a rendszer processzor-maszkja).
pub fn get_affinity(pid: u32) -> Result<(usize, usize), String> {
    // SAFETY: a handle-t nyitjuk, használjuk, és mindenképp lezárjuk.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return Err(win_error(GetLastError()));
        }
        let (mut process_mask, mut system_mask) = (0usize, 0usize);
        let ok = GetProcessAffinityMask(h, &mut process_mask, &mut system_mask);
        let err = GetLastError();
        CloseHandle(h);
        if ok == 0 {
            Err(win_error(err))
        } else {
            Ok((process_mask, system_mask))
        }
    }
}

/// Egy folyamat affinitás-maszkjának beállítása (a 0 maszk érvénytelen).
pub fn set_affinity(pid: u32, mask: usize) -> Result<(), String> {
    if mask == 0 {
        return Err(tr("Legalább egy processzort ki kell választani.").to_string());
    }
    // SAFETY: a handle-t nyitjuk, használjuk, és mindenképp lezárjuk.
    unsafe {
        let h = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if h == 0 {
            return Err(win_error(GetLastError()));
        }
        let ok = SetProcessAffinityMask(h, mask);
        let err = GetLastError();
        CloseHandle(h);
        if ok == 0 {
            Err(win_error(err))
        } else {
            Ok(())
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Új feladat futtatása (a Windows "Futtatás" ablakának megfelelője)
// ═══════════════════════════════════════════════════════════════════════════

/// A beírt szöveget (program, mappa, dokumentum, URL – esetleg paraméterekkel) fájlra és paraméterekre bontja.
fn split_command(s: &str) -> (String, String) {
    let s = s.trim();
    // Idézőjeles útvonal: "C:\Program Files\x.exe" -arg
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return (rest[..end].to_string(), rest[end + 1..].trim().to_string());
        }
    }
    // URL vagy létező útvonal egészében (mappa, dokumentum, szóközös útvonal).
    if s.contains("://") || std::path::Path::new(s).exists() {
        return (s.to_string(), String::new());
    }
    // Idézőjel nélküli, szóközös útvonal + paraméterek: a legrövidebb létező fájl-előtag a program.
    for (i, ch) in s.char_indices() {
        if ch == ' ' && std::path::Path::new(&s[..i]).is_file() {
            return (s[..i].to_string(), s[i + 1..].trim().to_string());
        }
    }
    // Egyébként az első szó a program (a PATH-ból oldódik fel, pl. "notepad foo.txt"), a többi paraméter.
    match s.find(' ') {
        Some(i) => (s[..i].to_string(), s[i + 1..].trim().to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// Elindít egy programot / megnyit egy mappát, dokumentumot vagy URL-t. `admin`: rendszergazdaként ("runas", UAC-kéréssel).
/// A ShellExecuteEx a Futtatás ablak mechanizmusa, így környezeti változók, PATH, társított programok és URL-ek is mennek.
/// UAC-kérésnél blokkol, ezért külön szálról hívandó.
pub fn run_task(command: &str, admin: bool) -> Result<(), String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err(tr("Adj meg egy programot, mappát, dokumentumot vagy internetes erőforrást.").to_string());
    }
    let expanded = crate::winreg::expand_env(cmd);
    let (file, params) = split_command(&expanded);
    let verb = wide(if admin { "runas" } else { "open" });
    let file_w = wide(&file);
    let params_w = wide(&params);
    let mut info = ShellExecuteInfoW {
        cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
        mask: 0x0000_0400, // SEE_MASK_FLAG_NO_UI: a hibát mi jelezzük, nem a Windows ablaka
        hwnd: 0,
        verb: verb.as_ptr(),
        file: file_w.as_ptr(),
        parameters: if params.is_empty() { std::ptr::null() } else { params_w.as_ptr() },
        directory: std::ptr::null(),
        show: 1, // SW_SHOWNORMAL
        inst_app: 0,
        id_list: std::ptr::null_mut(),
        class: std::ptr::null(),
        hkey_class: 0,
        hot_key: 0,
        icon_or_monitor: 0,
        process: 0,
    };
    // SAFETY: az `info` és a benne lévő mutatók (verb, file, params) a hívás végéig élnek.
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err(win_error(GetLastError()));
        }
    }
    Ok(())
}

/// Natív "Megnyitás" fájlválasztó (a Tallózás gombhoz). `None`, ha a felhasználó mégsem választott. Blokkol: külön szálról hívandó.
pub fn browse_for_file() -> Option<String> {
    // Szűrő: "név\0minta\0név\0minta\0\0"
    let mut filter: Vec<u16> = Vec::new();
    for part in [tr("Programok (*.exe)"), "*.exe", tr("Minden fájl (*.*)"), "*.*"] {
        filter.extend(part.encode_utf16());
        filter.push(0);
    }
    filter.push(0);
    let title = wide(tr("Tallózás"));
    let mut buf = vec![0u16; 1024];
    let mut ofn = OpenFileNameW {
        struct_size: std::mem::size_of::<OpenFileNameW>() as u32,
        hwnd_owner: 0,
        instance: 0,
        filter: filter.as_ptr(),
        custom_filter: std::ptr::null_mut(),
        max_custom_filter: 0,
        filter_index: 1,
        file: buf.as_mut_ptr(),
        max_file: buf.len() as u32,
        file_title: std::ptr::null_mut(),
        max_file_title: 0,
        initial_dir: std::ptr::null(),
        title: title.as_ptr(),
        // OFN_NOCHANGEDIR | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_EXPLORER
        flags: 0x0000_0008 | 0x0000_0800 | 0x0000_1000 | 0x0008_0000,
        file_offset: 0,
        file_extension: 0,
        def_ext: std::ptr::null(),
        cust_data: 0,
        hook: std::ptr::null(),
        template_name: std::ptr::null(),
        reserved_ptr: std::ptr::null_mut(),
        reserved_dw: 0,
        flags_ex: 0,
    };
    // SAFETY: az `ofn` mutatói (szűrő, puffer, cím) a hívás végéig élnek; a COM-ot a hívó szálon inicializáljuk (STA).
    let ok = unsafe {
        CoInitializeEx(std::ptr::null_mut(), 0x2);
        GetOpenFileNameW(&mut ofn)
    };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = String::from_utf16_lossy(&buf[..end]);
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Ablak-keret és DWM-segédek
// ═══════════════════════════════════════════════════════════════════════════

const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_FRAMECHANGED: u32 = 0x0020;

/// A natív címsorból elrejti a szöveget és az ikont (a gombok, a húzás és a Mica megmarad). A program a saját,
/// nagyobb ikonját és címét a bal sáv tetején rajzolja, így nem jelenik meg duplán. Az ablak címe (tálca, Alt+Tab)
/// változatlan marad.
pub fn hide_caption_text_and_icon(hwnd: isize) {
    // WTNCA_NODRAWCAPTION (1) | WTNCA_NODRAWICON (2); WTA_NONCLIENT = 1
    let opts = WtaOptions { flags: 0x3, mask: 0x3 };
    // SAFETY: az `opts` a hívás végéig él, a méret megegyezik a struktúráéval.
    unsafe {
        SetWindowThemeAttribute(hwnd, 1, &opts, std::mem::size_of::<WtaOptions>() as u32);
    }
}

/// A DWM-keret újraszámoltatása (WM_NCCALCSIZE). Enélkül a Mica / az átlátszó felület néha csak egy
/// maximalizálás–visszaállítás után kerül érvénybe.
pub fn frame_changed(hwnd: isize) {
    // SAFETY: csak a keretet frissíti; a pozíció, a méret és a Z-sorrend nem változik.
    unsafe {
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    }
}

/// Az ablak szélességét `dw` képponttal változtatja (nem maximalizált, nem kicsinyített ablaknál). Egy +1 / −1 pár
/// kikényszeríti az úszólánc újrakonfigurálását anélkül, hogy bármi látható változna.
pub fn resize_by(hwnd: isize, dw: i32) -> bool {
    // SAFETY: a `rect` egy élő, kimeneti struktúra; a hívások csak a saját ablakunkra vonatkoznak.
    unsafe {
        if IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
            return false;
        }
        let mut r = WinRect { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(hwnd, &mut r) == 0 {
            return false;
        }
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            (r.right - r.left) + dw,
            r.bottom - r.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) != 0
    }
}

struct FindTitle {
    title: String,
    pid: u32,
    found: isize,
}

unsafe extern "system" fn find_title_cb(hwnd: isize, lparam: isize) -> i32 {
    // SAFETY: az lparam a hívó (`find_own_window_by_title`) élő `FindTitle` példányára mutat.
    let d = &mut *(lparam as *mut FindTitle);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != d.pid {
        return 1;
    }
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == d.title {
        d.found = hwnd;
        return 0;
    }
    1
}

/// A saját folyamat egy felső szintű ablakának HWND-je cím alapján (a külön natív ablakként nyíló párbeszédekhez,
/// amelyek HWND-jét az egui nem adja át közvetlenül). `None`, ha (még) nincs ilyen ablak.
pub fn find_own_window_by_title(title: &str) -> Option<isize> {
    // SAFETY: a callback csak a `data`-t éri el, ami a hívás végéig él.
    unsafe {
        let mut data = FindTitle { title: title.to_string(), pid: GetCurrentProcessId(), found: 0 };
        EnumWindows(find_title_cb, &mut data as *mut FindTitle as isize);
        if data.found == 0 {
            None
        } else {
            Some(data.found)
        }
    }
}
