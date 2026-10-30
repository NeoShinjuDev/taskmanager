/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! A három "listás" fül – Alkalmazáselőzmények, Indítási alkalmazások, Szolgáltatások –, egy közös,
//! rendezhető táblázatrajzoló, valamint a háttérszálakkal és a futásidejű beállításokkal kapcsolatos segédfüggvények.
//!
//! A modul a főmodul (`main.rs`) gyermeke, ezért eléri annak privát elemeit (TaskApp mezői, rajzoló segédek).

use super::*;

use crate::apphistory::AppUsage;
use crate::lists::{self, ListsCmd, ListsEvent};
use crate::menus::{self, MenuAction};
use crate::services::{ServiceAction, ServiceEntry};
use crate::startup::{self, Impact, StartupEntry, StartupSource};
use std::sync::atomic::Ordering as AtomicOrdering;

// ═══════════════════════════════════════════════════════════════════════════
// Közös táblázat
// ═══════════════════════════════════════════════════════════════════════════

/// Egy tábla rendezési és görgetési állapota.
#[derive(Default)]
pub struct TableState {
    pub sort: usize,
    pub desc: bool,
    pub scroll_x: f32,
}

/// A három fül UI-állapota egy helyen (a `TaskApp` egyetlen mezője).
pub struct TabState {
    pub ah_table: TableState,
    pub ah_filter: String,
    pub ah_all: bool,
    pub su_table: TableState,
    /// Kijelölt indítási elem: (forrás, érték neve).
    pub su_sel: Option<(StartupSource, String)>,
    pub sv_table: TableState,
    pub sv_filter: String,
    /// Kijelölt szolgáltatás neve.
    pub sv_sel: Option<String>,
    /// A Részletek fül kijelölt PID-je.
    pub det_sel: Option<u32>,
    /// A nyitott "Processzor-affinitás" ablak (ha van).
    pub affinity: Option<menus::AffinityDialog>,
    /// A Felhasználók fül kijelölt alkalmazás-sora: (munkamenet-azonosító, kisbetűs folyamatnév).
    pub usr_sel: Option<(u32, String)>,
    /// A Folyamatok és a Felhasználók tábla vízszintes görgetése (a fejléc ezt követi).
    pub proc_scroll_x: f32,
    pub usr_scroll_x: f32,
    /// A nyitott "Új feladat létrehozása" ablak (ha van).
    pub new_task: Option<menus::NewTaskDialog>,
    /// Az új feladat indításának (háttérszálon futó) eredménye.
    pub task_rx: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// A fájlválasztó (háttérszálon futó) eredménye.
    pub browse_rx: Option<std::sync::mpsc::Receiver<Option<String>>>,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            // Alkalmazáselőzmények: processzoridő szerint csökkenően.
            ah_table: TableState { sort: 1, desc: true, scroll_x: 0.0 },
            ah_filter: String::new(),
            ah_all: false,
            su_table: TableState::default(),
            su_sel: None,
            sv_table: TableState::default(),
            sv_filter: String::new(),
            sv_sel: None,
            det_sel: None,
            affinity: None,
            usr_sel: None,
            proc_scroll_x: 0.0,
            usr_scroll_x: 0.0,
            new_task: None,
            task_rx: None,
            browse_rx: None,
        }
    }
}

/// Oszlop: felirat, szélesség, jobbra igazított-e, számként rendezendő-e (alapból csökkenő).
struct Col {
    title: &'static str,
    width: f32,
    right: bool,
    numeric: bool,
    /// Extra bal oldali behúzás (a név oszlopban az ikon számára).
    pad: f32,
}

/// Egy cella szövegének kirajzolása az adott oszlopban (a cella határára vágva).
fn cell(painter: &egui::Painter, row: Rect, xs: &[f32], cols: &[Col], i: usize, text: &str, color: Color32) {
    let x0 = row.left() + xs[i];
    let clip = Rect::from_min_max(pos2(x0, row.top()), pos2(x0 + cols[i].width - 6.0, row.bottom()))
        .intersect(painter.clip_rect());
    let p = painter.with_clip_rect(clip);
    let font = FontId::proportional(13.0);
    if cols[i].right {
        p.text(pos2(x0 + cols[i].width - 10.0, row.center().y), Align2::RIGHT_CENTER, text, font, color);
    } else {
        p.text(pos2(x0 + 10.0 + cols[i].pad, row.center().y), Align2::LEFT_CENTER, text, font, color);
    }
}

fn dash(s: &str) -> String {
    if s.is_empty() {
        "—".to_string()
    } else {
        s.to_string()
    }
}

/// Rögzített, kattintásra rendező fejléc + virtualizált sorok függőleges és vízszintes görgetéssel.
/// A sorokat a hívó rendezi / szűri és a `draw_row(ui, sor_téglalap, sor_index, oszlop_x_eltolások)` rajzolja.
fn table(
    ui: &mut Ui,
    id: &'static str,
    cols: &[Col],
    st: &mut TableState,
    rows: usize,
    selected: Option<usize>,
    mut draw_row: impl FnMut(&mut Ui, Rect, usize, &[f32], &egui::Response),
) -> Option<usize> {
    let mut xs: Vec<f32> = Vec::with_capacity(cols.len() + 1);
    xs.push(0.0);
    for c in cols {
        let last = xs[xs.len() - 1];
        xs.push(last + c.width);
    }
    let total_w = xs[xs.len() - 1];
    let avail_w = ui.available_width();

    // Fejléc: a vízszintes görgetéssel együtt mozog.
    let (hrect, _) = ui.allocate_exact_size(vec2(avail_w, 28.0), Sense::hover());
    let hp = ui.painter_at(hrect);
    let off = st.scroll_x;
    let mut clicked: Option<usize> = None;
    for (i, c) in cols.iter().enumerate() {
        let x0 = hrect.left() + xs[i] - off;
        let cell_rect = Rect::from_min_max(pos2(x0, hrect.top()), pos2(x0 + c.width, hrect.bottom()));
        let visible = cell_rect.intersect(hrect);
        if visible.width() <= 1.0 {
            continue;
        }
        let resp = ui
            .interact(visible, ui.id().with((id, "hdr", i)), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let active = st.sort == i;
        let color = if active { col_text() } else { col_muted() };
        let clipped = hp.with_clip_rect(visible);
        let cy = hrect.center().y;
        let font = FontId::proportional(12.5);
        let title_rect = if c.right {
            clipped.text(pos2(x0 + c.width - 10.0, cy), Align2::RIGHT_CENTER, c.title, font, color)
        } else {
            clipped.text(pos2(x0 + 10.0, cy), Align2::LEFT_CENTER, c.title, font, color)
        };
        if active {
            let ax = if c.right { title_rect.left() - 10.0 } else { title_rect.right() + 10.0 };
            tri(&clipped, pos2(ax, cy), if st.desc { 0 } else { 1 }, color);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    hp.line_segment([hrect.left_bottom(), hrect.right_bottom()], Stroke::new(1.0_f32, overlay(18)));

    // Tartalom.
    let row_w = total_w.max(avail_w);
    let mut clicked_row: Option<usize> = None;
    let out = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::both().id_salt(id).auto_shrink([false, false]).show_rows(
            ui,
            ROW_H,
            rows,
            |ui, range| {
                for i in range {
                    let (rect, resp) = ui.allocate_exact_size(vec2(row_w, ROW_H), Sense::click());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let row = rect.shrink2(vec2(0.0, 1.0));
                    if selected == Some(i) {
                        ui.painter().rect_filled(row, CornerRadius::same(6), accent().gamma_multiply(0.35));
                    } else if resp.hovered() {
                        ui.painter().rect_filled(row, CornerRadius::same(4), overlay(10));
                    }
                    draw_row(ui, rect, i, &xs, &resp);
                    // Bal és jobb kattintásra is kijelöljük a sort (a jobb klikk a menüt is megnyitja).
                    if resp.clicked() || resp.secondary_clicked() {
                        clicked_row = Some(i);
                    }
                }
            },
        )
    });
    st.scroll_x = out.inner.state.offset.x;

    if let Some(i) = clicked {
        if st.sort == i {
            st.desc = !st.desc;
        } else {
            st.sort = i;
            st.desc = cols[i].numeric; // számoknál a legnagyobb legyen elöl
        }
    }
    clicked_row
}

/// "h:mm:ss" (a Task Manager is így írja a processzoridőt).
fn fmt_hms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn fmt_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        trf("{0} h {1} min", &[&h, &m])
    } else if m > 0 {
        trf("{0} min {1} s", &[&m, &s])
    } else {
        trf("{0} s", &[&s])
    }
}

fn impact_color(i: Impact) -> Color32 {
    match i {
        Impact::High => DANGER_RED,
        Impact::Medium => WARN_AMBER,
        Impact::Low => OK_GREEN,
        Impact::Unknown => col_muted(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Háttérszálak és beállítások kezelése
// ═══════════════════════════════════════════════════════════════════════════

impl TaskApp {
    /// A `sys-lists` szál adatainak átvétele (SOSEM blokkol) + annak jelzése, melyik lista látszik.
    pub(super) fn pump_lists(&mut self) {
        let mask = match self.tab {
            Tab::Services => lists::INTEREST_SERVICES,
            // A Részletek fül tudni akarja, melyik folyamat hostol szolgáltatást ("Ugrás szolgáltatás(ok)ra").
            Tab::Details => lists::INTEREST_SERVICES,
            Tab::Startup => lists::INTEREST_STARTUP,
            _ => 0,
        };
        let mut events: Vec<ListsEvent> = Vec::new();
        let mut fresh: Option<(lists::ListsData, u64)> = None;
        if let Some(l) = &self.lists {
            l.interest.store(mask, AtomicOrdering::Relaxed);
            while let Ok(e) = l.events.try_recv() {
                events.push(e);
            }
            let v = l.shared.version.load(AtomicOrdering::Acquire);
            if v != self.lists_version {
                match l.shared.data.try_lock() {
                    Ok(g) => fresh = Some((g.clone(), v)),
                    Err(TryLockError::Poisoned(p)) => fresh = Some((p.into_inner().clone(), v)),
                    Err(TryLockError::WouldBlock) => {} // a szál éppen ír: a következő frame-ben jön
                }
            }
        }
        if let Some((d, v)) = fresh {
            self.lists_data = d;
            self.lists_version = v;
        }
        for e in events {
            match e {
                ListsEvent::StartupToggled { name, enabled, result } => match result {
                    Ok(()) => {
                        let msg = if enabled {
                            trf("“{0}” enabled at startup.", &[&name])
                        } else {
                            trf("“{0}” disabled at startup.", &[&name])
                        };
                        self.set_status(msg, true);
                    }
                    Err(e) => {
                        self.set_status(format!("„{name}”: {e}"), false);
                        // A helyi (optimista) állapotot a következő frissítés visszaállítja a valós értékre.
                        if let Some(l) = &self.lists {
                            let _ = l.cmd.send(ListsCmd::RefreshNow);
                        }
                    }
                },
                ListsEvent::ServiceControlled { name, action, result } => match result {
                    Ok(()) => {
                        let msg = match action {
                            ServiceAction::Start => trf("Service “{0}” started.", &[&name]),
                            ServiceAction::Stop => trf("Service “{0}” stopped.", &[&name]),
                            ServiceAction::Restart => trf("Service “{0}” restarted.", &[&name]),
                        };
                        self.set_status(msg, true);
                        if let Some(l) = &self.lists {
                            let _ = l.cmd.send(ListsCmd::RefreshNow);
                        }
                    }
                    Err(e) => {
                        self.set_status(format!("„{name}”: {e}"), false);
                        if let Some(l) = &self.lists {
                            let _ = l.cmd.send(ListsCmd::RefreshNow);
                        }
                    }
                },
            }
        }
    }

    /// A folyamatok összesített (processzoridő, memória) adata futtatható-fájlnév szerint – az indítási hatás becsléséhez.
    pub(super) fn update_run_stats(&mut self, procs: &[ProcInfo]) {
        let mut m: HashMap<String, (u64, f64)> = HashMap::with_capacity(procs.len());
        for p in procs {
            let e = m.entry(p.name_lc.clone()).or_insert((0, 0.0));
            e.0 += p.cpu_time_ms;
            e.1 += p.mem_mb;
        }
        self.run_stats = m;
    }

    /// A Beállítások fülön véglegesített (csúszka elengedve) időzítési értékek érvényesítése futás közben.
    pub(super) fn apply_runtime_settings(&mut self) {
        self.settings.sanitize();
        let new_ms = (self.settings.sample_interval_secs * 1000.0).round() as u32;
        let old_ms = self.interval_ms.swap(new_ms, AtomicOrdering::Relaxed);
        set_hist_params(self.settings.history_capacity(), self.settings.chart_window_secs);
        if old_ms != new_ms {
            // A régi minták más időlépcsőjűek voltak: tiszta lappal indul a grafikon.
            self.hist = Histories::default();
        }
    }

    // ═══════════════════════════ Alkalmazáselőzmények ═══════════════════════════
    pub(super) fn ui_app_history(&mut self, ui: &mut Ui) {
        let elapsed = fmt_elapsed(self.app_history.elapsed_secs);
        let subtitle = trf("Resource usage since monitoring started – {0} ago", &[&elapsed]);
        page_header(ui, tr("App history"), &subtitle);

        let mut reset = false;
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.tabs.ah_filter)
                    .hint_text(tr("Search by app name…"))
                    .desired_width(280.0),
            );
            ui.add_space(10.0);
            ui.checkbox(&mut self.tabs.ah_all, tr("Show history for all processes"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(tr("Delete usage history")).clicked() {
                    reset = true;
                }
            });
        });
        if self.app_history.simulated {
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr(
                    "CPU time is real, measured since the program started. The Network and Notifications columns are simulated for now (per-process traffic needs an ETW session); the structure is ready for a real source.",
                ))
                .size(11.5)
                .color(col_muted()),
            );
        }
        ui.add_space(8.0);

        if reset {
            if let Some(m) = &self.monitor {
                let _ = m.cmd.send(Command::ResetAppHistory);
            }
            self.app_history = Default::default();
        }
        if self.app_history.rows.is_empty() {
            ui.label(RichText::new(tr("Collecting data…")).color(col_muted()));
            return;
        }

        let q = self.tabs.ah_filter.trim().to_lowercase();
        let show_all = self.tabs.ah_all;
        let mut list: Vec<&AppUsage> = self
            .app_history
            .rows
            .iter()
            .filter(|a| (show_all || a.is_app) && (q.is_empty() || a.key.contains(&q)))
            .collect();
        let (sort, desc) = (self.tabs.ah_table.sort, self.tabs.ah_table.desc);
        list.sort_by(|a, b| {
            let o = match sort {
                0 => a.key.cmp(&b.key),
                1 => a.cpu_time_ms.cmp(&b.cpu_time_ms),
                2 => a.network_mb.total_cmp(&b.network_mb),
                _ => a.notifications_mb.total_cmp(&b.notifications_mb),
            };
            if desc {
                o.reverse()
            } else {
                o
            }
        });

        let cols = [
            Col { title: tr("App name"), width: 340.0, right: false, numeric: false, pad: 26.0 },
            Col { title: tr("CPU time"), width: 150.0, right: true, numeric: true, pad: 0.0 },
            Col { title: tr("Network (MB)"), width: 150.0, right: true, numeric: true, pad: 0.0 },
            Col { title: tr("Notifications (MB)"), width: 170.0, right: true, numeric: true, pad: 0.0 },
        ];
        let icons = &self.icons;
        let mut want_icons: Vec<String> = Vec::new();
        table(ui, "ah_list", &cols, &mut self.tabs.ah_table, list.len(), None, |ui, rect, i, xs, _resp| {
            let a = list[i];
            let p = ui.painter();
            let c = if a.running { col_text() } else { col_muted() };
            menus::draw_exe_icon(p, icons, a.exe.as_ref(), pos2(rect.left() + xs[0] + 20.0, rect.center().y), &mut want_icons);
            cell(p, rect, xs, &cols, 0, &a.name, c);
            cell(p, rect, xs, &cols, 1, &fmt_hms(a.cpu_time_ms), c);
            cell(p, rect, xs, &cols, 2, &comma(format!("{:.2}", a.network_mb)), c);
            cell(p, rect, xs, &cols, 3, &comma(format!("{:.2}", a.notifications_mb)), c);
        });
        for path in want_icons {
            self.request_icon(&path);
        }
    }

    // ═══════════════════════════ Indítási alkalmazások ═══════════════════════════
    pub(super) fn ui_startup(&mut self, ui: &mut Ui) {
        let total = self.lists_data.startup.len();
        let enabled_n = self.lists_data.startup.iter().filter(|e| e.enabled).count();
        let subtitle = trf("{0} startup items, {1} enabled", &[&total, &enabled_n]);
        page_header(ui, tr("Startup apps"), &subtitle);
        self.status_line(ui);

        // Felső rész: legutóbbi BIOS-idő.
        let bios_text = match self.lists_data.bios_ms {
            Some(ms) => comma(format!("{:.1} s", ms as f64 / 1000.0)),
            None if self.lists_data.startup_loaded => tr("Not available").to_string(),
            None => tr("Loading data…").to_string(),
        };
        let bios_missing = self.lists_data.bios_ms.is_none() && self.lists_data.startup_loaded;
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Last BIOS time:")).size(13.0).color(col_muted()));
                let r = ui.label(RichText::new(&bios_text).size(18.0).strong());
                if bios_missing {
                    r.on_hover_text(tr(
                        "The motherboard UEFI does not expose the BIOS time (Windows Task Manager leaves it blank in that case).",
                    ));
                }
            });
        });

        if !self.elevated && self.lists_data.startup.iter().any(|e| e.source.needs_admin()) {
            ui.label(
                RichText::new(tr("Administrator rights are required to switch HKLM and all-users folder items."))
                    .size(11.5)
                    .color(col_muted()),
            );
            ui.add_space(6.0);
        }

        if !self.lists_data.startup_loaded {
            ui.label(RichText::new(tr("Loading data…")).color(col_muted()));
            return;
        }
        if self.lists_data.startup.is_empty() {
            ui.label(RichText::new(tr("No startup items.")).color(col_muted()));
            return;
        }

        let stats = &self.run_stats;
        let impact_of = |e: &StartupEntry| startup::estimate_impact(stats.get(&e.exe_name_lc).copied());
        let mut list: Vec<&StartupEntry> = self.lists_data.startup.iter().collect();
        let (sort, desc) = (self.tabs.su_table.sort, self.tabs.su_table.desc);
        list.sort_by(|a, b| {
            let o = match sort {
                0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                1 => a.publisher.to_lowercase().cmp(&b.publisher.to_lowercase()),
                2 => a.enabled.cmp(&b.enabled),
                3 => impact_of(a).cmp(&impact_of(b)),
                _ => a.source.label().cmp(b.source.label()),
            };
            if desc {
                o.reverse()
            } else {
                o
            }
        });
        let sel_idx = self
            .tabs
            .su_sel
            .as_ref()
            .and_then(|(s, v)| list.iter().position(|e| e.source == *s && &e.value_name == v));

        let cols = [
            Col { title: tr("Name"), width: 270.0, right: false, numeric: false, pad: 26.0 },
            Col { title: tr("Vendor"), width: 230.0, right: false, numeric: false, pad: 0.0 },
            Col { title: tr("Status"), width: 190.0, right: false, numeric: false, pad: 0.0 },
            Col { title: tr("Startup impact"), width: 210.0, right: false, numeric: false, pad: 0.0 },
            Col { title: tr("Source"), width: 210.0, right: false, numeric: false, pad: 0.0 },
        ];
        let elevated = self.elevated;
        let icons = &self.icons;
        let mut want_icons: Vec<String> = Vec::new();
        let mut toggle: Option<(usize, bool)> = None;
        let mut blocked = false;
        let mut menu_act: Option<MenuAction> = None;
        let clicked = table(ui, "su_list", &cols, &mut self.tabs.su_table, list.len(), sel_idx, |ui, rect, i, xs, row_resp| {
            let e = list[i];
            let cy = rect.center().y;
            let p = ui.painter();

            // Név (ikonnal; a teljes parancssor tooltipben).
            menus::draw_exe_icon(p, icons, e.exe.as_ref(), pos2(rect.left() + xs[0] + 20.0, cy), &mut want_icons);
            cell(p, rect, xs, &cols, 0, &e.name, col_text());
            let name_cell = Rect::from_min_max(pos2(rect.left() + xs[0], rect.top()), pos2(rect.left() + xs[1], rect.bottom()));
            ui.interact(name_cell, ui.id().with(("su_name", i)), Sense::hover()).on_hover_text(e.command.as_str());

            cell(p, rect, xs, &cols, 1, &dash(&e.publisher), col_muted());

            // Állapot: működő kapcsoló + felirat.
            let can = !e.source.needs_admin() || elevated;
            let x0 = rect.left() + xs[2];
            let sw = Rect::from_min_size(pos2(x0 + 10.0, cy - 9.0), vec2(36.0, 18.0));
            let resp = ui
                .interact(sw, ui.id().with(("su_tgl", i)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let mut track = if e.enabled { accent() } else { overlay(40) };
            if !can {
                track = track.gamma_multiply(0.45);
            }
            p.rect_filled(sw, CornerRadius::same(9), track);
            let knob_x = if e.enabled { sw.right() - 9.0 } else { sw.left() + 9.0 };
            p.circle_filled(pos2(knob_x, cy), 6.5, Color32::WHITE);
            let state_text = if e.enabled { tr("Enabled") } else { tr("Disabled") };
            let state_color = if e.enabled { col_text() } else { crate::theme::col_disabled() };
            p.text(pos2(x0 + 56.0, cy), Align2::LEFT_CENTER, state_text, FontId::proportional(13.0), state_color);
            if resp.clicked() {
                if can {
                    toggle = Some((i, !e.enabled));
                } else {
                    blocked = true;
                }
            }
            if !can {
                resp.clone().on_hover_text(tr("Administrator rights required"));
            }

            // Hatás (becslés) és forrás.
            let impact = impact_of(e);
            cell(p, rect, xs, &cols, 3, impact.label(), impact_color(impact));
            cell(p, rect, xs, &cols, 4, e.source.label(), col_muted());

            // Helyi menü (a sorra és a kapcsolóra kattintva is ugyanez nyílik).
            let mut menu = |ui: &mut Ui| {
                ui.set_min_width(230.0);
                let text = if e.enabled { tr("Disable") } else { tr("Enable") };
                if menus::item(ui, text, true, false) {
                    if can {
                        toggle = Some((i, !e.enabled));
                    } else {
                        blocked = true;
                    }
                }
                ui.separator();
                let path = e.target_path();
                if menus::item(ui, tr("Open file location"), path.is_some(), false) {
                    if let Some(pth) = &path {
                        menu_act = Some(MenuAction::OpenLocation(pth.clone()));
                    }
                }
                if menus::item(ui, tr("Search online"), true, false) {
                    menu_act = Some(MenuAction::SearchOnline(e.name.clone()));
                }
                if menus::item(ui, tr("Properties"), path.is_some(), false) {
                    if let Some(pth) = &path {
                        menu_act = Some(MenuAction::Properties(pth.clone()));
                    }
                }
            };
            row_resp.clone().context_menu(|ui| menu(ui));
            resp.clone().context_menu(|ui| menu(ui));
        });
        ui.label(
            RichText::new(tr(
                "The impact is an estimate from the running process's CPU time and memory; items that are not running show “Not measured”.",
            ))
            .size(11.5)
            .color(col_muted()),
        );

        let new_sel = clicked.map(|i| (list[i].source, list[i].value_name.clone()));
        let action = toggle.map(|(i, state)| (list[i].source, list[i].value_name.clone(), list[i].name.clone(), state));
        // A `list` innentől nem él, így már kérhetjük az ikonokat (&mut self).
        for path in want_icons {
            self.request_icon(&path);
        }
        if let Some(k) = new_sel {
            self.tabs.su_sel = Some(k);
        }
        if let Some((source, value_name, name, state)) = action {
            // Optimista helyi frissítés; a háttérszál valós eredménye a következő verziófrissítéssel érkezik.
            if let Some(e) = self
                .lists_data
                .startup
                .iter_mut()
                .find(|e| e.source == source && e.value_name == value_name)
            {
                e.enabled = state;
            }
            if let Some(l) = &self.lists {
                let _ = l.cmd.send(ListsCmd::SetStartup { source, value_name, name, enabled: state });
            }
        }
        if blocked {
            self.set_status(tr("Administrator rights are required to change this item.").to_string(), false);
        }
        if let Some(a) = menu_act {
            self.run_menu_action(a);
        }
    }

    // ═══════════════════════════ Szolgáltatások ═══════════════════════════
    pub(super) fn ui_services(&mut self, ui: &mut Ui) {
        let total = self.lists_data.services.len();
        let running = self.lists_data.services.iter().filter(|s| s.state.is_running()).count();
        let subtitle = trf("{0} services, {1} running", &[&total, &running]);
        page_header(ui, tr("Services"), &subtitle);
        self.status_line(ui);

        let mut open = false;
        ui.horizontal(|ui| {
            if ui.button(tr("Open Services")).clicked() {
                open = true;
            }
            ui.add_space(10.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.tabs.sv_filter)
                    .hint_text(tr("Search by name, PID or description…"))
                    .desired_width(320.0),
            );
        });
        if open {
            self.run_menu_action(MenuAction::OpenServicesConsole);
        }
        ui.add_space(8.0);

        if let Some(err) = &self.lists_data.services_error {
            ui.label(RichText::new(format!("{}: {err}", tr("Service list error"))).color(DANGER_RED));
        }
        if !self.lists_data.services_loaded {
            ui.label(RichText::new(tr("Loading data…")).color(col_muted()));
            return;
        }

        let q = self.tabs.sv_filter.trim().to_lowercase();
        let mut list: Vec<&ServiceEntry> = self
            .lists_data
            .services
            .iter()
            .filter(|s| {
                q.is_empty()
                    || s.name.to_lowercase().contains(&q)
                    || s.display.to_lowercase().contains(&q)
                    || s.pid.to_string().contains(&q)
            })
            .collect();
        let (sort, desc) = (self.tabs.sv_table.sort, self.tabs.sv_table.desc);
        list.sort_by(|a, b| {
            let o = match sort {
                0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                1 => a.pid.cmp(&b.pid),
                2 => a.display.to_lowercase().cmp(&b.display.to_lowercase()),
                3 => a.state.is_running().cmp(&b.state.is_running()),
                _ => a.group.to_lowercase().cmp(&b.group.to_lowercase()),
            };
            if desc {
                o.reverse()
            } else {
                o
            }
        });
        let sel_idx = self.tabs.sv_sel.as_ref().and_then(|n| list.iter().position(|s| &s.name == n));

        let cols = [
            Col { title: tr("Name"), width: 240.0, right: false, numeric: false, pad: 26.0 },
            Col { title: "PID", width: 90.0, right: true, numeric: true, pad: 0.0 },
            Col { title: tr("Description"), width: 430.0, right: false, numeric: false, pad: 0.0 },
            Col { title: tr("Status"), width: 140.0, right: false, numeric: false, pad: 0.0 },
            Col { title: tr("Group"), width: 230.0, right: false, numeric: false, pad: 0.0 },
        ];
        let mut menu_act: Option<MenuAction> = None;
        let clicked = table(ui, "sv_list", &cols, &mut self.tabs.sv_table, list.len(), sel_idx, |ui, rect, i, xs, resp| {
            let s = list[i];
            let p = ui.painter();
            let cy = rect.center().y;
            // Dupla fogaskerék ikon + név.
            menus::draw_gears(p, pos2(rect.left() + xs[0] + 20.0, cy));
            cell(p, rect, xs, &cols, 0, &s.name, col_text());
            let pid = if s.pid == 0 { "—".to_string() } else { s.pid.to_string() };
            cell(p, rect, xs, &cols, 1, &pid, col_muted());
            cell(p, rect, xs, &cols, 2, &dash(&s.display), col_muted());
            // Állapot: színes pont + felirat.
            let x0 = rect.left() + xs[3];
            let on = s.state.is_running();
            p.circle_filled(pos2(x0 + 14.0, cy), 4.0, if on { OK_GREEN } else { col_muted() });
            let clip = Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + cols[3].width - 6.0, rect.bottom()))
                .intersect(p.clip_rect());
            p.with_clip_rect(clip).text(
                pos2(x0 + 28.0, cy),
                Align2::LEFT_CENTER,
                s.state.label(),
                FontId::proportional(13.0),
                if on { col_text() } else { col_muted() },
            );
            cell(p, rect, xs, &cols, 4, &dash(&s.group), col_muted());
            resp.clone().context_menu(|ui| menus::service_menu(ui, s, &mut menu_act));
        });
        let new_sel = clicked.map(|i| list[i].name.clone());
        if let Some(n) = new_sel {
            self.tabs.sv_sel = Some(n);
        }
        if let Some(a) = menu_act {
            self.run_menu_action(a);
        }
    }
}
