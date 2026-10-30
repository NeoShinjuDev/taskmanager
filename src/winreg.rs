/*
 * Copyright (c) 2026 Neo. All rights reserved.
 * 
 * EN: This file is part of taskmanager. Usage is subject to the terms of the 
 * Custom License found in the LICENSE file in the root of this project.
 * 
 * HU: Ez a fájl a(z) taskmanager része. A felhasználásra a projekt gyökerében 
 * található Egyéni Licenc (LICENSE.hu) feltételei érvényesek.
 */

//! Minimális registry-burkoló nyers Win32 FFI-vel (advapi32). Szándékosan nem a `windows` crate-et használja,
//! hogy ne függjön a feature-készlettől és a crate-verzió szignatúráitól – ugyanúgy, mint a `sysdata.rs`.
//!
//! A kulcsok `Drop`-ban záródnak. Minden hiba `Err(kód)` / `None`: a hívók sosem panikolnak.

use std::ffi::c_void;

pub const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;

pub const KEY_QUERY_VALUE: u32 = 0x0001;
pub const KEY_SET_VALUE: u32 = 0x0002;
pub const KEY_WOW64_64KEY: u32 = 0x0100;
pub const KEY_WOW64_32KEY: u32 = 0x0200;

pub const ERROR_ACCESS_DENIED: u32 = 5;

const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_BINARY: u32 = 3;
const REG_DWORD: u32 = 4;
const REG_QWORD: u32 = 11;

const ERROR_MORE_DATA: i32 = 234;
const ERROR_NO_MORE_ITEMS: i32 = 259;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(key: isize, sub: *const u16, options: u32, sam: u32, out: *mut isize) -> i32;
    fn RegCreateKeyExW(
        key: isize,
        sub: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *mut c_void,
        out: *mut isize,
        disposition: *mut u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        ty: *mut u32,
        data: *mut u8,
        size: *mut u32,
    ) -> i32;
    fn RegSetValueExW(key: isize, name: *const u16, reserved: u32, ty: u32, data: *const u8, size: u32) -> i32;
    fn RegEnumValueW(
        key: isize,
        index: u32,
        name: *mut u16,
        name_len: *mut u32,
        reserved: *mut u32,
        ty: *mut u32,
        data: *mut u8,
        data_len: *mut u32,
    ) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn ExpandEnvironmentStringsW(src: *const u16, dst: *mut u16, size: u32) -> u32;
    fn GetSystemTimeAsFileTime(ft: *mut u64);
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A jelenlegi idő Windows FILETIME-ként (100 ns egységek 1601-01-01 óta).
pub fn filetime_now() -> u64 {
    let mut ft = 0u64;
    // SAFETY: a FILETIME két u32, a kimenet egyetlen, 8 bájtos u64 (little endian: alsó, felső rész).
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    ft
}

/// `%VAR%` kifejezések kibontása (REG_EXPAND_SZ értékekhez).
pub fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = wide(s);
    // SAFETY: kétlépéses hívás (méret, majd adat); a puffer a visszaadott méretű.
    unsafe {
        let n = ExpandEnvironmentStringsW(src.as_ptr(), std::ptr::null_mut(), 0);
        if n == 0 {
            return s.to_string();
        }
        let mut buf = vec![0u16; n as usize];
        let n2 = ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), n);
        if n2 == 0 || n2 > n {
            return s.to_string();
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }
}

/// Egy registry-érték típusos tartalma. A REG_EXPAND_SZ már ki van bontva.
#[derive(Clone, Debug)]
pub enum RegValue {
    Str(String),
    Dword(u32),
    Qword(u64),
    Binary(Vec<u8>),
}

fn decode(ty: u32, buf: Vec<u8>) -> RegValue {
    match ty {
        REG_SZ | REG_EXPAND_SZ => {
            let units: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
            let s = String::from_utf16_lossy(&units[..end]);
            RegValue::Str(if ty == REG_EXPAND_SZ { expand_env(&s) } else { s })
        }
        REG_DWORD if buf.len() >= 4 => RegValue::Dword(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])),
        REG_QWORD if buf.len() >= 8 => RegValue::Qword(u64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ])),
        _ => RegValue::Binary(buf),
    }
}

/// Megnyitott registry-kulcs (RAII).
pub struct RegKey(isize);

impl RegKey {
    /// Meglévő kulcs megnyitása. `sam`: KEY_QUERY_VALUE / KEY_SET_VALUE | KEY_WOW64_xxKEY.
    pub fn open(root: isize, sub: &str, sam: u32) -> Result<RegKey, u32> {
        let s = wide(sub);
        let mut h = 0isize;
        // SAFETY: érvényes, NUL-lezárt UTF-16 kulcsnév és kimeneti mutató.
        let rc = unsafe { RegOpenKeyExW(root, s.as_ptr(), 0, sam, &mut h) };
        if rc == 0 {
            Ok(RegKey(h))
        } else {
            Err(rc as u32)
        }
    }

    /// Kulcs megnyitása, és ha nem létezik, létrehozása.
    pub fn create(root: isize, sub: &str, sam: u32) -> Result<RegKey, u32> {
        let s = wide(sub);
        let mut h = 0isize;
        let mut disposition = 0u32;
        // SAFETY: érvényes mutatók; a biztonsági leíró és az osztály NULL (alapértelmezett).
        let rc = unsafe {
            RegCreateKeyExW(
                root,
                s.as_ptr(),
                0,
                std::ptr::null(),
                0,
                sam,
                std::ptr::null_mut(),
                &mut h,
                &mut disposition,
            )
        };
        if rc == 0 {
            Ok(RegKey(h))
        } else {
            Err(rc as u32)
        }
    }

    /// Egy érték beolvasása név alapján. `None`, ha nincs ilyen érték vagy nem olvasható.
    pub fn value(&self, name: &str) -> Option<RegValue> {
        let n = wide(name);
        let mut ty = 0u32;
        let mut size = 0u32;
        // SAFETY: első hívás csak a méretet kéri (NULL adatmutató).
        let rc = unsafe {
            RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null_mut(), &mut ty, std::ptr::null_mut(), &mut size)
        };
        if rc != 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize + 2];
        let mut size2 = buf.len() as u32;
        // SAFETY: a puffer legalább `size` bájt, a hossz a tényleges kapacitás.
        let rc = unsafe {
            RegQueryValueExW(self.0, n.as_ptr(), std::ptr::null_mut(), &mut ty, buf.as_mut_ptr(), &mut size2)
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(size2 as usize);
        Some(decode(ty, buf))
    }

    /// REG_BINARY érték írása.
    pub fn set_binary(&self, name: &str, data: &[u8]) -> Result<(), u32> {
        let n = wide(name);
        // SAFETY: a `data` szelet élő, a hossza megegyezik a megadott mérettel.
        let rc = unsafe { RegSetValueExW(self.0, n.as_ptr(), 0, REG_BINARY, data.as_ptr(), data.len() as u32) };
        if rc == 0 {
            Ok(())
        } else {
            Err(rc as u32)
        }
    }

    /// A kulcs összes értéke (név, tartalom). A túl nagy (>64 KB) értékeket kihagyja.
    pub fn values(&self) -> Vec<(String, RegValue)> {
        let mut out = Vec::new();
        let mut name = vec![0u16; 16_384];
        let mut data = vec![0u8; 65_536];
        for index in 0u32..8192 {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let mut ty = 0u32;
            // SAFETY: a pufferek hossza a megadott kapacitással egyezik; a visszaadott hossz ≤ kapacitás.
            let rc = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut ty,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            match rc {
                0 => {}
                ERROR_NO_MORE_ITEMS => break,
                ERROR_MORE_DATA => continue,
                _ => break,
            }
            let n = String::from_utf16_lossy(&name[..(name_len as usize).min(name.len())]);
            let d = data[..(data_len as usize).min(data.len())].to_vec();
            out.push((n, decode(ty, d)));
        }
        out
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: a handle a `RegKey` sajátja, csak itt záródik.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
