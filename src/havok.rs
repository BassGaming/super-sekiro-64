//! Real collision: reads Sekiro's live Havok (`hknp`) world and hands Mario the game's own triangles.
//!
//! The approach and the shape decoders follow er-mario's `havok_col.rs` (Elden Ring uses the same
//! `hknp` physics and the same `fsnpCustomParamCompressedMeshShape`). Sekiro's Havok is older, so
//! nothing here trusts Elden Ring's offsets blindly:
//!   * the `hknpWorld` is found by scanning memory for objects with its vtable (once, on a thread)
//!   * its body array, the body stride and the body's shape pointer offset are worked out from what
//!     the memory holds (an `hkArray` whose elements point at objects with `Shape` RTTI names)
//!   * the compressed-mesh data pointer offset in the shape is found by trying candidates and
//!     keeping the one whose data decodes to triangles inside the mesh's own bounding box
//! Everything found is logged, so a wrong guess shows in the log instead of as odd behaviour.
//!
//! Only meshes (the map's collision) and compounds of them are used: capsules and convex pieces
//! are characters, ragdolls and props. And only meshes the player's own body collides with, by the
//! game's `NS_SPRJ::FrpgCollisionFilter` (its pair check at 0x140936A20): a body's filter info is
//! the u32 at body+0x6C, its layer `info & 0x7F`; layer a collides with layer b when bit (b & 63)
//! of the u64 at filter + 0x20 + ((b >> 6) + a * 2) * 8 is set; and two bodies of the same
//! non-zero subgroup `(info >> 7) & 3` never collide. (Without this, Mario bumped into walls only
//! the camera or the AI use, and the 6000-triangle budget filled with them.)

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use fromsoftware_shared::{Program, rtti::find_rtti_classes};
use glam::{Mat3, Vec3};
use pelite::pe64::Pe;
use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};

use crate::log;
use crate::mem::{class_name_of, read_u32, read_u64, readable};

pub type Tri = [Vec3; 3];

/// Mesh bucket size (m).
const CELL: f32 = 4.0;

/// The discovered layout.
#[derive(Clone, Copy, Debug)]
struct Layout {
    world: usize,
    /// the hknpWorld vtable (a world rebuilt elsewhere is noticed when this stops matching)
    vtable: usize,
    /// offset of the body `hkArray` in the world
    bodies: usize,
    stride: usize,
    /// offset of the shape pointer in a body (the translation is at +0x30, rotation columns before)
    shape: usize,
}

/// 0 not started, 1 running, 2 ready, 3 failed
static STATE: AtomicU8 = AtomicU8::new(0);
static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);
/// The game's collision filter (`NS_SPRJ::FrpgCollisionFilter`), 0 if not found.
static FILTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The player's body filter info (u32 at body+0x6C), u32::MAX if unknown.
static PLAYER_INFO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);
const BODY_FILTER: usize = 0x6c;
/// u32 at body+0x78: 0xFFFFFFFF when the body is out of the world's collision (its broadphase id?)
const BODY_BROADPHASE: usize = 0x78;

/// Collision layers that are water (Sekiro's "swim hit"): not solid for Mario, their top is the
/// water level. From `water_layers = 0x.., 0x..` in sk_mario.ini; by default 0x2e (the surface of
/// the reservoir's water, with the lake bed on 0x5b under it).
fn water_layers() -> &'static [u32] {
    static LAYERS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    LAYERS.get_or_init(|| {
        let v: Vec<u32> = crate::paths::config("water_layers")
            .map(|s| {
                s.split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(|t| {
                        let t = t.trim();
                        t.strip_prefix("0x").map(|h| u32::from_str_radix(h, 16).ok()).unwrap_or_else(|| t.parse().ok())
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![0x2e]);
        log(format!("havok: water layers {v:#x?} (water_layers in sk_mario.ini)"));
        v
    })
}

/// The water surfaces near Mario (world space), from the last query.
static WATER: Mutex<Vec<Tri>> = Mutex::new(Vec::new());

/// The height of a flat-ish triangle at (x, z), if (x, z) is inside it seen from above.
fn height_at(t: &Tri, x: f32, z: f32) -> Option<f32> {
    let n = (t[1] - t[0]).cross(t[2] - t[0]);
    if n.length_squared() < 1e-9 || (n.y / n.length()).abs() < 0.7 {
        return None;
    }
    let (a, b, c) = (glam::Vec2::new(t[0].x, t[0].z), glam::Vec2::new(t[1].x, t[1].z), glam::Vec2::new(t[2].x, t[2].z));
    let p = glam::Vec2::new(x, z);
    let cross = |u: glam::Vec2, v: glam::Vec2| u.x * v.y - u.y * v.x;
    let (d1, d2, d3) = (cross(b - a, p - a), cross(c - b, p - b), cross(a - c, p - c));
    let inside = (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0);
    // plane: n . (q - t0) = 0  ->  y
    inside.then(|| t[0].y - (n.x * (x - t[0].x) + n.z * (z - t[0].z)) / n.y)
}

/// The water level at (x, z) (world m): the highest water surface there, if any.
pub fn water_level_at(x: f32, z: f32) -> Option<f32> {
    WATER.lock().unwrap_or_else(|e| e.into_inner()).iter().filter_map(|t| height_at(t, x, z)).reduce(f32::max)
}

/// The game's own check: do bodies with these filter infos collide?
fn collides(filter: usize, a: u32, b: u32) -> bool {
    let (la, lb) = ((a & 0x7f) as usize, (b & 0x7f) as usize);
    let at = filter + 0x20 + ((lb >> 6) + la * 2) * 8;
    if !readable(at, 8) {
        return true;
    }
    if (u64_at(at) >> (lb & 63)) & 1 == 0 {
        return false;
    }
    let (ga, gb) = ((a >> 7) & 3, (b >> 7) & 3);
    !(ga != 0 && gb != 0 && ga == gb)
}

/// Offset of the `hknpCompressedMeshShapeData` pointer in a compressed mesh shape (0: unknown).
static MESH_DATA: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn ready() -> bool {
    STATE.load(Ordering::Relaxed) == 2
}

/// Discovery finished (found or not).
pub fn settled() -> bool {
    STATE.load(Ordering::Relaxed) >= 2
}

// ---- discovery -----------------------------------------------------------------------------------

/// Starts looking for the Havok world (once; a background thread).
pub fn start() {
    if STATE.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed).is_err() {
        return;
    }
    std::thread::spawn(|| {
        // during a load screen the world exists but holds no bodies yet: try again until it does
        let t = std::time::Instant::now();
        for attempt in 0u32.. {
            let verbose = attempt % 30 == 0;
            // the slow full-memory scan only now and then (the pointer search is instant)
            let allow_scan = attempt % 30 == 15;
            if discover(verbose, allow_scan) {
                log(format!("havok: discovery succeeded after {:.1} s ({} attempts)", t.elapsed().as_secs_f32(), attempt + 1));
                STATE.store(2, Ordering::Relaxed);
                return;
            }
            if verbose {
                log(format!("havok: no populated world yet (attempt {}, {:.0} s); retrying every 2 s", attempt + 1, t.elapsed().as_secs_f32()));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

/// Restarts discovery (the world was rebuilt: a load screen).
pub fn restart() {
    if STATE.load(Ordering::Relaxed) == 2 {
        *LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        STATE.store(0, Ordering::Relaxed);
        start();
    }
}

fn vtable_of(program: &Program, wanted: &str) -> Option<usize> {
    find_rtti_classes(program)
        .find(|c| c.name == wanted)
        .and_then(|c| program.rva_to_va(c.vftable).ok())
        .map(|v| v as usize)
}

/// Writable committed memory holding `vtable` as an aligned qword (object instances).
fn find_instances(vtable: usize, max: usize) -> Vec<usize> {
    const WRITABLE: u32 = 0x04 | 0x40 | 0x80;
    let needle = vtable as u64;
    let mut hits = Vec::new();
    let mut addr: usize = 0x10000;
    let mut mbi: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut buf = vec![0u64; 1 << 17];
    loop {
        let r = unsafe { VirtualQuery(Some(addr as *const c_void), &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if r == 0 {
            break;
        }
        let (base, size) = (mbi.BaseAddress as usize, mbi.RegionSize);
        if mbi.State.0 == MEM_COMMIT.0 && mbi.Protect.0 & 0x101 == 0 && mbi.Protect.0 & WRITABLE != 0 {
            let mut off = 0;
            while off < size && hits.len() < max {
                let n = (size - off).min(buf.len() * 8);
                let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, n) };
                if crate::mem::read_bytes(base + off, bytes) {
                    for (k, &q) in buf[..n / 8].iter().enumerate() {
                        if q == needle {
                            hits.push(base + off + k * 8);
                        }
                    }
                }
                off += n;
            }
        }
        let next = base.wrapping_add(size);
        if next <= addr || hits.len() >= max {
            break;
        }
        addr = next;
    }
    hits
}

/// Objects reachable from `roots` by following object pointers (a pointer whose target starts
/// with a vtable in the game image), breadth first, up to `depth` steps: those with `vtable`.
fn reachable_instances(roots: &[usize], vtable: usize, depth: usize, max_objects: usize) -> Vec<usize> {
    let image = 0x1_4000_0000u64..0x1_4600_0000u64;
    let mut seen = std::collections::HashSet::new();
    let mut level: Vec<usize> = roots.iter().copied().filter(|&r| r >= 0x10000).collect();
    let mut hits = Vec::new();
    let mut buf = [0u64; 0x400 / 8];
    for _ in 0..=depth {
        let mut next = Vec::new();
        for &obj in &level {
            if !seen.insert(obj) || seen.len() > max_objects {
                continue;
            }
            let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, 0x400) };
            if !crate::mem::read_bytes(obj, bytes) && !crate::mem::read_bytes(obj, &mut bytes[..0x100]) {
                continue;
            }
            for &q in buf.iter() {
                if q < 0x10000 || q >= 0x8000_0000_0000 || q % 8 != 0 {
                    continue;
                }
                let Some(vt) = read_u64(q as usize) else { continue };
                if vt as usize == vtable {
                    if !hits.contains(&(q as usize)) {
                        hits.push(q as usize);
                    }
                } else if image.contains(&vt) {
                    next.push(q as usize);
                }
            }
        }
        level = next;
    }
    hits
}

/// The body array layout of a candidate world: (sampled bodies with shapes, layout).
fn layout_of(w: usize, vtable: usize, class: &mut impl FnMut(usize) -> Option<String>) -> Option<(usize, Layout)> {
    let mut best: Option<(usize, usize, Layout)> = None;
    for off in (0x10..0x400).step_by(8) {
        let (Some(p), Some(n), Some(cap)) = (read_u64(w + off), read_u32(w + off + 8), read_u32(w + off + 12)) else { continue };
        let (p, n, cap) = (p as usize, n as usize, (cap & 0x3fff_ffff) as usize);
        if p < 0x10000 || !(16..=2_000_000).contains(&n) || cap < n {
            continue;
        }
        for stride in [0x80usize, 0x90, 0xa0, 0xb0, 0xc0, 0xd0] {
            if !readable(p, n * stride) {
                continue;
            }
            for so in (0x40..stride).step_by(8) {
                let samples = 48.min(n);
                let (mut hits, mut nonzero) = (0, 0);
                for k in 0..samples {
                    let body = p + (k * n / samples) * stride;
                    let Some(s) = read_u64(body + so) else { continue };
                    if s == 0 {
                        continue;
                    }
                    nonzero += 1;
                    if class(s as usize).is_some_and(|c| c.contains("Shape")) {
                        hits += 1;
                    }
                }
                if hits >= 8 && hits * 10 >= nonzero * 8 && best.is_none_or(|(b, bn, _)| (hits, n) > (b, bn)) {
                    best = Some((hits, n, Layout { world: w, vtable, bodies: off, stride, shape: so }));
                }
            }
        }
    }
    best.map(|(h, _, l)| (h, l))
}

fn discover(verbose: bool, allow_scan: bool) -> bool {
    let program = Program::current();
    let Some(vt) = vtable_of(&program, "hknpWorld") else {
        log("havok: hknpWorld not in RTTI");
        return false;
    };
    let mut names: HashMap<u64, Option<String>> = HashMap::new();
    let mut class = |p: usize| -> Option<String> {
        let vt = read_u64(p)?;
        names.entry(vt).or_insert_with(|| class_name_of(&program, p)).clone()
    };
    // quick: the world is a few pointers away from the player's physics (its character body)
    let roots = [crate::player::physics_ptr().unwrap_or(0), crate::player::ptr()];
    let mut worlds = reachable_instances(&roots, vt, 4, 40_000);
    let mut how = "pointer search from the player";
    if worlds.is_empty() && allow_scan {
        // slow: every object in memory with the hknpWorld vtable
        how = "memory scan";
        worlds = find_instances(vt, 16);
    }
    if verbose || !worlds.is_empty() && how == "memory scan" {
        log(format!("havok: hknpWorld vtable {vt:#x}, {} instance(s) by {how}: {worlds:#x?}", worlds.len()));
    }
    let mut best: Option<(usize, Layout)> = None;
    for &w in &worlds {
        if let Some((hits, l)) = layout_of(w, vt, &mut class) {
            if best.is_none_or(|(b, bl)| (hits, body_count(&l)) > (b, body_count(&bl))) {
                best = Some((hits, l));
            }
        }
    }
    let Some((hits, layout)) = best else {
        if verbose && !worlds.is_empty() {
            log("havok: no body array with shapes in the world yet (loading?)");
            static DUMPED: AtomicU8 = AtomicU8::new(0);
            if DUMPED.fetch_add(1, Ordering::Relaxed) == 0 {
                for &w in worlds.iter().take(1) {
                    dump_head(&program, w);
                }
            }
        }
        return false;
    };
    log(format!(
        "havok: world {:#x}, bodies at +{:#x} ({} bodies), stride {:#x}, shape at +{:#x} ({hits} sampled bodies have shapes)",
        layout.world,
        layout.bodies,
        body_count(&layout),
        layout.stride,
        layout.shape
    ));
    *LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = Some(layout);
    // the game's collision filter (built by 0x140932320, 0x828 bytes). A pointer search reads only
    // an object's first 0x400 bytes, so it can be out of its sight: then every object in memory is
    // checked (slow: Mario collides with everything meanwhile). Several objects carry its vtable;
    // the world's own filter is allocated right next to the world.
    if let Some(fvt) = vtable_of(&program, "NS_SPRJ::FrpgCollisionFilter") {
        // (it has been 0x5000 bytes after the world every time)
        let mut filters: Vec<usize> = [layout.world + 0x5000].into_iter().filter(|&f| read_u64(f) == Some(fvt as u64)).collect();
        let mut how = "next to the world";
        if filters.is_empty() {
            filters = reachable_instances(&[layout.world], fvt, 3, 20_000);
            how = "pointer search from the world";
        }
        if filters.is_empty() {
            filters = find_instances(fvt, 16);
            how = "memory scan";
        }
        filters.retain(|&f| readable(f, 0x828));
        filters.sort_by_key(|&f| f.abs_diff(layout.world));
        match filters.first() {
            Some(&f) => {
                FILTER.store(f, Ordering::Relaxed);
                PLAYER_INFO.store(u32::MAX, Ordering::Relaxed);
                log(format!("havok: collision filter {f:#x} (FrpgCollisionFilter nearest the world, by {how}; {} instance(s): {filters:#x?})", filters.len()));
            }
            None => log("havok: FrpgCollisionFilter not found; Mario collides with every map mesh"),
        }
    }
    true
}

fn body_count(l: &Layout) -> usize {
    read_u32(l.world + l.bodies + 8).unwrap_or(0) as usize
}

/// Debug: the head of an object, pointers annotated with their class.
fn dump_head(program: &Program, base: usize) {
    for off in (0..0x100).step_by(8) {
        let Some(q) = read_u64(base + off) else { continue };
        let name = class_name_of(program, q as usize).unwrap_or_default();
        let next = read_u32(q as usize + 8).unwrap_or(0);
        log(format!("havok:   {base:#x}+{off:#x} = {q:#x} {name} (next u32 {next})"));
    }
}

// ---- decoding (er-mario's decoders; validated rather than trusted) -------------------------------

fn u32_at(a: usize) -> u32 {
    unsafe { (a as *const u32).read_unaligned() }
}

fn f32_at(a: usize) -> f32 {
    unsafe { (a as *const f32).read_unaligned() }
}

fn u64_at(a: usize) -> usize {
    unsafe { (a as *const u64).read_unaligned() as usize }
}

fn vec3_at(a: usize) -> Vec3 {
    Vec3::new(f32_at(a), f32_at(a + 4), f32_at(a + 8))
}

/// How a section's fields are read. Elden Ring's decoder (er-mario) reads +0x4c / +0x50 as plain
/// start indices and the counts from +0x58 (packed vertices) / +0x59 (primitives). Havok's own
/// `hkcdStaticMeshTreeBase::Section` packs them as `offset << 8 | count` (+0x4c shared vertices,
/// +0x50 primitives). Whichever makes the sections' primitive counts add up to the mesh's is used.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SectionCodec {
    Plain,
    Packed,
}

/// (first packed vertex, packed count, first shared index, first primitive, primitive count)
fn section_fields(a: usize, codec: SectionCodec) -> (usize, usize, usize, usize, usize) {
    let first_packed = u32_at(a + 0x48) as usize;
    let num_packed = unsafe { *((a + 0x58) as *const u8) } as usize;
    let (shared, prims) = (u32_at(a + 0x4c) as usize, u32_at(a + 0x50) as usize);
    match codec {
        SectionCodec::Plain => (first_packed, num_packed, shared, prims, unsafe { *((a + 0x59) as *const u8) } as usize),
        SectionCodec::Packed => (first_packed, num_packed, shared >> 8, prims >> 8, prims & 0xff),
    }
}

static CODEC: AtomicU8 = AtomicU8::new(0);
static MESH_LOGS: AtomicU8 = AtomicU8::new(0);

/// Decodes a `hknpCompressedMeshShapeData` into shape-local triangles. Layout (Elden Ring's):
/// +0x30 domain min, +0x40 domain max, +0x60 sections (0x60 each), +0x70 primitives (u8 x4),
/// +0x80 shared indices (u16), +0x90 packed vertices (u32 11/11/10), +0xa0 shared vertices
/// (u64 21/21/22); section +0x30 offset, +0x3c scale, then the fields of `section_fields`.
fn decode_mesh_data(md: usize) -> Option<Vec<Tri>> {
    if !readable(md, 0xb0) {
        return None;
    }
    let arr = |off: usize, elem: usize, max: usize| -> Option<(usize, usize)> {
        let p = u64_at(md + off);
        let n = u32_at(md + off + 8) as usize;
        (n <= max && (n == 0 || readable(p, n * elem))).then_some((p, n))
    };
    let (secs, nsec) = arr(0x60, 0x60, 50_000)?;
    let (prims, nprims) = arr(0x70, 4, 8_000_000)?;
    let (sidx, nsidx) = arr(0x80, 2, 8_000_000)?;
    let (packed, npacked) = arr(0x90, 4, 8_000_000)?;
    let (shared, nshared) = arr(0xa0, 8, 8_000_000)?;
    if nsec == 0 || nprims == 0 {
        return None;
    }
    let dmin = vec3_at(md + 0x30);
    let dmax = vec3_at(md + 0x40);
    let span = dmax - dmin;
    if !(dmin.is_finite() && dmax.is_finite()) || span.min_element() < 0.0 || span.max_element() > 100_000.0 {
        return None;
    }
    // which codec: the one whose primitive counts add up (decided once, then kept)
    let sum = |codec| (0..nsec).map(|s| section_fields(secs + s * 0x60, codec).4).sum::<usize>();
    let (sum_plain, sum_packed) = (sum(SectionCodec::Plain), sum(SectionCodec::Packed));
    let codec = match CODEC.load(Ordering::Relaxed) {
        1 => SectionCodec::Plain,
        2 => SectionCodec::Packed,
        _ if sum_packed == nprims && sum_plain != nprims => SectionCodec::Packed,
        _ if sum_plain == nprims && sum_packed != nprims => SectionCodec::Plain,
        _ => SectionCodec::Packed,
    };
    if CODEC.load(Ordering::Relaxed) == 0 && sum_plain != sum_packed && (sum_plain == nprims || sum_packed == nprims) {
        CODEC.store(if codec == SectionCodec::Plain { 1 } else { 2 }, Ordering::Relaxed);
        log(format!("havok: mesh sections use the {codec:?} layout ({nprims} primitives; plain sums to {sum_plain}, packed to {sum_packed})"));
    }
    let mut tris = Vec::new();
    for s in 0..nsec {
        let a = secs + s * 0x60;
        let off = vec3_at(a + 0x30);
        let scale = vec3_at(a + 0x3c);
        let (first_packed, num_packed, shared_start, prim_start, num_prims) = section_fields(a, codec);
        let vert = |i: usize| -> Option<Vec3> {
            if i < num_packed {
                let k = first_packed + i;
                if k >= npacked {
                    return None;
                }
                let v = u32_at(packed + k * 4);
                Some(off + scale * Vec3::new((v & 0x7ff) as f32, ((v >> 11) & 0x7ff) as f32, (v >> 22) as f32))
            } else {
                let k = shared_start + i - num_packed;
                if k >= nsidx {
                    return None;
                }
                let si = unsafe { ((sidx + k * 2) as *const u16).read_unaligned() } as usize;
                if si >= nshared {
                    return None;
                }
                let v = unsafe { ((shared + si * 8) as *const u64).read_unaligned() };
                let f = Vec3::new(
                    (v & 0x1f_ffff) as f32 / 0x1f_ffff as f32,
                    ((v >> 21) & 0x1f_ffff) as f32 / 0x1f_ffff as f32,
                    (v >> 42) as f32 / 0x3f_ffff as f32,
                );
                Some(dmin + span * f)
            }
        };
        for p in prim_start..(prim_start + num_prims).min(nprims) {
            let idx = u32_at(prims + p * 4).to_le_bytes().map(|b| b as usize);
            // (a primitive whose first two indices match is a custom convex piece, not a triangle)
            if idx[0] == idx[1] {
                continue;
            }
            let (Some(a), Some(b), Some(c)) = (vert(idx[0]), vert(idx[1]), vert(idx[2])) else { continue };
            tris.push([a, b, c]);
            if idx[3] != idx[2] {
                if let Some(d) = vert(idx[3]) {
                    tris.push([a, c, d]);
                }
            }
        }
    }
    // valid only if the triangles are where the data's own box says they are
    let lo = dmin - Vec3::splat(1.0 + span.max_element() * 0.05);
    let hi = dmax + Vec3::splat(1.0 + span.max_element() * 0.05);
    let inside = tris.iter().filter(|t| t.iter().all(|v| v.cmple(hi).all() && v.cmpge(lo).all())).count();
    let ok = !tris.is_empty() && inside * 10 >= tris.len() * 9;
    if MESH_LOGS.load(Ordering::Relaxed) == 0 && ok {
        let hex = |a: usize, n: usize| (0..n / 4).map(|k| format!("{:08x}", u32_at(a + k * 4))).collect::<Vec<_>>().join(" ");
        log(format!("havok: first mesh data {md:#x} [0..0xb0]: {}", hex(md, 0xb0)));
        for k in 0..nsec.min(3) {
            log(format!("havok:   section {k} [0..0x60]: {}", hex(secs + k * 0x60, 0x60)));
        }
    }
    if MESH_LOGS.fetch_add(1, Ordering::Relaxed) < 6 {
        log(format!(
            "havok: mesh {md:#x}: {nsec} sections, {nprims} primitives (plain sums {sum_plain}, packed {sum_packed}), {npacked} packed / {nshared} shared vertices -> {} triangles ({inside} inside its box, {codec:?}){}",
            tris.len(),
            if ok { "" } else { " REJECTED" }
        ));
    }
    ok.then_some(tris)
}

/// A compressed mesh shape's triangles: the data pointer's offset is found once (Elden Ring: +0x48).
fn decode_mesh_shape(shape: usize) -> Option<Vec<Tri>> {
    if !readable(shape, 0x100) {
        return None;
    }
    let known = MESH_DATA.load(Ordering::Relaxed);
    if known != 0 {
        return decode_mesh_data(u64_at(shape + known));
    }
    let candidates = std::iter::once(0x48).chain((0x10..0x100).step_by(8).filter(|&o| o != 0x48));
    for off in candidates {
        let md = u64_at(shape + off);
        if md < 0x10000 || md % 8 != 0 {
            continue;
        }
        if let Some(t) = decode_mesh_data(md) {
            MESH_DATA.store(off, Ordering::Relaxed);
            log(format!("havok: compressed mesh data at shape+{off:#x} ({} triangles in the first mesh)", t.len()));
            return Some(t);
        }
    }
    None
}

/// Where a compound shape keeps its instances: (array offset in the shape, instance stride, child
/// shape offset in an instance). Elden Ring: (0x48, 0x80, 0x50); found once by trying candidates.
static COMPOUND: Mutex<Option<(usize, usize, usize)>> = Mutex::new(None);

fn compound_layout(shape: usize, class: &mut impl FnMut(usize) -> Option<String>) -> Option<(usize, usize, usize)> {
    if let Some(l) = *COMPOUND.lock().unwrap_or_else(|e| e.into_inner()) {
        return Some(l);
    }
    for arr in (0x30..0x100).step_by(8) {
        let inst = u64_at(shape + arr);
        let n = u32_at(shape + arr + 8) as usize;
        if inst < 0x10000 || n == 0 || n > 100_000 {
            continue;
        }
        for stride in [0x80usize, 0x90, 0x70, 0xa0, 0x60] {
            if !readable(inst, n.min(16) * stride) {
                continue;
            }
            for child in (0x40..stride).step_by(8) {
                let k = n.min(16);
                let good = (0..k)
                    .filter(|&i| {
                        let a = inst + i * stride;
                        (vec3_at(a).length() - 1.0).abs() < 0.05 && class(u64_at(a + child)).is_some_and(|c| c.contains("Shape"))
                    })
                    .count();
                if good * 10 >= k * 9 {
                    let l = (arr, stride, child);
                    *COMPOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
                    log(format!("havok: compound instances at shape+{arr:#x}, {stride:#x} each, child at +{child:#x}"));
                    return Some(l);
                }
            }
        }
    }
    None
}

/// `hknpCompoundShape`: instances with rotation columns +0x00/+0x10/+0x20, translation +0x30,
/// scale +0x40 and a child shape (layout from `compound_layout`). Mesh children only.
fn decode_compound(shape: usize, class: &mut impl FnMut(usize) -> Option<String>) -> Option<Vec<Tri>> {
    if !readable(shape, 0x100) {
        return None;
    }
    let (arr, stride, child_off) = compound_layout(shape, class)?;
    let inst = u64_at(shape + arr);
    let n = (u32_at(shape + arr + 8) as usize).min(4096);
    if n == 0 || !readable(inst, n * stride) {
        return None;
    }
    let mut out = Vec::new();
    for k in 0..n {
        let a = inst + k * stride;
        let (c0, c1, c2) = (vec3_at(a), vec3_at(a + 0x10), vec3_at(a + 0x20));
        let t = vec3_at(a + 0x30);
        let scale = vec3_at(a + 0x40);
        if ![c0, c1, c2, t, scale].iter().all(|v| v.is_finite()) || c0.length() > 100.0 || t.length() > 100_000.0 {
            continue;
        }
        let scale = if scale.min_element() > 0.0 { scale } else { Vec3::ONE };
        let child = u64_at(a + child_off);
        let name = class(child).unwrap_or_default();
        let tris = if name.contains("CompressedMeshShape") {
            decode_mesh_shape(child)
        } else if name.contains("ExternMeshShape") {
            decode_extern(child, class)
        } else {
            None
        };
        for tri in tris.unwrap_or_default() {
            out.push(tri.map(|v| {
                let v = v * scale;
                c0 * v.x + c1 * v.y + c2 * v.z + t
            }));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Where an extern mesh's geometry keeps its arrays: (vertices offset, triangles offset, triangle
/// entry size, index size). Found once by looking at the geometry object's `hkArray`s.
static EXTERN: Mutex<Option<(usize, usize, usize, usize, usize)>> = Mutex::new(None);

/// The `hkArray`s (offset, data, count) in the head of an object.
fn arrays_in(obj: usize, len: usize) -> Vec<(usize, usize, usize)> {
    (0x08..len)
        .step_by(8)
        .filter_map(|o| {
            let (p, n, cap) = (u64_at(obj + o), u32_at(obj + o + 8) as usize, (u32_at(obj + o + 12) & 0x3fff_ffff) as usize);
            (p >= 0x10000 && p % 4 == 0 && n > 0 && n < 4_000_000 && cap >= n).then_some((o, p, n))
        })
        .collect()
}

/// `hknpExternMeshShape`: a pointer to its geometry object (class name containing
/// `ExternMeshShapeGeometry`), which holds a vertex array (vec4) and a triangle array (3 or 4
/// indices of 2 or 4 bytes). Offsets found once and logged.
fn decode_extern(shape: usize, class: &mut impl FnMut(usize) -> Option<String>) -> Option<Vec<Tri>> {
    if !readable(shape, 0x100) {
        return None;
    }
    let geo = (0x10..0x100).step_by(8).map(|o| u64_at(shape + o)).find(|&p| class(p).is_some_and(|c| c.contains("ExternMeshShapeGeometry")))?;
    if !readable(geo, 0x80) {
        return None;
    }
    let known = *EXTERN.lock().unwrap_or_else(|e| e.into_inner());
    // the arrays are in the geometry object itself, or (`hknpDefaultExternMeshShapeGeometry`) in
    // the `hkGeometry` it points to (vertices, then triangles of a, b, c, material)
    let holder = |via: usize| if via == 0 { Some(geo) } else { Some(u64_at(geo + via)).filter(|&g| g >= 0x10000 && readable(g, 0x40)) };
    let layouts: Vec<(usize, usize, usize, usize, usize)> = match known {
        Some(l) => vec![l],
        None => {
            let mut out = Vec::new();
            for via in [0usize, 0x10, 0x18, 0x20, 0x28] {
                let Some(obj) = holder(via) else { continue };
                let arrays = arrays_in(obj, if via == 0 { 0x80 } else { 0x40 });
                for &(vo, _, _) in &arrays {
                    for &(to, _, _) in &arrays {
                        if vo != to {
                            for (entry, index) in [(16, 4), (12, 4), (6, 2), (8, 2)] {
                                out.push((via, vo, to, entry, index));
                            }
                        }
                    }
                }
            }
            if out.is_empty() {
                static LOGGED: AtomicU8 = AtomicU8::new(0);
                if LOGGED.fetch_add(1, Ordering::Relaxed) < 2 {
                    let hex = (0..0x80 / 8).map(|k| format!("{:016x}", u64_at(geo + k * 8))).collect::<Vec<_>>().join(" ");
                    log(format!("havok: extern mesh geometry {geo:#x} has no arrays: {hex}"));
                }
            }
            out
        }
    };
    for (via, vo, to, entry, index) in layouts {
        let Some(obj) = holder(via) else { continue };
        let (verts, nv) = (u64_at(obj + vo), u32_at(obj + vo + 8) as usize);
        let (tris, nt) = (u64_at(obj + to), u32_at(obj + to + 8) as usize);
        if nv == 0 || nt == 0 || nv > 4_000_000 || nt > 4_000_000 || !readable(verts, nv * 16) || !readable(tris, nt * entry) {
            continue;
        }
        let v: Vec<Vec3> = (0..nv).map(|i| vec3_at(verts + i * 16)).collect();
        if !v.iter().take(64).all(|p| p.is_finite() && p.abs().max_element() < 20_000.0) {
            continue;
        }
        let idx = |t: usize, k: usize| {
            let a = tris + t * entry + k * index;
            if index == 4 { u32_at(a) as usize } else { (unsafe { (a as *const u16).read_unaligned() }) as usize }
        };
        if !(0..nt.min(64)).all(|t| (0..3).all(|k| idx(t, k) < nv)) {
            continue;
        }
        let out: Vec<Tri> = (0..nt)
            .filter_map(|t| {
                let (a, b, c) = (idx(t, 0), idx(t, 1), idx(t, 2));
                (a < nv && b < nv && c < nv && a != b && b != c).then(|| [v[a], v[b], v[c]])
            })
            .collect();
        if out.is_empty() {
            continue;
        }
        if known.is_none() {
            *EXTERN.lock().unwrap_or_else(|e| e.into_inner()) = Some((via, vo, to, entry, index));
            let (lo, hi) = out.iter().flatten().fold((Vec3::MAX, Vec3::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            log(format!(
                "havok: extern mesh geometry: via +{via:#x}, vertices at +{vo:#x}, triangles at +{to:#x} ({entry}-byte entries, {index}-byte indices); first: {nv} vertices, {nt} triangles, box {lo:.1} .. {hi:.1}"
            ));
        }
        return Some(out);
    }
    None
}

/// A decoded mesh in shape-local space, bucketed into cells for quick queries.
struct Mesh {
    tris: Vec<Tri>,
    cells: HashMap<(i32, i32, i32), Vec<u32>>,
    /// farthest vertex from the shape origin
    reach: f32,
}

impl Mesh {
    fn new(tris: Vec<Tri>) -> Self {
        let mut cells: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        let mut reach: f32 = 0.0;
        let c = |v: f32| (v / CELL).floor() as i32;
        for (i, t) in tris.iter().enumerate() {
            if !t.iter().all(|v| v.is_finite() && v.abs().max_element() < 50_000.0) {
                continue;
            }
            let lo = t[0].min(t[1]).min(t[2]);
            let hi = t[0].max(t[1]).max(t[2]);
            reach = reach.max(lo.length()).max(hi.length());
            let span = (c(hi.x) - c(lo.x) + 1) as i64 * (c(hi.y) - c(lo.y) + 1) as i64 * (c(hi.z) - c(lo.z) + 1) as i64;
            if span > 20_000 {
                continue;
            }
            for x in c(lo.x)..=c(hi.x) {
                for y in c(lo.y)..=c(hi.y) {
                    for z in c(lo.z)..=c(hi.z) {
                        cells.entry((x, y, z)).or_default().push(i as u32);
                    }
                }
            }
        }
        Mesh { tris, cells, reach }
    }

    /// Triangles touching the local box `lo..hi`.
    fn query(&self, lo: Vec3, hi: Vec3, out: &mut Vec<u32>) {
        let c = |v: f32| (v / CELL).floor() as i32;
        let start = out.len();
        for x in c(lo.x)..=c(hi.x) {
            for y in c(lo.y)..=c(hi.y) {
                for z in c(lo.z)..=c(hi.z) {
                    if let Some(list) = self.cells.get(&(x, y, z)) {
                        out.extend_from_slice(list);
                    }
                }
            }
        }
        out[start..].sort_unstable();
        let mut w = start;
        for r in start..out.len() {
            if r == start || out[r] != out[w - 1] {
                out[w] = out[r];
                w += 1;
            }
        }
        out.truncate(w);
        out.retain(|&i| {
            let t = &self.tris[i as usize];
            let (a, b) = (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2]));
            a.cmple(hi).all() && b.cmpge(lo).all()
        });
    }
}

/// A shape we looked at: its vtable and data pointer (to notice the memory being reused), and
/// the mesh if it is one we collide with.
struct Cached {
    vtable: usize,
    key: usize,
    mesh: Option<Arc<Mesh>>,
    /// how far the shape reaches from its body (m); < 0: never collides (not a map mesh)
    reach: f32,
}

/// How far ahead of needing it a mesh is decoded (m).
const AHEAD: f32 = 20.0;

/// How far a compressed mesh reaches from its body's origin, from the box its data keeps its
/// vertices in (md +0x30 min, +0x40 max), without decoding it.
fn domain_reach(md: usize) -> Option<f32> {
    if !readable(md, 0x50) {
        return None;
    }
    let (lo, hi) = (vec3_at(md + 0x30), vec3_at(md + 0x40));
    let far = lo.abs().max(hi.abs()).length();
    (far.is_finite() && far < 100_000.0).then_some(far)
}

static CACHE: Mutex<Option<HashMap<usize, Cached>>> = Mutex::new(None);
static STATS_LOGGED: AtomicU8 = AtomicU8::new(0);

/// World-space triangles near `center` (a box `radius` wide each way horizontally, `below` / `above`
/// vertically). None until the world is found.
pub fn query(center: Vec3, radius: f32, below: f32, above: f32) -> Option<Vec<Tri>> {
    let layout = (*LAYOUT.lock().unwrap_or_else(|e| e.into_inner()))?;
    if read_u64(layout.world) != Some(layout.vtable as u64) {
        log("havok: the world is gone (a load screen?); looking for it again");
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        restart();
        return None;
    }
    let program = Program::current();
    let arr = layout.world + layout.bodies;
    let (bodies, count) = (read_u64(arr)? as usize, (read_u32(arr + 8)? as usize).min(2_000_000));
    if count == 0 || !readable(bodies, count * layout.stride) {
        return None;
    }
    let mut cache_guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache_guard.get_or_insert_with(HashMap::new);
    let mut names: HashMap<usize, Option<String>> = HashMap::new();
    let mut class = |p: usize| -> Option<String> {
        if !readable(p, 8) {
            return None;
        }
        let vt = u64_at(p);
        names.entry(vt).or_insert_with(|| class_name_of(&program, p)).clone()
    };

    let log_stats = STATS_LOGGED.load(Ordering::Relaxed) < 2;
    let mut seen_classes: HashMap<String, u32> = HashMap::new();
    let mut out = Vec::new();
    let mut idx = Vec::new();
    // the player's movement body: layer 1 (the filter's setup, 0x140933390, gives layer 1 the map
    // layers a character walks on: 0x27, 0x28, 0x18, 0x19, ...; layer 0xb, where the closest capsule
    // usually is, is a hit box that collides with almost nothing). The one closest to the character.
    let filter = FILTER.load(Ordering::Relaxed);
    let player_info = {
        let feet = crate::player::position().map(Vec3::from);
        let mut best: Option<(f32, u32)> = None;
        if let Some(feet) = feet {
            for i in 0..count {
                let body = bodies + i * layout.stride;
                if u64_at(body + layout.shape) < 0x10000 || u32_at(body + BODY_FILTER) & 0x7f != 1 {
                    continue;
                }
                let d = vec3_at(body + 0x30).distance(feet + Vec3::Y);
                if d < 3.0 && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, u32_at(body + BODY_FILTER)));
                }
            }
        }
        let info = best.map(|(_, i)| i).unwrap_or(1);
        if PLAYER_INFO.swap(info, Ordering::Relaxed) != info {
            log(format!("havok: the player's movement body filter info {info:#x} ({})", if best.is_some() { "its body" } else { "no layer 1 body near, layer 1 assumed" }));
            if filter != 0 {
                let solid: Vec<String> = (0..128u32).filter(|&l| collides(filter, info, l)).map(|l| format!("{l:#x}")).collect();
                log(format!("havok: the filter lets it collide with layers [{}]", solid.join(" ")));
            }
        }
        Some(info)
    };
    let mut layers: HashMap<u32, (u32, bool)> = HashMap::new();
    let mut outside: std::collections::BTreeMap<u32, u32> = Default::default();
    let water_layers = water_layers();
    let mut water = Vec::new();
    // now and then: which layers have a surface right above / below Mario (finds the water layer)
    static DIAG_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let diag = {
        let mut at = DIAG_AT.lock().unwrap_or_else(|e| e.into_inner());
        let due = at.is_none_or(|t| t.elapsed().as_secs_f32() > 3.0);
        if due {
            *at = Some(std::time::Instant::now());
        }
        due
    };
    let mut surfaces: std::collections::BTreeMap<(u32, bool), Vec<f32>> = Default::default();
    // (bodies near Mario, of them solid): a filter that leaves nothing solid is the wrong one
    let mut near = (0u32, 0u32);
    let mut sources: Vec<(usize, usize)> = Vec::new();
    for i in 0..count {
        let body = bodies + i * layout.stride;
        let shape = u64_at(body + layout.shape);
        if shape < 0x10000 {
            continue;
        }
        let t = vec3_at(body + 0x30);
        if !t.is_finite() {
            continue;
        }
        // only what the player's body collides with (and water, and near Mario for the diagnostics)
        let info = u32_at(body + BODY_FILTER);
        let layer = info & 0x7f;
        let is_water = water_layers.contains(&layer);
        // a body the world doesn't collide with: the u32 at +0x78 is 0xFFFFFFFF. The
        // map's whole second copy on layer 0x28 with its invisible walls, opened doors, switched
        // off barriers look like this; every floor and wall Sekiro walks on has a valid one.
        let in_world = u32_at(body + BODY_BROADPHASE) != u32::MAX;
        let solid = !is_water && in_world && (filter == 0 || player_info.is_none_or(|p| collides(filter, p, info)));
        if log_stats {
            let e = layers.entry(layer).or_insert((0, solid));
            e.0 += 1;
            if !in_world {
                *outside.entry(layer).or_default() += 1;
            }
        }
        let flat = Vec3::new(t.x - center.x, 0.0, t.z - center.z).length();
        // (only bodies in the world: while the map loads around a respawn none are, and none are solid)
        if !is_water && in_world && flat < radius + 50.0 {
            near.0 += 1;
            near.1 += solid as u32;
        }
        if !solid && !is_water && !(diag && flat < 60.0) {
            continue;
        }
        // cheap reject by the cached reach (unknown shapes are looked at once)
        if let Some(c) = cache.get(&shape) {
            if c.reach < 0.0 || flat - c.reach > radius + AHEAD {
                continue;
            }
        }
        if !readable(shape, 0x100) {
            continue;
        }
        let vtable = u64_at(shape);
        let data_off = MESH_DATA.load(Ordering::Relaxed).max(0x48);
        let key = u64_at(shape + data_off);
        let fresh = cache.get(&shape).is_some_and(|c| c.vtable == vtable && c.key == key && c.mesh.is_some());
        if !fresh {
            let name = class(shape).unwrap_or_default();
            let is_mesh = name.contains("CompressedMeshShape");
            let is_extern = name.contains("ExternMeshShape");
            if log_stats && flat < 30.0 {
                *seen_classes.entry(name.clone()).or_default() += 1;
            }
            if !is_mesh && !is_extern && !name.contains("CompoundShape") {
                cache.insert(shape, Cached { vtable, key, mesh: None, reach: -1.0 });
                continue;
            }
            // far meshes: only their reach (from the data's box) until Mario comes near
            if is_mesh && MESH_DATA.load(Ordering::Relaxed) != 0 {
                if let Some(reach) = domain_reach(key) {
                    if flat - reach > radius + AHEAD {
                        cache.insert(shape, Cached { vtable, key, mesh: None, reach });
                        continue;
                    }
                }
            }
            let tris = if is_mesh {
                decode_mesh_shape(shape)
            } else if is_extern {
                decode_extern(shape, &mut class)
            } else {
                decode_compound(shape, &mut class)
            };
            let key = u64_at(shape + MESH_DATA.load(Ordering::Relaxed).max(0x48));
            let mesh = tris.map(|t| Arc::new(Mesh::new(t)));
            let reach = mesh.as_ref().map(|m| m.reach).unwrap_or(-1.0);
            cache.insert(shape, Cached { vtable, key, mesh, reach });
        }
        let Some(mesh) = cache.get(&shape).and_then(|c| c.mesh.clone()) else { continue };
        if flat - mesh.reach > radius {
            continue;
        }
        // body transform: rotation columns at +0x00/+0x10/+0x20, translation +0x30
        let rot = Mat3::from_cols(vec3_at(body), vec3_at(body + 0x10), vec3_at(body + 0x20));
        let orthonormal = [rot.x_axis, rot.y_axis, rot.z_axis].iter().all(|v| (v.length() - 1.0).abs() < 0.02);
        let rot = if orthonormal { rot } else { Mat3::IDENTITY };
        let inv = rot.transpose();
        // the query box in body space (a rotated box: bound it generously)
        let local = inv * (center - t);
        let r = Vec3::new(radius, below.max(above), radius).length();
        idx.clear();
        mesh.query(local - Vec3::splat(r), local + Vec3::splat(r), &mut idx);
        for &k in &idx {
            let w = mesh.tris[k as usize].map(|v| rot * v + t);
            if !solid {
                if diag {
                    if let Some(y) = height_at(&w, center.x, center.z).filter(|y| (y - center.y).abs() < 6.0) {
                        surfaces.entry((layer, is_water)).or_default().push(y);
                    }
                }
                if is_water {
                    water.push(w);
                }
                continue;
            }
            if diag {
                if let Some(y) = height_at(&w, center.x, center.z).filter(|y| (y - center.y).abs() < 6.0) {
                    surfaces.entry((layer, true)).or_default().push(y);
                }
            }
            let lo = w[0].min(w[1]).min(w[2]);
            let hi = w[0].max(w[1]).max(w[2]);
            if hi.x < center.x - radius || lo.x > center.x + radius || hi.z < center.z - radius || lo.z > center.z + radius {
                continue;
            }
            if hi.y < center.y - below || lo.y > center.y + above {
                continue;
            }
            out.push(w);
            sources.push((body, shape));
        }
    }
    *SOURCES.lock().unwrap_or_else(|e| e.into_inner()) = out.iter().copied().zip(sources).map(|(t, (b, s))| (t, b, s)).collect();
    *WATER.lock().unwrap_or_else(|e| e.into_inner()) = water;
    // (and only when that lasts: one odd query, a warp or a load, mustn't make every invisible wall
    // solid for the rest of the session)
    static NONE_SOLID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if filter != 0 && near.0 >= 10 && near.1 == 0 {
        if NONE_SOLID.fetch_add(1, Ordering::Relaxed) + 1 >= 10 {
            log(format!("havok: the collision filter {filter:#x} leaves none of {} bodies near Mario solid: not the right one, not filtering", near.0));
            FILTER.store(0, Ordering::Relaxed);
        }
    } else {
        NONE_SOLID.store(0, Ordering::Relaxed);
    }
    if diag {
        static LAST: Mutex<String> = Mutex::new(String::new());
        let line = surfaces
            .iter()
            .map(|((l, used), ys)| {
                let mut ys = ys.clone();
                ys.sort_by(f32::total_cmp);
                ys.dedup_by(|a, b| (*a - *b).abs() < 0.05);
                let ys: Vec<String> = ys.iter().map(|y| format!("{:+.2}", y - center.y)).collect();
                format!("{l:#x}{} [{}]", if *used { "" } else { " (not solid)" }, ys.join(" "))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last != line {
            log(format!("havok: surfaces right above/below Mario by layer (m from his feet): {line}"));
            *last = line;
        }
    }
    if log_stats {
        STATS_LOGGED.fetch_add(1, Ordering::Relaxed);
        let mut v: Vec<_> = seen_classes.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        log(format!("havok: shapes of bodies within 30 m: {v:?}; {} triangles near Mario", out.len()));
        let mut l: Vec<_> = layers.into_iter().collect();
        l.sort();
        let fmt = |want: bool| l.iter().filter(|(_, (_, ok))| *ok == want).map(|(k, (n, _))| format!("{k:#x}x{n}")).collect::<Vec<_>>().join(" ");
        log(format!("havok: body layers the player collides with: [{}]; skipped: [{}]", fmt(true), fmt(false)));
        let out: Vec<String> = outside.iter().map(|(l, n)| format!("{l:#x}x{n}")).collect();
        log(format!("havok: bodies out of the world (broadphase id invalid, not solid) by layer: [{}]", out.join(" ")));
    }
    Some(out)
}

/// Where each triangle of the last query came from: (triangle, body, shape).
static SOURCES: Mutex<Vec<(Tri, usize, usize)>> = Mutex::new(Vec::new());

/// What a collision triangle is: its body, layer, shape class, and the body's raw fields (to see
/// what sets bodies the game doesn't collide with apart).
pub fn describe(tri: &Tri) -> String {
    let src = SOURCES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(t, _, _)| t == tri).map(|&(_, b, s)| (b, s));
    let Some((body, shape)) = src else { return "(not from the last query)".into() };
    let program = Program::current();
    let class = if readable(shape, 8) { class_name_of(&program, shape).unwrap_or_default() } else { String::new() };
    let info = u32_at(body + BODY_FILTER);
    let index = LAYOUT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .and_then(|l| read_u64(l.world + l.bodies).map(|b| (body - b as usize) / l.stride));
    let raw = if readable(body, 0xb0) {
        (0x40..0xb0).step_by(8).map(|o| format!("{:016x}", u64_at(body + o))).collect::<Vec<_>>().join(" ")
    } else {
        String::new()
    };
    format!(
        "body {body:#x} (#{index:?}) at {:.2}, layer {:#x} (info {info:#x}), shape {shape:#x} {class}; body +0x40..: {raw}",
        vec3_at(body + 0x30),
        info & 0x7f
    )
}

/// The first hit of the segment `from..to` against `tris` (world space), and the triangle.
pub fn raycast_tri(tris: &[Tri], from: Vec3, to: Vec3) -> Option<(Vec3, Tri)> {
    let d = to - from;
    let mut best: Option<(f32, Tri)> = None;
    for t in tris {
        let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-9 {
            continue;
        }
        let inv = 1.0 / det;
        let s = from - t[0];
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = d.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let k = e2.dot(q) * inv;
        if (0.0..=1.0).contains(&k) && best.is_none_or(|(b, _)| k < b) {
            best = Some((k, *t));
        }
    }
    best.map(|(k, t)| (from + d * k, t))
}

/// The first hit of the segment `from..to` against `tris` (world space).
pub fn raycast(tris: &[Tri], from: Vec3, to: Vec3) -> Option<Vec3> {
    let d = to - from;
    let mut best: Option<f32> = None;
    for t in tris {
        let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
        let p = d.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-9 {
            continue;
        }
        let inv = 1.0 / det;
        let s = from - t[0];
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = d.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let k = e2.dot(q) * inv;
        if (0.0..=1.0).contains(&k) && best.is_none_or(|b| k < b) {
            best = Some(k);
        }
    }
    best.map(|k| from + d * k)
}
