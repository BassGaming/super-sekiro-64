//! Where the mod's files live: everything sits next to sk_mario.dll (the mod folder), so the mod
//! works wherever the player unpacks it. sk_mario.ini there can point at the SM64 ROM; without it
//! the first .z64/.n64/.v64 in the mod folder is used.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW,
};
use windows::core::PCWSTR;

pub const CONFIG: &str = "sk_mario.ini";

/// The folder er_mario.dll was loaded from.
pub fn mod_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let mut module = HMODULE::default();
        let mut buf = [0u16; 1024];
        let len = unsafe {
            let anchor = mod_dir as *const () as *const u16;
            let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
            if GetModuleHandleExW(flags, PCWSTR(anchor), &mut module).is_err() {
                return PathBuf::from(".");
            }
            GetModuleFileNameW(Some(module), &mut buf) as usize
        };
        let dll = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
        dll.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    })
}

pub fn file(name: &str) -> PathBuf {
    mod_dir().join(name)
}

/// `key = value` from er_mario.ini (lines starting with # or ; are comments).
/// A value without what follows it on the line: `"quoted"` up to its closing quote, else up to a
/// `#`, `;` or ` (` note (`hud = off   (the overlay)` is "off").
fn value(v: &str) -> String {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('"') {
        return rest.split('"').next().unwrap_or("").to_string();
    }
    let end = [v.find('#'), v.find(';'), v.find(" (")].into_iter().flatten().min().unwrap_or(v.len());
    v[..end].trim().to_string()
}

pub fn config(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(file(CONFIG)).ok()?;
    text.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        let (k, v) = line.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| value(v))
    })
}

/// The ROM file: `rom = ...` from the config (absolute, or relative to the mod folder), else the
/// first N64 ROM in the mod folder.
pub fn rom_path() -> Option<PathBuf> {
    if let Some(p) = config("rom").filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return Some(if p.is_absolute() { p } else { mod_dir().join(p) });
    }
    let mut roms: Vec<PathBuf> = std::fs::read_dir(mod_dir())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["z64", "n64", "v64"].contains(&e.to_ascii_lowercase().as_str()))
        })
        .collect();
    roms.sort();
    roms.into_iter().next()
}

/// Reads the ROM and brings it into .z64 (big-endian) byte order. Only the US version works (the
/// data offsets libsm64 and the mod use are the US ROM's).
pub fn read_rom() -> Result<Vec<u8>, String> {
    let path = rom_path().ok_or_else(|| format!("no SM64 ROM in {} (and no rom = ... in {CONFIG})", mod_dir().display()))?;
    let mut rom = std::fs::read(&path).map_err(|e| format!("could not read ROM {}: {e}", path.display()))?;
    match rom.get(..4) {
        Some([0x80, 0x37, 0x12, 0x40]) => {}
        // .v64: byte-swapped 16-bit words
        Some([0x37, 0x80, 0x40, 0x12]) => rom.chunks_exact_mut(2).for_each(|c| c.swap(0, 1)),
        // .n64: little-endian 32-bit words
        Some([0x40, 0x12, 0x37, 0x80]) => rom.chunks_exact_mut(4).for_each(|c| c.reverse()),
        _ => return Err(format!("{} is not an N64 ROM", path.display())),
    }
    let name = String::from_utf8_lossy(&rom[0x20..0x34]).trim().to_string();
    if name != "SUPER MARIO 64" || rom.get(0x3E) != Some(&b'E') || rom.len() != 8 * 1024 * 1024 {
        return Err(format!("{} is not the US Super Mario 64 ROM ({name:?})", path.display()));
    }
    Ok(rom)
}
