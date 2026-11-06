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
//! Beállítások: nyelv, téma, oldalsáv-effekt, jogosultság, grafikon-időablak,
//! mintavételezési idő. JSON-fájlban tároljuk:
//! %APPDATA%\Neo Softwares\Task Manager\settings.json. Hiba esetén (nincs fájl, sérült) az alapértékek élnek.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Grafikon-időablak határok (másodperc).
pub const CHART_WINDOW_MIN: f32 = 30.0;
pub const CHART_WINDOW_MAX: f32 = 120.0;
pub const CHART_WINDOW_DEFAULT: f32 = 60.0;

/// Mintavételezési idő határok (másodperc).
pub const SAMPLE_INTERVAL_MIN: f32 = 0.25;
pub const SAMPLE_INTERVAL_MAX: f32 = 5.0;
pub const SAMPLE_INTERVAL_DEFAULT: f32 = 1.0;

/// A képkockasebesség-korlát választható értékei (képkocka / másodperc); a 0 = nincs saját korlát, csak a V-Sync
/// (a monitor frissítési gyakorisága) szab határt.
pub const FPS_LIMITS: [u32; 4] = [0, 30, 60, 120];
pub const FPS_LIMIT_DEFAULT: u32 = 60;

/// A Felhasználók tábla alapértelmezett oszlopai bitmaszkként (a bitek sorrendje a `UCol` felsorolásé):
/// Állapot, CPU, Memória, Lemez, Hálózat, GPU.
pub const USERS_COLUMNS_DEFAULT: u32 = 0b01_1111_1000;
/// A tábla összes oszlopát lefedő maszk (10 oszlop).
pub const USERS_COLUMNS_ALL: u32 = 0b11_1111_1111;
/// A Folyamatok tábla alapértelmezett oszlopai (a bitek sorrendje a `PCol` felsorolásé): PID, CPU, Memória, GPU.
pub const PROC_COLUMNS_DEFAULT: u32 = (1 << 3) | (1 << 6) | (1 << 7) | (1 << 10);

/// Egy tábla oszlop-beállításai: a látható oszlopok (bitmaszk; `None` = az alapértelmezett) és a felhasználó által
/// állított szélességek (0 = alapértelmezett szélesség). Fájlba a `changed` kivételével minden mező kerül.
#[derive(Clone, Debug, Default)]
pub struct ColPrefs {
    pub vis: Option<u32>,
    pub widths: Vec<f32>,
    /// Erőforrásértékek százalékként (bitek: 1 memória, 2 lemez, 4 hálózat); 0 = mind abszolút érték.
    pub pct: u8,
    /// Módosult, és még nincs elmentve (a program az egérgomb felengedése után menti).
    pub changed: bool,
}

impl ColPrefs {
    /// Az `i`-edik oszlop szélessége: a felhasználó által állított, vagy az alapértelmezett.
    pub fn width(&self, i: usize, default: f32) -> f32 {
        match self.widths.get(i) {
            Some(w) if *w >= 30.0 => *w,
            _ => default,
        }
    }

    pub fn set_width(&mut self, i: usize, w: f32) {
        if self.widths.len() <= i {
            self.widths.resize(i + 1, 0.0);
        }
        self.widths[i] = w;
        self.changed = true;
    }

    /// Van-e eltérés az alapértelmezettől (látható oszlopok vagy szélességek).
    pub fn is_customized(&self) -> bool {
        self.vis.is_some() || self.pct != 0 || self.widths.iter().any(|w| *w > 0.0)
    }

    /// Vissza az alapértelmezett oszlopokra és szélességekre.
    pub fn reset(&mut self) {
        self.vis = None;
        self.widths.clear();
        self.pct = 0;
        self.changed = true;
    }
}

/// Az ablak háttér-effektusa (a bal sáv és a címsor anyaga).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backdrop {
    /// A rendszerhez igazodik: Windows 11-en Mica, Windows 10-en Acrylic, régebbin Tömör.
    Auto,
    /// Mica (Windows 11).
    Mica,
    /// Acrylic (Windows 10 / 11).
    Acrylic,
    /// Átlátszóság nélkül, egyszínű bal sáv és címsor.
    Solid,
}

impl Backdrop {
    /// Minden ismert effektus (a mentett kulcs értelmezéséhez); a választható lista: `available()`.
    const ALL: [Backdrop; 4] = [Backdrop::Auto, Backdrop::Mica, Backdrop::Acrylic, Backdrop::Solid];

    /// Az ezen a Windowson ténylegesen választható effektusok: Windows 11-en az Automatikus, a Mica, az Acrylic és a
    /// Tömör; Windows 10-en (1803-tól) a Mica nem létezik, ezért nincs a listán; régebbi rendszeren csak az
    /// Automatikus (ami Tömörre oldódik fel) és a Tömör.
    pub fn available() -> Vec<Backdrop> {
        let mut v = vec![Backdrop::Auto];
        if crate::backdrop::mica_supported() {
            v.push(Backdrop::Mica);
        }
        if crate::backdrop::acrylic_supported() {
            v.push(Backdrop::Acrylic);
        }
        v.push(Backdrop::Solid);
        v
    }

    pub fn key(self) -> &'static str {
        match self {
            Backdrop::Auto => "auto",
            Backdrop::Mica => "mica",
            Backdrop::Acrylic => "acrylic",
            Backdrop::Solid => "solid",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.key() == s)
    }

    /// Átlátszó-e a bal sáv (minden effektus, kivéve a tömör).
    pub fn is_transparent(self) -> bool {
        self != Backdrop::Solid
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemePref {
    /// A Windows témáját követi (sötét / világos).
    System,
    Dark,
    Light,
}

// JAVÍTÁS: A Settings struct-ra kötelező rárakni a #[derive(Clone)], 
// mert a save() függvényben a `self.clone()`-t használja a kód!
/// A program adatmappája: `%APPDATA%\Neo Softwares\Task Manager` (beállítások, nyelvfájlok). A korábbi helyekről –
/// `%APPDATA%\TaskManager`, illetve a még régebbi `%APPDATA%\Feladatkezelo` – az első indításkor átköltözteti a
/// tartalmat, így a beállítások nem vesznek el.
pub fn app_data_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = PathBuf::from(std::env::var_os("APPDATA")?);
        let dir = base.join("Neo Softwares").join("Task Manager");
        if !dir.exists() {
            for old in [base.join("TaskManager"), base.join("Feladatkezelo")] {
                if !old.exists() {
                    continue;
                }
                if let Some(parent) = dir.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&old, &dir).is_ok() {
                    break;
                }
            }
        }
        // A megszűnt diagnosztikai napló maradványa.
        let _ = std::fs::remove_file(dir.join("backdrop.log"));
        Some(dir)
    })
    .clone()
}

/// A Teljesítmény fül nézet-beállításai; a grafikonok jobb gombos menüjéből állíthatók, és mentődnek.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfPrefs {
    /// A processzor grafikonja NUMA-csomópontonként (csomópontonként egy grafikon) az összterhelés helyett.
    pub cpu_numa: bool,
    /// A lemez olvasási és írási sebessége egy közös grafikonon (különben külön-külön).
    pub disk_combined: bool,
    /// A videókártya motoronkénti nézete (3D, Copy, Video Encode, Video Decode) az összefoglaló helyett.
    pub gpu_engines: bool,
    /// A videókártya megosztott memóriájának grafikonja is látszik (a dedikált mellett).
    pub gpu_shared: bool,
    /// A hálózati adapter fel- és letöltése külön grafikonon (egymás alatt) egy közös helyett.
    pub net_separate: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// A felület nyelve: nyelvkód ("hu", "en", vagy egy `Langs` mappabeli nyelvfájl kódja), vagy "auto".
    pub lang: String,
    pub theme: ThemePref,
    /// Az ablak háttér-effektusa (Mica / Acrylic / Tömör).
    pub backdrop: Backdrop,
    /// Induláskor rendszergazdai jogot kér (UAC), hogy minden folyamat adata olvasható legyen.
    pub elevate: bool,
    /// Az ablak mindig a többi ablak felett marad (induláskor is). Alapértelmezetten kikapcsolva.
    pub always_on_top: bool,
    /// Tálcaikonok az értesítési területen: 0 = nincs, 1 = CPU, 2 = CPU + memória + GPU (alapértelmezetten 1).
    pub tray_mode: u8,
    /// A táblák oszlop-beállításai tábla-azonosító szerint ("proc", "users", "ah", "su", "sv").
    pub tables: BTreeMap<String, ColPrefs>,
    /// A grafikonok időintervalluma másodpercben (30–120).
    pub chart_window_secs: f32,
    /// A mintavételezés gyakorisága másodpercben (0.25–5.0).
    pub sample_interval_secs: f32,
    /// A Teljesítmény fül nézetei (grafikon-típusok).
    pub perf: PerfPrefs,
    /// Legfeljebb ennyiszer rajzolja újra az ablakot másodpercenként (0 = csak a V-Sync korlátoz; lásd `FPS_LIMITS`).
    pub fps_limit: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: "auto".to_string(),
            theme: ThemePref::Dark,
            backdrop: Backdrop::Auto,
            elevate: true,
            always_on_top: false,
            tray_mode: 1,
            tables: BTreeMap::new(),
            chart_window_secs: CHART_WINDOW_DEFAULT,
            sample_interval_secs: SAMPLE_INTERVAL_DEFAULT,
            perf: PerfPrefs::default(),
            fps_limit: FPS_LIMIT_DEFAULT,
        }
    }
}

impl Settings {
    /// Határokon kívüli / ismeretlen értékek javítása.
    pub fn sanitize(&mut self) {
        // NaN / végtelen esetén az alapérték (a clamp a NaN-t változatlanul hagyná).
        if !self.chart_window_secs.is_finite() {
            self.chart_window_secs = CHART_WINDOW_DEFAULT;
        }
        if !self.sample_interval_secs.is_finite() {
            self.sample_interval_secs = SAMPLE_INTERVAL_DEFAULT;
        }
        self.chart_window_secs =
            self.chart_window_secs.clamp(CHART_WINDOW_MIN, CHART_WINDOW_MAX);
        self.sample_interval_secs =
            self.sample_interval_secs.clamp(SAMPLE_INTERVAL_MIN, SAMPLE_INTERVAL_MAX);
        if !FPS_LIMITS.contains(&self.fps_limit) {
            self.fps_limit = FPS_LIMIT_DEFAULT;
        }
    }

    /// Hány mintát kell tárolni a grafikonhoz: időablak / mintavételezési idő (+1 a bal szélső pontnak).
    pub fn history_capacity(&self) -> usize {
        let n = (self.chart_window_secs / self.sample_interval_secs).round();
        if n.is_finite() {
            (n as usize + 1).clamp(2, 4801)
        } else {
            61
        }
    }

    fn path() -> Option<PathBuf> {
        app_data_dir().map(|d| d.join("settings.json"))
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        // JAVÍTÁS: Nem simán `path()`, hanem `Self::path()`, mivel az impl blokkon belül van a metódus!
        let Some(p) = Self::path() else { return s };
        let Ok(text) = std::fs::read_to_string(p) else { return s };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return s };
        s.lang = v
            .get("language")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty() && c.len() <= 16 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
            .unwrap_or("auto")
            .to_string();
        s.theme = match v.get("theme").and_then(Value::as_str) {
            Some("system") => ThemePref::System,
            Some("light") => ThemePref::Light,
            _ => ThemePref::Dark,
        };
        // Régi beállításfájl: a "sidebar_mica": false a Tömör módnak felel meg.
        if let Some(false) = v.get("sidebar_mica").and_then(Value::as_bool) {
            s.backdrop = Backdrop::Solid;
        }
        if let Some(b) = v.get("backdrop").and_then(Value::as_str).and_then(Backdrop::from_key) {
            s.backdrop = b;
        }
        // Másik gépről átvett beállítás: ha az effektus ezen a Windowson nem létezik (pl. Mica Windows 10-en), Automatikus.
        if !Backdrop::available().contains(&s.backdrop) {
            s.backdrop = Backdrop::Auto;
        }
        if let Some(b) = v.get("elevate").and_then(Value::as_bool) {
            s.elevate = b;
        }
        if let Some(b) = v.get("always_on_top").and_then(Value::as_bool) {
            s.always_on_top = b;
        }
        // Régebbi beállításfájl: a tálcaikon egyszerű ki / be kapcsoló volt.
        if let Some(b) = v.get("tray_icon").and_then(Value::as_bool) {
            s.tray_mode = if b { 1 } else { 0 };
        }
        if let Some(n) = v.get("tray_mode").and_then(Value::as_u64) {
            s.tray_mode = (n as u8).min(2);
        }
        if let Some(obj) = v.get("tables").and_then(Value::as_object) {
            for (k, t) in obj.iter().take(32) {
                let vis = t.get("vis").and_then(Value::as_u64).map(|n| (n as u32) & 0xFFFF);
                let widths: Vec<f32> = t
                    .get("widths")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .take(32)
                            .map(|w| w.as_f64().unwrap_or(0.0) as f32)
                            .map(|w| if w >= 30.0 { w.min(1200.0) } else { 0.0 })
                            .collect()
                    })
                    .unwrap_or_default();
                let pct = t.get("pct").and_then(Value::as_u64).map_or(0, |n| (n as u8) & 7);
                s.tables.insert(k.clone(), ColPrefs { vis, widths, pct, changed: false });
            }
        }
        // Régebbi beállításfájl: a Felhasználók oszlopai külön mezőben voltak.
        if !s.tables.contains_key("users") {
            if let Some(n) = v.get("users_columns").and_then(Value::as_u64) {
                s.tables.insert(
                    "users".to_string(),
                    ColPrefs { vis: Some((n as u32) & USERS_COLUMNS_ALL), ..Default::default() },
                );
            }
        }
        // Új mezők: ha hiányoznak vagy érvénytelenek, default marad, majd clamp.
        if let Some(x) = v.get("chart_window_secs").and_then(Value::as_f64) {
            s.chart_window_secs = x as f32;
        }
        if let Some(x) = v.get("sample_interval_secs").and_then(Value::as_f64) {
            s.sample_interval_secs = x as f32;
        }
        if let Some(n) = v.get("fps_limit").and_then(Value::as_u64) {
            s.fps_limit = n.min(u32::MAX as u64) as u32; // az érvénytelen értéket a sanitize() állítja vissza
        }
        if let Some(o) = v.get("perf").and_then(Value::as_object) {
            let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
            s.perf = PerfPrefs {
                cpu_numa: flag("cpu_numa"),
                disk_combined: flag("disk_combined"),
                gpu_engines: flag("gpu_engines"),
                gpu_shared: flag("gpu_shared"),
                net_separate: flag("net_separate"),
            };
        }
        s.sanitize();
        s
    }

    pub fn save(&self) {
        // JAVÍTÁS: Itt is `Self::path()` kell `path()` helyett.
        let Some(p) = Self::path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Mentés előtt is érvényesítünk, hogy a fájlba mindig szabályos érték kerüljön.
        let mut c = self.clone();
        c.sanitize();
        let tables: serde_json::Map<String, Value> = c
            .tables
            .iter()
            .filter(|(_, p)| p.is_customized())
            .map(|(k, p)| {
                let widths: Vec<i64> = p.widths.iter().map(|w| w.round() as i64).collect();
                (k.clone(), json!({ "vis": p.vis, "widths": widths, "pct": p.pct }))
            })
            .collect();
        let v = json!({
            "language": c.lang,
            "theme": match c.theme { ThemePref::System => "system", ThemePref::Dark => "dark", ThemePref::Light => "light" },
            "backdrop": c.backdrop.key(),
            "elevate": c.elevate,
            "always_on_top": c.always_on_top,
            "tray_mode": c.tray_mode,
            "tables": Value::Object(tables),
            "chart_window_secs": c.chart_window_secs,
            "sample_interval_secs": c.sample_interval_secs,
            "fps_limit": c.fps_limit,
            "perf": {
                "cpu_numa": c.perf.cpu_numa,
                "disk_combined": c.perf.disk_combined,
                "gpu_engines": c.perf.gpu_engines,
                "gpu_shared": c.perf.gpu_shared,
                "net_separate": c.perf.net_separate,
            },
        });
        // Atomikus mentés: előbb ideiglenes fájlba írunk, majd átnevezzük, így egy félbeszakadt írás
        // (áramszünet, összeomlás) sosem hagy csonka beállításfájlt.
        let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            if std::fs::rename(&tmp, &p).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}
