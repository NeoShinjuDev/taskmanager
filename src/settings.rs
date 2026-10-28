/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Beállítások: nyelv, téma, oldalsáv-effekt, jogosultság, grafikon-időablak,
//! mintavételezési idő. JSON-fájlban tároljuk:
//! %APPDATA%\Feladatkezelo\settings.json. Hiba esetén (nincs fájl, sérült) az alapértékek élnek.

use crate::i18n::LangPref;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// Grafikon-időablak határok (másodperc).
pub const CHART_WINDOW_MIN: f32 = 30.0;
pub const CHART_WINDOW_MAX: f32 = 120.0;
pub const CHART_WINDOW_DEFAULT: f32 = 60.0;

/// Mintavételezési idő határok (másodperc).
pub const SAMPLE_INTERVAL_MIN: f32 = 0.25;
pub const SAMPLE_INTERVAL_MAX: f32 = 5.0;
pub const SAMPLE_INTERVAL_DEFAULT: f32 = 1.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemePref {
    /// A Windows témáját követi (sötét / világos).
    System,
    Dark,
    Light,
}

// JAVÍTÁS: A Settings struct-ra kötelező rárakni a #[derive(Clone)], 
// mert a save() függvényben a `self.clone()`-t használja a kód!
#[derive(Clone, Debug)]
pub struct Settings {
    pub lang: LangPref,
    pub theme: ThemePref,
    /// A bal oldali sáv áttetsző (Mica), vagy tömör színű.
    pub sidebar_mica: bool,
    /// Induláskor rendszergazdai jogot kér (UAC), hogy minden folyamat adata olvasható legyen.
    pub elevate: bool,
    /// A grafikonok időintervalluma másodpercben (30–120).
    pub chart_window_secs: f32,
    /// A mintavételezés gyakorisága másodpercben (0.25–5.0).
    pub sample_interval_secs: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: LangPref::Auto,
            theme: ThemePref::Dark,
            sidebar_mica: true,
            elevate: true,
            chart_window_secs: CHART_WINDOW_DEFAULT,
            sample_interval_secs: SAMPLE_INTERVAL_DEFAULT,
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

    /// Időablak Duration-ként a puffer-tartás és a rajzolás számára.
    pub fn chart_window(&self) -> Duration {
        Duration::from_secs_f32(self.chart_window_secs)
    }

    /// Mintavételezési idő Duration-ként a háttérszál számára.
    pub fn sample_interval(&self) -> Duration {
        Duration::from_secs_f32(self.sample_interval_secs)
    }

    fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA")
            .map(|a| PathBuf::from(a).join("Feladatkezelo").join("settings.json"))
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        // JAVÍTÁS: Nem simán `path()`, hanem `Self::path()`, mivel az impl blokkon belül van a metódus!
        let Some(p) = Self::path() else { return s };
        let Ok(text) = std::fs::read_to_string(p) else { return s };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return s };
        s.lang = match v.get("language").and_then(Value::as_str) {
            Some("hu") => LangPref::Hu,
            Some("en") => LangPref::En,
            _ => LangPref::Auto,
        };
        s.theme = match v.get("theme").and_then(Value::as_str) {
            Some("system") => ThemePref::System,
            Some("light") => ThemePref::Light,
            _ => ThemePref::Dark,
        };
        if let Some(b) = v.get("sidebar_mica").and_then(Value::as_bool) {
            s.sidebar_mica = b;
        }
        if let Some(b) = v.get("elevate").and_then(Value::as_bool) {
            s.elevate = b;
        }
        // Új mezők: ha hiányoznak vagy érvénytelenek, default marad, majd clamp.
        if let Some(x) = v.get("chart_window_secs").and_then(Value::as_f64) {
            s.chart_window_secs = x as f32;
        }
        if let Some(x) = v.get("sample_interval_secs").and_then(Value::as_f64) {
            s.sample_interval_secs = x as f32;
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
        let v = json!({
            "language": match c.lang { LangPref::Auto => "auto", LangPref::Hu => "hu", LangPref::En => "en" },
            "theme": match c.theme { ThemePref::System => "system", ThemePref::Dark => "dark", ThemePref::Light => "light" },
            "sidebar_mica": c.sidebar_mica,
            "elevate": c.elevate,
            "chart_window_secs": c.chart_window_secs,
            "sample_interval_secs": c.sample_interval_secs,
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
