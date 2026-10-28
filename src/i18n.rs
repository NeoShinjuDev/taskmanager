/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of feladatkezelo. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) feladatkezelo része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */


//! Többnyelvűség: magyar / angol, automatikus rendszernyelv-felismeréssel.
//!
//! Működés: a forráskódban minden felirat a magyar szöveg, `tr("magyar szöveg")` hívásba csomagolva.
//! Angol nyelvnél a `tr` a szótárból (lent) keresi ki a fordítást; ha nincs találat, a magyar
//! szöveg marad. Új felirat felvétele: írd be a kódba `tr("...")`-ként, és vedd fel ide egy sort.
//! A jelenlegi nyelv egy globális, szálbiztos jelző, ezért a háttérszálak is használhatják.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LangPref {
    /// A Windows kijelzőnyelve alapján: magyar rendszernél magyar, minden máshol angol.
    Auto,
    Hu,
    En,
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// A Windows kijelzőnyelve magyar-e (a LANGID alsó 10 bitje az elsődleges nyelv, 0x0E = magyar).
pub fn system_is_hungarian() -> bool {
    // SAFETY: paraméter nélküli, mellékhatás-mentes Win32 hívás.
    let id = unsafe { GetUserDefaultUILanguage() };
    (id & 0x3FF) == 0x0E
}

/// A kiválasztott nyelv érvényesítése (automatikus esetén a rendszernyelv felismerésével).
pub fn apply(pref: LangPref) {
    let english = match pref {
        LangPref::Auto => !system_is_hungarian(),
        LangPref::Hu => false,
        LangPref::En => true,
    };
    ENGLISH.store(english, Ordering::Relaxed);
}

/// Igaz, ha jelenleg angol a felület nyelve.
pub fn en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Magyar felirat -> az aktuális nyelven. A bemenet statikus szöveg, így a kimenet is az.
pub fn tr(hu: &'static str) -> &'static str {
    if !en() {
        return hu;
    }
    match hu {
        // ── navigáció és állapot ──
        "Folyamatok" => "Processes",
        "Teljesítmény" => "Performance",
        "Alkalmazáselőzmények" => "App history",
        "Indítási alkalmazások" => "Startup apps",
        "Felhasználók" => "Users",
        "Részletek" => "Details",
        "Szolgáltatások" => "Services",
        "Beállítások" => "Settings",
        "Információ" => "About",
        "Feladatkezelő" => "Task Manager",
        "Hardvermonitor" => "Hardware monitor",
        "Háttérszál leállt" => "Background thread stopped",
        "Háttérszál újraindítása" => "Restart background thread",
        "Háttérszál újraindítva." => "Background thread restarted.",
        "Adatok betöltése…" => "Loading data…",
        "Nincs friss adat" => "No fresh data",
        "Élő adat" => "Live data",
        "Ez a nézet még fejlesztés alatt áll" => "This view is still under development",
        "Placeholder – ide kerül majd a tartalom." => "Placeholder – content will go here.",

        // ── folyamatok ──
        "Keresés név vagy PID alapján…" => "Search by name or PID…",
        "Folyamat bezárása" => "End process",
        "Összes bezárása" => "End all",
        "Név" => "Name",
        "Memória" => "Memory",
        "Megerősítés" => "Confirm",
        "Igen, bezárás" => "Yes, end",
        "Mégse" => "Cancel",
        "Ez a folyamat nem zárható be (rendszer / saját folyamat)." => {
            "This process cannot be ended (system / own process)."
        }
        "Kritikus rendszerfolyamat, nem zárható be:" => "Critical system process, cannot be ended:",
        "sikertelen" => "failed",

        // ── teljesítmény: közös ──
        "Valós idejű hardverdiagnosztika" => "Real-time hardware diagnostics",
        "Kihasználtság" => "Utilization",
        "Kihasználtság % – az utolsó 2 perc" => "Utilization % – last 2 minutes",
        "Sebesség" => "Speed",
        "Gyártó" => "Vendor",
        "Üzemidő" => "Uptime",
        "Forrás" => "Source",
        "Adatforrások" => "Data sources",
        "Folyamatok száma" => "Processes",

        // ── processzor ──
        "Processzor" => "Processor",
        "Logikai processzorok" => "Logical processors",
        "Magonkénti kihasználtság" => "Per-core utilization",
        "Mag" => "Core",
        "Hőmérséklet" => "Temperature",
        "Összesített hőmérséklet" => "Overall temperature",
        "Összesített" => "Overall",
        "ACPI hőzóna (PDH – Thermal Zone Information)" => "ACPI thermal zone (PDH – Thermal Zone Information)",

        // ── memória ──
        "Memóriahasználat" => "Memory usage",
        "Használatban (tömörített)" => "In use (compressed)",
        "Rendelkezésre álló" => "Available",
        "Lefoglalt" => "Committed",
        "Gyorsítótárazott" => "Cached",
        "Lapozható készlet" => "Paged pool",
        "Nem lapozható készlet" => "Non-paged pool",
        "Sebesség:" => "Speed:",
        "Használt bővítőhelyek:" => "Slots used:",
        "Méret:" => "Form factor:",
        "Hardverek számára fenntartva:" => "Hardware reserved:",

        // ── GPU ──
        "Dedikált GPU-memória használata" => "Dedicated GPU memory usage",
        "Dedikált memória" => "Dedicated memory",
        "Hőfok" => "Temperature",
        "GPU órajel" => "GPU clock",
        "VRAM órajel" => "VRAM clock",
        "Fogyasztás" => "Power",
        "Ventilátor" => "Fan",
        "A GPU nem található." => "GPU not found.",
        "Nem található videókártya (DXGI)." => "No graphics adapter found (DXGI).",
        "WDDM-perf aktív" => "WDDM perf active",
        "WDDM-perf nem elérhető" => "WDDM perf unavailable",
        "Gyártó-specifikus ág még nincs" => "No vendor-specific backend yet",
        "NVIDIA: NVML-ág csonk (fordítsd --features nvidia kapcsolóval)" => {
            "NVIDIA: NVML branch is a stub (build with --features nvidia)"
        }
        "ADL nem érhető el:" => "ADL unavailable:",
        "ADL: nem található AMD adapter" => "ADL: no AMD adapter found",
        "ADL: nem támogatott ezen a GPU-n" => "ADL: not supported on this GPU",

        // ── lemez ──
        "Aktív időtartam" => "Active time",
        "Lemez adatátviteli sebesség" => "Disk transfer rate",
        "Átlagos válaszidő" => "Average response time",
        "Olvasási sebesség" => "Read speed",
        "Írási sebesség" => "Write speed",
        "Kapacitás:" => "Capacity:",
        "Formázott:" => "Formatted:",
        "Rendszerlemez:" => "System disk:",
        "Lapozófájl:" => "Page file:",
        "Típus:" => "Type:",
        "Modell:" => "Model:",
        "Igen" => "Yes",
        "Nem" => "No",

        // ── hálózat ──
        "Átviteli sebesség" => "Throughput",
        "Küldés" => "Send",
        "Fogadás" => "Receive",
        "Adapter neve:" => "Adapter name:",
        "Kapcsolat típusa:" => "Connection type:",
        "IPv4-cím:" => "IPv4 address:",
        "IPv6-cím:" => "IPv6 address:",
        "Kapcsolat sebessége:" => "Link speed:",
        "Mobil" => "Cellular",

        // ── beállítások ──
        "Nyelv, téma és megjelenés" => "Language, theme and appearance",
        "Nyelv" => "Language",
        "Automatikus (rendszernyelv)" => "Automatic (system language)",
        "Téma" => "Theme",
        "Rendszer szerinti" => "Follow system",
        "Sötét" => "Dark",
        "Világos" => "Light",
        "Megjelenés" => "Appearance",
        "Oldalsáv: Mica effekt (áttetsző háttér)" => "Sidebar: Mica effect (translucent background)",
        "A beállítások automatikusan mentődnek." => "Settings are saved automatically.",
        "Grafikonok" => "Graphs",
        "Grafikon időintervalluma" => "Graph time window",
        "A grafikonok ennyi időt mutatnak vissza." => "How much time the graphs show back.",
        "Mintavételezési idő" => "Sampling interval",
        "Milyen gyakran frissülnek a mérések." => "How often the measurements refresh.",

        // ── információ ──
        "A program adatai" => "About this program",
        "Pehelykönnyű, fagyásbiztos feladatkezelő és hardvermonitor" => {
            "Lightweight, freeze-proof task manager and hardware monitor"
        }
        "Verzió:" => "Version:",
        "Fejlesztő:" => "Developer:",
        "Segítő:" => "Assisted by:",
        "Programozási nyelv:" => "Language:",
        "Fordító:" => "Compiler:",
        "Grafikus felület:" => "GUI toolkit:",
        "Fordítás dátuma:" => "Build date:",
        "Build típusa:" => "Build type:",
        "Célplatform:" => "Target:",

        // ── hibák, figyelmeztetések ──
        "A hardverfelismerés összeomlott." => "Hardware detection crashed.",
        "Egy hardverlekérdezés hibára futott, a következő ciklusban újrapróbáljuk." => {
            "A hardware query failed; retrying in the next cycle."
        }
        // ── beállítások (legördülők) ──
        "Oldalsáv háttere" => "Sidebar background",
        "Tömör" => "Solid",

        // ── felhasználók ──
        "Felhasználó" => "User",
        "Állapot" => "Status",
        "Lemez" => "Disk",
        "Hálózat" => "Network",
        "Fut" => "Running",
        "Csatlakoztatva" => "Connected",
        "Szétkapcsolva" => "Disconnected",
        "Tétlen" => "Idle",
        "Ismeretlen" => "Unknown",
        "Nincs bejelentkezett felhasználó." => "No signed-in user.",
        "A folyamatonkénti hálózati forgalomhoz rendszergazdai jogú ETW-munkamenet kell, ezért a Hálózat oszlop egyelőre nem elérhető. A Lemez érték a folyamatok I/O-forgalmának összege (a fájl-, eszköz- és hálózati műveleteket együtt méri)." => {
            "Per-process network traffic needs an elevated ETW session, so the Network column is not available yet. The Disk value is the sum of the processes' I/O traffic (file, device and network operations together)."
        }

        // ── oldalsáv, jogosultság ──
        "Mica-szerű (a címsorhoz igazítva)" => "Mica-like (matches the title bar)",
        "A bal sáv színe a Windows 11 Mica címsorához igazodik; a tartalmi rész mindig tömör." => {
            "The sidebar color matches the Windows 11 Mica title bar; the content area is always solid."
        }
        "Rendszergazdai jog kérése induláskor" => "Ask for administrator rights at startup",
        "A változás a következő indításkor lép életbe." => "The change takes effect on the next start.",
        "Jogosultság:" => "Privileges:",
        "Rendszergazda" => "Administrator",
        "Normál felhasználó" => "Standard user",

        // ── folyamatok: szakaszok; részletek ──
        "Alkalmazások" => "Apps",
        "Háttérfolyamatok" => "Background processes",
        "Felhasználónév" => "User name",
        "Memória (privát)" => "Memory (private)",
        "Munkakészlet" => "Working set",
        "Architektúra" => "Architecture",
        "Leírás" => "Description",
        "Nem válaszol" => "Not responding",
        "Keresés név, PID vagy felhasználó alapján…" => "Search by name, PID or user…",

        // ── alkalmazáselőzmények, indítási alkalmazások, szolgáltatások, időzítés ──
        "A HKLM-es és a közös mappás elemek kapcsolásához rendszergazdai jog szükséges." => "Administrator rights are required to switch HKLM and all-users folder items.",
        "A kapcsoláshoz rendszergazdai jog szükséges." => "Administrator rights are required to change this item.",
        "A szolgáltatáskezelő nem indítható" => "Could not start the Services console",
        "Adatok gyűjtése…" => "Collecting data…",
        "Alacsony" => "Low",
        "Alkalmazás neve" => "Name",
        "Az összes folyamat előzményei" => "Show history for all processes",
        "Csoport" => "Group",
        "Előzmények törlése" => "Delete usage history",
        "Engedélyezve" => "Enabled",
        "Hatása az indításra" => "Startup impact",
        "Hozzáférés megtagadva – rendszergazdai jog szükséges." => "Access denied – administrator rights required.",
        "Hálózat (MB)" => "Network (MB)",
        "Indítás…" => "Starting…",
        "Indítópult-mappa (felhasználó)" => "Startup folder (user)",
        "Indítópult-mappa (közös)" => "Startup folder (all users)",
        "Keresés alkalmazásnév alapján…" => "Search by app name…",
        "Keresés név, PID vagy leírás alapján…" => "Search by name, PID or description…",
        "Közepes" => "Medium",
        "Legutóbbi BIOS művelet ideje:" => "Last BIOS time:",
        "Letiltva" => "Disabled",
        "Leállítva" => "Stopped",
        "Leállítás…" => "Stopping…",
        "Magas" => "High",
        "Nem elérhető" => "Not available",
        "Nincs indítási elem." => "No startup items.",
        "Nincs mérve" => "Not measured",
        "Processzoridő" => "CPU time",
        "Registry-hiba" => "Registry error",
        "Rendszergazdai jog szükséges" => "Administrator rights required",
        "Szolgáltatáslista hiba" => "Service list error",
        "Szolgáltatások megnyitása" => "Open Services",
        "Szüneteltetve" => "Paused",
        "Értesítések (MB)" => "Notifications (MB)",
        "A processzoridő valós, a program indulása óta mért érték. A Hálózat és az Értesítések oszlop egyelőre szimulált adat (a folyamatonkénti forgalomhoz ETW-munkamenet kell); a struktúra kész a valós forrásra." => "CPU time is real, measured since the program started. The Network and Notifications columns are simulated for now (per-process traffic needs an ETW session); the structure is ready for a real source.",
        "Az alaplap UEFI-je nem adja át a BIOS-időt (a Windows Task Manager ilyenkor üresen hagyja)." => "The motherboard UEFI does not expose the BIOS time (Windows Task Manager leaves it blank in that case).",
        "A hatás becsült érték a már futó folyamat processzoridejéből és memóriájából; a nem futó elemeknél „Nincs mérve”." => "The impact is an estimate from the running process's CPU time and memory; items that are not running show “Not measured”.",
        "Grafikonok időintervalluma" => "Chart time window",
        "Nyelv, téma, megjelenés és időzítés" => "Language, theme, appearance and timing",
        "A rendszergazdai jog kérése a következő indításkor lép életbe; a grafikon-időintervallum és a mintavételezési idő azonnal érvényes." => "Asking for administrator rights takes effect on the next start; the chart time window and the sampling interval apply immediately.",

        // ── helyi menük ──
        "A szolgáltatás már fut." => "The service is already running.",
        "A szolgáltatás nem fut." => "The service is not running.",
        "A szolgáltatás nem válaszolt időben." => "The service did not respond in time.",
        "Affinitás beállítva:" => "Affinity set:",
        "Affinitás beállítása" => "Set affinity",
        "Ehhez a folyamathoz nem található látható ablak." => "No visible window was found for this process.",
        "Engedélyezés" => "Enable",
        "Feladat befejezése" => "End task",
        "Folyamat" => "Process",
        "Folyamatstruktúra leállítása" => "End process tree",
        "Fájl helyének megnyitása" => "Open file location",
        "Indítás" => "Start",
        "Kibontás" => "Expand",
        "Legalább egy processzort ki kell választani." => "Select at least one processor.",
        "Letiltás" => "Disable",
        "Leállítás" => "Stop",
        "Mely processzorokon futhat a folyamat?" => "Which processors may the process run on?",
        "Mind" => "All",
        "Más, futó szolgáltatások függnek ettől a szolgáltatástól." => "Other running services depend on this service.",
        "Normál" => "Normal",
        "Normál alatt" => "Below normal",
        "Normál felett" => "Above normal",
        "OK" => "OK",
        "Online keresés" => "Search online",
        "Prioritás beállítva:" => "Priority set:",
        "Prioritás beállítása" => "Set priority",
        "Processzor-affinitás" => "Processor affinity",
        "Részletek megjelenítése" => "Go to details",
        "Tulajdonságok" => "Properties",
        "Ugrás szolgáltatás(ok)ra" => "Go to service(s)",
        "Win32-hiba" => "Win32 error",
        "Átváltás" => "Switch to",
        "Érvénytelen paraméter." => "Invalid parameter.",
        "Összecsukás" => "Collapse",
        "Újraindítás" => "Restart",

        // ── új feladat, Mica-beállítás ──
        "A bal sáv átlátszó, így a Windows 11 Mica anyaga a címsorral egybefüggően látszik át rajta; a tartalmi rész mindig tömör. Ha a sáv fekete marad, válaszd a „Tömör” módot." => "The sidebar is transparent, so the Windows 11 Mica material shows through it, continuous with the title bar; the content area is always solid. If the sidebar stays black, choose “Solid”.",
        "A kérést megszakítottad." => "You cancelled the request.",
        "A rendszer nem találja a megadott fájlt." => "The system cannot find the specified file.",
        "Adj meg egy programot, mappát, dokumentumot vagy internetes erőforrást." => "Enter a program, folder, document or internet resource.",
        "Az új feladat elindult." => "The new task has started.",
        "Az új feladat nem indítható" => "The new task could not be started",
        "Feladat létrehozása rendszergazdai jogosultságokkal" => "Create this task with administrator privileges",
        "Mica (átlátszó, a címsorral egybefüggő)" => "Mica (transparent, continuous with the title bar)",
        "Minden fájl (*.*)" => "All files (*.*)",
        "Programok (*.exe)" => "Programs (*.exe)",
        "Tallózás" => "Browse",
        "Tallózás…" => "Browse…",
        "pl. notepad, cmd, C:\\Windows" => "e.g. notepad, cmd, C:\\Windows",
        "Új feladat futtatása" => "Run new task",
        "Új feladat létrehozása" => "Create new task",
        "Írd be a megnyitni kívánt program, mappa, dokumentum vagy internetes erőforrás nevét, a Windows megnyitja neked." => "Type the name of a program, folder, document or internet resource, and Windows will open it for you.",

        // ── információ ──
        "Minden jog fenntartva." => "All rights reserved.",

        // ── háttéreffektek ──
        "Háttéreffekt" => "Background effect",
        "Automatikus (a rendszerhez igazítva)" => "Automatic (matches your system)",
        "Mica (Windows 11)" => "Mica (Windows 11)",
        "Acrylic (Windows 10 / 11)" => "Acrylic (Windows 10 / 11)",
        "Aero Glass (klasszikus üveg)" => "Aero Glass (classic glass)",
        "Jelenleg használt háttéreffekt" => "Effect currently in use",
        "A bal sáv és a címsor azonos anyagot használ (Mica, Acrylic vagy Aero Glass), így egybefüggő felületet adnak; a tartalmi rész mindig tömör. Ha a sáv fehér vagy fekete marad, válaszd a „Tömör” módot." => "The sidebar and the title bar use the same material (Mica, Acrylic or Aero Glass), so they form one continuous surface; the content area is always solid. If the sidebar stays white or black, choose “Solid”.",

        _ => hu,
    }
}
