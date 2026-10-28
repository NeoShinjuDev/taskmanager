/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Az Információ fül adatai KÜLSŐ fájlból (`about.json`), így újrafordítás nélkül szerkeszthetők.
//!
//! A fájlt ebben a sorrendben keresi: a futtatható fájl mappája, az aktuális munkakönyvtár (cargo run esetén a projekt
//! gyökere), `%APPDATA%\Feladatkezelo`. Ha egyik sincs meg (vagy hibás), a fordításkor beágyazott alapértelmezés él.
//! A szövegek lehetnek sima szövegek vagy `{"hu": "...", "en": "..."}` objektumok. A sorok értékeiben helyettesítők
//! használhatók: {version} {privilege} {developer} {rustc} {gui} {build_date} {build_type} {target} {year}.
//! A Copyright-sor évét a program a futási idő évéből állítja be, így nem kell évente frissíteni.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::i18n::{en, tr};

/// A beágyazott alapértelmezés (a projekt gyökerében lévő about.json).
const DEFAULT_JSON: &str = include_str!("../about.json");

#[derive(Clone, Debug, Default)]
pub struct AboutRow {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Default)]
pub struct AboutInfo {
    pub title: String,
    pub subtitle: String,
    /// A Copyright-sorban szereplő név.
    pub holder: String,
    pub rows: Vec<AboutRow>,
}

/// Napok száma 1970-01-01 óta -> év (Howard Hinnant algoritmusa; csak az év kell).
fn year_from_days(days: i64) -> i64 {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y + 1
    } else {
        y
    }
}

/// A jelenlegi év (UTC szerint; az év-határnál legfeljebb órákat téved).
pub fn current_year() -> i64 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    year_from_days((secs / 86_400) as i64)
}

/// Egy szöveges mező: sima szöveg, vagy nyelvenkénti objektum ({"hu": .., "en": ..}).
fn loc(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(m)) => {
            let key = if en() { "en" } else { "hu" };
            m.get(key)
                .or_else(|| m.get("hu"))
                .or_else(|| m.values().next())
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        }
        _ => String::new(),
    }
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("about.json"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("about.json"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(PathBuf::from(appdata).join("Feladatkezelo").join("about.json"));
    }
    out
}

fn expand(s: &str, vars: &[(&str, String)]) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

impl AboutInfo {
    /// Betölti a külső fájlt (vagy az alapértelmezést). `elevated`: emelt jogosultsággal fut-e a program.
    pub fn load(elevated: bool) -> Self {
        let parsed = candidates()
            .into_iter()
            .find_map(|p| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()))
            .or_else(|| serde_json::from_str::<Value>(DEFAULT_JSON).ok())
            .unwrap_or(Value::Null);

        let developer = {
            let d = loc(parsed.get("developer"));
            if d.is_empty() { "Neo".to_string() } else { d }
        };
        let vars: Vec<(&str, String)> = vec![
            ("version", env!("CARGO_PKG_VERSION").to_string()),
            (
                "privilege",
                if elevated { tr("Rendszergazda").to_string() } else { tr("Normál felhasználó").to_string() },
            ),
            ("developer", developer.clone()),
            ("rustc", env!("RUSTC_VERSION").to_string()),
            (
                "gui",
                if cfg!(feature = "gpu-wgpu") {
                    "egui / eframe 0.33 (wgpu)".to_string()
                } else {
                    "egui / eframe 0.33 (OpenGL)".to_string()
                },
            ),
            ("build_date", env!("BUILD_DATE").to_string()),
            ("build_type", if cfg!(debug_assertions) { "Debug" } else { "Release" }.to_string()),
            ("target", format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH)),
            ("year", current_year().to_string()),
        ];

        let rows = parsed
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|r| AboutRow {
                        label: loc(r.get("label")),
                        value: expand(&loc(r.get("value")), &vars),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let holder = {
            let h = loc(parsed.get("copyright_holder"));
            if h.is_empty() { developer } else { h }
        };
        let title = {
            let t = loc(parsed.get("title"));
            if t.is_empty() { tr("Feladatkezelő").to_string() } else { t }
        };
        Self { title, subtitle: loc(parsed.get("subtitle")), holder, rows }
    }

    /// "Copyright © 2026 Neo. Minden jog fenntartva." – az év automatikus, a zárómondat nyelvfüggő.
    pub fn copyright(&self) -> String {
        format!("Copyright © {} {}. {}", current_year(), self.holder, tr("Minden jog fenntartva."))
    }
}
