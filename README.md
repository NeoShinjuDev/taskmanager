# 🖥️ feladatkezelo (vB.0.4.2)

**EN:** A lightweight, crash-proof Task Manager and Hardware Monitor for Windows systems. A unique feature of the application is that potential errors or panics in the hardware query modules will not crash the entire program, guaranteeing maximum stability and continuous execution.

**HU:** Pehelykönnyű, fagyásbiztos Feladatkezelő és Hardvermonitor Windows rendszerekre. Az alkalmazás egyedi jellemzője, hogy a hardverlekérdező modulok esetleges hibái vagy panikjai nem rántják magukkal a teljes programot, így garantálva a maximális stabilitást és folyamatos futást.

---

## ✨ Features / Főbb funkciók

### English
* **Process Management:** List running processes, set priorities, and force terminate tasks (Win32 API / `TerminateProcess`).
* **Resource Monitoring:** Real-time tracking of CPU, RAM, and disk usage (`sysinfo` and PDH counters).
* **GPU Monitoring:** Integrated AMD ADL (`atiadlxx.dll`) runtime loading and optional NVIDIA NVML support.
* **Modern Windows 11 UI:** `egui` and `eframe` based graphical interface with hardware acceleration and native DWM hívások hívások (Acrylic / Mica effects).
* **External Integration:** LibreHardwareMonitor web-bridge support for accurate CPU temperature readings.

### Magyar
* **Folyamatkezelés:** Futó folyamatok listázása, prioritások állítása és kényszerített leállítása (Win32 API / `TerminateProcess`).
* **Erőforrás-figyelés:** CPU, RAM és lemezhasználat valós idejű monitorozása (`sysinfo` és PDH számlálók).
* **GPU Monitorozás:** Integrált AMD ADL (`atiadlxx.dll`) futásidejű betöltés és opcionális NVIDIA NVML támogatás.
* **Modern Windows 11 UI:** `egui` és `eframe` alapú grafikus felület hardveres gyorsítással, natív DWM hívásokkal (Acrylic / Mica effektusok).
* **Külső integráció:** LibreHardwareMonitor web-híd támogatás a CPU hőfokok pontos kijelzéséhez.

---

## 🛠️ Technologies Used / Alkalmazott technológiák

* **Language / Nyelv:** Rust (Edition 2021, MSRV: 1.88)
* **GUI Framework / Grafikus keretrendszer:** `egui` + `eframe` (v0.33 intentionally pinned due to the App::update architecture / szándékosan rögzítve az App::update architektúra miatt)
* **System API / Rendszer API:** `sysinfo`, `windows-rs` (Win32 DWM, DXGI, Performance, Threading)
* **Graphics Backend / Grafikus Backend:** `wgpu` (DirectX 12 / Vulkan thread/szál) or/vagy `glow` (OpenGL)

---

## 🚀 Build & Run / Fordítás és Futtatás

**EN:** Since the application uses low-level Win32 API calls and native graphics, a Windows environment is highly recommended for building and running.

**HU:** Mivel az alkalmazás alacsony szintű Win32 API hívásokat és natív grafikát használ, a futtatáshoz Windows környezet ajánlott.

### Default start / Alapértelmezett indítás (wgpu - DX12/Vulkan):
```bash
cargo run --release
```

### Fallback mode / Tartalék mód (OpenGL):
```bash
# Use this if wgpu window management or Acrylic/Mica effects fail to load
# Használd ezt, ha a wgpu ablakkezelés vagy az Acrylic/Mica effektek nem működnek
cargo run --release --no-default-features --features gpu-glow
```

### Enable NVIDIA NVML support / NVIDIA NVML támogatás bekapcsolása:
```bash
cargo run --release --features nvidia
```

---

## 📦 Profile Settings / Profil Beállítások (Release)

**EN:** For maximum performance and minimal binary size, the project utilizes the following optimizations during release builds:
**HU:** A maximális teljesítmény és a minimális méret érdekében a projekt az alábbi optimalizációkat használja a release build során:

```toml
opt-level = 3        # Maximum code optimization / Maximális kódoptimalizálás
lto = "fat"          # Full Link-Time Optimization / Teljes Link-Time Optimization
codegen-units = 1    # Maximum optimization width / Maximális optimalizációs szélesség
strip = true         # Remove all debug info and symbols / Debug információk és szimbólumok eltávolítása
```

---

## 📄 License / Licenc

> [!WARNING]
> **EN: Custom Source-Available License**
> This project is protected by copyright law. It is free for personal, private, and educational use. However, commercial use, revenue generation (including SaaS, ads, or donations), public distribution of unmodified code, and public sharing of forks without prior written consent are **strictly prohibited**.
> Read the full [LICENSE](LICENSE) file in the root directory for legal terms.

> [!CAUTION]
> **HU: Egyéni Forrás-Elérhetőségű Licenc**
> Ez a projekt szerzői jogi védelem alatt áll. Magán-, saját és oktatási célra ingyenesen használható. A kereskedelmi felhasználás, a bármilyen formájú bevételszerzés (SaaS, reklámok, donációk), az eredeti kód, valamint a módosított változatok (Forkok) engedély nélküli nyilvános terjesztése **szigorúan tilos**.
> A teljes jogi szövegért olvassa el a gyökérmappában található [LICENSE.hu](LICENSE.hu) fájlt.
