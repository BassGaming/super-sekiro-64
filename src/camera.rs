//! Sekiro's camera, read from the `SprjCamera` singleton.
//!
//! `SprjCamera` holds `SprjPersCam`s at `+0x8..+0x20`. Like Elden Ring's `CSCam`, each has a 4x4
//! matrix at `+0x10` (rows: right, up, forward, position) followed by fov/aspect/near/far. We don't
//! trust that blindly: a camera is only used when its first three rows are orthonormal and its
//! position is near the player, and the forward row is flipped if it points away from the player.

use std::sync::atomic::{AtomicBool, Ordering};

use fromsoftware_shared::FromStatic;
use glam::Vec3;

use crate::mem::{read_f32, read_u64};

#[repr(C)]
#[fromsoftware_shared::singleton("SprjCamera")]
struct SprjCamera {
    _vftable: usize,
}

const PERS_CAMS: [usize; 4] = [0x8, 0x10, 0x18, 0x20];
const MATRIX: usize = 0x10;

#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub right: Vec3,
    pub up: Vec3,
    /// Look direction (towards the player).
    pub forward: Vec3,
    pub pos: Vec3,
}

static LOGGED: AtomicBool = AtomicBool::new(false);
static WARNED: AtomicBool = AtomicBool::new(false);

fn row(base: usize, i: usize) -> Option<Vec3> {
    let at = base + MATRIX + i * 0x10;
    let v = Vec3::new(read_f32(at)?, read_f32(at + 4)?, read_f32(at + 8)?);
    v.is_finite().then_some(v)
}

fn read_cam(pers: usize) -> Option<Cam> {
    Some(Cam { right: row(pers, 0)?, up: row(pers, 1)?, forward: row(pers, 2)?, pos: row(pers, 3)? })
}

fn orthonormal(c: &Cam) -> bool {
    let unit = |v: Vec3| (v.length() - 1.0).abs() < 0.02;
    unit(c.right)
        && unit(c.up)
        && unit(c.forward)
        && c.right.dot(c.up).abs() < 0.02
        && c.right.dot(c.forward).abs() < 0.02
        && c.up.dot(c.forward).abs() < 0.02
}

/// The game's render camera, or `None` if nothing plausible is found.
pub fn active(player: Vec3) -> Option<Cam> {
    let cam = <SprjCamera as FromStatic>::instance_ptr().ok().filter(|p| !p.is_null())? as usize;
    let cams: Vec<(usize, Option<Cam>)> = PERS_CAMS
        .iter()
        .map(|&slot| {
            let pers = read_u64(cam + slot).map(|p| p as usize).filter(|&p| p >= 0x10000);
            (slot, pers.and_then(read_cam))
        })
        .collect();

    if !LOGGED.swap(true, Ordering::Relaxed) {
        crate::log(format!("camera: SprjCamera @ {cam:#x}, player at {player:.2?}"));
        for (slot, c) in &cams {
            crate::log(format!("camera:   +{slot:#x}: {c:.3?}"));
        }
    }

    // the first valid camera within reach of the player (the render camera comes first)
    let pick = cams
        .iter()
        .filter_map(|(_, c)| *c)
        .find(|c| orthonormal(c) && c.pos.distance(player) < 40.0);
    let Some(mut c) = pick else {
        if !WARNED.swap(true, Ordering::Relaxed) {
            crate::log("camera: no plausible camera matrix found; movement stays world-relative");
        }
        return None;
    };
    if c.forward.dot(player - c.pos) < 0.0 {
        c.forward = -c.forward;
    }
    Some(c)
}

/// The `SprjPersCam`s, for writing.
fn pers_cams() -> Vec<usize> {
    let Some(cam) = <SprjCamera as FromStatic>::instance_ptr().ok().filter(|p| !p.is_null()) else { return Vec::new() };
    let mut out: Vec<usize> = PERS_CAMS
        .iter()
        .filter_map(|&slot| read_u64(cam as usize + slot).map(|p| p as usize).filter(|&p| p >= 0x10000))
        .collect();
    out.dedup();
    out
}

/// Writes rows right, up, forward, position into every camera of the set, keeping the game's
/// handedness (the sign of right · (up × forward)).
pub fn write(fwd: Vec3, pos: Vec3) {
    let fwd = fwd.normalize_or(Vec3::Z);
    for pers in pers_cams() {
        let Some(c) = read_cam(pers) else { continue };
        if !orthonormal(&c) {
            continue;
        }
        let handed = c.right.dot(c.up.cross(c.forward)).signum();
        let mut right = Vec3::Y.cross(fwd).normalize_or(Vec3::X);
        let up = fwd.cross(right).normalize_or(Vec3::Y);
        if right.dot(up.cross(fwd)).signum() != handed {
            right = -right;
        }
        let up = if up.y < 0.0 { -up } else { up };
        // (row 2 is the look direction: the loading screen's default camera at (0, 4, -8) looks
        // at the origin with forward (0, -0.447, 0.894))
        let rows = [right, up, fwd, pos];
        for (i, r) in rows.iter().enumerate() {
            let at = pers + MATRIX + i * 0x10;
            unsafe {
                std::ptr::write_volatile(at as *mut f32, r.x);
                std::ptr::write_volatile((at + 4) as *mut f32, r.y);
                std::ptr::write_volatile((at + 8) as *mut f32, r.z);
            }
        }
    }
}

/// The render camera's vertical field of view (radians), for projecting onto the screen.
pub fn fov() -> Option<f32> {
    let pers = *pers_cams().first()?;
    // fov, aspect, near, far follow the matrix (like Elden Ring's CSCam)
    let f = read_f32(pers + MATRIX + 0x40)?;
    (f > 0.1 && f < 3.0).then_some(f)
}

/// The game's load-screen camera: while loading it parks the render camera at (0, 4, -8) looking
/// at the origin (seen in every log). Nothing near the player is shown then.
pub fn is_load_screen() -> bool {
    let Some(pers) = pers_cams().first().copied() else { return true };
    let Some(c) = read_cam(pers) else { return true };
    c.pos.distance(Vec3::new(0.0, 4.0, -8.0)) < 0.01 && c.forward.distance(Vec3::new(0.0, -0.447, 0.894)) < 0.01
}
