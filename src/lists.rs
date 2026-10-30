/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! A "lassú" listák háttérszála: Szolgáltatások és Indítási alkalmazások.
//!
//! Külön szál (`sys-lists`), hogy a szolgáltatás-felsorolás vagy a registry-olvasás soha ne lassítsa a `hw-monitor`
//! mintavételezését, és az UI se várjon rájuk. Az UI a `Shared.data`-t `try_lock`-kal olvassa (verziószámmal jelezve,
//! hogy van-e újdonság), a parancsok (kapcsolás, frissítés) csatornán mennek.
//!
//! ÉRDEKLŐDÉSI MASZK: a szál csak azt frissíti, ami látható fülön van (a Szolgáltatások kb. 2 másodpercenként, az
//! Indítási alkalmazások 5 másodpercenként), így háttérben nulla többletterhelés.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::services::{self, ServiceAction, ServiceCache, ServiceEntry};
use crate::startup::{self, StartupEntry, StartupSource};

pub const INTEREST_SERVICES: u8 = 0b01;
pub const INTEREST_STARTUP: u8 = 0b10;

const SERVICES_PERIOD: Duration = Duration::from_secs(2);
const STARTUP_PERIOD: Duration = Duration::from_secs(5);

/// A szál által frissített adatok (az UI másolatot kap).
#[derive(Clone, Debug, Default)]
pub struct ListsData {
    pub services: Vec<ServiceEntry>,
    pub services_loaded: bool,
    pub services_error: Option<String>,
    pub startup: Vec<StartupEntry>,
    pub startup_loaded: bool,
    /// Legutóbbi BIOS művelet ideje (ms); `None`, ha a firmware nem adja át.
    pub bios_ms: Option<u64>,
}

pub struct ListsShared {
    pub data: Mutex<ListsData>,
    /// Minden írás után nő; az UI ebből látja, hogy érdemes újra másolni.
    pub version: AtomicU64,
}

/// UI -> szál parancsok.
pub enum ListsCmd {
    /// Egy indítási bejegyzés engedélyezése / letiltása.
    SetStartup { source: StartupSource, value_name: String, name: String, enabled: bool },
    /// Szolgáltatás indítása / leállítása / újraindítása (külön szálon fut, mert a leállítás várakozhat).
    ServiceControl { name: String, action: ServiceAction },
    /// Azonnali újraolvasás.
    RefreshNow,
}

/// Szál -> UI események.
pub enum ListsEvent {
    StartupToggled { name: String, enabled: bool, result: Result<(), String> },
    ServiceControlled { name: String, action: ServiceAction, result: Result<(), String> },
}

pub struct ListsHandle {
    pub shared: Arc<ListsShared>,
    pub cmd: Sender<ListsCmd>,
    pub events: Receiver<ListsEvent>,
    /// Az UI ide írja, melyik lista látszik éppen (INTEREST_* bitek).
    pub interest: Arc<AtomicU8>,
}

pub fn start(ctx: egui::Context) -> Option<ListsHandle> {
    let shared = Arc::new(ListsShared { data: Mutex::new(ListsData::default()), version: AtomicU64::new(0) });
    let interest = Arc::new(AtomicU8::new(0));
    let (cmd_tx, cmd_rx) = mpsc::channel::<ListsCmd>();
    let (ev_tx, ev_rx) = mpsc::channel::<ListsEvent>();

    let worker_shared = Arc::clone(&shared);
    let worker_interest = Arc::clone(&interest);
    thread::Builder::new()
        .name("sys-lists".into())
        .spawn(move || lists_thread(ctx, worker_shared, cmd_rx, ev_tx, worker_interest))
        .ok()?;
    Some(ListsHandle { shared, cmd: cmd_tx, events: ev_rx, interest })
}

fn due(last: Option<Instant>, period: Duration) -> bool {
    last.map_or(true, |t| t.elapsed() >= period)
}

fn lists_thread(
    ctx: egui::Context,
    shared: Arc<ListsShared>,
    commands: Receiver<ListsCmd>,
    events: Sender<ListsEvent>,
    interest: Arc<AtomicU8>,
) {
    crate::lower_current_thread_priority();

    let mut svc_cache = ServiceCache::default();
    let mut publisher_cache: HashMap<String, String> = HashMap::new();
    let mut last_services: Option<Instant> = None;
    let mut last_startup: Option<Instant> = None;

    loop {
        match commands.recv_timeout(Duration::from_millis(300)) {
            Ok(ListsCmd::SetStartup { source, value_name, name, enabled }) => {
                let result = catch_unwind(AssertUnwindSafe(|| startup::set_enabled(source, &value_name, enabled)))
                    .unwrap_or_else(|_| Err("panic".to_string()));
                if events.send(ListsEvent::StartupToggled { name, enabled, result }).is_err() {
                    break;
                }
                last_startup = None; // az eredmény azonnal látsszon a listában
            }
            Ok(ListsCmd::ServiceControl { name, action }) => {
                let tx = events.clone();
                let c = ctx.clone();
                let _ = thread::Builder::new().name("svc-control".into()).spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| services::control(&name, action)))
                        .unwrap_or_else(|_| Err("panic".to_string()));
                    let _ = tx.send(ListsEvent::ServiceControlled { name, action, result });
                    c.request_repaint();
                });
                last_services = None; // az átmeneti állapot (Indítás…/Leállítás…) hamar látszódjon
            }
            Ok(ListsCmd::RefreshNow) => {
                last_services = None;
                last_startup = None;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break, // az UI bezárult
        }

        let mask = interest.load(Ordering::Relaxed);
        let mut changed = false;

        if mask & INTEREST_SERVICES != 0 && due(last_services, SERVICES_PERIOD) {
            let res = catch_unwind(AssertUnwindSafe(|| services::enumerate(&mut svc_cache)))
                .unwrap_or_else(|_| Err("panic".to_string()));
            let mut g = shared.data.lock().unwrap_or_else(|e| e.into_inner());
            match res {
                Ok(list) => {
                    g.services = list;
                    g.services_error = None;
                }
                Err(e) => g.services_error = Some(e),
            }
            g.services_loaded = true;
            last_services = Some(Instant::now());
            changed = true;
        }

        if mask & INTEREST_STARTUP != 0 && due(last_startup, STARTUP_PERIOD) {
            let list = catch_unwind(AssertUnwindSafe(|| startup::list(&mut publisher_cache))).unwrap_or_default();
            // A BIOS-idő a legutóbbi rendszerindításé: elég egyszer (és minden kézi frissítéskor) lekérdezni.
            let bios = if last_startup.is_none() {
                Some(catch_unwind(AssertUnwindSafe(startup::last_bios_time_ms)).unwrap_or(None))
            } else {
                None
            };
            let mut g = shared.data.lock().unwrap_or_else(|e| e.into_inner());
            g.startup = list;
            g.startup_loaded = true;
            if let Some(b) = bios {
                g.bios_ms = b;
            }
            last_startup = Some(Instant::now());
            changed = true;
        }

        if changed {
            shared.version.fetch_add(1, Ordering::Release);
            ctx.request_repaint();
        }
    }
}
