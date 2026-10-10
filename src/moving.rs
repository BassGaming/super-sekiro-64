//! Moving collision (lifts, doors, pushed props) as libsm64 surface objects, so Mario rides and
//! bumps into them like SM64 platforms.
//!
//! After each static collision query the bodies it used are watched; the first time one of them
//! moves it is taken out of the static collision and becomes a surface object, updated every tick.

use std::collections::HashMap;

use eldenring::position::HavokPosition;
use glam::{Quat, Vec3};

use crate::havok_col::HavokCollision;
use crate::{collision, log, sm64, worker};

/// Most triangles per moving object (lift platforms are small; skip anything map-sized).
const MAX_TRIS: usize = 3000;
/// Stop tracking objects this far from Mario (SM64 units).
const FORGET: f32 = 4000.0;
/// A body that moves further than this in one tick (metres, degrees) was put there, not moved:
/// riding it along flung Mario ~40 m when a big box near a lift snapped round by 115 degrees.
const SNAP_METRES: f32 = 1.0;
const SNAP_DEGREES: f32 = 10.0;

fn snapped(p0: Vec3, q0: Quat, p: Vec3, q: Quat) -> bool {
    p0.distance(p) > SNAP_METRES || q0.angle_between(q).to_degrees() > SNAP_DEGREES
}

struct Tracked {
    id: u32,
    /// rotation when the object was built (its triangles are baked in that pose)
    q0: Quat,
    shape: usize,
    /// its triangles (body space), to measure how far Mario is from the nearest one
    mesh: std::sync::Arc<crate::havok_col::Mesh>,
    /// pose last tick
    last: (Vec3, Quat),
}

#[derive(Default)]
pub struct Moving {
    /// static bodies near Mario and their transform when last queried
    watch: HashMap<u32, (Vec3, Quat, usize)>,
    tracked: HashMap<u32, Tracked>,
}

fn sm(origin: [f32; 3], p: Vec3) -> Vec3 {
    Vec3::from(collision::er_to_sm(origin, &HavokPosition(p.x, p.y, p.z, 0.0)))
}

impl Moving {
    /// After a static query: remember where the bodies it used are now.
    pub fn watch_query(&mut self, h: &HavokCollision) {
        self.watch.clear();
        for &i in &h.last_bodies {
            if let Some((p, q)) = h.transform(i) {
                self.watch.insert(i, (p, q, h.shape_of(i)));
            }
        }
    }

    /// The world origin shifted (every body jumped): take a fresh snapshot instead of seeing motion.
    pub fn rewatch(&mut self, h: &HavokCollision) {
        self.watch_query(h);
    }

    /// Every tick. Returns true when the static collision must be rebuilt (a body became dynamic or
    /// went back to static).
    pub fn update(&mut self, h: &mut HavokCollision, origin: [f32; 3], mario: [f32; 3]) -> bool {
        h.refresh_bodies();
        let mut rebuild = false;
        // newly moving bodies -> surface objects
        let moved: Vec<u32> = self
            .watch
            .iter()
            .filter(|(i, (p0, q0, shape))| {
                !self.tracked.contains_key(i)
                    && h.shape_of(**i) == *shape
                    && h.transform(**i).is_some_and(|(p, q)| p.distance(*p0) > 0.01 || q.dot(*q0).abs() < 0.99999)
            })
            .map(|(i, _)| *i)
            .collect();
        for i in moved {
            let Some((p, q, shape)) = self.watch.remove(&i) else { continue };
            if let Some((p1, q1)) = h.transform(i).filter(|(p1, q1)| snapped(p, q, *p1, *q1)) {
                log(format!("moving: body #{i} snapped to a new pose, staying static ({p:.2?} {q:.3?} -> {p1:.2?} {q1:.3?})"));
                self.watch.insert(i, (p1, q1, shape));
                rebuild = true;
                continue;
            }
            let Some(mesh) = h.mesh_of(i) else { continue };
            // Build at the last static pose, attach riders there, then apply this tick's motion.
            // Building at the new pose discards the first displacement of a starting lift.
            if mesh.tris().len() > MAX_TRIS {
                continue;
            }
            let center = sm(origin, p);
            // convex shapes: every face points away from the middle (built once, so it must not depend
            // on where Mario happens to be)
            let convex_middle = (h.is_convex(i) || h.is_boxed(i) || mesh.small_closed()).then(|| {
                let local = mesh.tris().iter().flatten().copied().sum::<Vec3>() / (mesh.tris().len() * 3) as f32;
                sm(origin, q * local + p)
            });
            let mut surfaces = Vec::with_capacity(mesh.tris().len());
            for t in mesh.tris() {
                let w = t.map(|v| sm(origin, q * v + p));
                let local = w.map(|v| (v - center).round().to_array().map(|x| x as i32));
                let mid = convex_middle.map(|mid| (mid - center).to_array());
                if let Some(v) = crate::collision_geometry::surface_vertices(local, mid, None) {
                    surfaces.push(sm64::SM64Surface::grass(v));
                }
            }
            if surfaces.is_empty() {
                continue;
            }
            let n = surfaces.len();
            let transform = sm64::SM64ObjectTransform { position: center.into(), euler_rotation: [0.0; 3] };
            let id = worker::call("object create", move |_| {
                let object = sm64::SM64SurfaceObject { transform, surface_count: surfaces.len() as u32, surfaces: surfaces.as_ptr() };
                unsafe { sm64::sm64_surface_object_create(&object) }
            });
            if let Some(id) = id {
                log(format!("moving: body #{i} is moving, now a surface object ({n} triangles, small closed mesh {})", mesh.small_closed()));
                self.tracked.insert(i, Tracked { id, q0: q, shape: h.shape_of(i), mesh: mesh.clone(), last: (p, q) });
                h.exclude.insert(i);
                rebuild = true;
            }
        }
        // move tracked objects along with their bodies
        let mut moves = Vec::new();
        let mut forget = Vec::new();
        let mut snaps = Vec::new();
        for (i, t) in self.tracked.iter_mut() {
            if h.shape_of(*i) != t.shape {
                forget.push(*i);
                continue;
            }
            let Some((p, q)) = h.transform(*i) else {
                log(format!("moving: body #{i} gone (no transform), dropping"));
                forget.push(*i);
                continue;
            };
            if snapped(t.last.0, t.last.1, p, q) {
                log(format!("moving: body #{i} snapped to a new pose, back to static ({:.2?} {:.3?} -> {p:.2?} {q:.3?})", t.last.0, t.last.1));
                forget.push(*i);
                snaps.push((*i, p, q, t.shape));
                continue;
            }
            t.last = (p, q);
            let center = sm(origin, p);
            // outside the body's whole bounding sphere (+40 m)? Lifts are e.g. 258 m tall cylinders whose
            // top is the platform, so neither the origin nor the vertices say where Mario can touch it
            let dist = (center.distance(Vec3::from(mario)) - t.mesh.radius() / crate::SCALE).max(0.0);
            if dist > FORGET {
                log(format!("moving: body #{i} {dist:.0} units away, dropping"));
                forget.push(*i);
                continue;
            }
            let euler_rotation = platform_rotation(q * t.q0.inverse());
            moves.push((t.id, sm64::SM64ObjectTransform { position: center.into(), euler_rotation }));
        }
        if !moves.is_empty() {
            worker::call("object move", move |_| {
                for (id, transform) in &moves {
                    unsafe { sm64::sm64_surface_object_move(*id, transform) };
                }
            });
        }
        for (i, p, q, shape) in snaps {
            self.watch.insert(i, (p, q, shape));
        }
        for i in forget {
            if let Some(t) = self.tracked.remove(&i) {
                let id = t.id;
                worker::call("object delete", move |_| unsafe { sm64::sm64_surface_object_delete(id) });
                h.exclude.remove(&i);
                rebuild = true;
            }
        }
        rebuild
    }

    /// Debug (F5): the moving objects near Mario: body, where it is, its yaw since it was built,
    /// and its size.
    pub fn describe(&self, h: &HavokCollision, origin: [f32; 3], mario: [f32; 3]) -> Vec<String> {
        let m = Vec3::from(mario);
        let mut out = Vec::new();
        for (i, t) in &self.tracked {
            let Some((p, q)) = h.transform(*i) else { continue };
            let center = sm(origin, p);
            let d = q * t.q0.inverse();
            let d = Quat::from_xyzw(d.x, -d.y, -d.z, d.w);
            let f = d * Vec3::Z;
            out.push(format!(
                "  moving body #{i}: {:.0} units from Mario, yaw {:.0} deg since built, radius {:.1} m, {} triangles",
                center.distance(m),
                f.x.atan2(f.z).to_degrees(),
                t.mesh.radius(),
                t.mesh.tris().len()
            ));
            // its faces as SM64 sees them now (built at q0, turned by the object's rotation since)
            let mid = t.mesh.small_closed().then(|| t.mesh.tris().iter().flatten().copied().sum::<Vec3>() / (t.mesh.tris().len() * 3) as f32);
            out.push(format!("    small closed mesh {}, middle {:?}", t.mesh.small_closed(), mid.map(|v| sm(origin, q * v + p))));
            for tri in t.mesh.tris() {
                let w = tri.map(|v| sm(origin, q * v + p));
                let n = (w[1] - w[0]).cross(w[2] - w[1]).normalize_or_zero();
                out.push(format!(
                    "    face {:?} n {:.2?}, Mario {:.0} from its plane",
                    w.map(|v| v.round().to_array()), n, n.dot(m - w[0])
                ));
            }
        }
        out
    }

    /// Deletes every surface object (Mario is going away).
    pub fn clear(&mut self, h: &mut HavokCollision) {
        let ids: Vec<u32> = self.tracked.drain().map(|(i, t)| {
            h.exclude.remove(&i);
            t.id
        }).collect();
        self.watch.clear();
        if !ids.is_empty() {
            worker::call("object delete", move |_| {
                for id in ids {
                    unsafe { sm64::sm64_surface_object_delete(id) };
                }
            });
        }
    }
}

/// libsm64's ZXY matrix is R_y * R_x * R_z; its public API negates degree angles.
fn platform_rotation(delta: Quat) -> [f32; 3] {
    let mirrored = Quat::from_xyzw(delta.x, -delta.y, -delta.z, delta.w);
    let (yaw, pitch, roll) = mirrored.to_euler(glam::EulerRot::YXZ);
    [-pitch.to_degrees(), -yaw.to_degrees(), -roll.to_degrees()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_rotation_matches_reflected_havok_in_all_axes() {
        for q in [Quat::IDENTITY, Quat::from_rotation_y(0.7), Quat::from_euler(glam::EulerRot::YXZ, 0.7, -0.3, 0.2)] {
            let angles = platform_rotation(q).map(|x| -x.to_radians());
            let engine = Quat::from_euler(glam::EulerRot::YXZ, angles[1], angles[0], angles[2]);
            let reflect = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
            for v in [Vec3::X, Vec3::Y, Vec3::Z] {
                assert!((engine * v - reflect(q * reflect(v))).length() < 1e-5);
            }
        }
    }
}
