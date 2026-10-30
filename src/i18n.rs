/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Többnyelvűség: dinamikusan betöltött nyelvfájlokkal.
//!
//! Működés: a forráskódban minden felirat ANGOL szöveg (hogy a fordítók könnyen megértsék), `tr("English text")`
//! hívásba csomagolva. A `tr` az aktuális nyelvfájl szótárában keresi ki a fordítást (a kulcs az angol szöveg); ha nincs
//! találat, az angol szöveg marad. A változó értékeket tartalmazó mondatokhoz a `trf("… {0} … {1}", &[&a, &b])` való:
//! a sablon fordítható, az értékeket a program írja be a `{0}`, `{1}` … jelölők helyére.
//! Új felirat felvétele: írd be a kódba `tr("...")`-ként, és vedd fel mindkét beágyazott nyelvfájlba (`en.json`: azonos
//! szöveg, `hu.json`: magyar fordítás).
//!
//! Nyelvfájlok (JSON: `code`, `name`, `strings`):
//!  * a program két nyelvet tartalmaz beágyazva (`assets/lang/hu.json`, `assets/lang/en.json`);
//!  * minden további nyelv a `%APPDATA%\TaskManager\Langs` mappából töltődik be, újrafordítás nélkül.
//! A Beállítások › Nyelv listájában a beágyazott nyelvek után a mappában talált nyelvek jelennek meg.
//! A betöltött szótár a program élettartamára "kiszivárogtatott" (`'static`) szöveg, hogy a `tr` továbbra is
//! `&'static str`-t adhasson vissza; a betöltések gyorsítótárazottak (fájlonként csak módosításkor töltődik újra).
//! A jelenlegi nyelv globális, szálbiztos, ezért a háttérszálak is használhatják.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::SystemTime;

use serde_json::Value;

const BUILTIN_HU: &str = include_str!("../assets/lang/hu.json");
const BUILTIN_EN: &str = include_str!("../assets/lang/en.json");

/// Szótár: magyar kulcs -> fordítás.
type Dict = HashMap<String, &'static str>;

/// Egy elérhető nyelv a listában.
#[derive(Clone, Debug)]
pub struct LangInfo {
    /// Nyelvkód (pl. "hu", "en", "de", "pt-BR").
    pub code: String,
    /// A nyelv saját neve, ahogy a listában megjelenik (pl. "Deutsch").
    pub name: String,
    /// A nyelvfájl helye; a beágyazott nyelveknél nincs.
    pub path: Option<PathBuf>,
}

static CURRENT: RwLock<Option<&'static Dict>> = RwLock::new(None);
static CURRENT_CODE: RwLock<String> = RwLock::new(String::new());
static AVAILABLE: Mutex<Vec<LangInfo>> = Mutex::new(Vec::new());
static CACHE: OnceLock<Mutex<HashMap<String, (Option<SystemTime>, &'static Dict)>>> = OnceLock::new();

#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
    fn GetUserPreferredUILanguages(flags: u32, num: *mut u32, buf: *mut u16, len: *mut u32) -> i32;
}
const MUI_LANGUAGE_NAME: u32 = 0x8;

/// A Windows kijelzőnyelvei elsőbbségi sorrendben (pl. ["hu-HU", "en-US"]).
fn system_languages() -> Vec<String> {
    // SAFETY: a puffer mérete a rendszer által visszaadott `len`; a hívások mellékhatás-mentesek.
    unsafe {
        let (mut num, mut len) = (0u32, 0u32);
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, std::ptr::null_mut(), &mut len) != 0 && len > 0 {
            let mut buf = vec![0u16; len as usize];
            if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut num, buf.as_mut_ptr(), &mut len) != 0 {
                let out: Vec<String> =
                    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect();
                if !out.is_empty() {
                    return out;
                }
            }
        }
        // Tartalék: a LANGID alsó 10 bitje az elsődleges nyelv, 0x0E = magyar.
        if (GetUserDefaultUILanguage() & 0x3FF) == 0x0E {
            return vec!["hu".to_string()];
        }
    }
    Vec::new()
}

/// A nyelvfájlok mappája: `%APPDATA%\TaskManager\Langs`.
pub fn langs_dir() -> Option<PathBuf> {
    crate::settings::app_data_dir().map(|d| d.join("Langs"))
}

/// Megnyitja a nyelvfájlok mappáját a Fájlkezelőben (létrehozza, ha még nincs).
pub fn open_langs_dir() {
    if let Some(dir) = langs_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
}

fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Egy beolvasott nyelvfájl (még nem "kiszivárogtatott" szöveggel).
struct RawLang {
    info: LangInfo,
    strings: HashMap<String, String>,
}

fn parse_lang(text: &str) -> Option<RawLang> {
    let v: Value = serde_json::from_str(text).ok()?;
    let code = v.get("code")?.as_str()?.trim().to_string();
    if !valid_code(&code) {
        return None;
    }
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| code.clone());
    let mut strings = HashMap::new();
    for (k, val) in v.get("strings")?.as_object()? {
        if let Some(s) = val.as_str() {
            if !s.is_empty() {
                strings.insert(k.clone(), s.to_string());
            }
        }
    }
    Some(RawLang { info: LangInfo { code, name, path: None }, strings })
}

/// Újraolvassa az elérhető nyelvek listáját: a két beágyazott nyelv + a `Langs` mappa érvényes JSON-fájljai.
pub fn rescan() {
    let mut list = vec![
        LangInfo { code: "hu".to_string(), name: "Magyar".to_string(), path: None },
        LangInfo { code: "en".to_string(), name: "English".to_string(), path: None },
    ];
    if let Some(dir) = langs_dir() {
        let _ = std::fs::create_dir_all(&dir); // hogy a felhasználó megtalálja, hová tegye a fájlokat
        // Kész sablon az új nyelvekhez (az angol nyelvfájl másolata). Az aláhúzással kezdődő fájlokat a lista kihagyja.
        let template = dir.join("_template.json");
        if std::fs::read_to_string(&template).map(|c| c != BUILTIN_EN).unwrap_or(true) {
            let _ = std::fs::write(&template, BUILTIN_EN);
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json")))
                .filter(|p| !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('_')))
                .collect();
            files.sort();
            for p in files {
                let Some(raw) = std::fs::read_to_string(&p).ok().and_then(|t| parse_lang(&t)) else { continue };
                // A beágyazott nyelvek (és a már felvett kódok) nem írhatók felül.
                if list.iter().any(|l| l.code.eq_ignore_ascii_case(&raw.info.code)) {
                    continue;
                }
                let mut info = raw.info;
                info.path = Some(p);
                list.push(info);
            }
        }
    }
    if let Ok(mut a) = AVAILABLE.lock() {
        *a = list;
    }
}

/// Az elérhető nyelvek (az első híváskor beolvassa őket).
pub fn available() -> Vec<LangInfo> {
    let empty = AVAILABLE.lock().map(|a| a.is_empty()).unwrap_or(true);
    if empty {
        rescan();
    }
    AVAILABLE.lock().map(|a| a.clone()).unwrap_or_default()
}

/// A nyelv szótára (gyorsítótárazva; a mappában lévő fájl csak módosításkor töltődik újra).
fn dict_for(info: &LangInfo) -> Option<&'static Dict> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mtime = info.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok());
    if let Ok(c) = cache.lock() {
        if let Some((t, d)) = c.get(&info.code) {
            if *t == mtime {
                return Some(*d);
            }
        }
    }
    let text = match &info.path {
        Some(p) => std::fs::read_to_string(p).ok()?,
        None => match info.code.as_str() {
            "hu" => BUILTIN_HU.to_string(),
            "en" => BUILTIN_EN.to_string(),
            _ => return None,
        },
    };
    let raw = parse_lang(&text)?;
    let dict: Dict = raw.strings.into_iter().map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect();
    let dict: &'static Dict = Box::leak(Box::new(dict));
    if let Ok(mut c) = cache.lock() {
        c.insert(info.code.clone(), (mtime, dict));
    }
    Some(dict)
}

/// A kiválasztott nyelv érvényesítése. `pref`: nyelvkód, vagy "auto" (a Windows kijelzőnyelve szerint; ha nincs hozzá
/// nyelvfájl, angol).
pub fn apply(pref: &str) {
    let list = available();
    let find = |code: &str| list.iter().find(|l| l.code.eq_ignore_ascii_case(code)).cloned();
    let chosen = if pref.is_empty() || pref.eq_ignore_ascii_case("auto") {
        system_languages()
            .iter()
            .find_map(|s| find(s).or_else(|| find(s.split('-').next().unwrap_or(""))))
            .or_else(|| find("en"))
    } else {
        find(pref).or_else(|| find("en"))
    };
    let Some(chosen) = chosen else { return };
    let dict = dict_for(&chosen).or_else(|| find("en").and_then(|e| dict_for(&e)));
    if let Ok(mut w) = CURRENT.write() {
        *w = dict;
    }
    if let Ok(mut w) = CURRENT_CODE.write() {
        *w = chosen.code;
    }
}

/// A jelenleg használt nyelv kódja (pl. "hu").
pub fn code() -> String {
    CURRENT_CODE.read().map(|c| c.clone()).unwrap_or_default()
}

/// Magyar felirat -> az aktuális nyelven. A bemenet statikus szöveg, így a kimenet is az. Ha a nyelvfájlban nincs
/// fordítás, a magyar szöveg marad.
pub fn tr(hu: &'static str) -> &'static str {
    let cur: Option<&'static Dict> = match CURRENT.read() {
        Ok(g) => *g,
        Err(_) => None,
    };
    cur.and_then(|d| d.get(hu).copied()).unwrap_or(hu)
}

/// Fordítható sablon változó értékekkel: a `{0}`, `{1}` … jelölők helyére a megadott értékek kerülnek.
/// Példa: `trf("{0} folyamat · {1} alkalmazás", &[&a, &b])`.
pub fn trf(hu: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let t = tr(hu);
    let mut out = String::with_capacity(t.len() + 16);
    let mut rest = t;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('}') {
            if let Ok(i) = after[..end].parse::<usize>() {
                if let Some(a) = args.get(i) {
                    out.push_str(&a.to_string());
                    rest = &after[end + 1..];
                    continue;
                }
            }
        }
        out.push('{');
        rest = after;
    }
    out.push_str(rest);
    out
}
