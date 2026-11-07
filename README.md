<!--
## ☕ Support the Project

If you find this Task Manager useful and want to support its development, you can buy me a coffee or sponsor the project:

<a href="https://ko-fi.com/neoshinju"><img src="https://img.shields.io/badge/Ko--fi-Support%20Me-ff5e5b?logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
<a href="https://paypal.me/neoshinju"><img src="https://img.shields.io/badge/PayPal-Donate-blue?logo=paypal&logoColor=white" alt="PayPal"></a>
<a href="https://github.com/sponsors/NeoShinjuDev"><img src="https://img.shields.io/badge/GitHub%20Sponsors-Support-ea4aaa?logo=github-sponsors&logoColor=white" alt="GitHub Sponsors"></a>

---
-->
<p align="center">
  <img src="https://img.shields.io/badge/Language-Rust-orange?logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/Rust_Version-1.88+-orange?logo=rust" alt="Rust Version">
  <a href="https://github.com/NeoShinjuDev/taskmanager/blob/main/LICENSE"><img src="https://img.shields.io/badge/License-GPLv3-blue.svg" alt="License"></a>
  <a href="https://github.com/NeoShinjuDev/taskmanager/releases"><img src="https://img.shields.io/badge/Version-v0.7.0-yellow" alt="Version"></a>
  <img src="https://img.shields.io/badge/Status-Beta-orange" alt="Status">
  <img src="https://img.shields.io/badge/Maintenance-Active-green.svg" alt="Mainteance">
  <img src="https://img.shields.io/badge/Platform-Windows-blue.svg?logo=windows" alt="Platform">
  <img src="https://img.shields.io/badge/Architecture-x64-blue" alt="Architecture">
  <img src="https://img.shields.io/badge/Distribution-Portable-success" alt="Portable">
  <img src="https://img.shields.io/github/last-commit/NeoShinjuDev/taskmanager.svg" alt="Last Commit">
  <img src="https://img.shields.io/github/issues/NeoShinjuDev/taskmanager.svg" alt="Issues">
  <img src="https://img.shields.io/github/stars/NeoShinjuDev/taskmanager.svg" alt="Stars">
</p>

# 🖥️ Task Manager

A lightweight, crash-proof **Task Manager and Hardware Monitor** for Windows 11 (Windows 10 is supported with fewer visual effects), written in Rust with `egui` / `eframe`.

Its defining design goal is stability: every hardware query runs on a separate background thread and is wrapped in `catch_unwind`. If a driver call panics, hangs, or returns garbage, the UI keeps running, and the affected value simply stays empty. The interface thread never waits for hardware.

---

## ✨ At a glance

* A familiar Windows 11 Task Manager layout: navigation on the left, content on the right, one continuous material (Mica / Acrylic) across the title bar and the sidebar.
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
| **Processes** | Running applications and background processes, grouped by program (a browser with dozens of processes becomes one expandable row). Columns for CPU, memory, disk, network and GPU (plus type, status, vendor, PID, process name, command line and GPU engine on request); sorting; search by name or PID. Right-click actions: end task, end process tree, set priority, set processor affinity, open file location, properties, search online, go to Details / Services. A **Run new task** dialog (with an "as administrator" option) is available as well. Applications that have no icon (or whose icon cannot be loaded) get a clean generic application icon, like in Windows. |
| **Performance** | Tiles for CPU, memory, every disk, every network adapter and every GPU, each with a live mini graph. Selecting a tile opens a large graph with details. **CPU:** utilization, speed, process count, logical processors, vendor, uptime and per-core usage bars (at most two cores per row), and below them an info card with base speed, sockets, cores, logical processors, virtualization, Hyper-V support and L1 / L2 / L3 cache sizes. **Memory:** in use, available, committed, cached, paged / non-paged pool. **Disk:** active time, response time, and read / write speed on separate graphs or on one combined graph. **Network:** send / receive speed, adapter name, connection type, IPv4 / IPv6, link speed, upload and download on one graph or on two separate ones, and a *Network details* window with the adapter's detailed counters (right-click the graph). **GPU:** utilization (or the per-engine view with 3D, Copy, Video Encode and Video Decode graphs), dedicated memory (and optionally the shared GPU memory), temperature, hotspot, core / VRAM clocks, power, fan. The data blocks below the graphs sit in rounded cards and are responsive: three per row when there is room, two when the window gets narrower. Right-clicking a graph opens a menu: the CPU graph can show the overall utilization or one graph per NUMA node, the disk graphs can be separate or combined, the GPU can switch between the summary and the engine view, and every graph can be copied as text. The choices are saved. A button at the top right opens the Windows **Resource Monitor**. |
| **App history** | Cumulative resource usage of applications since monitoring started (CPU time, network, downloads, uploads, notifications), with a reset button. Network, downloads and uploads are measured per process while the ETW session runs (administrator rights); notifications are still simulated. |
| **Startup apps** | Items started with Windows (registry, startup folders and Microsoft Store / MSIX apps), shown by their program description like in Windows. Shortcut targets, Squirrel launchers (e.g. Discord's `Update.exe`) and Store apps (name, publisher and logo from the package manifest) are resolved, so vendor, icon and running state are known. *Properties* on a Store app opens its page in Windows Settings. A switch to enable or disable each of them, an estimated startup impact, optional columns for running state, disable time and command line, and the last BIOS time when the firmware exposes it. Changing machine-wide items needs administrator rights. |
| **Users** | Signed-in users (with their profile pictures) and the processes that belong to each user, in expandable rows, with CPU, memory, disk, network and GPU usage. The network column needs administrator rights (see below). Right-click the table header to choose more columns (session ID, session, client name, GPU engine); the default view is the previous one, and the choice is remembered. |
| **Details** | A flat, sortable list of all processes with PID, user name, architecture, working set, private memory, description and responsiveness. |
| **Services** | All Windows services with their state; start, stop and restart from the context menu or from the Start / Stop / Restart buttons at the top right (they act on the selected service). The button that opens the classic Services console is at the top right of the page. |
| **Tables** | In the Processes, Users, Details, App history and Startup apps tables, right-click the header to choose which columns are shown (Processes and Users also have a *Resource values* submenu: memory, disk and network as values or percentages), and drag the edge of a column header to change its width (double-click resets it). Text that does not fit is shortened with an ellipsis (hover it to see the full text in a bubble), columns are separated by thin lines, every column border can be dragged, and everything is remembered. The Services table keeps its fixed columns (widths can still be changed). |
| **Notification area** | Small meter icons with a tooltip: one for the CPU, or (setting *Multiple*) three for CPU, memory and GPU in the colors of the charts. Each is a rounded square that fills from the bottom; click it to bring the window to the front, right-click for *Open / Exit*. Off / CPU only / Multiple can be chosen in the settings. |
| **Settings** | Language, theme, background effect, "ask for administrator rights at startup", "always on top of other windows" (off by default), chart time window (30–120 s), sampling interval (0.25–5 s) and frame rate limit (V-Sync / 30 / 60 / 120 FPS, 60 by default). Settings are applied immediately and saved automatically. |
| **About** | Program information. |

---

## 🌡️ Hardware monitoring

The hardware layer is vendor independent and chosen at start-up:

* **CPU** – the vendor is detected with `CPUID`. AMD is fully supported, Intel has a stub for temperatures, and everything else falls back to a generic backend.
* **GPU** – adapters are enumerated through DXGI (PCI vendor ID). **AMD** uses PDH, DXGI, WDDM adapter performance (`D3DKMT`, the same source Windows' own Task Manager uses) and the optionally loaded ADL library (`atiadlxx.dll`, loaded at runtime, so no SDK is needed to build). **NVIDIA** has an NVML branch behind the optional `nvidia` feature (currently a stub). Intel and everything else use the generic WDDM backend.
* **CPU temperature** – Windows does not expose it without a kernel driver, and AMD Ryzen CPUs have no per-core sensors at all (only package / CCD temperatures). Without a driver the app shows what Windows itself provides: the ACPI thermal zones, listed as *Thermal zone 1, 2, …*. When there are none, the temperature is simply not shown instead of a made-up number. Reading the CPU sensors directly (AMD SMN, Intel MSR) needs a kernel driver and is not implemented yet.
* **Per-process network traffic** is measured with an ETW (Event Tracing for Windows) session on the `Microsoft-Windows-Kernel-Network` provider, which counts every TCP and UDP packet per owning process. Starting an ETW session needs administrator rights, so the Network column on the *Users* tab is filled only when the program runs as administrator (otherwise the tab says so and offers a "Restart as administrator" button). The session is stopped when the program exits. The Disk column is the sum of each process's I/O traffic.
* **Power usage** and its trend are *estimates*: a weighted sum of a process's CPU, GPU, disk and network activity, with the trend being a ~2-minute moving average (the Windows value comes from an energy model this program does not have). *CPU time (since start)* and *Disk I/O (since start)* on the Startup apps tab are the running process's totals since it started, not the boot-trace values the Windows Task Manager shows.
* **Metered / non-metered network** (App history): whether the internet connection is metered is asked through a short hidden `powershell.exe` call every two minutes; its traffic is then counted as metered or non-metered.
* **Memory** per process is the *private working set* (what the Windows Task Manager shows), read straight from the system's process list, so applications made of many processes (browsers, Electron apps) are not counted several times for their shared pages.

---

## 🎨 Appearance

### Themes and default colors
Light, dark, or follow the system.

### Windows accent color
The active-tab marker, selected rows, the selected Performance tile, switches and sliders use the accent color set in *Windows Settings → Personalization → Colors*. The app follows a change while it is running.

### Background effects
Choose in *Settings → Background effect* (*Automatic* picks the best one your system supports; options that do not exist on your Windows version are not listed):

* **Mica** (Windows 11) – the material of the title bar continues through the sidebar.
* **Acrylic** (Windows 10 / 11) – blurred, translucent.
* **Solid** – no transparency; the title bar color is matched to the sidebar. This is also the safe fallback.

On **Windows 10** the program draws its own title bar and window frame (minimize / maximize / close buttons, a thin outline, resizing from the edges). Windows 10 always paints the native title bar opaque, and its Acrylic would also cover the invisible resize border outside the visible frame, so the native frame cannot be used there. Windows 11 keeps the native frame. To get the native frame back on Windows 10 (for troubleshooting), start the program with the environment variable `TASKMANAGER_NATIVE_FRAME=1`.

Mica and Acrylic can occasionally turn white (for example after the display was off, after sleep, or when the system limits the app to save power). While the window is visible the program checks the sidebar every two seconds, and after waking up or regaining focus right away; if it is white in the dark theme, the effect is re-applied automatically (with growing pauses if that does not help). If the program is rendered in software, or the sidebar stays pure black, *Solid* is used for that session; the saved setting is not changed.

The content area is always solid. The window cannot be resized below **1000 × 640**, which is the smallest size at which all data fits comfortably.

---

## 🌍 Languages

The program ships with two built-in languages, **Hungarian** and **English**. Both are stored as JSON language files embedded in the executable (`assets/lang/hu.json`, `assets/lang/en.json`).

Any other language is loaded **dynamically** from:

```
%APPDATA%\Neo Softwares\Task Manager\Langs\
```

(the folder is created on first start; *Settings → Language → Open the language files folder* opens it).

**Adding a language**

1. Copy `hu.json` (or `en.json`) into the `Langs` folder and rename it, for example `de.json`.
2. Change `"code"` (for example `"de"` or `"pt-BR"`) and `"name"` (the name shown in the language list, written in that language).
3. Translate the **values** in `"strings"`. The **keys** are the program's English texts and must not be changed. Placeholders such as `{0}` and `{1}` are filled in by the program and must stay in the translation.
4. In *Settings → Language*, press **Reload language files** and select the new language.

Texts missing from a language file simply stay in English, so a partial translation works. The built-in language codes `hu` and `en` cannot be overridden. With the **Automatic** setting the app picks the first of the Windows display languages that has a language file, and falls back to English.

---

## 🗂️ Files and folders

| Path | Purpose |
|---|---|
| `%APPDATA%\Neo Softwares\Task Manager\settings.json` | Saved settings. |
| `%APPDATA%\Neo Softwares\Task Manager\Langs\` | Additional language files. |

---

## 🧱 Architecture in short

* **UI thread** – never blocks. It reads the newest snapshot with `try_lock`, so even a frozen driver cannot freeze the window. Old snapshots do not pile up in memory while the window is minimized.
* **`hw-monitor` thread** – CPU, memory, disk, network and GPU sampling (0.25–5 s, configurable). Every collector runs under `catch_unwind`, which is why `panic = "abort"` is deliberately **not** set.
* **`sys-lists` thread** – services and startup items; it works only for the tab that is visible, so slow SCM / registry calls never delay the sampling.
* **`icon-loader` thread** – application icons, low priority, requested only for visible rows.
* The process runs at *above normal* priority and its worker threads at lower priority.
* **Frame rate limit** – eframe redraws the window right after every mouse event on Windows, so quickly moving the mouse can saturate the CPU and GPU of a weak computer. *Settings → Frame rate limit* (60 FPS by default) makes the UI thread wait until the minimum time between two frames has passed; events that arrive meanwhile are handled together in the next frame. *Monitor refresh rate (V-Sync)* adds no limit of its own.

Source layout: `main.rs` (application, UI, hardware layer), `backdrop.rs` (DWM / window effects), `winframe.rs` (own title bar and frame on Windows 10), `theme.rs` (palette and Windows accent color), `i18n.rs` (language files), `settings.rs`, `about.rs`, `lists.rs`, `services.rs`, `startup.rs`, `apphistory.rs`, `menus.rs`, `tabs.rs`, `shell.rs`, `sysdata.rs`, `winreg.rs`.

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

This project is licensed under the **GNU General Public License v3.0 (GPLv3)**. 

This means you are free to run, study, share, and modify the software, as long as any derivative works are also distributed under the same GPLv3 license and their source code is made public. See the [LICENSE](LICENSE) file for more details.
