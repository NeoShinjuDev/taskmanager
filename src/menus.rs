/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Helyi (jobb klikkes) menük a gyári Task Manager mintájára – Folyamatok, Részletek, Indítási alkalmazások,
//! Szolgáltatások –, a hozzájuk tartozó műveletek végrehajtása, az ikon-rajzoló segédek (exe-ikon, dupla fogaskerék)
//! és a Processzor-affinitás párbeszédablak.
//!
//! A menük csak egy `MenuAction`-t állítanak be (nem hívnak `&mut self` metódust), a tényleges műveletet a `TaskApp`
//! `run_menu_action`-je hajtja végre a rajzolás után – így a sorok kirajzolása közben nincs kölcsönzési ütközés.

use super::*;

use crate::services::{ServiceAction, ServiceEntry, ServiceState};
use crate::backdrop;
use crate::shell;
use std::sync::mpsc;

/// Egy menüpont által kért művelet.
#[derive(Clone, Debug)]
pub enum MenuAction {
    ToggleExpand(String),
    SwitchTo(Vec<u32>),
    EndTask { label: String, targets: Vec<(u32, String)> },
    EndTree { pid: u32, name: String },
    SetPriority { pid: u32, class: u32, name: String },
    SetAffinity { pid: u32, name: String },
    OpenLocation(String),
    Properties(String),
    SearchOnline(String),
    GoToDetails(u32),
    GoToServices(u32),
    ServiceControl { name: String, action: ServiceAction },
    OpenServicesConsole,
    /// A Felhasználók fülön egy felhasználó sorának ki-/becsukása.
    ToggleUserExpand(u32),
    /// Megnyitja az "Új feladat futtatása" ablakot.
    NewTask,
}

/// Az "Új feladat futtatása" ablak állapota.
#[derive(Default)]
pub struct NewTaskDialog {
    pub command: String,
    pub admin: bool,
    /// Az első megjelenéskor a beviteli mezőre teszi a fókuszt.
    pub focused: bool,
    /// Az ablak kezdőpozíciója (a főablak közepe) – egyszer számoljuk ki, hogy az ablak szabadon mozgatható maradjon.
    pub pos: Option<Pos2>,
    /// A natív ablak címsorának Mica-ja már be van-e állítva.
    pub mica_done: bool,
    /// Hányszor kerestük eddig az ablak HWND-jét (a keresést néhány tucat frame után feladjuk).
    pub mica_tries: u32,
}

/// A "Processzor-affinitás" ablak állapota.
pub struct AffinityDialog {
    pub pid: u32,
    pub name: String,
    /// A kijelölt processzorok maszkja.
    pub mask: usize,
    /// A rendszer processzorainak maszkja (csak ezekhez mutatunk jelölőnégyzetet).
    pub system_mask: usize,
}

/// Egy menüpont: kattintásra (és ha engedélyezett) `true`, és bezárja a menüt.
pub(crate) fn item(ui: &mut Ui, text: &str, enabled: bool, bold: bool) -> bool {
    let label = if bold { RichText::new(text).strong() } else { RichText::new(text) };
    let clicked = ui.add_enabled(enabled, egui::Button::new(label)).clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// Pipás menüpont (jelölőnégyzet-szerű): a pipa a bal oldalon látszik. Kattintásra `true`; a menüt NEM zárja be, így
/// egymás után több elem is kiválasztható.
pub(crate) fn check_item(ui: &mut Ui, text: &str, checked: bool) -> bool {
    let w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect.shrink2(vec2(2.0, 0.0)), CornerRadius::same(6), overlay(16));
    }
    let cy = rect.center().y;
    if checked {
        let c = pos2(rect.left() + 16.0, cy);
        let stroke = Stroke::new(1.6_f32, col_text());
        p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x - 1.5, c.y + 3.5)], stroke);
        p.line_segment([pos2(c.x - 1.5, c.y + 3.5), pos2(c.x + 5.0, c.y - 4.0)], stroke);
    }
    p.text(pos2(rect.left() + 34.0, cy), Align2::LEFT_CENTER, text, FontId::proportional(13.5), col_text());
    resp.clicked()
}

/// A Szolgáltatások fül eszköztárának ikonjai.
#[derive(Clone, Copy)]
pub(crate) enum SvcIcon {
    Start,
    Stop,
    Restart,
}

/// A Szolgáltatások fül eszköztár-gombja (ikon + felirat), a gyári Task Manager mintájára. Letiltva halvány, és nem
/// kattintható. Igaz, ha a gombra kattintottak.
pub(crate) fn svc_button(ui: &mut Ui, icon: SvcIcon, text: &str, enabled: bool) -> bool {
    let color = if enabled { col_text() } else { col_muted().gamma_multiply(0.55) };
    // A felirat szélessége közelítő (a betűszám alapján): így nem kell előre kiszámolni a szövegelrendezést.
    let text_w = text.chars().count() as f32 * 7.6;
    let size = vec2(12.0 + 16.0 + 8.0 + text_w + 12.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), overlay(16));
    }
    let cy = rect.center().y;
    let c = pos2(rect.left() + 12.0 + 8.0, cy);
    let stroke = Stroke::new(1.5_f32, color);
    match icon {
        SvcIcon::Start => {
            // Üres háromszög (lejátszás)
            p.add(Shape::closed_line(
                vec![pos2(c.x - 4.0, c.y - 6.0), pos2(c.x - 4.0, c.y + 6.0), pos2(c.x + 6.0, c.y)],
                stroke,
            ));
        }
        SvcIcon::Stop => {
            // Üres négyzet
            let sq = Rect::from_center_size(c, vec2(11.0, 11.0));
            p.add(Shape::closed_line(
                vec![sq.left_top(), sq.right_top(), sq.right_bottom(), sq.left_bottom()],
                stroke,
            ));
        }
        SvcIcon::Restart => {
            // Körív nyílheggyel (újraindítás): 40°-tól 320°-ig, óramutató járása szerint.
            let r = 6.5_f32;
            let mut pts = Vec::new();
            let mut a = 40.0_f32;
            while a <= 320.0 {
                let t = a.to_radians();
                pts.push(pos2(c.x + r * t.cos(), c.y + r * t.sin()));
                a += 15.0;
            }
            p.add(Shape::line(pts, stroke));
            let t = 320.0_f32.to_radians();
            let tip = pos2(c.x + r * t.cos(), c.y + r * t.sin());
            let tangent = vec2(-t.sin(), t.cos());
            let normal = vec2(t.cos(), t.sin());
            p.line_segment([tip, tip - tangent * 4.5 + normal * 3.5], stroke);
            p.line_segment([tip, tip - tangent * 4.5 - normal * 3.5], stroke);
        }
    }
    p.text(pos2(rect.left() + 12.0 + 16.0 + 8.0, cy), Align2::LEFT_CENTER, text, FontId::proportional(13.5), color);
    enabled && resp.clicked()
}

// ═══════════════════════════════════════════════════════════════════════════
// Folyamatok fül
// ═══════════════════════════════════════════════════════════════════════════

/// A Folyamatok fül egy sorához (csoport vagy gyerek folyamat) tartozó menü adatai.
pub(crate) struct ProcMenuSpec {
    label: String,
    pids: Vec<u32>,
    targets: Vec<(u32, String)>,
    exe: Option<String>,
    has_window: bool,
    /// (csoportkulcs, jelenleg le van-e nyitva) – csak a több tagú csoportoknál.
    expand: Option<(String, bool)>,
}

impl ProcMenuSpec {
    pub(crate) fn for_row(g: &Group, child: Option<&ProcInfo>, expanded: bool) -> Self {
        match child {
            Some(m) => Self {
                label: m.name.clone(),
                pids: vec![m.pid],
                targets: vec![(m.pid, m.name.clone())],
                exe: m.exe.clone(),
                has_window: m.has_window,
                expand: None,
            },
            None => Self {
                label: g.name.clone(),
                pids: g.members.iter().map(|m| m.pid).collect(),
                targets: g.members.iter().map(|m| (m.pid, m.name.clone())).collect(),
                exe: g.exe.clone(),
                has_window: g.members.iter().any(|m| m.has_window),
                expand: if g.members.len() > 1 { Some((g.key.clone(), expanded)) } else { None },
            },
        }
    }
}

impl ProcMenuSpec {
    /// A Felhasználók fül egy alkalmazás-sorához (egy munkamenet azonos nevű folyamatai).
    pub(crate) fn for_user_app(a: &UserApp) -> Self {
        Self {
            label: a.name.clone(),
            pids: a.targets.iter().map(|(pid, _)| *pid).collect(),
            targets: a.targets.clone(),
            exe: a.exe.clone(),
            has_window: a.has_window,
            expand: None,
        }
    }
}

pub(crate) fn process_menu(ui: &mut Ui, s: &ProcMenuSpec, out: &mut Option<MenuAction>) {
    ui.set_min_width(230.0);
    if let Some((key, open)) = &s.expand {
        let text = if *open { tr("Collapse") } else { tr("Expand") };
        if item(ui, text, true, true) {
            *out = Some(MenuAction::ToggleExpand(key.clone()));
        }
    }
    if item(ui, tr("Switch to"), s.has_window, false) {
        *out = Some(MenuAction::SwitchTo(s.pids.clone()));
    }
    if item(ui, tr("End task"), !s.targets.is_empty(), false) {
        *out = Some(MenuAction::EndTask { label: s.label.clone(), targets: s.targets.clone() });
    }
    ui.separator();
    if item(ui, tr("Go to details"), !s.pids.is_empty(), false) {
        *out = Some(MenuAction::GoToDetails(s.pids.iter().copied().min().unwrap_or(0)));
    }
    if item(ui, tr("Open file location"), s.exe.is_some(), false) {
        if let Some(p) = &s.exe {
            *out = Some(MenuAction::OpenLocation(p.clone()));
        }
    }
    if item(ui, tr("Search online"), true, false) {
        *out = Some(MenuAction::SearchOnline(s.label.clone()));
    }
    if item(ui, tr("Properties"), s.exe.is_some(), false) {
        if let Some(p) = &s.exe {
            *out = Some(MenuAction::Properties(p.clone()));
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Részletek fül
// ═══════════════════════════════════════════════════════════════════════════

pub(crate) fn details_menu(ui: &mut Ui, p: &ProcInfo, hosts_services: bool, out: &mut Option<MenuAction>) {
    ui.set_min_width(230.0);
    if item(ui, tr("End task"), true, false) {
        *out = Some(MenuAction::EndTask { label: p.name.clone(), targets: vec![(p.pid, p.name.clone())] });
    }
    if item(ui, tr("End process tree"), true, false) {
        *out = Some(MenuAction::EndTree { pid: p.pid, name: p.name.clone() });
    }
    ui.separator();
    ui.menu_button(tr("Set priority"), |ui| {
        let classes = [
            (tr("High"), shell::PRIORITY_HIGH),
            (tr("Above normal"), shell::PRIORITY_ABOVE_NORMAL),
            (tr("Normal"), shell::PRIORITY_NORMAL),
            (tr("Below normal"), shell::PRIORITY_BELOW_NORMAL),
            (tr("Low"), shell::PRIORITY_IDLE),
        ];
        for (label, class) in classes {
            if item(ui, label, true, false) {
                *out = Some(MenuAction::SetPriority { pid: p.pid, class, name: p.name.clone() });
            }
        }
    });
    if item(ui, tr("Set affinity"), true, false) {
        *out = Some(MenuAction::SetAffinity { pid: p.pid, name: p.name.clone() });
    }
    ui.separator();
    if item(ui, tr("Open file location"), p.exe.is_some(), false) {
        if let Some(e) = &p.exe {
            *out = Some(MenuAction::OpenLocation(e.clone()));
        }
    }
    if item(ui, tr("Search online"), true, false) {
        *out = Some(MenuAction::SearchOnline(p.name.clone()));
    }
    if item(ui, tr("Properties"), p.exe.is_some(), false) {
        if let Some(e) = &p.exe {
            *out = Some(MenuAction::Properties(e.clone()));
        }
    }
    if item(ui, tr("Go to service(s)"), hosts_services, false) {
        *out = Some(MenuAction::GoToServices(p.pid));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Szolgáltatások fül
// ═══════════════════════════════════════════════════════════════════════════

pub(crate) fn service_menu(ui: &mut Ui, s: &ServiceEntry, out: &mut Option<MenuAction>) {
    ui.set_min_width(230.0);
    let running = s.state.is_running();
    if item(ui, tr("Start"), s.state == ServiceState::Stopped, false) {
        *out = Some(MenuAction::ServiceControl { name: s.name.clone(), action: ServiceAction::Start });
    }
    if item(ui, tr("Stop"), running, false) {
        *out = Some(MenuAction::ServiceControl { name: s.name.clone(), action: ServiceAction::Stop });
    }
    if item(ui, tr("Restart"), running, false) {
        *out = Some(MenuAction::ServiceControl { name: s.name.clone(), action: ServiceAction::Restart });
    }
    ui.separator();
    if item(ui, tr("Open Services"), true, false) {
        *out = Some(MenuAction::OpenServicesConsole);
    }
    if item(ui, tr("Search online"), true, false) {
        let q = if s.display.is_empty() { s.name.clone() } else { s.display.clone() };
        *out = Some(MenuAction::SearchOnline(q));
    }
    if item(ui, tr("Go to details"), s.pid != 0, false) {
        *out = Some(MenuAction::GoToDetails(s.pid));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Közös fejléc-gombok és állapotsor
// ═══════════════════════════════════════════════════════════════════════════

/// A jobb oldali két gomb: "Új feladat futtatása" és "Folyamat bezárása" (a kijelölt elem nevével). A bezárás gomb
/// piros; több kijelölt folyamatnál "Összes bezárása (n)". Visszaadja: (bezárás kattintva, új feladat kattintva).
/// A hívónak már egy `ui.horizontal`-ban kell lennie: a gombok a sor jobb szélére igazodnak.
pub(crate) fn task_buttons(ui: &mut Ui, sel_label: &str, sel_count: usize) -> (bool, bool) {
    let mut kill = false;
    let mut new_task = false;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let text = if sel_count > 1 {
            format!("{} ({})", tr("End all"), sel_count)
        } else {
            tr("End process").to_string()
        };
        let btn = egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(DANGER_RED.gamma_multiply(0.85))
            .corner_radius(CornerRadius::same(8));
        if ui.add_enabled(sel_count > 0, btn).clicked() {
            kill = true;
        }
        let new_btn = egui::Button::new(tr("Run new task")).corner_radius(CornerRadius::same(8));
        if ui.add(new_btn).clicked() {
            new_task = true;
        }
        if !sel_label.is_empty() {
            // A kijelölt elem neve a gombok és a keresőmező közötti szabad helyre fér: ha hosszabb, "…"-ra rövidül
            // (nem lóg a keresőmezőbe), az egérrel fölé víve pedig a teljes név látszik.
            ui.add(egui::Label::new(RichText::new(sel_label).color(col_muted())).truncate())
                .on_hover_text(sel_label);
        }
    });
    (kill, new_task)
}

impl TaskApp {
    /// A Felhasználók fülön kijelölt alkalmazás: (név, a munkamenet ilyen nevű folyamatai).
    pub(crate) fn users_selection(&self) -> (String, Vec<(u32, String)>) {
        let Some((session, key)) = &self.tabs.usr_sel else { return Default::default() };
        let Some(g) = self.groups.iter().find(|g| &g.key == key) else { return Default::default() };
        let targets: Vec<(u32, String)> =
            g.members.iter().filter(|m| m.session_id == *session).map(|m| (m.pid, m.name.clone())).collect();
        if targets.is_empty() {
            return Default::default();
        }
        (g.name.clone(), targets)
    }

    /// A legutóbbi művelet eredménye (8 másodpercig látszik). Minden fülön ugyanígy jelenik meg.
    pub(crate) fn status_line(&self, ui: &mut Ui) {
        if let Some((msg, ok, at)) = &self.status {
            if at.elapsed() < Duration::from_secs(8) {
                ui.add_space(4.0);
                ui.label(RichText::new(msg).size(12.5).color(if *ok { OK_GREEN } else { DANGER_RED }));
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Ikonok
// ═══════════════════════════════════════════════════════════════════════════

/// Egy futtatható fájl ikonja (18×18) a megadott középpontban. A még nem ismert ikonokat a `want`-ba gyűjti,
/// hogy a hívó a rajzolás után kérhesse az ikon-szálról (ugyanaz a minta, mint a Folyamatok fülön).
pub(crate) fn draw_exe_icon(
    painter: &egui::Painter,
    icons: &HashMap<String, IconSlot>,
    exe: Option<&String>,
    center: Pos2,
    want: &mut Vec<String>,
) {
    let icon_rect = Rect::from_center_size(center, vec2(18.0, 18.0));
    match exe.and_then(|e| icons.get(e)) {
        Some(IconSlot::Ready(tex)) => painter.image(
            tex.id(),
            icon_rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        ),
        _ => painter.rect_filled(icon_rect.shrink(2.0), CornerRadius::same(4), overlay(28)),
    };
    if let Some(e) = exe {
        if !icons.contains_key(e) && !want.contains(e) {
            want.push(e.clone());
        }
    }
}

/// Egy fogaskerék: vastag gyűrű + körben a fogak.
fn draw_gear(p: &egui::Painter, c: Pos2, r: f32, teeth: usize, rot: f32, color: Color32) {
    p.circle_stroke(c, r * 0.72, Stroke::new(r * 0.5, color));
    for k in 0..teeth {
        let a = rot + k as f32 * std::f32::consts::TAU / teeth as f32;
        let (s, co) = a.sin_cos();
        let dir = vec2(co, s);
        let perp = vec2(-s, co);
        let inner = c + dir * (r * 0.85);
        let outer = c + dir * (r * 1.25);
        let (w_in, w_out) = (r * 0.28, r * 0.2);
        p.add(egui::Shape::convex_polygon(
            vec![inner - perp * w_in, inner + perp * w_in, outer + perp * w_out, outer - perp * w_out],
            color,
            Stroke::NONE,
        ));
    }
}

/// A szolgáltatások ikonja: két egymásba kapcsolódó fogaskerék (18×18 területen, `center` körül).
pub(crate) fn draw_gears(p: &egui::Painter, center: Pos2) {
    let color = if theme::is_dark() { Color32::from_rgb(150, 178, 210) } else { Color32::from_rgb(84, 112, 150) };
    draw_gear(p, center + vec2(-2.0, 2.5), 4.4, 8, 0.0, color);
    draw_gear(p, center + vec2(4.0, -4.0), 3.0, 6, 0.3, color);
}

// ═══════════════════════════════════════════════════════════════════════════
// Műveletek végrehajtása és az affinitás-ablak
// ═══════════════════════════════════════════════════════════════════════════

impl TaskApp {
    pub(crate) fn run_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::ToggleExpand(key) => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
            MenuAction::SwitchTo(pids) => {
                if !shell::focus_window_of_pids(&pids) {
                    self.set_status(tr("No visible window was found for this process.").to_string(), false);
                }
            }
            MenuAction::EndTask { label, targets } => {
                // Csoportnál megerősítést kérünk (egy kattintással sok folyamat halna meg) – ugyanúgy, mint a gombnál.
                if targets.len() > 1 {
                    self.confirm = Some((label, targets));
                } else {
                    self.send_kill(label, targets);
                }
            }
            MenuAction::EndTree { pid, name } => {
                let targets = self.process_tree(pid, &name);
                if targets.len() > 1 {
                    self.confirm = Some((name, targets));
                } else {
                    self.send_kill(name, targets);
                }
            }
            MenuAction::SetPriority { pid, class, name } => match shell::set_priority(pid, class) {
                Ok(()) => self.set_status(format!("{} {name}", tr("Priority set:")), true),
                Err(e) => self.set_status(format!("{name}: {e}"), false),
            },
            MenuAction::SetAffinity { pid, name } => match shell::get_affinity(pid) {
                Ok((mask, system_mask)) => {
                    self.tabs.affinity = Some(AffinityDialog { pid, name, mask, system_mask });
                }
                Err(e) => self.set_status(format!("{name}: {e}"), false),
            },
            MenuAction::OpenLocation(path) => {
                if let Err(e) = shell::reveal_in_explorer(&path) {
                    self.set_status(e, false);
                }
            }
            MenuAction::Properties(path) => {
                if let Err(e) = shell::show_properties(&path) {
                    self.set_status(e, false);
                }
            }
            MenuAction::SearchOnline(query) => {
                if let Err(e) = shell::search_online(&query) {
                    self.set_status(e, false);
                }
            }
            MenuAction::GoToDetails(pid) => {
                self.det_filter = pid.to_string();
                self.tabs.det_sel = Some(pid);
                self.tab = Tab::Details;
            }
            MenuAction::GoToServices(pid) => {
                self.tabs.sv_filter = pid.to_string();
                self.tab = Tab::Services;
            }
            MenuAction::ServiceControl { name, action } => {
                if let Some(l) = &self.lists {
                    let _ = l.cmd.send(lists::ListsCmd::ServiceControl { name, action });
                }
            }
            MenuAction::OpenServicesConsole => {
                if let Err(e) = shell::open_services_console() {
                    self.set_status(format!("{}: {e}", tr("Could not start the Services console")), false);
                }
            }
            MenuAction::ToggleUserExpand(id) => {
                if !self.expanded_users.remove(&id) {
                    self.expanded_users.insert(id);
                }
            }
            MenuAction::NewTask => {
                self.tabs.new_task = Some(NewTaskDialog::default());
            }
        }
    }

    /// Az "Új feladat futtatása" ablak + a háttérben futó indítás / tallózás eredményének átvétele.
    pub(crate) fn new_task_dialog(&mut self, ctx: &egui::Context) {
        // 1) Háttérből érkező eredmények (a ShellExecute UAC-kérésnél, a fájlválasztó pedig modálisan blokkol).
        let mut started: Option<Result<(), String>> = None;
        if let Some(rx) = &self.tabs.task_rx {
            match rx.try_recv() {
                Ok(r) => started = Some(r),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => started = Some(Err("megszakadt".to_string())),
            }
        }
        if let Some(r) = started {
            self.tabs.task_rx = None;
            match r {
                Ok(()) => self.set_status(tr("The new task has started.").to_string(), true),
                Err(e) => self.set_status(format!("{}: {e}", tr("The new task could not be started")), false),
            }
        }
        let mut browsed: Option<Option<String>> = None;
        if let Some(rx) = &self.tabs.browse_rx {
            match rx.try_recv() {
                Ok(p) => browsed = Some(p),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => browsed = Some(None),
            }
        }
        if let Some(p) = browsed {
            self.tabs.browse_rx = None;
            if let (Some(path), Some(d)) = (p, self.tabs.new_task.as_mut()) {
                // Szóközös útvonalat idézőjelbe teszünk, hogy a paraméterek szétválasztása ne törje el.
                d.command = if path.contains(' ') { format!("\"{path}\"") } else { path };
            }
        }

        // 2) Az ablak: önálló, natív ablak (mozgatható, jobb felső sarkában X gombbal), a címsora Mica-t kap.
        let browsing = self.tabs.browse_rx.is_some();
        let window_icon = self.window_icon.clone();
        let dark = theme::is_dark();
        let backdrop_pref = self.settings.backdrop;
        let solid_rgb = nav_solid_rgb();
        let title = tr("Create new task");
        let size = vec2(470.0, 226.0);
        let parent_rect = ctx.input(|i| i.viewport().outer_rect);
        let Some(d) = self.tabs.new_task.as_mut() else { return };
        if d.pos.is_none() {
            d.pos = Some(match parent_rect {
                Some(r) => r.center() - size / 2.0,
                None => pos2(240.0, 200.0),
            });
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_window_level(egui::WindowLevel::AlwaysOnTop)
            .with_transparent(true);
        if let Some(p) = d.pos {
            builder = builder.with_position(p);
        }
        if let Some(icon) = window_icon {
            builder = builder.with_icon(icon);
        }

        let mut run = false;
        let mut cancel = false;
        let mut browse = false;
        let mut mica_done = d.mica_done;
        let mut mica_tries = d.mica_tries;
        // A tartalom a (futtat, mégse, tallózás) jelzőket adja vissza, így nem kell közös változókat kölcsönöznie.
        let mut content = |ui: &mut Ui| -> (bool, bool, bool) {
            let (mut run, mut cancel, mut browse) = (false, false, false);
            ui.label(
                RichText::new(tr(
                    "Type the name of a program, folder, document or internet resource, and Windows will open it for you.",
                ))
                .color(col_muted()),
            );
            ui.add_space(8.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut d.command)
                    .hint_text(tr("e.g. notepad, cmd, C:\\Windows"))
                    .desired_width(f32::INFINITY),
            );
            if !d.focused {
                edit.request_focus();
                d.focused = true;
            }
            // Enter a szövegmezőben = OK.
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                run = true;
            }
            ui.add_space(6.0);
            ui.checkbox(&mut d.admin, tr("Create this task with administrator privileges"));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!browsing, egui::Button::new(tr("Browse…"))).clicked() {
                    browse = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                    if ui.add_enabled(!d.command.trim().is_empty(), egui::Button::new(tr("OK"))).clicked() {
                        run = true;
                    }
                });
            });
            (run, cancel, browse)
        };

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("new_task_dialog"),
            builder,
            |vctx, class| {
                let mut out = (false, false, false);
                if class == egui::ViewportClass::Embedded {
                    // Tartalék: ha a háttér nem tud külön natív ablakot nyitni, beágyazott ablakban jelenik meg.
                    let mut open = true;
                    egui::Window::new(title).open(&mut open).collapsible(false).resizable(false).show(vctx, |ui| {
                        out = content(ui);
                    });
                    if !open {
                        cancel = true;
                    }
                } else {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(theme::content_fill()).inner_margin(Margin::same(18)))
                        .show(vctx, |ui| {
                            out = content(ui);
                        });
                    // A natív X gomb (és az Alt+F4) bezárási kérést küld; az Esc is bezárja.
                    if vctx.input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                    // Mica a címsorban: az ablak HWND-jét címe alapján keressük meg (az első néhány frame-ben).
                    if !mica_done && mica_tries < 120 {
                        mica_tries += 1;
                        if let Some(h) = shell::find_own_window_by_title(title) {
                            backdrop::apply(h, dark, backdrop_pref, solid_rgb);
                            shell::frame_changed(h);
                            mica_done = true;
                        } else {
                            vctx.request_repaint();
                        }
                    }
                }
                run |= out.0;
                cancel |= out.1;
                browse |= out.2;
            },
        );
        d.mica_done = mica_done;
        d.mica_tries = mica_tries;

        if browse {
            let (tx, rx) = mpsc::channel::<Option<String>>();
            let c = ctx.clone();
            let _ = std::thread::Builder::new().name("browse".into()).spawn(move || {
                let _ = tx.send(shell::browse_for_file());
                c.request_repaint();
            });
            self.tabs.browse_rx = Some(rx);
        }
        if run {
            if let Some(d) = self.tabs.new_task.take() {
                let (command, admin) = (d.command, d.admin);
                let (tx, rx) = mpsc::channel::<Result<(), String>>();
                let c = ctx.clone();
                let _ = std::thread::Builder::new().name("new-task".into()).spawn(move || {
                    let r = std::panic::catch_unwind(|| shell::run_task(&command, admin))
                        .unwrap_or_else(|_| Err("panic".to_string()));
                    let _ = tx.send(r);
                    c.request_repaint();
                });
                self.tabs.task_rx = Some(rx);
            }
        } else if cancel {
            self.tabs.new_task = None;
        }
    }

    /// A folyamat és az összes leszármazottja (szülő-gyerek kapcsolat alapján), (PID, név) párokként.
    fn process_tree(&self, root: u32, root_name: &str) -> Vec<(u32, String)> {
        let all: Vec<&ProcInfo> = self.groups.iter().flat_map(|g| g.members.iter()).collect();
        let mut out = vec![(root, root_name.to_string())];
        let mut seen: HashSet<u32> = HashSet::new();
        seen.insert(root);
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            // Előbb összegyűjtjük a gyerekeket (a szűrő csak itt kölcsönzi a `seen`-t), és csak utána módosítjuk.
            let children: Vec<&&ProcInfo> =
                all.iter().filter(|c| c.parent_pid == parent && !seen.contains(&c.pid)).collect();
            for c in children {
                if seen.insert(c.pid) {
                    out.push((c.pid, c.name.clone()));
                    frontier.push(c.pid);
                }
            }
        }
        out
    }

    /// A Processzor-affinitás ablak (modális jellegű, középre igazítva).
    pub(crate) fn affinity_dialog(&mut self, ctx: &egui::Context) {
        let Some(d) = self.tabs.affinity.as_mut() else { return };
        let mut decision: Option<bool> = None;
        egui::Window::new(tr("Processor affinity"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("{}: {}", tr("Process"), d.name));
                ui.label(RichText::new(tr("Which processors may the process run on?")).color(col_muted()));
                ui.add_space(6.0);
                egui::Grid::new("affinity_grid").spacing([18.0, 6.0]).show(ui, |ui| {
                    let mut n = 0usize;
                    for i in 0..(usize::BITS as usize) {
                        let bit = 1usize << i;
                        if d.system_mask & bit == 0 {
                            continue;
                        }
                        let mut on = d.mask & bit != 0;
                        if ui.checkbox(&mut on, format!("CPU {i}")).changed() {
                            if on {
                                d.mask |= bit;
                            } else {
                                d.mask &= !bit;
                            }
                        }
                        n += 1;
                        if n % 4 == 0 {
                            ui.end_row();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("All")).clicked() {
                        d.mask = d.system_mask;
                    }
                    if ui.add_enabled(d.mask != 0, egui::Button::new(tr("OK"))).clicked() {
                        decision = Some(true);
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                if let Some(d) = self.tabs.affinity.take() {
                    match shell::set_affinity(d.pid, d.mask) {
                        Ok(()) => self.set_status(format!("{} {}", tr("Affinity set:"), d.name), true),
                        Err(e) => self.set_status(format!("{}: {e}", d.name), false),
                    }
                }
            }
            Some(false) => self.tabs.affinity = None,
            None => {}
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Háttéreffekt: indulás utáni "lökés" + önellenőrzés
// ═══════════════════════════════════════════════════════════════════════════

/// A tömör bal sáv színe (RGB); Tömör módban a címsor is ezt kapja, hogy egybeessen.
pub(crate) fn nav_solid_rgb() -> (u8, u8, u8) {
    let c = theme::nav_fill(false);
    (c.r(), c.g(), c.b())
}

impl TaskApp {
    /// Az átlátszó háttér (Mica / Acrylic) néha "fehéren" indul, és csak egy maximalizálás–visszaállítás után
    /// kerül érvénybe. Ezt automatikusan kezeljük, lépcsőzetesen:
    ///  1. amint az ablak látható: az effektus újraalkalmazása (a sötét/világos jelző átbillentésével), a kliensterület
    ///     fekete GDI-festése (az üveg = fekete), a keret újraszámoltatása és egy 1 képpontos átméretezés;
    ///  2. fél másodperc múlva MEGMÉRJÜK a bal sáv képernyőn látszó pixelét; ha még mindig fehér (sötét témában),
    ///  3. akkor ugyanazt tesszük, amit kézzel tennél: animáció nélküli maximalizálás, majd visszaállítás.
    /// A mérés eredménye a `%APPDATA%\TaskManager\backdrop.log` fájlba kerül (diagnosztika).
    pub(crate) fn drive_backdrop_fix(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(h) = self.hwnd else { return };
        let hw = h.0 as isize;

        // Az 1 képpontos átméretezés visszaállítása.
        if self.nudge_back {
            shell::resize_by(hw, -1);
            self.nudge_back = false;
            ctx.request_repaint();
            return;
        }

        let now = Instant::now();
        match self.fix_stage {
            0 => {
                let Some(n) = self.refresh_in else { return };
                if !shell::is_visible(hw) {
                    // Amíg az ablak nem látható, nincs mit frissíteni (az eframe az első frame után mutatja meg).
                    ctx.request_repaint_after(Duration::from_millis(100));
                    return;
                }
                if n > 0 {
                    self.refresh_in = Some(n - 1);
                    ctx.request_repaint();
                    return;
                }
                self.refresh_in = None;
                let rgb = nav_solid_rgb();
                // A sötét/világos jelző átbillentése kényszeríti a DWM-et az anyag újraértékelésére.
                backdrop::apply(hw, !dark, self.settings.backdrop, rgb);
                let eff = backdrop::apply(hw, dark, self.settings.backdrop, rgb);
                shell::hide_caption_text_and_icon(hw);
                // A GDI-fekete festés csak kiterjesztett keretnél ad üveget; különben tényleg feketére festené a sávot.
                if self.settings.backdrop.is_transparent() && backdrop::client_is_glass() {
                    backdrop::paint_client_black(hw);
                }
                shell::frame_changed(hw);
                if shell::resize_by(hw, 1) {
                    self.nudge_back = true;
                }
                backdrop::log(&format!(
                    "1. lépés: kért={:?} használt={:?} build={} sötét={dark}",
                    self.settings.backdrop,
                    eff,
                    backdrop::os_build()
                ));
                self.fix_stage = 1;
                self.fix_at = now + Duration::from_millis(500);
                ctx.request_repaint_after(Duration::from_millis(520));
            }
            1 | 3 => {
                if now < self.fix_at {
                    ctx.request_repaint_after(Duration::from_millis(60));
                    return;
                }
                let transparent = self.settings.backdrop.is_transparent();
                let luma = backdrop::sample_nav_luma(hw);
                let wrong = transparent && luma.is_some_and(|l| backdrop::luma_looks_wrong(l, dark));
                backdrop::log(&format!("ellenőrzés ({}): fényesség={luma:?} hibás={wrong}", self.fix_stage));
                if self.fix_stage == 1 && wrong && shell::is_visible(hw) && !shell::is_maximized(hw) {
                    // 3. lépés: maximalizálás, majd visszaállítás – animáció nélkül, hogy alig látsszon.
                    backdrop::set_transitions_enabled(hw, false);
                    shell::show_maximized(hw);
                    self.fix_stage = 2;
                    self.fix_at = now + Duration::from_millis(250);
                    ctx.request_repaint_after(Duration::from_millis(270));
                } else {
                    self.fix_stage = 0;
                }
            }
            2 => {
                if now < self.fix_at {
                    ctx.request_repaint_after(Duration::from_millis(60));
                    return;
                }
                shell::show_restored(hw);
                backdrop::set_transitions_enabled(hw, true);
                backdrop::log("3. lépés: maximalizálás + visszaállítás megtörtént");
                self.fix_stage = 3;
                self.fix_at = now + Duration::from_millis(500);
                ctx.request_repaint_after(Duration::from_millis(520));
            }
            _ => self.fix_stage = 0,
        }
    }
}
