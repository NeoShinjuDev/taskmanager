/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Build-szkript: a fordítás dátumát (UTC) és a rustc verzióját környezeti változóként adja át a
//! programnak (`env!("BUILD_DATE")`, `env!("RUSTC_VERSION")`), hogy az Információ oldal kiírhassa.
//! Szándékosan nincs `rerun-if-changed`: így minden csomag-változásnál (azaz minden újrafordításnál)
//! újra lefut, és a dátum mindig az aktuális fordításé.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Napok száma 1970-01-01 óta -> (év, hónap, nap). Howard Hinnant algoritmusa.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn main() {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let (hh, mm) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    println!("cargo:rustc-env=BUILD_DATE={y:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "rustc (ismeretlen)".to_string());
    println!("cargo:rustc-env=RUSTC_VERSION={version}");
}
