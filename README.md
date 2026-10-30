# 🖥️ Task Manager (v0.5.4-beta)

A lightweight, crash-proof **Task Manager and Hardware Monitor** for Windows 11 (Windows 10 is supported with fewer visual effects), written in Rust with `egui` / `eframe`.

Its defining design goal is stability: every hardware query runs on a separate background thread and is wrapped in `catch_unwind`. If a driver call panics, hangs, or returns garbage, the UI keeps running, and the affected value simply stays empty. The interface thread never waits for hardware.

---

## ✨ At a glance

* A familiar Windows 11 Task Manager layout: navigation on the left, content on the right, one continuous material (Mica / Acrylic / Aero glass) across the title bar and the sidebar.
* Live CPU, memory, disk, network and GPU monitoring with per-resource graphs.
* Grouped process list with the usual actions (end task, end process tree, priority, affinity, …).
* Startup apps, services, users, app history and a flat "Details" list.
* Dark / light / system theme, using the **Windows accent color** for selections.
* **Languages are loaded from files**: Hungarian and English are built in, any other language can be added by dropping a JSON file into a folder, with no rebuild.
* No dependency on `explorer.exe`: the app is a plain window with DWM, PDH, DXGI and Win32 calls.

---

## 📑 What is in each tab

| Tab | What it does |
|---|---|
| **Processes** | Running applications and background processes, grouped by program (a browser with dozens of processes becomes one expandable row). Columns for CPU, memory, GPU and disk; sorting; search by name, PID or user. Right-click actions: end task, end process tree, set priority, set processor affinity, open file location, properties, search online, go to Details / Services. A **Run new task** dialog (with an "as administrator" option) is available as well. |
| **Performance** | Tiles for CPU, memory, every disk, every network adapter and every GPU, each with a live mini graph. Selecting a tile opens a large graph with details. **CPU:** utilization, speed, process count, logical processors, vendor, uptime and per-core usage bars (at most two cores per row). **Memory:** in use, available, committed, cached, paged / non-paged pool. **Disk:** active time, response time, read / write speed. **Network:** send / receive speed, adapter name, connection type, IPv4 / IPv6, link speed. **GPU:** utilization, dedicated memory, temperature, hotspot, core / VRAM clocks, power, fan. The info blocks below the graphs are responsive: three per row when there is room, two when the window gets narrower. |
| **App history** | Cumulative resource usage of applications since monitoring started, with a reset button. |
| **Startup apps** | Items started with Windows (registry and startup folders), a switch to enable or disable each of them, an estimated startup impact, and the last BIOS time when the firmware exposes it. Changing machine-wide items needs administrator rights. |
| **Users** | Signed-in users (with their profile pictures) and the processes that belong to each user, in expandable rows. |
| **Details** | A flat, sortable list of all processes with PID, user name, architecture, working set, private memory, description and responsiveness. |
| **Services** | All Windows services with their state; start, stop and restart from the context menu, and a shortcut to the classic Services console. |
| **Settings** | Language, theme, background effect, "ask for administrator rights at startup", chart time window (30–120 s) and sampling interval (0.25–5 s). Settings are applied immediately and saved automatically. |
| **About** | Program information. |

---

## 🌡️ Hardware monitoring

The hardware layer is vendor independent and chosen at start-up:

* **CPU** – the vendor is detected with `CPUID`. AMD is fully supported, Intel has a stub for temperatures, and everything else falls back to a generic backend.
* **GPU** – adapters are enumerated through DXGI (PCI vendor ID). **AMD** uses PDH, DXGI, WDDM adapter performance (`D3DKMT`, the same source Windows' own Task Manager uses) and the optionally loaded ADL library (`atiadlxx.dll`, loaded at runtime, so no SDK is needed to build). **NVIDIA** has an NVML branch behind the optional `nvidia` feature (currently a stub). Intel and everything else use the generic WDDM backend.
* **CPU temperature** – Windows does not expose it without a kernel driver, and AMD Ryzen CPUs have no per-core sensors at all (only package / CCD temperatures). The app therefore reads it from **LibreHardwareMonitor's remote web server**: run LibreHardwareMonitor as administrator and enable *Options → Remote Web Server → Run* (port 8085). When it is not available, the app explains why instead of showing a made-up number.
* **Per-process network traffic** needs an elevated ETW session, so the Network column is not available yet. The Disk column is the sum of each process's I/O traffic.

---

## 🎨 Appearance

### Themes and default colors
Light, dark, or follow the system.

### Windows accent color
The active-tab marker, selected rows, the selected Performance tile, switches and sliders use the accent color set in *Windows Settings → Personalization → Colors*. The app follows a change while it is running.

### Background effects
Choose in *Settings → Background effect*:

* **Mica** (Windows 11) – the material of the title bar continues through the sidebar.
* **Acrylic** (Windows 10 / 11) – blurred, translucent.
* **Aero Glass** – classic blurred glass tinted with the accent color. Windows 11 has no real Aero, so several methods are tried automatically: the app measures the pixels on screen and moves on to the next method if one shows black, finally falling back to the system acrylic. The title bar is tinted to match the glass.
* **Solid** – no transparency; the title bar color is matched to the sidebar. This is also the safe fallback.

The content area is always solid. The window cannot be resized below **1000 × 640**, which is the smallest size at which all data fits comfortably.

---

## 🌍 Languages

The program ships with two built-in languages, **Hungarian** and **English**. Both are stored as JSON language files embedded in the executable (`assets/lang/hu.json`, `assets/lang/en.json`).

Any other language is loaded **dynamically** from:

```
%APPDATA%\Feladatkezelo\Langs\
```

(the folder is created on first start; *Settings → Language → Open the language files folder* opens it).

**Adding a language**

1. Copy `hu.json` (or `en.json`) into the `Langs` folder and rename it, for example `de.json`.
2. Change `"code"` (for example `"de"` or `"pt-BR"`) and `"name"` (the name shown in the language list, written in that language).
3. Translate the **values** in `"strings"`. The **keys** are the program's original Hungarian texts and must not be changed. Placeholders such as `{0}` and `{1}` are filled in by the program and must stay in the translation.
4. In *Settings → Language*, press **Reload language files** and select the new language.

Texts missing from a language file simply stay in Hungarian, so a partial translation works. The built-in language codes `hu` and `en` cannot be overridden. With the **Automatic** setting the app picks the first of the Windows display languages that has a language file, and falls back to English.

---

## 🧱 Architecture in short

* **UI thread** – never blocks. It reads the newest snapshot with `try_lock`, so even a frozen driver cannot freeze the window. Old snapshots do not pile up in memory while the window is minimized.
* **`hw-monitor` thread** – CPU, memory, disk, network and GPU sampling (0.25–5 s, configurable). Every collector runs under `catch_unwind`, which is why `panic = "abort"` is deliberately **not** set.
* **`sys-lists` thread** – services and startup items; it works only for the tab that is visible, so slow SCM / registry calls never delay the sampling.
* **`icon-loader` thread** – application icons, low priority, requested only for visible rows.
* The process runs at *above normal* priority and its worker threads at lower priority.

Source layout: `main.rs` (application, UI, hardware layer), `backdrop.rs` (DWM / window effects), `theme.rs` (palette and Windows accent color), `i18n.rs` (language files), `settings.rs`, `about.rs`, `lists.rs`, `services.rs`, `startup.rs`, `apphistory.rs`, `menus.rs`, `tabs.rs`, `shell.rs`, `sysdata.rs`, `winreg.rs`.

---

## 🛠️ Technologies

* **Language:** Rust (edition 2021, MSRV 1.88)
* **GUI:** `egui` + `eframe` 0.33 (intentionally pinned: later versions replace `App::update` and change the panel API)
* **System:** `sysinfo`, `windows-rs` (DWM, DXGI, Performance counters, Threading), `libloading` (AMD ADL), `serde_json`, `image`
* **Graphics backend:** `wgpu` (DirectX 12 / Vulkan, default) or `glow` (OpenGL)

---

## 🚀 Build & run

The app uses low-level Win32 APIs and native graphics, so building on Windows is required.

```bash
# Default: wgpu (DirectX 12 / Vulkan)
cargo run --release

# Fallback: OpenGL (use it if the window is not transparent / Mica or Acrylic do not appear)
cargo run --release --no-default-features --features gpu-glow

# NVIDIA NVML support
cargo run --release --features nvidia
```

Release profile (maximum speed, minimum size):

```toml
opt-level = 3        # maximum code optimization
lto = "fat"          # full link-time optimization
codegen-units = 1    # maximum optimization width
strip = true         # remove debug info and symbols
```

---

## 📄 License

> [!WARNING]
> **Custom Source-Available License**
> This project is protected by copyright law. It is free for personal, private, and educational use. However, commercial use, revenue generation (including SaaS, ads, or donations), public distribution of unmodified code, and public sharing of forks without prior written consent are **strictly prohibited**.
> Read the full [LICENSE](LICENSE) file in the root directory for legal terms.
