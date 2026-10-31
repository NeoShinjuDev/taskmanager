/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Lemezek, hálózati adapterek és a CPU-hőfok LibreHardwareMonitor-híd.
//! Mindegyik a `hw-monitor` háttérszálon fut; a hibák nem végzetesek (üres lista / `None`).

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::core::w;
use windows::Win32::System::Performance::PDH_HCOUNTER;

use crate::i18n::tr;
use crate::{CpuTempSource, PdhQuery, TempReading};

// ═══════════════════════════════════════════════════════════════════════════
// Nyers Win32 FFI (saját, C-kompatibilis szignatúrákkal)
// ═══════════════════════════════════════════════════════════════════════════

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    fn DeviceIoControl(
        h: isize,
        code: u32,
        inbuf: *const c_void,
        insz: u32,
        outbuf: *mut c_void,
        outsz: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    #[link_name = "CloseHandle"]
    fn CloseHandleRaw(h: isize) -> i32;
    fn GetDiskFreeSpaceExW(path: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
}

#[link(name = "iphlpapi")]
extern "system" {
    fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut c_void,
        addrs: *mut u8,
        size: *mut u32,
    ) -> u32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn u32le(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

// ═══════════════════════════════════════════════════════════════════════════
// Lemezek
// ═══════════════════════════════════════════════════════════════════════════

/// A lemez nem változó adatai (modell, kapacitás, típus).
#[derive(Clone, Debug, Default)]
pub struct DiskStatic {
    pub index: u32,
    pub model: String,
    pub capacity_bytes: u64,
    /// Pl. "SSD (NVMe)", "HDD (SATA)".
    pub kind: String,
}

/// A lemez pillanatnyi adatai.
#[derive(Clone, Debug, Default)]
pub struct DiskTelemetry {
    pub index: u32,
    /// Meghajtóbetűk, pl. "C: D:".
    pub letters: String,
    pub active_pct: f32,
    pub resp_ms: f32,
    pub read_bps: f64,
    pub write_bps: f64,
    pub formatted_bytes: u64,
    pub is_system: bool,
    pub has_pagefile: bool,
}

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
/// FILE_ANY_ACCESS: nulla hozzáférési joggal nyitott kezelővel is működik (a LENGTH_INFO FILE_READ_ACCESS-t kér, ezért
/// a `CreateFileW(.., 0, ..)`-lal nyitott kezelőn hozzáférés-megtagadással elbukik, és a kapacitás 0 B maradna).
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;

fn ioctl(h: isize, code: u32, input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut returned = 0u32;
    // SAFETY: a puffer-mutatók és a méretek érvényesek a hívás idejére.
    let ok = unsafe {
        DeviceIoControl(
            h,
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            out.as_mut_ptr().cast(),
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(returned as usize)
}

/// STORAGE_PROPERTY_QUERY: tulajdonság-azonosító (0 = eszközleíró, 7 = keresési büntetés), standard lekérdezés.
fn storage_query(property_id: u32) -> [u8; 12] {
    let mut q = [0u8; 12];
    q[0..4].copy_from_slice(&property_id.to_le_bytes());
    q
}

fn c_string_at(buf: &[u8], off: usize) -> String {
    if off == 0 || off >= buf.len() {
        return String::new();
    }
    let end = buf[off..].iter().position(|&b| b == 0).map_or(buf.len(), |p| off + p);
    String::from_utf8_lossy(&buf[off..end]).trim().to_string()
}

fn bus_name(bus: u32) -> &'static str {
    match bus {
        1 => "SCSI",
        3 => "ATA",
        7 => "USB",
        8 => "RAID",
        0x0A => "SAS",
        0x0B => "SATA",
        0x0C => "SD",
        0x0D => "MMC",
        0x11 => "NVMe",
        _ => "",
    }
}

/// Egy fizikai meghajtó adatai a `\\.\PhysicalDriveN` eszközről. A nulla hozzáférési jog elég
/// a lekérdezéshez, ezért nem kell rendszergazdai jog.
fn query_physical_drive(index: u32) -> Option<DiskStatic> {
    let path = wide(&format!("\\\\.\\PhysicalDrive{index}"));
    // SAFETY: a path NUL-terminált; a 3 = FILE_SHARE_READ|WRITE, a 3 = OPEN_EXISTING.
    let h = unsafe { CreateFileW(path.as_ptr(), 0, 3, std::ptr::null_mut(), 3, 0, 0) };
    if h == -1 || h == 0 {
        return None;
    }
    let mut st = DiskStatic { index, ..Default::default() };

    // Eszközleíró: gyártó, termék, busz. (Eltolások: STORAGE_DEVICE_DESCRIPTOR.)
    let mut buf = vec![0u8; 1024];
    let mut bus = 0u32;
    if let Some(n) = ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &storage_query(0), &mut buf) {
        if n >= 36 {
            let vendor = c_string_at(&buf, u32le(&buf, 12) as usize);
            let product = c_string_at(&buf, u32le(&buf, 16) as usize);
            bus = u32le(&buf, 28);
            st.model = format!("{vendor} {product}").trim().to_string();
        }
    }
    // Keresési büntetés: igen = forgó lemez (HDD), nem = SSD.
    let mut sbuf = vec![0u8; 64];
    let mut seek: Option<bool> = None;
    if let Some(n) = ioctl(h, IOCTL_STORAGE_QUERY_PROPERTY, &storage_query(7), &mut sbuf) {
        if n >= 9 {
            seek = Some(sbuf[8] != 0);
        }
    }
    // Kapacitás: elsőként a DISK_GEOMETRY_EX (a DiskSize mező a 24. bájttól; ez nulla hozzáférési joggal is megy),
    // tartaléknak a LENGTH_INFO (ha a kezelő olvasási joggal nyílt volna).
    let mut gbuf = [0u8; 256];
    if let Some(n) = ioctl(h, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut gbuf) {
        if n >= 32 {
            let mut size = [0u8; 8];
            size.copy_from_slice(&gbuf[24..32]);
            st.capacity_bytes = u64::from_le_bytes(size);
        }
    }
    if st.capacity_bytes == 0 {
        let mut lbuf = [0u8; 8];
        if ioctl(h, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut lbuf).is_some() {
            st.capacity_bytes = u64::from_le_bytes(lbuf);
        }
    }
    // SAFETY: a handle-t mi nyitottuk.
    unsafe {
        CloseHandleRaw(h);
    }
    let media = match seek {
        Some(true) => "HDD",
        Some(false) => "SSD",
        None => "",
    };
    let bus = bus_name(bus);
    st.kind = match (media.is_empty(), bus.is_empty()) {
        (false, false) => format!("{media} ({bus})"),
        (false, true) => media.to_string(),
        (true, false) => bus.to_string(),
        (true, true) => String::new(),
    };
    Some(st)
}

/// "0 C: D:" -> (0, "C: D:"); a "_Total" példányt kihagyja.
fn parse_disk_instance(name: &str) -> Option<(u32, String)> {
    let mut it = name.splitn(2, ' ');
    let idx = it.next()?.parse().ok()?;
    Some((idx, it.next().unwrap_or("").trim().to_string()))
}

fn volume_total(cache: &mut HashMap<String, u64>, letter: &str) -> u64 {
    if let Some(v) = cache.get(letter) {
        return *v;
    }
    let p = wide(&format!("{letter}\\"));
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: a path NUL-terminált, a kimenetek érvényesek.
    let ok = unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut avail, &mut total, &mut free) };
    let t = if ok != 0 { total } else { 0 };
    cache.insert(letter.to_string(), t);
    t
}

pub struct DiskReader {
    q: Option<PdhQuery>,
    idle: Option<PDH_HCOUNTER>,
    resp: Option<PDH_HCOUNTER>,
    read: Option<PDH_HCOUNTER>,
    write: Option<PDH_HCOUNTER>,
    page: Option<PDH_HCOUNTER>,
    sizes: HashMap<String, u64>,
    system_letter: String,
}

impl DiskReader {
    pub fn new() -> Self {
        let mut r = Self {
            q: None,
            idle: None,
            resp: None,
            read: None,
            write: None,
            page: None,
            sizes: HashMap::new(),
            system_letter: std::env::var("SystemDrive")
                .unwrap_or_else(|_| "C:".to_string())
                .to_uppercase(),
        };
        if let Some(q) = PdhQuery::open() {
            r.idle = q.add(w!("\\PhysicalDisk(*)\\% Idle Time"));
            r.resp = q.add(w!("\\PhysicalDisk(*)\\Avg. Disk sec/Transfer"));
            r.read = q.add(w!("\\PhysicalDisk(*)\\Disk Read Bytes/sec"));
            r.write = q.add(w!("\\PhysicalDisk(*)\\Disk Write Bytes/sec"));
            r.page = q.add(w!("\\Paging File(*)\\% Usage"));
            q.collect(); // alapmérés
            r.q = Some(q);
        }
        r
    }

    /// A fizikai meghajtók felsorolása (PhysicalDrive0..31).
    pub fn enumerate_static() -> Vec<DiskStatic> {
        (0..32).filter_map(query_physical_drive).collect()
    }

    pub fn sample(&mut self, statics: &[DiskStatic]) -> Vec<DiskTelemetry> {
        let Some(q) = self.q.as_mut() else { return Vec::new() };
        if !q.collect() {
            return Vec::new();
        }
        let idle = self.idle.map(|c| q.read_array(c)).unwrap_or_default();
        let resp = self.resp.map(|c| q.read_array(c)).unwrap_or_default();
        let read = self.read.map(|c| q.read_array(c)).unwrap_or_default();
        let write = self.write.map(|c| q.read_array(c)).unwrap_or_default();
        let page = self.page.map(|c| q.read_array(c)).unwrap_or_default();

        let mut letters_by_idx: HashMap<u32, String> = HashMap::new();
        let mut idle_by_idx: HashMap<u32, f64> = HashMap::new();
        for (name, v) in &idle {
            if let Some((i, letters)) = parse_disk_instance(name) {
                letters_by_idx.insert(i, letters);
                idle_by_idx.insert(i, *v);
            }
        }
        let by_idx = |arr: &[(String, f64)]| -> HashMap<u32, f64> {
            arr.iter()
                .filter_map(|(n, v)| parse_disk_instance(n).map(|(i, _)| (i, *v)))
                .collect()
        };
        let (resp_by, read_by, write_by) = (by_idx(&resp), by_idx(&read), by_idx(&write));

        // Lapozófájl: a "Paging File" példánynevek a fájl elérési útját tartalmazzák (pl. "\??\C:\pagefile.sys").
        let mut page_letters: HashSet<char> = HashSet::new();
        for (name, _) in &page {
            if let Some(pos) = name.find(":\\") {
                if let Some(c) = name[..pos].chars().last() {
                    page_letters.insert(c.to_ascii_uppercase());
                }
            }
        }

        let mut out = Vec::new();
        for d in statics {
            let letters = letters_by_idx.get(&d.index).cloned().unwrap_or_default();
            let tokens: Vec<String> = letters.split_whitespace().map(|s| s.to_uppercase()).collect();
            let is_system = tokens.iter().any(|t| *t == self.system_letter);
            let has_pagefile =
                tokens.iter().any(|t| t.chars().next().map_or(false, |c| page_letters.contains(&c)));
            let mut formatted = 0u64;
            for t in &tokens {
                formatted += volume_total(&mut self.sizes, t);
            }
            let idle_pct = idle_by_idx.get(&d.index).copied().unwrap_or(100.0);
            out.push(DiskTelemetry {
                index: d.index,
                letters,
                active_pct: (100.0 - idle_pct).clamp(0.0, 100.0) as f32,
                resp_ms: (resp_by.get(&d.index).copied().unwrap_or(0.0) * 1000.0) as f32,
                read_bps: read_by.get(&d.index).copied().unwrap_or(0.0),
                write_bps: write_by.get(&d.index).copied().unwrap_or(0.0),
                formatted_bytes: formatted,
                is_system,
                has_pagefile,
            });
        }
        out
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Hálózati adapterek
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, Default)]
pub struct NetTelemetry {
    /// Rövid ("barátságos") név, pl. "Ethernet".
    pub name: String,
    /// Az adapter leírása / modellje.
    pub desc: String,
    /// "Ethernet" | "Wi-Fi" | "Mobil".
    pub kind: String,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    pub link_bps: u64,
    /// Bájt/másodperc.
    pub send_bps: f64,
    pub recv_bps: f64,
}

impl NetTelemetry {
    pub fn kind_label(&self) -> &'static str {
        match self.kind.as_str() {
            "Mobil" => tr("Cellular"),
            "Wi-Fi" => "Wi-Fi",
            _ => "Ethernet",
        }
    }
}

struct AdapterInfo {
    name: String,
    desc: String,
    kind: &'static str,
    ipv4: Option<String>,
    ipv6: Option<String>,
    link_bps: u64,
}

unsafe fn rd_u32(p: *const u8, off: usize) -> u32 {
    std::ptr::read_unaligned(p.add(off) as *const u32)
}
unsafe fn rd_i32(p: *const u8, off: usize) -> i32 {
    std::ptr::read_unaligned(p.add(off) as *const i32)
}
unsafe fn rd_u64(p: *const u8, off: usize) -> u64 {
    std::ptr::read_unaligned(p.add(off) as *const u64)
}
unsafe fn rd_ptr(p: *const u8, off: usize) -> *const u8 {
    std::ptr::read_unaligned(p.add(off) as *const *const u8)
}
unsafe fn wstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.add(len) != 0 && len < 512 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

/// Az IP_ADAPTER_ADDRESSES (Vista+) láncolt lista bejárása. Az eltolások (x64):
/// Next=8, FirstUnicastAddress=24, Description=64, FriendlyName=72, IfType=100, OperStatus=104,
/// TransmitLinkSpeed=184, ReceiveLinkSpeed=192; az unicast elemnél Next=8, lpSockaddr=16, iSockaddrLength=24.
unsafe fn parse_adapters(base: *const u8) -> Vec<AdapterInfo> {
    let mut out = Vec::new();
    let mut p = base;
    while !p.is_null() {
        let if_type = rd_u32(p, 100);
        let oper = rd_u32(p, 104); // 1 = IfOperStatusUp
        let kind = match if_type {
            6 => "Ethernet",
            71 => "Wi-Fi",
            243 | 244 => "Mobil",
            _ => "",
        };
        if oper == 1 && !kind.is_empty() {
            let mut ipv4: Option<String> = None;
            let mut ipv6_global: Option<String> = None;
            let mut ipv6_link: Option<String> = None;
            let mut u = rd_ptr(p, 24);
            while !u.is_null() {
                let sa = rd_ptr(u, 16);
                let len = rd_i32(u, 24);
                if !sa.is_null() && len >= 8 {
                    let family = std::ptr::read_unaligned(sa as *const u16);
                    if family == 2 {
                        let b = std::slice::from_raw_parts(sa.add(4), 4);
                        if ipv4.is_none() {
                            ipv4 = Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string());
                        }
                    } else if family == 23 && len >= 24 {
                        let b = std::slice::from_raw_parts(sa.add(8), 16);
                        let mut arr = [0u8; 16];
                        arr.copy_from_slice(b);
                        let ip = Ipv6Addr::from(arr);
                        if (ip.segments()[0] & 0xffc0) == 0xfe80 {
                            if ipv6_link.is_none() {
                                ipv6_link = Some(ip.to_string());
                            }
                        } else if ipv6_global.is_none() {
                            ipv6_global = Some(ip.to_string());
                        }
                    }
                }
                u = rd_ptr(u, 8);
            }
            let tx = rd_u64(p, 184);
            let rx = rd_u64(p, 192);
            let link = tx.max(rx);
            out.push(AdapterInfo {
                name: wstr(rd_ptr(p, 72) as *const u16),
                desc: wstr(rd_ptr(p, 64) as *const u16),
                kind,
                ipv4,
                ipv6: ipv6_global.or(ipv6_link),
                link_bps: if link < 1_000_000_000_000 { link } else { 0 },
            });
        }
        p = rd_ptr(p, 8);
    }
    out
}

fn list_adapters() -> Vec<AdapterInfo> {
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8)];
    for _ in 0..3 {
        // SAFETY: a puffer `size` bájt, 8 bájtra igazított. 0 = AF_UNSPEC, 0x0E = anycast/multicast/DNS kihagyása.
        let rc = unsafe {
            GetAdaptersAddresses(0, 0x0E, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size)
        };
        if rc == 111 {
            // ERROR_BUFFER_OVERFLOW: a `size` most a szükséges méretet tartalmazza.
            buf = vec![0u64; (size as usize).div_ceil(8)];
            continue;
        }
        if rc != 0 {
            return Vec::new();
        }
        // SAFETY: a puffert az operációs rendszer érvényes, láncolt listával töltötte fel.
        return unsafe { parse_adapters(buf.as_ptr().cast()) };
    }
    Vec::new()
}

/// A PDH a példánynevekben a zárójeleket szögletesre, a '#', '/', '\' jeleket '_'-ra cseréli.
fn pdh_instance_name(desc: &str) -> String {
    desc.replace('(', "[")
        .replace(')', "]")
        .replace('#', "_")
        .replace('/', "_")
        .replace('\\', "_")
        .to_lowercase()
}

pub struct NetReader {
    q: Option<PdhQuery>,
    sent: Option<PDH_HCOUNTER>,
    recv: Option<PDH_HCOUNTER>,
}

impl NetReader {
    pub fn new() -> Self {
        let mut r = Self { q: None, sent: None, recv: None };
        if let Some(q) = PdhQuery::open() {
            r.sent = q.add(w!("\\Network Interface(*)\\Bytes Sent/sec"));
            r.recv = q.add(w!("\\Network Interface(*)\\Bytes Received/sec"));
            q.collect();
            r.q = Some(q);
        }
        r
    }

    pub fn sample(&mut self) -> Vec<NetTelemetry> {
        let adapters = list_adapters();
        let (mut sent, mut recv): (Vec<(String, f64)>, Vec<(String, f64)>) = (Vec::new(), Vec::new());
        if let Some(q) = self.q.as_mut() {
            if q.collect() {
                sent = self.sent.map(|c| q.read_array(c)).unwrap_or_default();
                recv = self.recv.map(|c| q.read_array(c)).unwrap_or_default();
            }
        }
        let find = |arr: &[(String, f64)], key: &str| -> f64 {
            arr.iter()
                .find(|(n, _)| n.to_lowercase() == key)
                .or_else(|| arr.iter().find(|(n, _)| n.to_lowercase().starts_with(key)))
                .map_or(0.0, |(_, v)| *v)
        };
        adapters
            .into_iter()
            .map(|a| {
                let key = pdh_instance_name(&a.desc);
                NetTelemetry {
                    send_bps: find(&sent, &key),
                    recv_bps: find(&recv, &key),
                    name: a.name,
                    desc: a.desc,
                    kind: a.kind.to_string(),
                    ipv4: a.ipv4,
                    ipv6: a.ipv6,
                    link_bps: a.link_bps,
                }
            })
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CPU-hőfok: LibreHardwareMonitor / OpenHardwareMonitor web-híd
// ═══════════════════════════════════════════════════════════════════════════
//
// Windows alatt a CPU-hőfok kernel-driver nélkül nem olvasható. A LibreHardwareMonitor (LHM) viszont
// rendelkezik driverrel, és be tudja kapcsolni a helyi webszerverét (Options -> Remote Web Server -> Run,
// alapértelmezett port: 8085). Ha fut, innen olvassuk a hőmérsékleteket a `/data.json` címről.
// A kapcsolat csak a saját gépre (127.0.0.1) megy.

const LHM_PORT: u16 = 8085;

fn http_get_local(port: u16, path: &str) -> Option<String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(250)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(600))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(300))).ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAccept: application/json\r\n\r\n");
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    // Időtúllépésnél is megtartjuk, ami addig megérkezett.
    let _ = (&mut s).take(2_000_000).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| text[start..=end].to_string())
}

/// "45,0 °C" / "45.0 °C" -> 45.0
fn parse_celsius(v: &str) -> Option<f32> {
    v.split_whitespace().next()?.replace(',', ".").parse::<f32>().ok()
}

fn looks_like_cpu_sensor(name: &str) -> bool {
    let n = name.to_lowercase();
    !n.contains("gpu")
        && (n.starts_with("core")
            || n.starts_with("cpu")
            || n.starts_with("ccd")
            || n.contains("tctl")
            || n.contains("tdie")
            || n.contains("package"))
}

fn walk_sensors(v: &Value, out: &mut Vec<TempReading>) {
    let Some(obj) = v.as_object() else { return };
    let id = obj.get("SensorId").and_then(Value::as_str).unwrap_or("");
    let ty = obj.get("Type").and_then(Value::as_str).unwrap_or("");
    let text = obj.get("Text").and_then(Value::as_str).unwrap_or("");
    if let Some(val) = obj.get("Value").and_then(Value::as_str) {
        let is_temp =
            ty.eq_ignore_ascii_case("Temperature") || id.contains("/temperature/") || val.contains('°');
        let is_cpu = id.starts_with("/amdcpu/")
            || id.starts_with("/intelcpu/")
            || id.starts_with("/cpu/")
            || (id.is_empty() && looks_like_cpu_sensor(text));
        if is_temp && is_cpu {
            if let Some(c) = parse_celsius(val) {
                if (1.0..=125.0).contains(&c) {
                    out.push(TempReading { label: text.to_string(), celsius: c });
                }
            }
        }
    }
    if let Some(children) = obj.get("Children").and_then(Value::as_array) {
        for c in children {
            walk_sensors(c, out);
        }
    }
}

fn fetch_lhm_temps() -> Option<Vec<TempReading>> {
    let body = http_get_local(LHM_PORT, "/data.json")?;
    let json: Value = serde_json::from_str(&body).ok()?;
    let mut out = Vec::new();
    walk_sensors(&json, &mut out);
    (!out.is_empty()).then_some(out)
}

/// CPU-hőmérséklet-forrás: LibreHardwareMonitor web-szerver. Sikeres olvasás 1,5 másodpercenként,
/// sikertelen esetén 8 másodpercenként próbálkozik újra (hogy ne terheljük feleslegesen a gépet).
pub struct LhmWebTempSource {
    last_try: Option<Instant>,
    last_ok: bool,
    cached: Vec<TempReading>,
}

impl LhmWebTempSource {
    pub fn new() -> Self {
        Self { last_try: None, last_ok: false, cached: Vec::new() }
    }
}

impl CpuTempSource for LhmWebTempSource {
    fn label(&self) -> &'static str {
        "LibreHardwareMonitor (web, 127.0.0.1:8085)"
    }

    fn read(&mut self) -> Option<Vec<TempReading>> {
        let wait = if self.last_ok { Duration::from_millis(1500) } else { Duration::from_secs(8) };
        let due = self.last_try.map_or(true, |t| t.elapsed() >= wait);
        if due {
            self.last_try = Some(Instant::now());
            match fetch_lhm_temps() {
                Some(t) => {
                    self.cached = t;
                    self.last_ok = true;
                }
                None => {
                    self.cached.clear();
                    self.last_ok = false;
                }
            }
        }
        (!self.cached.is_empty()).then(|| self.cached.clone())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Folyamatonkénti kiegészítő adatok: privát munkakészlet és I/O (PDH)
// ═══════════════════════════════════════════════════════════════════════════
//
// A Task Manager "Memória" oszlopa a PRIVÁT munkakészlet. A sysinfo `Process::memory()` Windowson a
// teljes munkakészletet adja (a megosztott lapokkal együtt), ami sokkal nagyobb szám. A privát
// munkakészletet a PDH "Process\Working Set - Private" számlálója adja; a példányok neve a folyamat
// neve (+ "#1", "#2"...), ezért az "ID Process" számlálóval kötjük össze a PID-del.

#[derive(Clone, Copy, Default)]
pub struct ProcExtra {
    pub private_bytes: f64,
    /// Az összes I/O (fájl, eszköz, hálózat) bájt/másodperc – nem csak a lemezé.
    pub io_bps: f64,
}

pub struct ProcPdh {
    q: PdhQuery,
    ws: PDH_HCOUNTER,
    id: PDH_HCOUNTER,
    io: Option<PDH_HCOUNTER>,
}

impl ProcPdh {
    pub fn new() -> Option<Self> {
        let q = PdhQuery::open()?;
        let ws = q.add(w!("\\Process(*)\\Working Set - Private"))?;
        let id = q.add(w!("\\Process(*)\\ID Process"))?;
        let io = q.add(w!("\\Process(*)\\IO Data Bytes/sec"));
        q.collect(); // alapmérés a ráta-számlálóhoz
        Some(Self { q, ws, id, io })
    }

    /// PID -> (privát munkakészlet, I/O sebesség).
    pub fn sample(&mut self) -> HashMap<u32, ProcExtra> {
        let mut out: HashMap<u32, ProcExtra> = HashMap::new();
        if !self.q.collect() {
            return out;
        }
        let ids: HashMap<String, u32> = self
            .q
            .read_array(self.id)
            .into_iter()
            .map(|(n, v)| (n, v as u32))
            .collect();
        let ws = self.q.read_array(self.ws);
        let io = self.io.map(|c| self.q.read_array(c)).unwrap_or_default();
        for (name, v) in ws {
            if let Some(&pid) = ids.get(&name) {
                if pid != 0 {
                    out.entry(pid).or_default().private_bytes = v;
                }
            }
        }
        for (name, v) in io {
            if let Some(&pid) = ids.get(&name) {
                if pid != 0 {
                    out.entry(pid).or_default().io_bps = v.max(0.0);
                }
            }
        }
        out
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Folyamatonkénti PRIVÁT munkakészlet közvetlenül a rendszertől (NtQuerySystemInformation)
// ═══════════════════════════════════════════════════════════════════════════
//
// A PDH-s módszer a folyamatnevekhez fűzött "#N" példányszámokon át köti a számlálót a PID-hez; ez több azonos nevű
// folyamatnál (böngésző, Electron-alkalmazások) megbízhatatlan volt: a hiányzó értékeknél a program a TELJES
// munkakészletre esett vissza, ami a megosztott lapokat minden folyamatnál újra beleszámolja, ezért a csoportok
// összege a Windows Feladatkezelőjének sokszorosa lett.
//
// A SYSTEM_PROCESS_INFORMATION rekordban a `WorkingSetPrivateSize` mező pontosan ez a privát munkakészlet, PID-del
// együtt, névillesztés nélkül. Ugyanazt a rendszerhívást használja a sysinfo is, így ez egyetlen gyors hívás.

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, buf: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

/// A SYSTEM_PROCESS_INFORMATION eleje (a mezők igazítását a `repr(C)` adja, x86-on és x64-en is helyes).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)] // a mezők a rekord elrendezését adják; nem mindet olvassuk
struct SysProcInfoHead {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name_length: u16,
    image_name_max_length: u16,
    image_name_buffer: usize,
    base_priority: i32,
    unique_process_id: usize,
}

/// PID -> privát munkakészlet bájtban. Üres térkép, ha a lekérdezés nem sikerült (ilyenkor a hívó a PDH-s / teljes
/// munkakészlet-értékre esik vissza).
pub fn private_working_sets() -> HashMap<u32, u64> {
    const SYSTEM_PROCESS_INFORMATION: u32 = 5;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
    let mut out: HashMap<u32, u64> = HashMap::new();
    // u64-elemű puffer: 8 bájtra igazított, ahogy a rekordok is.
    let mut buf: Vec<u64> = vec![0; (512 * 1024) / 8];
    for _attempt in 0..8 {
        let cap_bytes = buf.len() * 8;
        let mut ret: u32 = 0;
        // SAFETY: a puffer `cap_bytes` bájt hosszú és élő; a `ret` kimeneti változó.
        let status = unsafe {
            NtQuerySystemInformation(SYSTEM_PROCESS_INFORMATION, buf.as_mut_ptr().cast(), cap_bytes as u32, &mut ret)
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            let need = (ret as usize).max(cap_bytes) + 128 * 1024;
            buf = vec![0; need.div_ceil(8)];
            continue;
        }
        if status < 0 {
            return out;
        }
        let base = buf.as_ptr().cast::<u8>();
        let head_len = std::mem::size_of::<SysProcInfoHead>();
        let mut off = 0usize;
        loop {
            if off + head_len > cap_bytes {
                break;
            }
            // SAFETY: az `off + head_len` a pufferen belül van; a rekordot igazítatlanul olvassuk.
            let head = unsafe { std::ptr::read_unaligned(base.add(off).cast::<SysProcInfoHead>()) };
            let pid = head.unique_process_id as u32;
            if head.working_set_private_size > 0 {
                out.insert(pid, head.working_set_private_size as u64);
            }
            if head.next_entry_offset == 0 {
                break;
            }
            off += head.next_entry_offset as usize;
        }
        return out;
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// Munkamenetek (bejelentkezett felhasználók) – WTS API
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Debug, Default)]
pub struct SessionInfo {
    pub id: u32,
    pub user: String,
    pub domain: String,
    /// WTS_CONNECTSTATE_CLASS: 0 = aktív, 1 = csatlakoztatva, 4 = szétkapcsolva, 5 = tétlen...
    pub state: u32,
    /// A munkamenet neve (pl. "Console", "RDP-Tcp#0").
    pub station: String,
    /// A távoli ügyfél gépének neve (helyi munkamenetnél üres).
    pub client: String,
}

#[repr(C)]
struct WtsSessionInfo {
    session_id: u32,
    win_station_name: *mut u16,
    state: u32,
}

#[link(name = "wtsapi32")]
extern "system" {
    fn WTSEnumerateSessionsW(
        server: isize,
        reserved: u32,
        version: u32,
        info: *mut *mut WtsSessionInfo,
        count: *mut u32,
    ) -> i32;
    fn WTSQuerySessionInformationW(
        server: isize,
        session: u32,
        class: u32,
        buf: *mut *mut u16,
        bytes: *mut u32,
    ) -> i32;
    fn WTSFreeMemory(p: *mut c_void);
}

#[link(name = "kernel32")]
extern "system" {
    fn ProcessIdToSessionId(pid: u32, session: *mut u32) -> i32;
}

/// Egy folyamat munkamenet-azonosítója (0 = szolgáltatások, 1+ = interaktív felhasználók).
pub fn session_of_pid(pid: u32) -> Option<u32> {
    let mut sid = 0u32;
    // SAFETY: érvényes kimeneti mutató; a hívás nem igényel külön jogosultságot.
    (unsafe { ProcessIdToSessionId(pid, &mut sid) } != 0).then_some(sid)
}

/// WTS-szöveges információ (5 = felhasználónév, 7 = tartomány / gépnév).
unsafe fn wts_string(session: u32, class: u32) -> String {
    let mut buf: *mut u16 = std::ptr::null_mut();
    let mut bytes = 0u32;
    if WTSQuerySessionInformationW(0, session, class, &mut buf, &mut bytes) == 0 || buf.is_null() {
        return String::new();
    }
    let s = wstr(buf as *const u16);
    WTSFreeMemory(buf.cast());
    s
}

/// A bejelentkezett felhasználók munkamenetei. Ha egyet sem találunk, a saját (helyi) felhasználót adjuk vissza.
pub fn list_sessions() -> Vec<SessionInfo> {
    let mut out = Vec::new();
    // SAFETY: a WTS-listát az operációs rendszer tölti ki, és a végén felszabadítjuk.
    unsafe {
        let mut info: *mut WtsSessionInfo = std::ptr::null_mut();
        let mut count = 0u32;
        if WTSEnumerateSessionsW(0, 0, 1, &mut info, &mut count) != 0 && !info.is_null() {
            for i in 0..count as usize {
                let s = &*info.add(i);
                let user = wts_string(s.session_id, 5);
                if user.is_empty() {
                    continue; // szolgáltatás-munkamenet vagy figyelő (nincs felhasználó)
                }
                out.push(SessionInfo {
                    id: s.session_id,
                    user,
                    domain: wts_string(s.session_id, 7),
                    state: s.state,
                    station: wts_string(s.session_id, 6),  // WTSWinStationName
                    client: wts_string(s.session_id, 10), // WTSClientName
                });
            }
            WTSFreeMemory(info.cast());
        }
    }
    if out.is_empty() {
        out.push(SessionInfo {
            id: session_of_pid(std::process::id()).unwrap_or(1),
            user: std::env::var("USERNAME").unwrap_or_default(),
            domain: std::env::var("USERDOMAIN").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_default(),
            state: 0,
            station: String::new(),
            client: String::new(),
        });
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// Jogosultság: induláskori rendszergazdai (UAC) kérés, SeDebugPrivilege
// ═══════════════════════════════════════════════════════════════════════════

#[link(name = "shell32")]
extern "system" {
    fn IsUserAnAdmin() -> i32;
    fn ShellExecuteW(
        hwnd: isize,
        operation: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> isize;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Luid {
    low: u32,
    high: i32,
}

/// TOKEN_PRIVILEGES egyetlen privilégiummal.
#[repr(C)]
struct TokenPrivileges {
    count: u32,
    luid: Luid,
    attributes: u32,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    fn GetTokenInformation(
        token: isize,
        class: u32,
        info: *mut c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
    fn LookupAccountSidW(
        system: *const u16,
        sid: *const c_void,
        name: *mut u16,
        name_len: *mut u32,
        domain: *mut u16,
        domain_len: *mut u32,
        use_: *mut u32,
    ) -> i32;
    fn LookupAccountNameW(
        system: *const u16,
        name: *const u16,
        sid: *mut u8,
        sid_len: *mut u32,
        domain: *mut u16,
        domain_len: *mut u32,
        use_: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const u8, out: *mut *mut u16) -> i32;
    fn RegGetValueW(
        hkey: isize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        ty: *mut u32,
        data: *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(
        token: isize,
        disable_all: i32,
        new_state: *const TokenPrivileges,
        buffer_len: u32,
        previous: *mut c_void,
        ret_len: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
    fn IsWow64Process2(process: isize, process_machine: *mut u16, native_machine: *mut u16) -> i32;
    fn LocalFree(h: *mut c_void) -> *mut c_void;
}

/// Rendszergazdai (emelt) jogosultsággal fut-e a program.
pub fn is_elevated() -> bool {
    // SAFETY: paraméter nélküli, mellékhatás-mentes Win32 hívás.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Újraindítja a programot rendszergazdaként (UAC-kérdés). `true`, ha az emelt példány elindult
/// (ilyenkor a hívó példánynak ki kell lépnie); `false`, ha a felhasználó elutasította.
/// A `--elevated` kapcsoló megakadályozza az ismételt újraindítást.
pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let file = wide(&exe.to_string_lossy());
    let mut params = String::from("--elevated");
    for a in std::env::args().skip(1) {
        if a != "--elevated" {
            params.push(' ');
            params.push_str(&a);
        }
    }
    let params = wide(&params);
    let verb = wide("runas");
    // SAFETY: NUL-terminált szövegek; a visszatérési érték > 32, ha a művelet sikerült.
    let r = unsafe { ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), params.as_ptr(), std::ptr::null(), 1) };
    r > 32
}

/// Bekapcsolja a SeDebugPrivilege-et: így a (rendszergazdai) program más felhasználók és rendszerfolyamatok
/// adataihoz is hozzáfér. Csak emelt jogosultsággal van hatása.
pub fn enable_debug_privilege() -> bool {
    // SAFETY: a saját folyamat tokenjét módosítjuk; minden megnyitott handle-t lezárunk.
    unsafe {
        let mut token = 0isize;
        // -1 = az aktuális folyamat pszeudo-handle-je; 0x28 = TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY
        if OpenProcessToken(-1, 0x28, &mut token) == 0 {
            return false;
        }
        let mut luid = Luid::default();
        let name = wide("SeDebugPrivilege");
        let mut ok = false;
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) != 0 {
            let tp = TokenPrivileges { count: 1, luid, attributes: 2 }; // 2 = SE_PRIVILEGE_ENABLED
            ok = AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0;
        }
        CloseHandleRaw(token);
        ok
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Folyamat-metaadatok: tulajdonos (felhasználó), architektúra, leírás
// ═══════════════════════════════════════════════════════════════════════════

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// A folyamat tulajdonosa (pl. "Neo", "SYSTEM" / "RENDSZER", "LOCAL SERVICE" / "HELYI SZOLGÁLTATÁS").
/// A nevet a Windows adja vissza, ezért a rendszer nyelvén jelenik meg.
fn query_owner(pid: u32) -> Option<String> {
    // SAFETY: a handle-ok lezárása minden úton megtörténik; a SID a tokeninformációs pufferben van.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return None;
        }
        let mut token = 0isize;
        let opened = OpenProcessToken(h, 0x8, &mut token) != 0; // 0x8 = TOKEN_QUERY
        CloseHandleRaw(h);
        if !opened {
            return None;
        }
        let mut len = 0u32;
        GetTokenInformation(token, 1, std::ptr::null_mut(), 0, &mut len); // 1 = TokenUser, a méretet kérdezzük
        let mut out = None;
        if len > 0 {
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            if GetTokenInformation(token, 1, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len) != 0 {
                // A TOKEN_USER első mezője a SID mutatója.
                let sid = *(buf.as_ptr() as *const *const c_void);
                let mut name = [0u16; 256];
                let mut nlen = 256u32;
                let mut dom = [0u16; 256];
                let mut dlen = 256u32;
                let mut use_ = 0u32;
                if LookupAccountSidW(
                    std::ptr::null(),
                    sid,
                    name.as_mut_ptr(),
                    &mut nlen,
                    dom.as_mut_ptr(),
                    &mut dlen,
                    &mut use_,
                ) != 0
                {
                    out = Some(String::from_utf16_lossy(&name[..nlen as usize]));
                }
            }
        }
        CloseHandleRaw(token);
        out
    }
}

/// "x64" / "x86" / "ARM64" / "ARM" – a folyamat architektúrája (üres, ha nem kérdezhető le).
fn query_arch(pid: u32) -> &'static str {
    // SAFETY: a handle-t lezárjuk; a kimenetek érvényes mutatók.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return "";
        }
        let (mut process_machine, mut native_machine) = (0u16, 0u16);
        let ok = IsWow64Process2(h, &mut process_machine, &mut native_machine) != 0;
        CloseHandleRaw(h);
        if !ok {
            return "";
        }
        // A 0 (UNKNOWN) a nem-WOW64 (natív) folyamatot jelzi: akkor a gép architektúrája a mérvadó.
        match if process_machine != 0 { process_machine } else { native_machine } {
            0x014c => "x86",
            0x8664 => "x64",
            0xAA64 => "ARM64",
            0x01c4 => "ARM",
            _ => "",
        }
    }
}

#[link(name = "version")]
extern "system" {
    fn GetFileVersionInfoSizeW(path: *const u16, handle: *mut u32) -> u32;
    fn GetFileVersionInfoW(path: *const u16, handle: u32, len: u32, data: *mut c_void) -> i32;
    fn VerQueryValueW(block: *const c_void, sub: *const u16, buf: *mut *mut c_void, len: *mut u32) -> i32;
}

/// Az exe "Fájlleírás" (FileDescription) mezője a verzióinformációból.
fn file_description(path: &str) -> Option<String> {
    file_version_string(path, "FileDescription")
}

/// Az exe "Gyártó" (CompanyName) mezője a verzióinformációból (az Indítási alkalmazások fülhöz).
pub fn file_company(path: &str) -> Option<String> {
    file_version_string(path, "CompanyName")
}

/// Egy tetszőleges StringFileInfo mező (pl. FileDescription, CompanyName) a verzióinformációból.
fn file_version_string(path: &str, field: &str) -> Option<String> {
    // SAFETY: a puffer 8 bájtra igazított, a VerQueryValue a pufferen belüli mutatókat ad vissza.
    unsafe {
        let p = wide(path);
        let mut handle = 0u32;
        let size = GetFileVersionInfoSizeW(p.as_ptr(), &mut handle);
        if size == 0 || size > 4 * 1024 * 1024 {
            return None;
        }
        let mut data = vec![0u64; (size as usize).div_ceil(8)];
        if GetFileVersionInfoW(p.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let mut buf: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        // A (nyelv, kódlap) párok táblázata.
        let mut pairs: Vec<(u16, u16)> = Vec::new();
        let sub = wide("\\VarFileInfo\\Translation");
        if VerQueryValueW(data.as_ptr().cast(), sub.as_ptr(), &mut buf, &mut len) != 0
            && len >= 4
            && !buf.is_null()
        {
            let n = (len / 4) as usize;
            let words = buf as *const u16;
            for i in 0..n {
                pairs.push((
                    std::ptr::read_unaligned(words.add(i * 2)),
                    std::ptr::read_unaligned(words.add(i * 2 + 1)),
                ));
            }
        }
        pairs.push((0x0409, 0x04b0));
        pairs.push((0x0409, 0x04e4));
        pairs.push((0x0000, 0x04b0));
        for (lang, cp) in pairs {
            let q = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}"));
            if VerQueryValueW(data.as_ptr().cast(), q.as_ptr(), &mut buf, &mut len) != 0
                && !buf.is_null()
                && len > 0
            {
                let s = wstr(buf as *const u16);
                if !s.trim().is_empty() {
                    return Some(s.trim().to_string());
                }
            }
        }
        None
    }
}

/// PID-enkénti (felhasználó, architektúra) és exe-nkénti (leírás) gyorsítótár. A lekérdezések költségesek,
/// ezért tickenként csak korlátozott számú új elemet oldunk fel (a többi a következő ciklusban jön).
#[derive(Default)]
pub struct ProcMetaCache {
    meta: HashMap<u32, (String, &'static str)>,
    desc: HashMap<String, String>,
}

impl ProcMetaCache {
    pub fn meta(&mut self, pid: u32, budget: &mut u32) -> (String, &'static str) {
        if let Some((u, a)) = self.meta.get(&pid) {
            return (u.clone(), *a);
        }
        if *budget == 0 {
            return (String::new(), "");
        }
        *budget -= 1;
        let entry = (query_owner(pid).unwrap_or_default(), query_arch(pid));
        self.meta.insert(pid, entry.clone());
        entry
    }

    pub fn description(&mut self, exe: &str, budget: &mut u32) -> String {
        if let Some(d) = self.desc.get(exe) {
            return d.clone();
        }
        if *budget == 0 {
            return String::new();
        }
        *budget -= 1;
        let d = file_description(exe).unwrap_or_default();
        self.desc.insert(exe.to_string(), d.clone());
        d
    }

    /// A már nem futó PID-ek törlése (a PID-ek újrahasznosíthatók).
    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.meta.retain(|pid, _| live.contains(pid));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Ablakok felderítése: melyik folyamatnak van látható ablaka ("alkalmazás"), melyik nem válaszol
// ═══════════════════════════════════════════════════════════════════════════

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: unsafe extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
    fn IsWindowVisible(h: isize) -> i32;
    fn GetWindowTextLengthW(h: isize) -> i32;
    fn GetWindow(h: isize, cmd: u32) -> isize;
    fn GetWindowLongPtrW(h: isize, index: i32) -> isize;
    fn GetWindowThreadProcessId(h: isize, pid: *mut u32) -> u32;
    fn IsHungAppWindow(h: isize) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(h: isize, attr: u32, out: *mut c_void, size: u32) -> i32;
}

struct WindowScan {
    apps: HashSet<u32>,
    hung: HashSet<u32>,
}

unsafe extern "system" fn enum_windows_cb(h: isize, lparam: isize) -> i32 {
    let scan = &mut *(lparam as *mut WindowScan);
    if IsWindowVisible(h) == 0 || GetWindowTextLengthW(h) == 0 {
        return 1;
    }
    // Csak a tulajdonos nélküli (felső szintű) ablakok; az eszköztár-ablakok (WS_EX_TOOLWINDOW) kimaradnak,
    // kivéve, ha kifejezetten alkalmazásablaknak jelölték (WS_EX_APPWINDOW).
    if GetWindow(h, 4) != 0 {
        return 1;
    }
    let ex = GetWindowLongPtrW(h, -20) as u32; // GWL_EXSTYLE
    if (ex & 0x0000_0080) != 0 && (ex & 0x0004_0000) == 0 {
        return 1;
    }
    // A "takart" (cloaked) ablakok (pl. felfüggesztett UWP-alkalmazások) nem számítanak láthatónak.
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(h, 14, (&mut cloaked as *mut u32).cast(), 4) == 0 && cloaked != 0 {
        return 1;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    if pid != 0 {
        scan.apps.insert(pid);
        if IsHungAppWindow(h) != 0 {
            scan.hung.insert(pid);
        }
    }
    1
}

/// (látható ablakkal rendelkező PID-ek, nem válaszoló PID-ek)
pub fn scan_windows() -> (HashSet<u32>, HashSet<u32>) {
    let mut scan = WindowScan { apps: HashSet::new(), hung: HashSet::new() };
    // SAFETY: a `scan` a hívás idejére él; a callback csak ezt éri el.
    unsafe {
        EnumWindows(enum_windows_cb, &mut scan as *mut WindowScan as isize);
    }
    (scan.apps, scan.hung)
}

// ═══════════════════════════════════════════════════════════════════════════
// Felhasználói profilkép (a Windows fiókképe)
// ═══════════════════════════════════════════════════════════════════════════
//
// A beállított fiókkép elérési útját a Windows a registryben tárolja:
// HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AccountPicture\Users\<SID>  (Image96, Image192, ... értékek).
// Ha nincs egyedi kép, az alapértelmezett fiókképet használjuk.

fn user_sid_string(user: &str) -> Option<String> {
    // SAFETY: a kétlépéses (méret, majd adat) hívás a LookupAccountName szokásos használata.
    unsafe {
        let name = wide(user);
        let (mut sid_len, mut dom_len, mut use_) = (0u32, 0u32, 0u32);
        LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            std::ptr::null_mut(),
            &mut dom_len,
            &mut use_,
        );
        if sid_len == 0 {
            return None;
        }
        let mut sid = vec![0u8; sid_len as usize];
        let mut dom = vec![0u16; dom_len.max(1) as usize];
        if LookupAccountNameW(
            std::ptr::null(),
            name.as_ptr(),
            sid.as_mut_ptr(),
            &mut sid_len,
            dom.as_mut_ptr(),
            &mut dom_len,
            &mut use_,
        ) == 0
        {
            return None;
        }
        let mut out: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(sid.as_ptr(), &mut out) == 0 || out.is_null() {
            return None;
        }
        let s = wstr(out as *const u16);
        LocalFree(out.cast());
        Some(s)
    }
}

fn reg_string_hklm(subkey: &str, value: &str) -> Option<String> {
    // SAFETY: kétlépéses (méret, majd adat) RegGetValueW hívás; a puffer elég nagy.
    unsafe {
        let sk = wide(subkey);
        let v = wide(value);
        let hklm = 0x8000_0002u32 as i32 as isize; // HKEY_LOCAL_MACHINE
        let mut size = 0u32;
        // 0x2 = RRF_RT_REG_SZ
        if RegGetValueW(hklm, sk.as_ptr(), v.as_ptr(), 0x2, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
            != 0
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        let mut size2 = (buf.len() * 2) as u32;
        if RegGetValueW(
            hklm,
            sk.as_ptr(),
            v.as_ptr(),
            0x2,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size2,
        ) != 0
        {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

/// A felhasználó profilképe 96×96-os RGBA képként. `None`, ha nem található vagy nem dekódolható.
pub fn load_user_avatar(user: &str) -> Option<(usize, usize, Vec<u8>)> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(sid) = user_sid_string(user) {
        let sub = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AccountPicture\\Users\\{sid}");
        for v in ["Image96", "Image192", "Image208", "Image240", "Image424", "Image448", "Image1080", "Image48", "Image40", "Image32"] {
            if let Some(p) = reg_string_hklm(&sub, v) {
                candidates.push(p);
            }
        }
    }
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
    candidates.push(format!("{program_data}\\Microsoft\\User Account Pictures\\user-192.png"));
    candidates.push(format!("{program_data}\\Microsoft\\User Account Pictures\\user.png"));
    for path in candidates {
        if let Ok(img) = image::open(&path) {
            let rgba = img.resize_to_fill(96, 96, image::imageops::FilterType::Triangle).to_rgba8();
            let (w, h) = rgba.dimensions();
            return Some((w as usize, h as usize, rgba.into_raw()));
        }
    }
    None
}
