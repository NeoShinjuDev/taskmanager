/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */


//! Indítási alkalmazások: a Windows Task Manager "Indítás" fülének logikája.
//!
//! FORRÁSOK (ugyanazok, amiket a Task Manager is mutat):
//!   * HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//!   * HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run            (64 bites nézet)
//!   * HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run (32 bites nézet)
//!   * a felhasználói és a közös Indítópult-mappa (shell:startup / shell:common startup)
//!
//! BE/KI KAPCSOLÁS: a Windows NEM törli a Run-értéket, hanem a párhuzamos `...\Explorer\StartupApproved\<Run|Run32|
//! StartupFolder>` kulcsba ír egy 12 bájtos bináris értéket ugyanazzal a névvel: az első bájt 0x02 = engedélyezve,
//! 0x03 = letiltva (a maradék 8 bájt a letiltás FILETIME-ja). Mi is ezt tesszük, így a Task Manager és a Beállítások
//! app ugyanazt az állapotot látja, és az eredeti bejegyzés (parancssor) érintetlen marad.
//!
//! A HKLM-es bejegyzések kapcsolásához rendszergazdai jog kell (a program alapból UAC-t kér induláskor).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::i18n::tr;
use crate::winreg::{
    self, RegKey, RegValue, ERROR_ACCESS_DENIED, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE,
    KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};

const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED_BASE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved";

/// Honnan származik a bejegyzés.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartupSource {
    HkcuRun,
    HklmRun,
    HklmRun32,
    UserFolder,
    CommonFolder,
}

impl StartupSource {
    pub const ALL: [StartupSource; 5] = [
        StartupSource::HkcuRun,
        StartupSource::HklmRun,
        StartupSource::HklmRun32,
        StartupSource::UserFolder,
        StartupSource::CommonFolder,
    ];

    /// Rövid, a felületen megjelenő forrás-felirat.
    pub fn label(self) -> &'static str {
        match self {
            StartupSource::HkcuRun => "HKCU\\Run",
            StartupSource::HklmRun => "HKLM\\Run",
            StartupSource::HklmRun32 => "HKLM\\Run (32 bit)",
            StartupSource::UserFolder => tr("Indítópult-mappa (felhasználó)"),
            StartupSource::CommonFolder => tr("Indítópult-mappa (közös)"),
        }
    }

    /// A kapcsoláshoz rendszergazdai jog kell-e (HKLM / közös mappa).
    pub fn needs_admin(self) -> bool {
        matches!(self, StartupSource::HklmRun | StartupSource::HklmRun32 | StartupSource::CommonFolder)
    }

    /// A StartupApproved kulcs helye: (gyökér, alkulcs).
    fn approved_location(self) -> (isize, String) {
        let (root, leaf) = match self {
            StartupSource::HkcuRun => (HKEY_CURRENT_USER, "Run"),
            StartupSource::HklmRun => (HKEY_LOCAL_MACHINE, "Run"),
            StartupSource::HklmRun32 => (HKEY_LOCAL_MACHINE, "Run32"),
            StartupSource::UserFolder => (HKEY_CURRENT_USER, "StartupFolder"),
            StartupSource::CommonFolder => (HKEY_LOCAL_MACHINE, "StartupFolder"),
        };
        (root, format!("{APPROVED_BASE}\\{leaf}"))
    }
}

/// Egy indítási bejegyzés.
#[derive(Clone, Debug)]
pub struct StartupEntry {
    pub source: StartupSource,
    /// A Run-érték neve, ill. a mappában a fájlnév (kiterjesztéssel) – ez a StartupApproved-érték neve is.
    pub value_name: String,
    /// Megjelenített név.
    pub name: String,
    pub command: String,
    /// A parancssorból kinyert futtatható fájl teljes elérési útja (ha meghatározható).
    pub exe: Option<String>,
    /// A futtatható fájl neve kisbetűvel (a futó folyamatokkal való párosításhoz).
    pub exe_name_lc: String,
    pub publisher: String,
    pub enabled: bool,
}

impl StartupEntry {
    /// A fájl, amire a "Fájl helyének megnyitása" és a "Tulajdonságok" vonatkozik: a futtatható fájl, mappás
    /// elemnél (parancsikon) maga a mappában lévő fájl.
    pub fn target_path(&self) -> Option<String> {
        self.exe.clone().or_else(|| match self.source {
            StartupSource::UserFolder | StartupSource::CommonFolder => Some(self.command.clone()),
            _ => None,
        })
    }
}

/// Becsült indítási hatás.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Impact {
    Unknown,
    Low,
    Medium,
    High,
}

impl Impact {
    pub fn label(self) -> &'static str {
        match self {
            Impact::Unknown => tr("Nincs mérve"),
            Impact::Low => tr("Alacsony"),
            Impact::Medium => tr("Közepes"),
            Impact::High => tr("Magas"),
        }
    }
}

/// Az indítási hatás BECSLÉSE a már futó folyamat adataiból: (összes processzoridő ms, memória MB).
///
/// A Windows saját értéke a rendszerindítás alatti CPU- és lemezhasználatból jön (ETW boot trace), ami utólag nem
/// érhető el publikus API-n. Ez a becslés ehhez közelít: a `None` (nem fut, tehát nincs mit mérni) → "Nincs mérve".
/// A küszöbök a Windows ismert kategóriáihoz igazodnak (Magas: > 1 s CPU, Közepes: 0,3–1 s), de az itteni processzoridő
/// a folyamat teljes életére vonatkozik, ezért a határokat felfelé skáláztuk.
pub fn estimate_impact(stats: Option<(u64, f64)>) -> Impact {
    match stats {
        None => Impact::Unknown,
        Some((cpu_ms, mem_mb)) => {
            if cpu_ms >= 10_000 || mem_mb >= 400.0 {
                Impact::High
            } else if cpu_ms >= 2_000 || mem_mb >= 100.0 {
                Impact::Medium
            } else {
                Impact::Low
            }
        }
    }
}

/// A parancssorból a futtatható fájl elérési útja: idézőjeles útvonal, vagy az első `.exe`-ig tartó rész.
pub fn extract_exe(cmd: &str) -> Option<String> {
    let c = cmd.trim();
    if c.is_empty() {
        return None;
    }
    if let Some(rest) = c.strip_prefix('"') {
        let end = rest.find('"')?;
        let p = rest[..end].trim();
        return if p.is_empty() { None } else { Some(p.to_string()) };
    }
    // ASCII-kisbetűsítés: a bájt-eltolások megegyeznek az eredeti szövegével.
    let lc = c.to_ascii_lowercase();
    if let Some(i) = lc.find(".exe") {
        return Some(c[..i + 4].to_string());
    }
    c.split_whitespace().next().map(|s| s.to_string())
}

fn file_name_lc(path: &str) -> String {
    path.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(path).to_lowercase()
}

/// A StartupApproved bináris érték első bájtja alapján: páratlan (0x03, 0x07...) = letiltva, páros (0x02, 0x06) = engedélyezve.
fn approved_is_enabled(data: &[u8]) -> bool {
    data.first().map_or(true, |b| b & 1 == 0)
}

/// Egy StartupApproved-kulcs tartalma: kisbetűs értéknév -> engedélyezve-e. Hiányzó kulcs/érték = engedélyezve.
fn approved_map(src: StartupSource) -> HashMap<String, bool> {
    let (root, sub) = src.approved_location();
    let Ok(key) = RegKey::open(root, &sub, KEY_QUERY_VALUE | KEY_WOW64_64KEY) else {
        return HashMap::new();
    };
    key.values()
        .into_iter()
        .filter_map(|(n, v)| match v {
            RegValue::Binary(b) => Some((n.to_lowercase(), approved_is_enabled(&b))),
            _ => None,
        })
        .collect()
}

/// Gyártó lekérdezése gyorsítótárral (a verzióinformáció olvasása lemez-I/O).
fn publisher_of(exe: &Option<String>, cache: &mut HashMap<String, String>) -> String {
    let Some(path) = exe else { return String::new() };
    if let Some(p) = cache.get(path) {
        return p.clone();
    }
    let p = crate::sysdata::file_company(path).unwrap_or_default();
    cache.insert(path.clone(), p.clone());
    p
}

fn collect_run(src: StartupSource, cache: &mut HashMap<String, String>, out: &mut Vec<StartupEntry>) {
    let (root, wow) = match src {
        StartupSource::HkcuRun => (HKEY_CURRENT_USER, 0),
        StartupSource::HklmRun => (HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY),
        StartupSource::HklmRun32 => (HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY),
        _ => return,
    };
    let Ok(key) = RegKey::open(root, RUN_SUBKEY, KEY_QUERY_VALUE | wow) else { return };
    let approved = approved_map(src);
    for (name, value) in key.values() {
        let RegValue::Str(command) = value else { continue };
        if name.is_empty() {
            continue; // az alapértelmezett (névtelen) érték nem indítási bejegyzés
        }
        let exe = extract_exe(&command);
        let enabled = approved.get(&name.to_lowercase()).copied().unwrap_or(true);
        out.push(StartupEntry {
            source: src,
            value_name: name.clone(),
            name,
            exe_name_lc: exe.as_deref().map(file_name_lc).unwrap_or_default(),
            publisher: publisher_of(&exe, cache),
            exe,
            command,
            enabled,
        });
    }
}

fn folder_path(src: StartupSource) -> Option<PathBuf> {
    let (var, fallback) = match src {
        StartupSource::UserFolder => ("APPDATA", "Microsoft\\Windows\\Start Menu\\Programs\\Startup"),
        StartupSource::CommonFolder => ("ProgramData", "Microsoft\\Windows\\Start Menu\\Programs\\StartUp"),
        _ => return None,
    };
    std::env::var_os(var).map(|b| PathBuf::from(b).join(fallback))
}

fn collect_folder(src: StartupSource, cache: &mut HashMap<String, String>, out: &mut Vec<StartupEntry>) {
    let Some(dir) = folder_path(src) else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let approved = approved_map(src);
    for item in rd.flatten() {
        let path = item.path();
        if !path.is_file() {
            continue;
        }
        let file_name = item.file_name().to_string_lossy().into_owned();
        if file_name.eq_ignore_ascii_case("desktop.ini") {
            continue;
        }
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| file_name.clone());
        let full = path.to_string_lossy().into_owned();
        // A .lnk parancsikon céljának feloldása COM-ot (IShellLink) igényelne; a közvetlen .exe-ket tudjuk kezelni.
        let is_exe = path.extension().map_or(false, |e| e.eq_ignore_ascii_case("exe"));
        let exe = if is_exe { Some(full.clone()) } else { None };
        let enabled = approved.get(&file_name.to_lowercase()).copied().unwrap_or(true);
        out.push(StartupEntry {
            source: src,
            value_name: file_name,
            name: stem,
            exe_name_lc: exe.as_deref().map(file_name_lc).unwrap_or_default(),
            publisher: publisher_of(&exe, cache),
            exe,
            command: full,
            enabled,
        });
    }
}

/// Az összes indítási bejegyzés (név szerint rendezve). A háttérszálról hívandó.
pub fn list(publisher_cache: &mut HashMap<String, String>) -> Vec<StartupEntry> {
    let mut out = Vec::new();
    collect_run(StartupSource::HkcuRun, publisher_cache, &mut out);
    collect_run(StartupSource::HklmRun, publisher_cache, &mut out);
    collect_run(StartupSource::HklmRun32, publisher_cache, &mut out);
    collect_folder(StartupSource::UserFolder, publisher_cache, &mut out);
    collect_folder(StartupSource::CommonFolder, publisher_cache, &mut out);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn describe_error(code: u32) -> String {
    if code == ERROR_ACCESS_DENIED {
        tr("Hozzáférés megtagadva – rendszergazdai jog szükséges.").to_string()
    } else {
        format!("{} ({code})", tr("Registry-hiba"))
    }
}

/// Egy bejegyzés engedélyezése / letiltása (StartupApproved).
pub fn set_enabled(source: StartupSource, value_name: &str, enabled: bool) -> Result<(), String> {
    let (root, sub) = source.approved_location();
    let key = RegKey::create(root, &sub, KEY_SET_VALUE | KEY_WOW64_64KEY).map_err(describe_error)?;
    let mut data = [0u8; 12];
    if enabled {
        data[0] = 0x02;
    } else {
        data[0] = 0x03;
        data[4..12].copy_from_slice(&winreg::filetime_now().to_le_bytes());
    }
    key.set_binary(value_name, &data).map_err(describe_error)
}

/// "Legutóbbi BIOS művelet ideje" ezredmásodpercben: a firmware POST-idő, amit a Windows a
/// HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Power\FwPOSTTime értékbe ment.
/// `None`, ha az alaplap/UEFI nem adja át (a Task Manager ilyenkor üresen hagyja).
pub fn last_bios_time_ms() -> Option<u64> {
    let key = RegKey::open(
        HKEY_LOCAL_MACHINE,
        "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Power",
        KEY_QUERY_VALUE | KEY_WOW64_64KEY,
    )
    .ok()?;
    let ms = match key.value("FwPOSTTime")? {
        RegValue::Dword(v) => v as u64,
        RegValue::Qword(v) => v,
        _ => return None,
    };
    if ms == 0 {
        None
    } else {
        Some(ms)
    }
}
