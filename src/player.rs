//! Sekiro local-player reader.
//!
//! Offsets discovered by `recon` for Sekiro 1.6.0.0 (the version we guard against):
//! ```text
//! WorldChrMan + 0x88   -> PlayerIns
//! PlayerIns   + 0x1ff8 -> ChrInsModuleContainer
//! container   + 0x68   -> SprjChrPhysicsModule
//! physics     + 0x80   -> world position (x, y, z, 1)
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};

use fromsoftware_shared::{FromStatic, Program};

use crate::mem::{class_name_of, read_f32, read_u64};

#[repr(C)]
#[fromsoftware_shared::singleton("WorldChrMan")]
struct WorldChrMan {
    _vftable: usize,
}

pub const WCM_MAIN_PLAYER: usize = 0x88;
pub const PLAYER_MODULES: usize = 0x1ff8;
pub const CONTAINER_PHYSICS: usize = 0x68;
pub const PHYSICS_POSITION: usize = 0x80;
/// `NoGravity` (byte): from the game's debug print "EventId:[%d] [NoDead:%d] [IsHide:%d]
/// [NoGravity:%d] [NoDamage:%d]" (code at 0x1409FC17B reads physics+0x92A for it).
pub const PHYSICS_NO_GRAVITY: usize = 0x92a;
/// Draw flags: bit 3 = IsDraw (debug print "IsEnableDraw:%d (IsDraw:%d ...)", 0x1409FB77A).
pub const PLAYER_DRAW_FLAGS: usize = 0x1a11;
/// One of the opacity multipliers `IsEnableDraw` requires to be > 0 (0x1409D8470 multiplies
/// +0x1A44, +0x1A48, +0x1A50, +0x1A54, +0x1A58).
pub const PLAYER_OPACITY: usize = 0x1a58;

static PLAYER_PTR: AtomicUsize = AtomicUsize::new(0);

/// Refreshes the local player pointer (cheap; re-checks the class name only when it changes).
pub fn update() -> Option<usize> {
    let wcm = <WorldChrMan as FromStatic>::instance_ptr().ok()?;
    if wcm.is_null() {
        return None;
    }
    let player = read_u64(wcm as usize + WCM_MAIN_PLAYER)? as usize;
    if player == 0 {
        return None;
    }
    if player != PLAYER_PTR.load(Ordering::Relaxed) {
        let program = Program::current();
        match class_name_of(&program, player) {
            Some(name) if name.to_ascii_lowercase().contains("player") => {
                PLAYER_PTR.store(player, Ordering::Relaxed);
            }
            _ => return None,
        }
    }
    Some(player)
}

pub fn ptr() -> usize {
    PLAYER_PTR.load(Ordering::Relaxed)
}

/// The player's `SprjChrPhysicsModule` (the module that holds position and Havok state).
pub fn physics_ptr() -> Option<usize> {
    let player = ptr();
    if player == 0 {
        return None;
    }
    let container = read_u64(player + PLAYER_MODULES)? as usize;
    read_u64(container + CONTAINER_PHYSICS).map(|p| p as usize)
}

/// The player's world position in metres.
pub fn position() -> Option<[f32; 3]> {
    let player = ptr();
    if player == 0 {
        return None;
    }
    let container = read_u64(player + PLAYER_MODULES)? as usize;
    if container == 0 {
        return None;
    }
    let physics = read_u64(container + CONTAINER_PHYSICS)? as usize;
    if physics == 0 {
        return None;
    }
    Some([
        read_f32(physics + PHYSICS_POSITION)?,
        read_f32(physics + PHYSICS_POSITION + 4)?,
        read_f32(physics + PHYSICS_POSITION + 8)?,
    ])
}

/// Moves the player (writes the physics module's world position; `w` is left alone).
pub fn set_position(p: [f32; 3]) -> bool {
    let Some(physics) = physics_ptr().filter(|&p| p >= 0x10000) else { return false };
    if !p.iter().all(|v| v.is_finite()) {
        return false;
    }
    let at = physics + PHYSICS_POSITION;
    unsafe {
        std::ptr::write_volatile(at as *mut f32, p[0]);
        std::ptr::write_volatile((at + 4) as *mut f32, p[1]);
        std::ptr::write_volatile((at + 8) as *mut f32, p[2]);
    }
    true
}

/// Turns the game's gravity for the player off (true) or back on.
pub fn set_no_gravity(on: bool) {
    let Some(physics) = physics_ptr().filter(|&p| p >= 0x10000) else { return };
    if crate::mem::readable(physics + PHYSICS_NO_GRAVITY, 1) {
        unsafe { std::ptr::write_volatile((physics + PHYSICS_NO_GRAVITY) as *mut u8, on as u8) };
    }
}

/// Hides (or shows again) Sekiro's character: opacity multiplier to 0 and the IsDraw bit cleared.
/// The game may set these back, so hiding is re-applied at several task groups each frame.
pub fn set_hidden(hidden: bool) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let player = ptr();
    if player == 0 || !crate::mem::readable(player + PLAYER_DRAW_FLAGS, 0x60) {
        return;
    }
    unsafe {
        let flags = (player + PLAYER_DRAW_FLAGS) as *mut u8;
        let opacity = (player + PLAYER_OPACITY) as *mut f32;
        if !LOGGED.swap(true, Ordering::Relaxed) {
            crate::log(format!(
                "player: draw flags {:#04x}, opacity multiplier {:.2} (hiding: {hidden})",
                std::ptr::read_volatile(flags),
                std::ptr::read_volatile(opacity)
            ));
        }
        let f = std::ptr::read_volatile(flags);
        std::ptr::write_volatile(flags, if hidden { f & !0x08 } else { f | 0x08 });
        std::ptr::write_volatile(opacity, if hidden { 0.0 } else { 1.0 });
    }
}

/// The player's (HP, max HP), from the data module (`ChrInsModuleContainer+0x18`, HP at +0x130,
/// max at +0x134). None if the values don't look like HP.
pub fn hp() -> Option<(i32, i32)> {
    let player = ptr();
    if player == 0 {
        return None;
    }
    let container = read_u64(player + PLAYER_MODULES)? as usize;
    let data = read_u64(container + 0x18)? as usize;
    let hp = crate::mem::read_u32(data + 0x130)? as i32;
    let max = crate::mem::read_u32(data + 0x134)? as i32;
    (max > 0 && max < 1_000_000 && (0..=max).contains(&hp)).then_some((hp, max))
}

/// The physics module's yaw (radians): +0x74, between zeros at +0x70 / +0x78 (the load log
/// showed +0x70 [0, 0.732, 0, 0] with the camera looking along -(sin, cos) of it). The character
/// faces -(sin yaw, 0, cos yaw).
pub const PHYSICS_YAW: usize = 0x74;

/// Where a character faces (horizontal, world), from its physics module.
pub fn facing_of(physics: usize) -> Option<glam::Vec3> {
    let yaw = crate::mem::read_f32(physics + PHYSICS_YAW).filter(|y| y.is_finite() && y.abs() < 7.0)?;
    Some(glam::Vec3::new(-yaw.sin(), 0.0, -yaw.cos()))
}

/// Turns the character to face `q` (a rotation about Y; the quaternion of Mario's facing, as
/// from_rotation_y(PI - face)): written as the physics module's yaw, if that field looks like
/// one (zeros around it). The first call logs what it found.
pub fn set_facing(q: glam::Quat) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(physics) = physics_ptr().filter(|&p| p >= 0x10000) else { return };
    if !crate::mem::readable(physics + 0x70, 0x10) {
        return;
    }
    let around = unsafe { std::ptr::read_volatile((physics + 0x70) as *const [f32; 4]) };
    let plausible = around[0] == 0.0 && around[2] == 0.0 && around[1].is_finite() && around[1].abs() < 7.0;
    let (axis, angle) = q.to_axis_angle();
    let yaw = if axis.y < 0.0 { -angle } else { angle };
    // as the game keeps it: within (-PI, PI]
    let yaw = (yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    if !LOGGED.swap(true, Ordering::Relaxed) {
        crate::log(format!("player: physics +0x70 {around:.3?}: yaw {} -> {yaw:.3}", if plausible { "found" } else { "not found, left alone" }));
    }
    if plausible {
        unsafe { std::ptr::write_volatile((physics + PHYSICS_YAW) as *mut f32, yaw) };
    }
}

/// The data module's flags (+0x228): bit 0 NoDead, bit 1 NoDamage (the game's debug print at
/// 0x1409FC16F reads them through 0x140BD5DF0 / 0x140BD5DB0).
pub const DATA_FLAGS: usize = 0x228;
pub const NO_DEAD: u8 = 1;
pub const NO_DAMAGE: u8 = 2;

/// Sets or clears data-module flags of the character at `chr` (a ChrIns).
pub fn set_data_flag(chr: usize, flag: u8, on: bool) {
    let Some(container) = read_u64(chr + PLAYER_MODULES).map(|c| c as usize).filter(|&c| c >= 0x10000) else { return };
    let Some(data) = read_u64(container + 0x18).map(|d| d as usize).filter(|&d| d >= 0x10000) else { return };
    if !crate::mem::readable(data + DATA_FLAGS, 1) {
        return;
    }
    unsafe {
        let f = (data + DATA_FLAGS) as *mut u8;
        let v = std::ptr::read_volatile(f);
        std::ptr::write_volatile(f, if on { v | flag } else { v & !flag });
    }
}

/// The container offset of the player's `SprjChrThrowModule` (found by its class name once).
static THROW_MODULE: AtomicUsize = AtomicUsize::new(0);

/// Whether the game offers a deathblow right now: the throw module's current throw target
/// (+0xA8, "CurrentThrowTarget" in its debug view at 0x140BE2263) is set.
pub fn deathblow_ready() -> bool {
    let player = ptr();
    if player == 0 {
        return false;
    }
    let Some(container) = read_u64(player + PLAYER_MODULES).map(|c| c as usize).filter(|&c| c >= 0x10000) else { return false };
    let mut off = THROW_MODULE.load(Ordering::Relaxed);
    if off == 0 {
        let program = Program::current();
        for o in (0..0x200).step_by(8) {
            let Some(m) = read_u64(container + o).map(|m| m as usize).filter(|&m| m >= 0x10000) else { continue };
            if class_name_of(&program, m).is_some_and(|n| n.contains("ThrowModule")) {
                crate::log(format!("player: throw module at container +{o:#x}"));
                off = o;
                break;
            }
        }
        if off == 0 {
            return false;
        }
        THROW_MODULE.store(off, Ordering::Relaxed);
    }
    let Some(throw) = read_u64(container + off).map(|m| m as usize).filter(|&m| m >= 0x10000) else { return false };
    read_u64(throw + 0xA8).is_some_and(|t| t >= 0x10000)
}
