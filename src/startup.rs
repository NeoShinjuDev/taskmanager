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
//! Indítási alkalmazások: a Windows Task Manager "Indítás" fülének logikája.
//!
//! FORRÁSOK (ugyanazok, amiket a Task Manager is mutat):
//!   * HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//!   * HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run            (64 bites nézet)
//!   * HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run (32 bites nézet)
//!   * a felhasználói és a közös Indítópult-mappa (shell:startup / shell:common startup); a .lnk parancsikonok célját a
//!     Shell Link bináris formátumból olvassuk ki (COM nélkül), így a gyártó, az ikon és a futás-állapot is megvan
//!   * a csomagolt (Microsoft Store / MSIX) alkalmazások indítási feladatai:
//!     HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\SystemAppData\
//!     <csomagcsalád>\<feladat>, a `State` értékkel (0 / 1 = letiltva, 2 = engedélyezve, 3 / 4 = házirend)
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
    KEY_ENUMERATE_SUB_KEYS, KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};

const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED_BASE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved";
/// A csomagolt alkalmazások nyilvántartása (felhasználónként): az indítási feladatok állapota és a csomagtár.
const APPMODEL_BASE: &str = "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel";

/// Honnan származik a bejegyzés.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartupSource {
    HkcuRun,
    HklmRun,
    HklmRun32,
    UserFolder,
    CommonFolder,
    /// Csomagolt (Microsoft Store / MSIX) alkalmazás indítási feladata.
    Packaged,
}

impl StartupSource {
    /// Rövid, a felületen megjelenő forrás-felirat.
    pub fn label(self) -> &'static str {
        match self {
            StartupSource::HkcuRun => "HKCU\\Run",
            StartupSource::HklmRun => "HKLM\\Run",
            StartupSource::HklmRun32 => "HKLM\\Run (32 bit)",
            StartupSource::UserFolder => tr("Startup folder (user)"),
            StartupSource::CommonFolder => tr("Startup folder (all users)"),
            StartupSource::Packaged => tr("Store app"),
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
            // A csomagolt alkalmazások állapota máshol van (lásd `set_packaged_enabled`); ide nem érkezik hívás.
            StartupSource::Packaged => (HKEY_CURRENT_USER, "Packaged"),
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
    /// A letiltás ideje (Windows FILETIME), ha a StartupApproved-érték tartalmazza.
    pub disabled_at: Option<u64>,
    /// Mappás elemnél a mappában lévő fájl (parancsikon) teljes útja.
    pub item_path: Option<String>,
}

impl StartupEntry {
    /// A fájl, amire a "Fájl helyének megnyitása" és a "Tulajdonságok" vonatkozik: a futtatható fájl; ha az nem
    /// ismert, mappás elemnél maga a mappában lévő fájl (parancsikon).
    pub fn target_path(&self) -> Option<String> {
        self.exe.clone().or_else(|| self.item_path.clone())
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
            Impact::Unknown => tr("Not measured"),
            Impact::Low => tr("Low"),
            Impact::Medium => tr("Medium"),
            Impact::High => tr("High"),
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

/// A StartupApproved érték: (engedélyezve-e, a letiltás ideje). A letiltott értékben a 4. bájttól 8 bájt FILETIME van.
fn approved_info(data: &[u8]) -> (bool, Option<u64>) {
    let enabled = approved_is_enabled(data);
    let at = if !enabled && data.len() >= 12 {
        let arr: [u8; 8] = data[4..12].try_into().unwrap_or([0; 8]);
        let t = u64::from_le_bytes(arr);
        if t > 0 {
            Some(t)
        } else {
            None
        }
    } else {
        None
    };
    (enabled, at)
}

/// Egy StartupApproved-kulcs tartalma: kisbetűs értéknév -> (engedélyezve-e, letiltás ideje). Hiányzó kulcs/érték =
/// engedélyezve.
fn approved_map(src: StartupSource) -> HashMap<String, (bool, Option<u64>)> {
    let (root, sub) = src.approved_location();
    let Ok(key) = RegKey::open(root, &sub, KEY_QUERY_VALUE | KEY_WOW64_64KEY) else {
        return HashMap::new();
    };
    key.values()
        .into_iter()
        .filter_map(|(n, v)| match v {
            RegValue::Binary(b) => Some((n.to_lowercase(), approved_info(&b))),
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
        let (enabled, disabled_at) = approved.get(&name.to_lowercase()).copied().unwrap_or((true, None));
        out.push(StartupEntry {
            source: src,
            value_name: name.clone(),
            name,
            exe_name_lc: exe.as_deref().map(file_name_lc).unwrap_or_default(),
            publisher: publisher_of(&exe, cache),
            exe,
            command,
            enabled,
            disabled_at,
            item_path: None,
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
        // Közvetlen .exe, vagy .lnk parancsikon: utóbbi célját a Shell Link formátumból olvassuk ki (COM nélkül).
        let ext_is = |want: &str| path.extension().map_or(false, |e| e.eq_ignore_ascii_case(want));
        let (exe, command) = if ext_is("lnk") {
            match read_lnk(&path) {
                Some((target, args)) => {
                    let command = if args.is_empty() { target.clone() } else { format!("{target} {args}") };
                    let exe = if target.to_ascii_lowercase().ends_with(".exe") { Some(target) } else { None };
                    (exe, command)
                }
                None => (None, full.clone()),
            }
        } else if ext_is("exe") {
            (Some(full.clone()), full.clone())
        } else {
            (None, full.clone())
        };
        let (enabled, disabled_at) = approved.get(&file_name.to_lowercase()).copied().unwrap_or((true, None));
        out.push(StartupEntry {
            source: src,
            value_name: file_name,
            name: stem,
            exe_name_lc: exe.as_deref().map(file_name_lc).unwrap_or_default(),
            publisher: publisher_of(&exe, cache),
            exe,
            command,
            enabled,
            disabled_at,
            item_path: Some(full),
        });
    }
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// NUL-lezárt ANSI szöveg a megadott helyen.
fn ansi_cstr(b: &[u8], at: usize) -> Option<String> {
    let s = b.get(at..)?;
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    Some(String::from_utf8_lossy(&s[..end]).into_owned())
}

/// NUL-lezárt UTF-16 szöveg a megadott helyen.
fn utf16_cstr(b: &[u8], at: usize) -> Option<String> {
    let s = b.get(at..)?;
    let units: Vec<u16> =
        s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    Some(String::from_utf16_lossy(&units))
}

/// A .lnk parancssori argumentumai (a LinkInfo után következő szövegblokkokból).
fn lnk_arguments(b: &[u8], mut pos: usize, flags: u32) -> Option<String> {
    let unicode = flags & 0x80 != 0;
    // Sorrend: név, relatív útvonal, munkakönyvtár, argumentumok.
    for bit in [0x04u32, 0x08, 0x10, 0x20] {
        if flags & bit == 0 {
            continue;
        }
        let n = le_u16(b, pos)? as usize;
        pos += 2;
        let bytes = if unicode { n * 2 } else { n };
        let raw = b.get(pos..pos.checked_add(bytes)?)?;
        if bit == 0x20 {
            return Some(if unicode {
                let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&units)
            } else {
                String::from_utf8_lossy(raw).into_owned()
            });
        }
        pos += bytes;
    }
    None
}

/// Egy .lnk parancsikon célja és parancssori argumentumai, a Shell Link bináris formátum alapján (COM nélkül).
/// Csak a helyi fájlra mutató parancsikonokat oldja fel (LinkInfo); egyébként (pl. csomagolt alkalmazás, hálózati
/// cél) `None`.
fn read_lnk(path: &std::path::Path) -> Option<(String, String)> {
    let b = std::fs::read(path).ok()?;
    if b.len() < 0x4C || le_u32(&b, 0)? != 0x4C {
        return None;
    }
    let flags = le_u32(&b, 0x14)?;
    let mut pos = 0x4Cusize;
    if flags & 0x01 != 0 {
        // HasLinkTargetIDList: 2 bájtos méret + a lista.
        pos += 2 + le_u16(&b, pos)? as usize;
    }
    if flags & 0x02 == 0 {
        return None; // nincs LinkInfo: nincs helyi cél
    }
    let li = pos;
    let size = le_u32(&b, li)? as usize;
    let header = le_u32(&b, li + 4)? as usize;
    let li_flags = le_u32(&b, li + 8)?;
    if li_flags & 1 == 0 {
        return None; // nem helyi kötetre mutat
    }
    // ANSI: LocalBasePath (+16), CommonPathSuffix (+24); az Unicode változat (+28, +32) a 0x24-es fejlécben van.
    let ansi_base = ansi_cstr(&b, li + le_u32(&b, li + 16)? as usize);
    let ansi_suffix = le_u32(&b, li + 24).and_then(|o| ansi_cstr(&b, li + o as usize));
    let (uni_base, uni_suffix) = if header >= 0x24 {
        (
            le_u32(&b, li + 28).filter(|&o| o != 0).and_then(|o| utf16_cstr(&b, li + o as usize)),
            le_u32(&b, li + 32).filter(|&o| o != 0).and_then(|o| utf16_cstr(&b, li + o as usize)),
        )
    } else {
        (None, None)
    };
    let base = uni_base.filter(|s| !s.is_empty()).or(ansi_base)?;
    let suffix = uni_suffix.or(ansi_suffix).unwrap_or_default();
    let target = if suffix.is_empty() {
        base
    } else if base.ends_with('\\') {
        format!("{base}{suffix}")
    } else {
        format!("{base}\\{suffix}")
    };
    if target.is_empty() {
        return None;
    }
    let args = lnk_arguments(&b, li + size, flags).unwrap_or_default();
    Some((target, args.trim().to_string()))
}

#[link(name = "shlwapi")]
extern "system" {
    fn SHLoadIndirectString(source: *const u16, out: *mut u16, out_len: u32, reserved: *mut std::ffi::c_void) -> i32;
}

/// Erőforrás-hivatkozás (`@{csomag?ms-resource://...}`) feloldása olvasható szöveggé.
fn load_indirect_string(src: &str) -> Option<String> {
    let s = winreg::wide(src);
    let mut buf = vec![0u16; 512];
    // SAFETY: érvényes, NUL-lezárt bemenet és egy 512 elemű kimeneti puffer a megadott kapacitással.
    let hr = unsafe { SHLoadIndirectString(s.as_ptr(), buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut()) };
    if hr < 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let t = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// A csomagtár (Repository) kulcsa a megadott gyökér alatt.
fn repository_base(root: isize) -> String {
    if root == HKEY_LOCAL_MACHINE {
        "SOFTWARE\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages"
            .to_string()
    } else {
        format!("{APPMODEL_BASE}\\Repository\\Packages")
    }
}

/// A csomagtár összes csomagja: (gyökér, a csomag teljes neve).
fn repository_packages() -> Vec<(isize, String)> {
    let mut out = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let sam = KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_WOW64_64KEY;
        if let Ok(k) = RegKey::open(root, &repository_base(root), sam) {
            out.extend(k.subkeys().into_iter().map(|n| (root, n)));
        }
    }
    out
}

/// A tanúsítvány-tulajdonosból ("CN=Spotify AB, O=...") a CN rész.
fn common_name(subject: &str) -> Option<String> {
    let i = subject.find("CN=")?;
    let rest = &subject[i + 3..];
    let cn = if let Some(q) = rest.strip_prefix('"') { q.split('"').next() } else { rest.split(',').next() };
    let cn = cn.unwrap_or("").trim();
    if cn.is_empty() {
        None
    } else {
        Some(cn.to_string())
    }
}

/// Egy csomagcsalád (pl. `SpotifyAB.SpotifyMusic_zpdnekdrzrea0`) megjelenített neve és gyártója a csomagtárból.
/// Ha a név nem oldható fel, a csomag azonosító-neve marad.
fn packaged_info(family: &str, repo: &[(isize, String)]) -> (String, String) {
    let (ident, pubid) = family.rsplit_once('_').unwrap_or((family, ""));
    let prefix = format!("{}_", ident.to_lowercase());
    let suffix = format!("__{}", pubid.to_lowercase());
    // A legnagyobb verziójú (betűrend szerinti utolsó) egyező csomag.
    let mut best: Option<&(isize, String)> = None;
    for item in repo {
        let lc = item.1.to_lowercase();
        if lc.starts_with(&prefix) && lc.ends_with(&suffix) && best.map_or(true, |b| item.1 > b.1) {
            best = Some(item);
        }
    }
    let mut name = ident.to_string();
    let mut publisher = String::new();
    if let Some((root, full)) = best {
        let sub = format!("{}\\{full}", repository_base(*root));
        if let Ok(k) = RegKey::open(*root, &sub, KEY_QUERY_VALUE | KEY_WOW64_64KEY) {
            if let Some(RegValue::Str(d)) = k.value("DisplayName") {
                let d = d.trim().to_string();
                if d.starts_with('@') {
                    if let Some(t) = load_indirect_string(&d) {
                        name = t;
                    }
                } else if d.to_lowercase().starts_with("ms-resource:") {
                    if let Some(t) = load_indirect_string(&format!("@{{{full}?{d}}}")) {
                        name = t;
                    }
                } else if !d.is_empty() {
                    name = d;
                }
            }
            if let Some(RegValue::Str(p)) = k.value("Publisher") {
                if let Some(cn) = common_name(&p) {
                    publisher = cn;
                }
            }
        }
    }
    (name, publisher)
}

/// A csomagolt (Microsoft Store / MSIX) alkalmazások indítási feladatai: a Windows a felhasználó kulcsaiban tárolja
/// az állapotukat (`State`: 0 / 1 = letiltva, 2 = engedélyezve, 3 / 4 = házirend által letiltva / engedélyezve).
fn collect_packaged(out: &mut Vec<StartupEntry>) {
    let base = format!("{APPMODEL_BASE}\\SystemAppData");
    let sam = KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_WOW64_64KEY;
    let Ok(root) = RegKey::open(HKEY_CURRENT_USER, &base, sam) else { return };
    let mut repo: Option<Vec<(isize, String)>> = None; // lustán töltjük be: csak ha van csomagolt bejegyzés
    for family in root.subkeys() {
        let Ok(fk) = RegKey::open(HKEY_CURRENT_USER, &format!("{base}\\{family}"), sam) else { continue };
        for task in fk.subkeys() {
            let sub = format!("{base}\\{family}\\{task}");
            let Ok(tk) = RegKey::open(HKEY_CURRENT_USER, &sub, KEY_QUERY_VALUE | KEY_WOW64_64KEY) else { continue };
            let Some(RegValue::Dword(state)) = tk.value("State") else { continue };
            if state > 4 {
                continue;
            }
            let enabled = matches!(state, 2 | 4);
            let disabled_at = match tk.value("LastDisabledTime") {
                Some(RegValue::Qword(t)) if !enabled && t > 0 => Some(t),
                _ => None,
            };
            let repo = &*repo.get_or_insert_with(repository_packages);
            let (name, publisher) = packaged_info(&family, repo);
            out.push(StartupEntry {
                source: StartupSource::Packaged,
                value_name: format!("{family}\\{task}"),
                name,
                command: family.clone(),
                exe: None,
                exe_name_lc: String::new(),
                publisher,
                enabled,
                disabled_at,
                item_path: None,
            });
        }
    }
}

/// Csomagolt alkalmazás indítási feladatának be/ki kapcsolása (ugyanazt a `State` értéket írja, mint a Windows).
fn set_packaged_enabled(value_name: &str, enabled: bool) -> Result<(), String> {
    let sub = format!("{APPMODEL_BASE}\\SystemAppData\\{value_name}");
    let key = RegKey::open(HKEY_CURRENT_USER, &sub, KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY)
        .map_err(describe_error)?;
    if let Some(RegValue::Dword(state)) = key.value("State") {
        if state == 3 || state == 4 {
            return Err(tr("This item is controlled by a policy.").to_string());
        }
    }
    if enabled {
        key.set_dword("State", 2).map_err(describe_error)?;
        let _ = key.set_dword("UserEnabledStartupOnce", 1);
    } else {
        key.set_dword("State", 1).map_err(describe_error)?;
        let _ = key.set_qword("LastDisabledTime", winreg::filetime_now());
    }
    Ok(())
}

/// Az összes indítási bejegyzés (név szerint rendezve). A háttérszálról hívandó.
pub fn list(publisher_cache: &mut HashMap<String, String>) -> Vec<StartupEntry> {
    let mut out = Vec::new();
    collect_run(StartupSource::HkcuRun, publisher_cache, &mut out);
    collect_run(StartupSource::HklmRun, publisher_cache, &mut out);
    collect_run(StartupSource::HklmRun32, publisher_cache, &mut out);
    collect_folder(StartupSource::UserFolder, publisher_cache, &mut out);
    collect_folder(StartupSource::CommonFolder, publisher_cache, &mut out);
    collect_packaged(&mut out);
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn describe_error(code: u32) -> String {
    if code == ERROR_ACCESS_DENIED {
        tr("Access denied – administrator rights required.").to_string()
    } else {
        format!("{} ({code})", tr("Registry error"))
    }
}

/// Egy bejegyzés engedélyezése / letiltása (StartupApproved).
pub fn set_enabled(source: StartupSource, value_name: &str, enabled: bool) -> Result<(), String> {
    if source == StartupSource::Packaged {
        return set_packaged_enabled(value_name, enabled);
    }
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
