//! Mario's SM64 model, taken from libsm64 (so from the player's ROM): a Mario standing on a flat
//! floor gives the mesh (per-triangle body part, part-local positions and normals, UVs, colours),
//! and the same Mario in his star dance gives the peace-sign hand.

use crate::sm64;

pub const PEACE_FRAME: usize = 55;
const ACT_STAR_DANCE_NO_EXIT: u32 = 0x1307;
/// the peace-sign hand is SM64 part 9 (right hand)
const RIGHT_HAND: i32 = 9;

#[derive(Clone, Copy)]
pub struct Tri {
    pub part: i32,
    pub local: [[f32; 3]; 3],
    pub normal: [[f32; 3]; 3],
    pub uv: [[f32; 2]; 3],
    pub color: [[f32; 3]; 3],
    pub world: [[f32; 3]; 3],
}

pub struct MarioModel {
    pub tris: Vec<Tri>,
    /// per-part 4x4 row-vector matrices (world = [local, 1] * M)
    pub matrices: Vec<[f32; 16]>,
    pub peace: Vec<Tri>,
    /// SM64's texture atlas (11 textures of 64x64, RGBA)
    pub atlas: Vec<u8>,
}

struct Capture {
    mats: Vec<f32>,
    part: Vec<i32>,
    local: Vec<f32>,
    normal: Vec<f32>,
}

fn capture() -> (Capture, usize) {
    let mut c = Capture {
        mats: vec![0.0; 64 * 16],
        part: vec![0; sm64::GEO_MAX_TRIANGLES],
        local: vec![0.0; sm64::GEO_MAX_TRIANGLES * 9],
        normal: vec![0.0; sm64::GEO_MAX_TRIANGLES * 9],
    };
    let parts = unsafe { sm64::sm64_er_get_parts(c.mats.as_mut_ptr(), c.part.as_mut_ptr(), c.local.as_mut_ptr(), c.normal.as_mut_ptr()) };
    (c, parts.max(0) as usize)
}

fn tris(c: &Capture, geo: &sm64::Geometry, filter: impl Fn(i32) -> bool) -> Vec<Tri> {
    let v3 = |a: &[f32], t: usize, k: usize| [a[t * 9 + k * 3], a[t * 9 + k * 3 + 1], a[t * 9 + k * 3 + 2]];
    (0..geo.used())
        .filter(|&t| filter(c.part[t]))
        .map(|t| Tri {
            part: c.part[t],
            local: [0, 1, 2].map(|k| v3(&c.local, t, k)),
            normal: [0, 1, 2].map(|k| v3(&c.normal, t, k)),
            uv: [0, 1, 2].map(|k| [geo.uv[t * 6 + k * 2], geo.uv[t * 6 + k * 2 + 1]]),
            color: [0, 1, 2].map(|k| v3(&geo.color, t, k)),
            world: [0, 1, 2].map(|k| v3(&geo.position, t, k)),
        })
        .collect()
}

/// Runs on the libsm64 thread, after sm64_global_init and before the audio starts (the star
/// dance queues its jingle). Leaves no Mario and no surfaces behind that matter: the game's own
/// surfaces replace the test floor when Mario mode starts.
pub fn export(geo: &mut sm64::Geometry, atlas: Vec<u8>) -> Option<MarioModel> {
    let e = 8000;
    let floor = [
        sm64::SM64Surface::grass([[-e, 0, -e], [e, 0, e], [e, 0, -e]]),
        sm64::SM64Surface::grass([[-e, 0, -e], [-e, 0, e], [e, 0, e]]),
    ];
    unsafe { sm64::sm64_static_surfaces_load(floor.as_ptr(), floor.len() as u32) };
    let id = unsafe { sm64::sm64_mario_create(0.0, 0.0, 0.0) };
    if id < 0 {
        return None;
    }
    let inputs = sm64::SM64MarioInputs::default();
    let mut state = sm64::SM64MarioState::default();
    let mut tick = |geo: &mut sm64::Geometry| {
        let mut b = geo.buffers();
        unsafe { sm64::sm64_mario_tick(id, &inputs, &mut state, &mut *b) };
    };
    for _ in 0..3 {
        tick(geo);
    }
    let (c, parts) = capture();
    let model_tris = tris(&c, geo, |_| true);
    let matrices = (0..parts).map(|p| c.mats[p * 16..p * 16 + 16].try_into().unwrap()).collect();
    // the star dance (the same Mario two ticks later, like a fresh one after five)
    for _ in 0..2 {
        tick(geo);
    }
    unsafe { sm64::sm64_set_mario_action(id, ACT_STAR_DANCE_NO_EXIT) };
    for _ in 0..=PEACE_FRAME {
        tick(geo);
    }
    let (c, _) = capture();
    let peace = tris(&c, geo, |p| p == RIGHT_HAND);
    unsafe { sm64::sm64_mario_delete(id) };
    Some(MarioModel { tris: model_tris, matrices, peace, atlas })
}
