//! M6: draw Mario inside Sekiro using the game's own immediate debug renderer.
//!
//! `RendMan` (singleton) holds `SprjEzDraw` debug drawers. The game queues debug primitives with:
//!   * `SprjEzDraw::set_color(this, *const F32Vector4)`               (RVA 0x59e6a0)
//!   * the triangle emitter that stores the triangle command          (RVA 0x25da940)
//! The emitter writes a 48-byte command (`origin` + two edge vectors) plus a handler pointer
//! (`0x14262DDB0`); at render time the handler calls the rasteriser `0x142633A50` with a render
//! context whose `+0x18` is the `FD4EzDrawState` in effect.
//!
//! How the rasteriser picks fill vs. outline (from its disassembly at `0x142633D5F`):
//!   * `state+0x24 == 0` -> 3 vertices as a triangle strip (filled)
//!   * `state+0x24 != 0` -> 4 vertices as a line strip (outline)
//!   * `state+0x28` only selects a raster-state variant (0/1 -> pipeline 0, 2 -> 4, 3 -> 2)
//! So `+0x24` (dirty flag `0x08`) is the fill mode; `+0x28` (flag `0x10`) is not.

use std::sync::atomic::{AtomicBool, Ordering};

use fromsoftware_shared::FromStatic;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::mem::{read_u32, read_u64};

#[repr(C)]
#[fromsoftware_shared::singleton("RendMan")]
struct RendMan {
    _vftable: usize,
}

/// Sekiro `Triangle`: origin + two edge vectors (each a 16-byte vector; `.w` is unused).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Triangle {
    origin: [f32; 4],
    edge1: [f32; 4],
    edge2: [f32; 4],
}

#[repr(C, align(16))]
struct Color([f32; 4]);

const SET_COLOR_RVA: usize = 0x59e6a0;
const DRAW_TRIANGLE_RVA: usize = 0x25da940;
/// The triangle command handler the renderer invokes while rasterising queued triangles.
const TRI_HANDLER_RVA: usize = 0x262ddb0;

/// `FD4EzDrawState` fields.
const STATE_FLAGS: usize = 0x10;
const STATE_FILL: usize = 0x24;
const FLAG_FILL: u32 = 0x08;

type SetColor = extern "C" fn(*mut u8, *const Color);
type DrawTriangle = extern "C" fn(*mut u8, *const Triangle);

static LOGGED: AtomicBool = AtomicBool::new(false);
static HANDLER_LOGGED: AtomicBool = AtomicBool::new(false);

/// Hooks the triangle command handler so every queued triangle rasterises filled, whatever fill
/// mode the render state carries at that moment (restored right after, so the game's own debug
/// drawing is untouched).
pub fn install_hook() {
    use ilhook::x64::{CallbackOption, HookFlags, hook_closure_retn};
    let base = unsafe { GetModuleHandleW(None) }.map(|m| m.0 as usize).unwrap_or(0);
    if base == 0 {
        return;
    }
    // handler(rcx = command context): render context at +0x38, its draw state at +0x18
    let handler = base + TRI_HANDLER_RVA;
    let handler_hook = |reg: *mut ilhook::x64::Registers, original: usize| -> usize {
        let cmd = unsafe { (*reg).rcx as usize };
        let state = read_u64(cmd + 0x38)
            .and_then(|ctx| read_u64(ctx as usize + 0x18))
            .map(|s| s as usize)
            .filter(|&s| s >= 0x10000);
        let saved = state.and_then(|s| read_u32(s + STATE_FILL).map(|v| (s, v)));
        if let Some((s, v)) = saved {
            if !HANDLER_LOGGED.swap(true, Ordering::Relaxed) {
                crate::log(format!("draw: render state {s:#x} fill field was {v}"));
            }
            unsafe { std::ptr::write_volatile((s + STATE_FILL) as *mut u32, 0) };
        }
        let f: extern "win64" fn(usize) -> usize = unsafe { std::mem::transmute(original) };
        let ret = f(cmd);
        if let Some((s, v)) = saved {
            unsafe { std::ptr::write_volatile((s + STATE_FILL) as *mut u32, v) };
        }
        ret
    };
    match unsafe { hook_closure_retn(handler, handler_hook, CallbackOption::None, HookFlags::empty()) } {
        Ok(h) => {
            std::mem::forget(h);
            crate::log("draw: triangle handler hook installed");
        }
        Err(e) => crate::log(format!("draw: triangle handler hook failed: {e:?}")),
    }
}

/// Resolves the `FD4EzDrawState` the way `SprjEzDraw::set_color` does: command buffer from the
/// double-buffered table at `ez+0x10`, then its draw-state pointer at `buffer+0x40`.
fn ez_draw_state(ez: usize) -> Option<usize> {
    let idx = read_u32(ez + 0x20)? as i32 as usize;
    let buffer = read_u64(ez + 0x10 + idx * 8)? as usize;
    let state = read_u64(buffer + 0x40)? as usize;
    if state < 0x10000 { None } else { Some(state) }
}

/// Resolves the `SprjEzDraw` (RendMan debug drawer) pointer.
fn ez_ptr() -> Option<*mut u8> {
    let Ok(rm) = <RendMan as FromStatic>::instance_ptr() else { return None };
    if rm.is_null() {
        return None;
    }
    let ez = read_u64(rm as usize + 0x30).map(|q| q as usize)?;
    if ez < 0x10000 { None } else { Some(ez as *mut u8) }
}

/// Asks for filled polygons at emit time (the handler hook enforces it at render time too).
fn set_fill_mode(ez: usize) {
    let Some(state) = ez_draw_state(ez) else { return };
    unsafe {
        if std::ptr::read_volatile((state + STATE_FILL) as *const u32) != 0 {
            std::ptr::write_volatile((state + STATE_FILL) as *mut u32, 0);
            let flags = std::ptr::read_volatile((state + STATE_FLAGS) as *const u32);
            std::ptr::write_volatile((state + STATE_FLAGS) as *mut u32, flags | FLAG_FILL);
        }
    }
}

/// Nearest texel of the SM64 atlas at `uv` (RGBA, 0..1).
fn sample(tex: &[u8], uv: glam::Vec2) -> glam::Vec4 {
    let (w, h) = (crate::sm64::TEXTURE_W, crate::sm64::TEXTURE_H);
    let x = ((uv.x * w as f32) as isize).clamp(0, w as isize - 1) as usize;
    let y = ((uv.y * h as f32) as isize).clamp(0, h as isize - 1) as usize;
    let i = (y * w + x) * 4;
    match tex.get(i..i + 4) {
        Some(p) => glam::Vec4::new(p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32) / 255.0,
        None => glam::Vec4::ZERO,
    }
}

/// Upper bound on sub-triangles spent on textured parts (face, buttons, cap logo) per frame.
const TEXTURE_BUDGET: usize = 3000;

/// One triangle Mario's parent triangle is split into, ready to queue.
struct Piece {
    v: [glam::Vec3; 3],
    color: [u8; 3],
}

pub fn draw() {
    // not on load screens
    if !crate::mario::shown() {
        return;
    }
    let frame = crate::mario::mesh_frame();
    let mesh = &frame.position;
    let tris = mesh.len() / 9;
    if tris == 0 {
        return;
    }
    let base = unsafe { GetModuleHandleW(None) }.map(|m| m.0 as usize).unwrap_or(0);
    if base == 0 {
        return;
    }
    let Some(ez) = ez_ptr() else { return };
    let set_color: SetColor = unsafe { std::mem::transmute(base + SET_COLOR_RVA) };
    let draw_triangle: DrawTriangle = unsafe { std::mem::transmute(base + DRAW_TRIANGLE_RVA) };
    let tex = crate::mario::texture();
    let has = |buf: &Vec<f32>, n: usize| buf.len() >= tris * n;
    let (has_color, has_normal, has_uv) = (has(&frame.color, 9), has(&frame.normal, 9), has(&frame.uv, 6));

    // (world space already, metres)
    let world = |t: usize, k: usize| {
        let i = t * 9 + k * 3;
        glam::Vec3::new(mesh[i], mesh[i + 1], mesh[i + 2])
    };
    let uv = |t: usize, k: usize| glam::Vec2::new(frame.uv[t * 6 + k * 2], frame.uv[t * 6 + k * 2 + 1]);
    let textured = |t: usize| has_uv && tex.is_some() && (0..3).any(|k| uv(t, k).x < 0.999 || uv(t, k).y < 0.999);

    // texture detail: split textured triangles so each piece covers ~2 texels, within the budget
    let texels = |t: usize| {
        let s = glam::Vec2::new(crate::sm64::TEXTURE_W as f32, crate::sm64::TEXTURE_H as f32);
        let (a, b, c) = (uv(t, 0) * s, uv(t, 1) * s, uv(t, 2) * s);
        a.distance(b).max(b.distance(c)).max(c.distance(a))
    };
    let mut splits: Vec<usize> = (0..tris).map(|t| if textured(t) { ((texels(t) / 2.0).ceil() as usize).clamp(1, 12) } else { 1 }).collect();
    let spent: usize = (0..tris).filter(|&t| textured(t)).map(|t| splits[t] * splits[t]).sum();
    if spent > TEXTURE_BUDGET {
        let k = (TEXTURE_BUDGET as f32 / spent as f32).sqrt();
        for t in (0..tris).filter(|&t| textured(t)) {
            splits[t] = ((splits[t] as f32 * k).floor() as usize).max(1);
        }
    }

    // back to front from the camera, so the result is right even without a depth test
    let mut order: Vec<usize> = (0..tris).collect();
    let player = glam::Vec3::from(crate::mario::world_pos());
    if let Some(cam) = crate::camera::active(crate::player::position().map(glam::Vec3::from).unwrap_or(player)) {
        let depth: Vec<f32> = (0..tris).map(|t| ((world(t, 0) + world(t, 1) + world(t, 2)) / 3.0).distance_squared(cam.pos)).collect();
        order.sort_by(|&a, &b| depth[b].total_cmp(&depth[a]));
    }

    let light = glam::Vec3::new(0.3, 1.0, 0.4).normalize();
    // the scene's light around Mario (light.rs), and its fog far away
    crate::light::update_target(player);
    let (tint, fog, fog_amount) = crate::light::shading();
    let quant = |c: glam::Vec3| {
        let c = (c * tint).lerp(fog, fog_amount);
        [c.x, c.y, c.z].map(|v| (v.clamp(0.0, 1.0) * 31.0).round() as u8)
    };
    let mut pieces: Vec<Piece> = Vec::with_capacity(tris + TEXTURE_BUDGET);
    for &t in &order {
        let i = t * 9;
        let vcol = |k: usize| {
            if has_color {
                glam::Vec3::new(frame.color[i + k * 3], frame.color[i + k * 3 + 1], frame.color[i + k * 3 + 2])
            } else {
                glam::Vec3::new(1.0, 0.2, 0.15)
            }
        };
        let base_col = (vcol(0) + vcol(1) + vcol(2)) / 3.0;
        let shade = if has_normal {
            // (world-space normals)
            let n = glam::Vec3::new(
                frame.normal[i] + frame.normal[i + 3] + frame.normal[i + 6],
                frame.normal[i + 1] + frame.normal[i + 4] + frame.normal[i + 7],
                frame.normal[i + 2] + frame.normal[i + 5] + frame.normal[i + 8],
            )
            .normalize_or_zero();
            0.45 + 0.55 * n.dot(light).max(0.0)
        } else {
            1.0
        };
        let (a, b, c) = (world(t, 0), world(t, 1), world(t, 2));
        if !(a.is_finite() && b.is_finite() && c.is_finite()) || a.abs().max_element() > 1.0e6 {
            continue;
        }
        let n = splits[t];
        if n <= 1 && !textured(t) {
            pieces.push(Piece { v: [a, b, c], color: quant(base_col * shade) });
            continue;
        }
        // barycentric grid: P(i, j) = A + (B - A) i/n + (C - A) j/n, same for uv
        let tex = tex.unwrap_or(&[]);
        let (ua, ub, uc) = (uv(t, 0), uv(t, 1), uv(t, 2));
        let at = |gi: usize, gj: usize| {
            let (fi, fj) = (gi as f32 / n as f32, gj as f32 / n as f32);
            (a + (b - a) * fi + (c - a) * fj, ua + (ub - ua) * fi + (uc - ua) * fj)
        };
        let mut push = |p: [(glam::Vec3, glam::Vec2); 3]| {
            // SM64's shader: mix(vertex colour, texel, texel alpha)
            let s = sample(tex, (p[0].1 + p[1].1 + p[2].1) / 3.0);
            let col = base_col.lerp(s.truncate(), s.w);
            pieces.push(Piece { v: [p[0].0, p[1].0, p[2].0], color: quant(col * shade) });
        };
        for gi in 0..n {
            for gj in 0..n - gi {
                push([at(gi, gj), at(gi + 1, gj), at(gi, gj + 1)]);
                if gi + gj + 1 < n {
                    push([at(gi + 1, gj), at(gi + 1, gj + 1), at(gi, gj + 1)]);
                }
            }
        }
    }

    set_fill_mode(ez as usize);
    let mut last: Option<[u8; 3]> = None;
    let mut color_changes = 0usize;
    for p in &pieces {
        if last != Some(p.color) {
            let c = p.color.map(|v| v as f32 / 31.0);
            set_color(ez, &Color([c[0], c[1], c[2], 1.0]));
            last = Some(p.color);
            color_changes += 1;
        }
        let [a, b, c] = p.v;
        // mirroring X flipped the winding: swap two vertices to keep the original facing
        let v4 = |q: glam::Vec3| [q.x, q.y, q.z, 0.0];
        draw_triangle(ez, &Triangle { origin: v4(a), edge1: v4(c - a), edge2: v4(b - a) });
    }

    if !LOGGED.swap(true, Ordering::Relaxed) {
        let textured_tris = (0..tris).filter(|&t| textured(t)).count();
        crate::log(format!(
            "draw: {tris} tris ({textured_tris} textured) -> {} pieces, {color_changes} colour changes",
            pieces.len()
        ));
    }
}
