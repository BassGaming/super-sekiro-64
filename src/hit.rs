//! Mario's own hits: bullets the game fires for Sekiro, so his punches, kicks, dives and ground
//! pounds go through Sekiro's damage pipeline (damage, posture, hit reactions, aggro) without
//! the sword being swung.
//!
//! Found in Sekiro 1.6.0.0:
//! ```text
//! [0x143D84F70]  SprjBulletManager (FD4Singleton)
//! 0x140B1AD20    spawn(manager, &mut handle: i32, &request) (callers: 0x140868440, 0x140FA5EA0)
//! 0x1407D2C40    request constructor (0xB8 bytes; 0x140B07190 releases it)
//! request +0x00  owner: the character's handle (ChrIns+8)
//!         +0x04  BehaviorParam row (-1: none; 0x140B07720 takes the bullet from its refId)
//!         +0x10  Bullet param row, used as is when +0x04 is -1
//!         +0x3C  flags: the game's callers set bits 0 and 2, clear 1 and 3
//!         +0x40  emitter transform, 3 x 4 (the 4 x 4 world matrix transposed: x, y, z rows with
//!                the translation in w); +0x70 the same again
//! ```
//! The bullets are rows 1-3 and 60 of the Bullet param (test rows nothing references), rewritten as
//! short, still hits around a point that carry the sword's own attack rows (AtkParam_Pc, so the
//! hit scales with Sekiro's attack power like a sword swing). Params come from
//! `SoloParamRepository`, found by their type names (BULLET_PARAM_ST, ATK_PARAM_ST).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use fromsoftware_shared::FromStatic;
use glam::Vec3;

use crate::log;
use crate::mem::{read_u32, read_u64, readable};

#[repr(C)]
#[fromsoftware_shared::singleton("SoloParamRepository")]
struct SoloParamRepository {
    _vftable: usize,
}

const BULLET_MANAGER: usize = 0x143D84F70;
const SPAWN: usize = 0x140B1AD20;
const REQUEST_NEW: usize = 0x1407D2C40;
const REQUEST_DROP: usize = 0x140B07190;

/// Mario's kinds of hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attack {
    /// punches and kicks (in front of him)
    Punch,
    /// dives and slide kicks (in front of him, wider)
    Dive,
    /// ground pound landings (all around him)
    Pound,
    /// a thrown boss crashing into something (around him)
    Throw,
}

impl Attack {
    /// (Bullet row, default AtkParam_Pc row, radius m, ini key)
    fn spec(self) -> (i32, i32, f32, &'static str) {
        match self {
            // 5000000: the Kusabimaru's first slash; 5000010: a heavier one (more posture)
            Attack::Punch => (1, 5000000, 0.7, "atk_punch"),
            Attack::Dive => (2, 5000000, 0.9, "atk_dive"),
            Attack::Pound => (3, 5000010, 1.6, "atk_pound"),
            Attack::Throw => (60, 5000010, 2.0, "atk_throw"),
        }
    }
    const ALL: [Attack; 4] = [Attack::Punch, Attack::Dive, Attack::Pound, Attack::Throw];
}

// BULLET_PARAM_ST (0x100 bytes)
const B_ATK: usize = 0x00;
const B_SFX: [usize; 3] = [0x04, 0x08, 0x0C];
const B_LIFE: usize = 0x10;
const B_HIT_RADIUS: usize = 0x44;
const B_HIT_RADIUS_MAX: usize = 0x48;
const B_SPREAD_TIME: usize = 0x4C;
const B_SP_EFFECT_SHOOTER: usize = 0x60;
const B_HIT_BULLET: usize = 0x68;
const B_SP_EFFECTS: usize = 0x6C;
const B_FLAGS_9B: usize = 0x9B; // bit 1: IsHitBothTeam
const B_EMITTER_BULLET: usize = 0xAC;
/// Bullet 4: a still 0.1 s hit around its point (the template)
const TEMPLATE: i32 = 4;

/// A param table in memory: (header, rows).
struct Table {
    header: usize,
    rows: Vec<(i32, usize)>,
}

impl Table {
    fn row(&self, id: i32) -> Option<usize> {
        self.rows.iter().find(|(r, _)| *r == id).map(|(_, a)| *a)
    }
}

static READY: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);
static SPAWNED: AtomicU32 = AtomicU32::new(0);
static TRIED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Whether Mario's hits are bullets (the params are patched) rather than Sekiro's sword.
pub fn ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Finds and patches the params (once they're loaded; retried every 2 s until then).
pub fn update() {
    if READY.load(Ordering::Relaxed) || FAILED.load(Ordering::Relaxed) {
        return;
    }
    if crate::paths::config("sword_hits").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) {
        log("hit: sword_hits is on in the ini: Mario's hits stay Sekiro's sword");
        FAILED.store(true, Ordering::Relaxed);
        return;
    }
    {
        let mut t = TRIED.lock().unwrap_or_else(|e| e.into_inner());
        if t.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
            return;
        }
        *t = Some(std::time::Instant::now());
    }
    match patch() {
        Ok(()) => READY.store(true, Ordering::Relaxed),
        Err(e) => {
            static TRIES: AtomicU32 = AtomicU32::new(0);
            let n = TRIES.fetch_add(1, Ordering::Relaxed);
            if n < 3 || n % 15 == 0 {
                log(format!("hit: not ready yet ({e}); Mario's hits are still Sekiro's sword"));
            }
            if n >= 60 {
                log("hit: giving up; Mario's hits stay Sekiro's sword");
                FAILED.store(true, Ordering::Relaxed);
            }
        }
    }
}

fn patch() -> Result<(), String> {
    if read_u64(BULLET_MANAGER).is_none_or(|m| m == 0) {
        return Err("no bullet manager".into());
    }
    let repo = <SoloParamRepository as FromStatic>::instance_ptr().map_err(|e| format!("SoloParamRepository: {e:?}"))?;
    let repo = repo as usize;
    let tables = find_tables(repo);
    let bullet = tables.iter().find(|(t, _)| t == "BULLET_PARAM_ST").map(|(_, t)| t);
    let atk_pc = tables.iter().filter(|(t, _)| t == "ATK_PARAM_ST").map(|(_, t)| t).find(|t| t.row(5000000).is_some());
    let names: Vec<String> = tables.iter().map(|(n, t)| format!("{n} ({} rows)", t.rows.len())).collect();
    let (Some(bullet), Some(atk_pc)) = (bullet, atk_pc) else {
        return Err(format!("params not found in SoloParamRepository {repo:#x}: {} tables [{}]", tables.len(), names.join(", ")));
    };
    log(format!(
        "hit: Bullet param at {:#x} ({} rows), AtkParam_Pc at {:#x} ({} rows); {} tables in all",
        bullet.header,
        bullet.rows.len(),
        atk_pc.header,
        atk_pc.rows.len(),
        tables.len()
    ));
    let template = bullet.row(TEMPLATE).ok_or("no bullet row 4")?;
    let mut tpl = [0u8; 0x100];
    if !crate::mem::read_bytes(template, &mut tpl) {
        return Err("bullet row 4 unreadable".into());
    }
    for attack in Attack::ALL {
        let (row, atk_default, radius, key) = attack.spec();
        let atk = crate::paths::config(key).and_then(|v| v.parse().ok()).unwrap_or(atk_default);
        if atk_pc.row(atk).is_none() {
            return Err(format!("{key} = {atk} is no AtkParam_Pc row"));
        }
        let at = bullet.row(row).ok_or(format!("no bullet row {row}"))?;
        let mut b = tpl;
        let put_i32 = |b: &mut [u8; 0x100], o: usize, v: i32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put_f32 = |b: &mut [u8; 0x100], o: usize, v: f32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        put_i32(&mut b, B_ATK, atk);
        for o in B_SFX {
            put_i32(&mut b, o, -1);
        }
        put_f32(&mut b, B_LIFE, 0.1);
        put_f32(&mut b, B_HIT_RADIUS, radius);
        put_f32(&mut b, B_HIT_RADIUS_MAX, radius);
        put_f32(&mut b, B_SPREAD_TIME, 0.0);
        put_i32(&mut b, B_SP_EFFECT_SHOOTER, -1);
        put_i32(&mut b, B_HIT_BULLET, -1);
        for k in 0..5 {
            put_i32(&mut b, B_SP_EFFECTS + k * 4, -1);
        }
        put_i32(&mut b, B_EMITTER_BULLET, -1);
        b[B_FLAGS_9B] &= !0x02; // only enemies
        if !write_bytes(at, &b) {
            return Err(format!("bullet row {row} not writable"));
        }
        log(format!("hit: {attack:?} is bullet {row}: attack {atk}, radius {radius} m"));
    }
    Ok(())
}

/// The param tables reachable from the repository (two pointer hops, then the file header):
/// (type name, table).
fn find_tables(repo: usize) -> Vec<(String, Table)> {
    let mut out: Vec<(String, Table)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let ptrs = |obj: usize, len: usize| -> Vec<usize> {
        let mut buf = vec![0u8; len];
        if !crate::mem::read_bytes(obj, &mut buf) {
            return Vec::new();
        }
        buf.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap()) as usize).filter(|&p| p > 0x10000 && p < 0x8000_0000_0000 && p % 8 == 0).collect()
    };
    for a in ptrs(repo, 0x4000) {
        for b in std::iter::once(a).chain(ptrs(a, 0x100)) {
            for h in std::iter::once(b).chain(ptrs(b, 0x100)) {
                if !seen.insert(h) {
                    continue;
                }
                if let Some(t) = table_at(h) {
                    out.push(t);
                }
            }
        }
    }
    out
}

/// A param file at `h`: type string offset at +0x10 (u64), row count at +0xA, rows from +0x40
/// (id, pad, data offset, name offset).
fn table_at(h: usize) -> Option<(String, Table)> {
    let mut head = [0u8; 0x40];
    if !crate::mem::read_bytes(h, &mut head) {
        return None;
    }
    let rows = u16::from_le_bytes([head[0xA], head[0xB]]) as usize;
    let type_off = u64::from_le_bytes(head[0x10..0x18].try_into().unwrap()) as usize;
    if rows == 0 || !(0x40 + rows * 0x18..0x100_0000).contains(&type_off) {
        return None;
    }
    let mut name = [0u8; 32];
    if !crate::mem::read_bytes(h + type_off, &mut name) {
        return None;
    }
    let len = name.iter().position(|&c| c == 0)?;
    let name = std::str::from_utf8(&name[..len]).ok()?;
    if len < 4 || !name.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_') {
        return None;
    }
    let mut table = vec![0u8; rows * 0x18];
    if !crate::mem::read_bytes(h + 0x40, &mut table) {
        return None;
    }
    let rows = table
        .chunks_exact(0x18)
        .map(|r| (i32::from_le_bytes(r[0..4].try_into().unwrap()), h + u64::from_le_bytes(r[8..16].try_into().unwrap()) as usize))
        .collect();
    Some((name.to_string(), Table { header: h, rows }))
}

fn write_bytes(addr: usize, data: &[u8]) -> bool {
    use windows::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut n = 0usize;
    unsafe { WriteProcessMemory(GetCurrentProcess(), addr as *const _, data.as_ptr() as *const _, data.len(), Some(&mut n)) }.is_ok()
        && n == data.len()
}

/// Fires Mario's hit at `at` (world coordinates, the middle of the hit), facing `forward` (world,
/// horizontal). Runs on the game's thread (a task).
pub fn fire(attack: Attack, at: Vec3, forward: Vec3) {
    if !ready() {
        return;
    }
    let player = crate::player::ptr();
    let Some(manager) = read_u64(BULLET_MANAGER).map(|m| m as usize).filter(|&m| m != 0) else { return };
    let Some(owner) = read_u32(player + 8).filter(|_| player != 0) else { return };
    if !at.is_finite() || !readable(SPAWN, 16) {
        return;
    }
    let (row, ..) = attack.spec();
    // axes: z forward (where Mario faces), y up, x = y cross z
    let z = Vec3::new(forward.x, 0.0, forward.z).try_normalize().unwrap_or(Vec3::Z);
    let (y, x) = (Vec3::Y, Vec3::Y.cross(z));
    let m: [f32; 12] = [x.x, y.x, z.x, at.x, x.y, y.y, z.y, at.y, x.z, y.z, z.z, at.z];
    #[repr(C, align(16))]
    struct Request([u8; 0x100]);
    let mut req = Request([0; 0x100]);
    let p = req.0.as_mut_ptr();
    let mut handle: i32 = -1;
    unsafe {
        let new: unsafe extern "system" fn(*mut u8) -> *mut u8 = std::mem::transmute(REQUEST_NEW);
        let spawn: unsafe extern "system" fn(usize, *mut i32, *mut u8) -> usize = std::mem::transmute(SPAWN);
        let drop: unsafe extern "system" fn(*mut u8) = std::mem::transmute(REQUEST_DROP);
        new(p);
        (p as *mut u32).write(owner);
        (p.add(4) as *mut i32).write(-1);
        (p.add(0x10) as *mut i32).write(row);
        let flags = (p.add(0x3C) as *mut u32).read();
        (p.add(0x3C) as *mut u32).write((flags | 1 | 4) & !(2 | 8));
        std::ptr::copy_nonoverlapping(m.as_ptr(), p.add(0x40) as *mut f32, 12);
        std::ptr::copy_nonoverlapping(m.as_ptr(), p.add(0x70) as *mut f32, 12);
        spawn(manager, &mut handle, p);
        drop(p);
    }
    if handle != -1 {
        crate::dmg::fired(attack, at, attack.spec().2);
    }
    let n = SPAWNED.fetch_add(1, Ordering::Relaxed);
    if n < 12 || (handle == -1 && n < 60) {
        log(format!("hit: {attack:?} at ({:.2}, {:.2}, {:.2}): bullet {row}, owner {owner:#x}, handle {handle}", at.x, at.y, at.z));
    }
}
