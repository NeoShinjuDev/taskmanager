/*
 * Copyright (c) 2026 [Saját Neved / Fejlesztői Neved]. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Alkalmazáselőzmények: alkalmazásonkénti erőforrás-használat a figyelés kezdete óta.
//!
//! ADATFORRÁSOK
//! ─────────────
//! * Processzoridő: VALÓS adat. A háttérszál minden mintavételnél a folyamatok kumulált CPU-idejének (sysinfo
//!   `accumulated_cpu_time`) változását adja hozzá az alkalmazás (folyamatnév) összegéhez. A program indulása előtti
//!   múltat nem ismerjük: minden folyamat az első észlelésekor "alapvonalat" kap.
//! * Hálózat / Értesítések (MB): a folyamatonkénti hálózati forgalomhoz ETW-munkamenet, az értesítésekhez a
//!   Windows-előzményadatbázis (SRUM) kellene – mindkettő nagyobb munka. Addig az `UsageSource` trait mögé bújtatott
//!   `MockUsageSource` ad SZIMULÁLT értékeket (a CPU-aktivitással arányosan), a struktúra viszont kész a valós adatokra:
//!   elég egy új `UsageSource`-implementációt írni, és a `AppHistoryTracker::with_source`-nak átadni.
//!
//! A tracker a `hw-monitor` háttérszálon fut; az UI csak az `AppHistory` másolatot kapja a Snapshot-ban.

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::ProcInfo;

/// Egy alkalmazás (azonos nevű folyamatok együtt) összesített használata.
#[derive(Clone, Debug, Default)]
pub struct AppUsage {
    pub name: String,
    /// Kisbetűs folyamatnév (egyedi kulcs).
    pub key: String,
    /// Összesített processzoridő ezredmásodpercben.
    pub cpu_time_ms: u64,
    pub network_mb: f64,
    pub notifications_mb: f64,
    /// Volt-e valaha látható ablaka (különben háttérfolyamat).
    pub is_app: bool,
    /// A legutóbbi mintavételkor futott-e.
    pub running: bool,
    /// Egy futtatható fájl elérési útja (az ikon betöltéséhez), ha ismert.
    pub exe: Option<String>,
}

/// Az UI-nak átadott pillanatkép.
#[derive(Clone, Debug, Default)]
pub struct AppHistory {
    pub rows: Vec<AppUsage>,
    /// Mióta gyűjtjük az adatokat (másodperc).
    pub elapsed_secs: u64,
    /// A Hálózat / Értesítések oszlop szimulált-e.
    pub simulated: bool,
}

/// A hálózati és az értesítési adatok forrása. Valós megvalósítás (ETW / SRUM) ide köthető be.
pub trait UsageSource: Send {
    /// Az adott tickben az alkalmazásra eső (hálózat MB, értesítés MB) növekmény.
    fn sample(&mut self, app_key: &str, cpu_delta_ms: u64, dt_secs: f32) -> (f64, f64);
    /// Szimulált adatot ad-e (a felület ezt jelzi a felhasználónak).
    fn is_simulated(&self) -> bool;
}

/// Szimulált forrás: a CPU-aktivitással arányos, alkalmazásnévből képzett "hajlam"-mal és kis véletlen-ingadozással.
pub struct MockUsageSource {
    state: u64,
}

impl MockUsageSource {
    pub fn new() -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9);
        Self { state: seed | 1 } // a xorshift állapota nem lehet 0
    }

    /// 0.0 .. 1.0 közötti pszeudovéletlen szám (xorshift64).
    fn next_f64(&mut self) -> f64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Alkalmazásnévből képzett állandó 0..1 érték (FNV-1a), hogy ugyanaz az app mindig "hasonlóan viselkedjen".
fn affinity(key: &str) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % 1000) as f64 / 1000.0
}

impl UsageSource for MockUsageSource {
    fn sample(&mut self, app_key: &str, cpu_delta_ms: u64, _dt_secs: f32) -> (f64, f64) {
        let cpu_s = cpu_delta_ms as f64 / 1000.0;
        if cpu_s <= 0.0 {
            return (0.0, 0.0);
        }
        let aff = affinity(app_key);
        let jitter = 0.5 + self.next_f64();
        let net = cpu_s * (0.02 + 0.8 * aff * aff) * jitter;
        let notif = if aff > 0.55 { cpu_s * 0.004 * aff * jitter } else { 0.0 };
        (net, notif)
    }

    fn is_simulated(&self) -> bool {
        true
    }
}

/// Az alkalmazásonkénti használat követője.
pub struct AppHistoryTracker {
    apps: HashMap<String, AppUsage>,
    /// PID -> (kumulált CPU-idő ms, kulcs) az előző mintavételből.
    prev: HashMap<u32, (u64, String)>,
    source: Box<dyn UsageSource>,
    started: Instant,
    last: Option<Instant>,
}

impl AppHistoryTracker {
    /// Alapértelmezett (szimulált hálózat/értesítés) forrással.
    pub fn new() -> Self {
        Self::with_source(Box::new(MockUsageSource::new()))
    }

    /// Saját (pl. valós ETW-s) forrással.
    pub fn with_source(source: Box<dyn UsageSource>) -> Self {
        Self { apps: HashMap::new(), prev: HashMap::new(), source, started: Instant::now(), last: None }
    }

    /// "Használati előzmények törlése": a számlálók nullázása, a gyűjtés újraindul.
    pub fn reset(&mut self) {
        self.apps.clear();
        self.prev.clear();
        self.started = Instant::now();
        self.last = None;
    }

    /// Egy mintavétel feldolgozása a folyamatlistából.
    pub fn update(&mut self, procs: &[ProcInfo]) {
        let now = Instant::now();
        let dt = self.last.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        self.last = Some(now);

        for a in self.apps.values_mut() {
            a.running = false;
        }
        let mut seen: HashMap<u32, (u64, String)> = HashMap::with_capacity(procs.len());
        for p in procs {
            let app = self.apps.entry(p.name_lc.clone()).or_insert_with(|| AppUsage {
                name: p.name.clone(),
                key: p.name_lc.clone(),
                ..Default::default()
            });
            app.running = true;
            if p.has_window {
                app.is_app = true;
            }
            if app.exe.is_none() {
                app.exe = p.exe.clone();
            }
            // Csak ugyanazon PID + név előző értékéhez viszonyítunk (a PID-ek újrahasznosíthatók).
            let delta = match self.prev.get(&p.pid) {
                Some((prev_ms, prev_key)) if *prev_key == p.name_lc => p.cpu_time_ms.saturating_sub(*prev_ms),
                _ => 0,
            };
            if delta > 0 {
                app.cpu_time_ms += delta;
                let (net, notif) = self.source.sample(&p.name_lc, delta, dt);
                app.network_mb += net;
                app.notifications_mb += notif;
            }
            seen.insert(p.pid, (p.cpu_time_ms, p.name_lc.clone()));
        }
        self.prev = seen;

        // Memóriavédelem: rengeteg különböző nevű, rövid életű folyamat esetén a már nem futó, üres sorokat eldobjuk.
        if self.apps.len() > 4000 {
            self.apps.retain(|_, a| a.running || a.cpu_time_ms > 0);
        }
    }

    /// Az UI-nak szánt pillanatkép (csak a ténylegesen értelmes sorok, processzoridő szerint csökkenő sorrendben).
    pub fn history(&self) -> AppHistory {
        let mut rows: Vec<AppUsage> =
            self.apps.values().filter(|a| a.cpu_time_ms > 0 || a.is_app).cloned().collect();
        rows.sort_by(|a, b| b.cpu_time_ms.cmp(&a.cpu_time_ms).then_with(|| a.key.cmp(&b.key)));
        AppHistory { rows, elapsed_secs: self.started.elapsed().as_secs(), simulated: self.source.is_simulated() }
    }
}
