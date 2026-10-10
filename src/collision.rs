//! Builds SM64 collision around Mario from Elden Ring raycasts.
//!
//! Floors: a grid of downward rays -> heightfield triangles.
//! Walls: fans of horizontal rays -> small wall panels facing Mario.

use eldenring::{
    cs::{CSHavokMan, PlayerIns},
    position::{HavokPosition, PositionDelta},
};
use fromsoftware_shared::FromStatic;

use crate::sm64::SM64Surface;
use crate::{SCALE, log};

thread_local! {
    static STATS: std::cell::RefCell<(u32,)> = const { std::cell::RefCell::new((0,)) };
}

const GRID_HALF: i32 = 8; // 17 x 17 samples
const GRID_STEP: f32 = 50.0; // SM64 units (0.5 m)
const RAY_UP: f32 = 150.0; // start rays this far above Mario's feet
const RAY_DOWN: f32 = 4000.0; // 40 m
const STEP_MAX: f32 = 125.0; // taller height jumps inside a cell are cliffs, not floor
const WALL_DIRS: usize = 64;
const WALL_HEIGHTS: [f32; 4] = [20.0, 70.0, 120.0, 170.0];
const WALL_REACH: f32 = 300.0;
const BACKOFF: f32 = 50.0;

pub fn sm_to_er(origin: [f32; 3], p: [f32; 3]) -> HavokPosition {
    // SM64 and Havok are mirrored on X
    HavokPosition(origin[0] - p[0] * SCALE, origin[1] + p[1] * SCALE, origin[2] + p[2] * SCALE, 0.0)
}

pub fn er_to_sm(origin: [f32; 3], p: &HavokPosition) -> [f32; 3] {
    [-(p.0 - origin[0]) / SCALE, (p.1 - origin[1]) / SCALE, (p.2 - origin[2]) / SCALE]
}

fn sm_delta(d: [f32; 3]) -> PositionDelta {
    PositionDelta(-d[0] * SCALE, d[1] * SCALE, d[2] * SCALE)
}

pub struct Caster<'a> {
    pub filter: u32,
    pub origin: [f32; 3],
    pub player: &'a PlayerIns,
}

impl Caster<'_> {
    /// Ray in SM64 space; returns the hit point in SM64 space.
    pub fn cast(&self, from: [f32; 3], delta: [f32; 3]) -> Option<[f32; 3]> {
        let havok = unsafe { CSHavokMan::instance() }.ok()?;
        let start = sm_to_er(self.origin, from);
        havok
            .phys_world
            .cast_ray(self.filter, &start, sm_delta(delta), self.player)
            .map(|hit| er_to_sm(self.origin, &hit))
    }
}

fn tri(a: [f32; 3], b: [f32; 3], c: [f32; 3], facing: [f32; 3]) -> SM64Surface {
    // SM64 normal = (b - a) x (c - b); flip winding so it points along `facing`
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let dot = n[0] * facing[0] + n[1] * facing[1] + n[2] * facing[2];
    let r = |p: [f32; 3]| [p[0].round() as i32, p[1].round() as i32, p[2].round() as i32];
    if dot >= 0.0 { SM64Surface::grass([r(a), r(b), r(c)]) } else { SM64Surface::grass([r(a), r(c), r(b)]) }
}

/// Surfaces around `mario` (SM64 space). Empty if nothing was hit.
pub fn build(c: &Caster, mario: [f32; 3]) -> Vec<SM64Surface> {
    let mut out = Vec::new();
    let cx = (mario[0] / GRID_STEP).round() * GRID_STEP;
    let cz = (mario[2] / GRID_STEP).round() * GRID_STEP;
    let n = (GRID_HALF * 2 + 1) as usize;

    // heightfield
    let mut h = vec![None; n * n];
    for i in 0..n {
        for j in 0..n {
            let x = cx + (i as i32 - GRID_HALF) as f32 * GRID_STEP;
            let z = cz + (j as i32 - GRID_HALF) as f32 * GRID_STEP;
            h[i * n + j] = c.cast([x, mario[1] + RAY_UP, z], [0.0, -RAY_DOWN, 0.0]).map(|p| p[1]);
        }
    }
    for i in 0..n - 1 {
        for j in 0..n - 1 {
            let (Some(a), Some(b), Some(d), Some(e)) = (h[i * n + j], h[(i + 1) * n + j], h[i * n + j + 1], h[(i + 1) * n + j + 1])
            else {
                continue;
            };
            let lo = a.min(b).min(d).min(e);
            let hi = a.max(b).max(d).max(e);
            if hi - lo > STEP_MAX {
                continue;
            }
            let x0 = cx + (i as i32 - GRID_HALF) as f32 * GRID_STEP;
            let z0 = cz + (j as i32 - GRID_HALF) as f32 * GRID_STEP;
            let (x1, z1) = (x0 + GRID_STEP, z0 + GRID_STEP);
            let up = [0.0, 1.0, 0.0];
            out.push(tri([x0, a, z0], [x1, b, z0], [x1, e, z1], up));
            out.push(tri([x0, a, z0], [x1, e, z1], [x0, d, z1], up));
        }
    }
    let floors = out.len();

    let mut dbg = ScanDebug { nearest: f32::MAX, ..Default::default() };
    // walls: a lidar-style ring scan, neighbouring hits stitched into a mesh
    let mut ring = vec![None; WALL_DIRS * WALL_HEIGHTS.len()];
    for k in 0..WALL_DIRS {
        let ang = k as f32 / WALL_DIRS as f32 * std::f32::consts::TAU;
        let d = [ang.sin(), 0.0, ang.cos()];
        for (hi, hh) in WALL_HEIGHTS.iter().enumerate() {
            // start behind Mario so a wall he is touching is never behind the ray's origin
            let from = [mario[0] - d[0] * BACKOFF, mario[1] + hh, mario[2] - d[2] * BACKOFF];
            let r = BACKOFF + WALL_REACH;
            let hit = c.cast(from, [d[0] * r, 0.0, d[2] * r]);
            if hit.is_some() {
                dbg.hits += 1;
            }
            // hits behind Mario's centre are walls he's already past: ignore them
            let hit = hit.filter(|p| (p[0] - mario[0]) * d[0] + (p[2] - mario[2]) * d[2] > 0.0);
            if hit.is_none() && dbg.hits > dbg.behind + dbg.kept {
                dbg.behind += 1;
            } else if let Some(p) = hit {
                dbg.kept += 1;
                let dd = ((p[0] - mario[0]).powi(2) + (p[2] - mario[2]).powi(2)).sqrt();
                dbg.nearest = dbg.nearest.min(dd);
            }
            ring[k * WALL_HEIGHTS.len() + hi] = hit;
        }
    }
    let at = |k: usize, h: usize| ring[(k % WALL_DIRS) * WALL_HEIGHTS.len() + h];
    let dist = |p: [f32; 3]| ((p[0] - mario[0]).powi(2) + (p[2] - mario[2]).powi(2)).sqrt();
    // lidar-style segmentation: neighbouring hits belong to the same surface unless the distance
    // from Mario jumps (a doorway or gap); grazing walls and pillar corners change smoothly
    let joined = |p: [f32; 3], q: [f32; 3]| {
        let (a, b) = (dist(p), dist(q));
        (a - b).abs() < 15.0 + 0.35 * a.max(b)
    };
    for k in 0..WALL_DIRS {
        for h in 0..WALL_HEIGHTS.len() - 1 {
            let (Some(p00), Some(p10), Some(p01), Some(p11)) = (at(k, h), at(k + 1, h), at(k, h + 1), at(k + 1, h + 1))
            else {
                continue;
            };
            if !(joined(p00, p10) && joined(p01, p11) && joined(p00, p01) && joined(p10, p11)) {
                dbg.unjoined += 1;
                continue;
            }
            // facing Mario, and only (near) vertical pieces: floors come from the heightfield
            let mid = [(p00[0] + p11[0]) / 2.0, 0.0, (p00[2] + p11[2]) / 2.0];
            let facing = [mario[0] - mid[0], 0.0, mario[2] - mid[2]];
            let u = [p10[0] - p00[0], p10[1] - p00[1], p10[2] - p00[2]];
            let v = [p01[0] - p00[0], p01[1] - p00[1], p01[2] - p00[2]];
            let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if l < 1e-3 || (n[1] / l).abs() > 0.45 {
                dbg.steep += 1;
                continue;
            }
            out.push(tri(p00, p10, p11, facing));
            out.push(tri(p00, p11, p01, facing));
        }
    }
    if out.is_empty() {
        log("collision: no hits around mario");
    }
    let _ = (floors, &dbg);
    out
}

/// A surface in Elden Ring world space (survives re-centring of SM64's origin).
#[derive(Clone, Copy)]
pub struct WorldTri {
    pub v: [[f32; 3]; 3],
    pub kind: i16,
    pub terrain: u16,
}

pub fn is_wall(s: &SM64Surface) -> bool {
    let [a, b, c] = s.vertices.map(|p| p.map(|x| x as f32));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    l > 0.0 && (n[1] / l).abs() < 0.45
}

pub fn to_world(origin: [f32; 3], s: &SM64Surface) -> WorldTri {
    let v = s.vertices.map(|p| {
        let h = sm_to_er(origin, p.map(|x| x as f32));
        [h.0, h.1, h.2]
    });
    WorldTri { v, kind: s.kind, terrain: s.terrain }
}

pub fn from_world(origin: [f32; 3], w: &WorldTri) -> SM64Surface {
    let v = w.v.map(|p| er_to_sm(origin, &HavokPosition(p[0], p[1], p[2], 0.0)).map(|x| x.round() as i32));
    SM64Surface { kind: w.kind, force: 0, terrain: w.terrain, vertices: v }
}

/// Logs what every candidate filter hits straight ahead at chest height (finding asset collision).
pub fn probe_forward(player: &PlayerIns, feet: [f32; 3], forward: [f32; 3]) {
    let Ok(havok) = (unsafe { CSHavokMan::instance() }) else { return };
    let start = HavokPosition(feet[0] - forward[0] * 0.5, feet[1] + 1.0, feet[2] - forward[2] * 0.5, 0.0);
    let delta = PositionDelta(forward[0] * 5.0, 0.0, forward[2] * 5.0);
    let mut candidates: Vec<u32> = (0..32).map(|b| 1u32 << b).collect();
    candidates.extend([0, 0xFFFF_FFFF, 0x08 | 0x0200_0000]);
    for f in candidates {
        match havok.phys_world.cast_ray(f, &start, delta, player) {
            Some(hit) => {
                let d = ((hit.0 - start.0).powi(2) + (hit.2 - start.2).powi(2)).sqrt();
                crate::dlog(format!("  forward {f:#010x}: hit at {d:.2} m"));
            }
            None => crate::dlog(format!("  forward {f:#010x}: miss")),
        }
    }
}

#[derive(Default)]
struct ScanDebug {
    hits: u32,
    behind: u32,
    kept: u32,
    nearest: f32,
    unjoined: u32,
    steep: u32,
}
