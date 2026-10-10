//! Runs libsm64 (the real Super Mario 64 movement/animation engine) inside Sekiro.
//!
//! SM64 ticks at 30 Hz on real time; the renderer interpolates between the last two ticks with one
//! factor per frame (also used by the camera, so the two never disagree). Mario stands on Sekiro's
//! real collision (havok.rs). Sekiro's character is pinned to Mario, hidden and weightless (it
//! stays the game's actor: enemies hit it, doors and menus work on it).
//!
//! The game takes its character back for a while when it must move it itself: the grappling hook,
//! and long falls (so the game's own fall deaths and kill triggers happen); Mario rides along and
//! takes over again where the character stops.
//!
//! F8 switches between Mario mode and playing Sekiro (Mario hidden and frozen; he comes back where
//! Sekiro is). F9 switches Lakitu's camera / Sekiro's.
//!
//! Coordinates: SM64 is right-handed, Sekiro left-handed. Like er-mario we mirror X, relative to a
//! local origin that follows Mario: `world = origin + (-x, y, z) * SCALE`.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use glam::Vec3;

use crate::sm64;
use crate::worker;

/// Metres per SM64 unit (Mario is ~160 units tall ≈ 1.6 m).
pub(crate) const SCALE: f32 = 0.01;
/// Flat-floor half-extent, in SM64 units (~40 m each way), when there's no real collision.
const FLOOR_HALF: i32 = 4000;
/// SM64's tick length.
const TICK: f32 = 1.0 / 30.0;
/// Collision around Mario (m): horizontal half-size, below, above; reloaded after moving this far.
const COL_RADIUS: f32 = 10.0;
const COL_BELOW: f32 = 15.0;
const COL_ABOVE: f32 = 10.0;
const COL_RELOAD_DIST: f32 = 3.0;
const COL_MAX_TRIS: usize = 10000;
/// The safety floor's depth under Mario (m).
const SAFETY_DEPTH: f32 = 30.0;
/// The game moving its character this far (m) on its own is a teleport (respawn, warp, load):
/// Mario goes there instead of pulling the character back.
const TELEPORT: f32 = 5.0;
/// Falling this far (m) below the last ground hands the character to the game's physics (its fall
/// deaths and kill triggers only happen to a character the game itself drops).
const FALL_HANDOFF: f32 = 10.0;
/// SM64's idle action.
const ACT_IDLE: u32 = 0x0C40_0201;
/// SM64's free fall.
const ACT_FREEFALL: u32 = 0x0100_088C;
const ACT_FLAG_AIR: u32 = 0x0000_0800;
/// SM64's ground pound landing (Sekiro's attack hits there).
const ACT_GROUND_POUND_LAND: u32 = 0x0080_023C;
/// SM64's dive and the belly slide after it
const ACT_DIVE: u32 = 0x0188_088A;
const ACT_DIVE_SLIDE: u32 = 0x0088_0456;
/// SM64's action groups (bits 6..8): stationary, and submerged (swimming)
const ACT_GROUP_MASK: u32 = 0x1C0;
const ACT_GROUP_SUBMERGED: u32 = 0x0C0;
/// F10 rewinds this far back (s), up to REWINDS times in a row.
const REWIND_STEP: f32 = 10.0;
const REWINDS: u32 = 5;
/// SM64's background songs for F7 (sequence ids: title, Bob-omb Battlefield, castle, water, Lethal
/// Lava Land, Bowser, snow, slide, Big Boo's Haunt, Hazy Maze Cave, wing cap, metal cap, Bowser's
/// road, merry-go-round, Koopa the Quick, boss, endless stairs, final Bowser, credits).
const SONGS: [u8; 19] = [0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0C, 0x0E, 0x0F, 0x11, 0x13, 0x14, 0x16, 0x18, 0x19, 0x1A];
/// The origin moves to Mario once he's this far from it (SM64 units).
const RECENTER: f32 = 5000.0;

static ROM: OnceLock<Vec<u8>> = OnceLock::new();
static TEXTURE: OnceLock<Vec<u8>> = OnceLock::new();
static ENGINE_READY: AtomicBool = AtomicBool::new(false);
static MARIO_ID: AtomicI32 = AtomicI32::new(-1);
/// Mario mode (F8): off = playing Sekiro, Mario hidden and frozen.
/// Mario mode (F8 / Select); a savegame starts in Sekiro mode.
static MARIO_MODE: AtomicBool = AtomicBool::new(false);
/// No player character since the last time in the world (the title screen): the next time in the
/// world is a savegame loaded.
static NO_PLAYER: AtomicBool = AtomicBool::new(true);
/// In the world (not a load screen).
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// The game moves its character itself for a while (grappling hook, a long fall): Mario rides.
static GAME_DRIVES: Mutex<Option<GameDrive>> = Mutex::new(None);
/// When the grappling hook was last pressed (B / F in Mario mode).
static GRAPPLE_PRESSED: Mutex<Option<Instant>> = Mutex::new(None);
/// The game pulling the character this long after the grappling hook was pressed is it (s)
const GRAPPLE_WINDOW: f32 = 1.2;
/// When Sekiro last lost HP (a knockback moves him: not the grappling hook)
static HURT_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// No deathblow until then: the game's deathblow target was there, but Sekiro's attack wasn't one
static DEATHBLOW_BLOCKED: Mutex<Option<Instant>> = Mutex::new(None);
/// A real deathblow moves the character at least this long (s); shorter was a plain attack
const DEATHBLOW_MIN: f32 = 0.9;
/// The character moving less than this in a frame: the game's pull is over (m)
const GRAPPLE_LANDED_STEP: f32 = 0.12;
/// This frame's interpolation factor between the last two ticks (f32 bits).
static ALPHA: AtomicU32 = AtomicU32::new(0x3f80_0000);
/// The height Mario last stood on real ground (world m).
static LAST_GROUND: Mutex<Option<f32>> = Mutex::new(None);
/// The safety floor's height (world m).
static SAFETY_Y: Mutex<Option<f32>> = Mutex::new(None);
/// The local origin (world metres).
static ORIGIN: Mutex<[f32; 3]> = Mutex::new([0.0; 3]);
/// Collision triangle winding: 0 unknown, 1 as is, 2 flipped.
static WINDING: AtomicU8 = AtomicU8::new(0);
/// The collision triangles loaded into SM64 (world space), for Lakitu's rays.
static LOADED: Mutex<Option<(Arc<Vec<crate::havok::Tri>>, Vec3, Instant)>> = Mutex::new(None);
/// Where we last put the character.
static PINNED: Mutex<Option<Vec3>> = Mutex::new(None);

/// A game menu is open (MenuMan's menu mode): the pad's buttons are the menu's.
static MENU: AtomicBool = AtomicBool::new(false);
/// Where Mario stood (on real ground) over the last minute, for F10.
static HISTORY: Mutex<std::collections::VecDeque<(Instant, Vec3)>> = Mutex::new(std::collections::VecDeque::new());
/// The rewind chain: how many F10s in a row, and the time the last one went back to.
static REWIND: Mutex<Option<(u32, Instant, Instant)>> = Mutex::new(None);
/// F7: SM64 music playing (and Sekiro's music volume to restore).
static MUSIC: Mutex<Option<(u16, Option<u8>)>> = Mutex::new(None);
/// The water level given to SM64 last (world m; NaN: none).
static WATER_SET: AtomicU32 = AtomicU32::new(0x7fc0_0000);

/// In the world, no menu open: Select is F8.
pub fn in_world_no_menu() -> bool {
    VISIBLE.load(Ordering::Relaxed) && !MENU.load(Ordering::Relaxed)
}

/// Mario mode in the world, no menu: Mario's buttons are kept from the game.
pub fn controls_game() -> bool {
    shown() && !MENU.load(Ordering::Relaxed)
}

fn update_menu() {
    use fromsoftware_shared::FromStatic;
    let open = sekiro::sprj::MenuMan::instance_ptr().ok().filter(|p| !p.is_null()).is_some_and(|p| {
        let m = p as usize;
        // MenuMan::is_menu_mode: flags[9] > 0 (flags start at +0x1c)
        crate::mem::read_u32(m + 0x1c + 9 * 4).is_some_and(|v| (v as i32) > 0)
    });
    if MENU.swap(open, Ordering::Relaxed) != open {
        crate::log(if open { "menu: open (the pad's buttons go to the game)" } else { "menu: closed" });
    }
}

struct GameDrive {
    why: &'static str,
    since: Instant,
    last: Vec3,
    last_moved: Instant,
    max_secs: f32,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// In the world (not a load screen) and in Mario mode: Mario is drawn, the HUD shown.
pub fn shown() -> bool {
    VISIBLE.load(Ordering::Relaxed) && MARIO_MODE.load(Ordering::Relaxed)
}

/// No player character: the title screen and main menu (and the start of a load from it).
pub fn at_title() -> bool {
    NO_PLAYER.load(Ordering::Relaxed)
}

fn in_mario_mode() -> bool {
    MARIO_MODE.load(Ordering::Relaxed)
}

/// Not a load screen (the game parks its camera at a fixed default while loading) for a second.
fn update_in_world() -> bool {
    static SINCE: Mutex<Option<Instant>> = Mutex::new(None);
    let loading = crate::player::position().is_none() || crate::camera::is_load_screen();
    let mut since = lock(&SINCE);
    if loading {
        *since = None;
    } else if since.is_none() {
        *since = Some(Instant::now());
    }
    let in_world = since.is_some_and(|t| t.elapsed().as_secs_f32() > 1.0);
    if VISIBLE.swap(in_world, Ordering::Relaxed) != in_world {
        crate::log(if in_world { "world: in the world" } else { "world: load screen" });
        if in_world && NO_PLAYER.swap(false, Ordering::Relaxed) {
            crate::log("world: a savegame loaded: Sekiro mode");
            if in_mario_mode() {
                set_mode(false);
            }
            crate::hud::banner("Press F1 for controls/help");
        }
    }
    in_world
}

fn origin() -> [f32; 3] {
    *lock(&ORIGIN)
}

/// SM64 position -> Sekiro world (metres).
pub fn to_world(p: [f32; 3]) -> [f32; 3] {
    let o = origin();
    [o[0] - p[0] * SCALE, o[1] + p[1] * SCALE, o[2] + p[2] * SCALE]
}

/// Sekiro world (metres) -> SM64 position.
pub fn to_sm64(p: [f32; 3]) -> [f32; 3] {
    let o = origin();
    [-(p[0] - o[0]) / SCALE, (p[1] - o[1]) / SCALE, (p[2] - o[2]) / SCALE]
}

/// The last two SM64 ticks' output in world space (metres), for interpolation.
struct Ticks {
    prev_mesh: Vec<f32>,
    mesh: Vec<f32>,
    color: Vec<f32>,
    normal: Vec<f32>,
    uv: Vec<f32>,
    prev_pos: Vec3,
    pos: Vec3,
    state: sm64::SM64MarioState,
}

static TICKS: Mutex<Option<Ticks>> = Mutex::new(None);

/// One frame of Mario for the renderer: world-space positions (metres) interpolated between the
/// last two ticks, world-space normals.
pub struct MeshFrame {
    pub position: Vec<f32>,
    pub color: Vec<f32>,
    pub normal: Vec<f32>,
    /// `tris*6` floats, texture atlas uv per vertex ((1,1) = untextured).
    pub uv: Vec<f32>,
}

fn alpha() -> f32 {
    f32::from_bits(ALPHA.load(Ordering::Relaxed))
}

pub fn mesh_frame() -> MeshFrame {
    let guard = lock(&TICKS);
    let Some(t) = guard.as_ref() else {
        return MeshFrame { position: Vec::new(), color: Vec::new(), normal: Vec::new(), uv: Vec::new() };
    };
    let a = alpha();
    let mut position = t.mesh.clone();
    if t.prev_mesh.len() == t.mesh.len() {
        // per vertex; big jumps (teleports) snap instead of sliding
        for (k, v) in position.chunks_exact_mut(3).enumerate() {
            let p = &t.prev_mesh[k * 3..k * 3 + 3];
            let d2 = (v[0] - p[0]).powi(2) + (v[1] - p[1]).powi(2) + (v[2] - p[2]).powi(2);
            if d2 < 4.0 {
                for i in 0..3 {
                    v[i] = p[i] + (v[i] - p[i]) * a;
                }
            }
        }
    }
    MeshFrame { position, color: t.color.clone(), normal: t.normal.clone(), uv: t.uv.clone() }
}

/// Mario's interpolated world position in metres.
pub fn world_pos() -> [f32; 3] {
    let guard = lock(&TICKS);
    let Some(t) = guard.as_ref() else { return origin() };
    let p = if t.prev_pos.distance(t.pos) < 2.0 { t.prev_pos.lerp(t.pos, alpha()) } else { t.pos };
    p.into()
}

fn state_action() -> Option<u32> {
    lock(&TICKS).as_ref().map(|t| t.state.action)
}

fn state() -> Option<sm64::SM64MarioState> {
    lock(&TICKS).as_ref().map(|t| t.state)
}

/// The SM64 texture atlas (`TEXTURE_W x TEXTURE_H` RGBA8), once libsm64 is initialised.
pub fn texture() -> Option<&'static [u8]> {
    TEXTURE.get().map(Vec::as_slice)
}

/// Loads the ROM and initialises libsm64 (Mario, sound, HUD). Runs on the startup thread.
pub fn start() {
    let rom = match crate::paths::read_rom() {
        Ok(rom) => rom,
        Err(e) => {
            crate::log(format!("mario: {e}; Mario stays off"));
            return;
        }
    };
    crate::log(format!("mario: SM64 ROM ok ({} bytes)", rom.len()));
    crate::hud::load(&rom);
    let _ = ROM.set(rom);
    let rom: &'static [u8] = ROM.get().unwrap();

    // Initialising reads the whole ROM and builds the texture atlas; give it room.
    let texture = worker::call_timeout("sm64_global_init", Duration::from_secs(20), move |_ctx| {
        let mut texture = vec![0u8; sm64::TEXTURE_W * sm64::TEXTURE_H * 4];
        unsafe { sm64::sm64_global_init(rom.as_ptr(), texture.as_mut_ptr()) };
        unsafe { sm64::sm64_audio_init(rom.as_ptr()) };
        texture
    });
    match texture {
        Some(texture) => {
            let _ = TEXTURE.set(texture);
            ENGINE_READY.store(true, Ordering::Relaxed);
            crate::log("mario: libsm64 initialised (with sound)");
            crate::audio::start();
        }
        None => crate::log("mario: libsm64 initialisation failed/hung; Mario stays off"),
    }
}

// ---- collision -----------------------------------------------------------------------------------

/// World triangles -> SM64 surfaces (origin-relative, mirrored; the mirroring flips the winding,
/// so it is flipped back: the game's floors then face up). `winding = keep` in the ini keeps it;
/// `winding = auto` decides once from the floors under Mario (a vote that a spot with geometry
/// facing both ways got wrong: Mario fell through every floor).
fn surfaces_from(tris: &[crate::havok::Tri], mario: Vec3) -> Vec<sm64::SM64Surface> {
    let conv = |v: Vec3| Vec3::from(to_sm64(v.into()));
    let normal = |t: &[Vec3; 3]| (t[1] - t[0]).cross(t[2] - t[1]);
    if WINDING.load(Ordering::Relaxed) == 0 {
        let setting = crate::paths::config("winding").map(|v| v.to_ascii_lowercase());
        match setting.as_deref() {
            Some("keep") => {
                WINDING.store(1, Ordering::Relaxed);
                crate::log("collision: winding kept (sk_mario.ini)");
            }
            Some("auto") => {}
            _ => {
                WINDING.store(2, Ordering::Relaxed);
                crate::log("collision: flipping the winding (the game's floors face up in SM64 that way)");
            }
        }
    }
    if WINDING.load(Ordering::Relaxed) == 0 {
        // floors under Mario must face up: count how the near, flat ones face as they are
        let (mut up, mut down) = (0, 0);
        for t in tris {
            let c = (t[0] + t[1] + t[2]) / 3.0;
            if Vec3::new(c.x - mario.x, 0.0, c.z - mario.z).length() > 3.0 || c.y > mario.y + 0.5 {
                continue;
            }
            let n = normal(&t.map(conv)).normalize_or_zero();
            if n.y > 0.7 {
                up += 1;
            } else if n.y < -0.7 {
                down += 1;
            }
        }
        if up + down >= 2 {
            let flip = down > up;
            WINDING.store(if flip { 2 } else { 1 }, Ordering::Relaxed);
            crate::log(format!(
                "collision: floors under Mario face {} ({up} up, {down} down): {}",
                if flip { "down" } else { "up" },
                if flip { "flipping the winding" } else { "winding kept" }
            ));
        }
    }
    let flip = WINDING.load(Ordering::Relaxed) == 2;
    let r = |p: Vec3| [p.x.round() as i32, p.y.round() as i32, p.z.round() as i32];
    tris.iter()
        .filter_map(|t| {
            let s = t.map(conv);
            if normal(&s).length_squared() < 1e-6 {
                return None;
            }
            Some(sm64::SM64Surface::grass(if flip { [r(s[0]), r(s[2]), r(s[1])] } else { [r(s[0]), r(s[1]), r(s[2])] }))
        })
        .collect()
}

/// Loads the real collision around `center` (world) into SM64. False if there's none to load.
fn load_collision(center: Vec3) -> bool {
    let Some(mut tris) = crate::havok::query(center, COL_RADIUS, COL_BELOW, COL_ABOVE) else { return false };
    if tris.is_empty() {
        return false;
    }
    if tris.len() > COL_MAX_TRIS {
        let d = |t: &crate::havok::Tri| ((t[0] + t[1] + t[2]) / 3.0).distance_squared(center);
        tris.sort_by(|a, b| d(a).total_cmp(&d(b)));
        tris.truncate(COL_MAX_TRIS);
    }
    let mut surfaces = surfaces_from(&tris, center);
    let n = surfaces.len();
    // the safety floor stays where it is while Mario is near it (moving it down under a Mario
    // standing on it dropped him another 30 m)
    let safety = {
        let mut y = lock(&SAFETY_Y);
        let keep = y.is_some_and(|y| center.y - y > 5.0 && center.y - y < SAFETY_DEPTH + 5.0);
        if !keep {
            *y = Some(center.y - SAFETY_DEPTH);
        }
        y.unwrap()
    };
    surfaces.extend(safety_floor(center, safety));
    let ok = worker::call_timeout("load surfaces", Duration::from_secs(3), move |_| unsafe {
        sm64::sm64_static_surfaces_load(surfaces.as_ptr(), surfaces.len() as u32)
    })
    .is_some();
    if ok {
        static FIRST: AtomicBool = AtomicBool::new(true);
        if FIRST.swap(false, Ordering::Relaxed) {
            crate::log(format!("collision: {n} real triangles loaded around Mario"));
        }
        *lock(&LOADED) = Some((Arc::new(tris), center, Instant::now()));
    }
    ok
}

/// A big floor far under Mario: SM64 freezes Mario where there's no floor at all (out of bounds),
/// so there always is one, if only to fall onto (landing there hands the character to the game).
fn safety_floor(center: Vec3, y: f32) -> [sm64::SM64Surface; 2] {
    let [x, _, z] = to_sm64(center.into());
    let fy = to_sm64([center.x, y, center.z])[1];
    let (cx, cz, fy, s) = (x as i32, z as i32, fy as i32, 8000);
    [
        sm64::SM64Surface::grass([[cx - s, fy, cz - s], [cx + s, fy, cz + s], [cx + s, fy, cz - s]]),
        sm64::SM64Surface::grass([[cx + s, fy, cz + s], [cx - s, fy, cz - s], [cx - s, fy, cz + s]]),
    ]
}

fn load_flat_floor(center: Vec3) {
    let [x, y, z] = to_sm64(center.into());
    let (cx, cz, fy) = (x as i32, z as i32, y as i32 - 10);
    let s = FLOOR_HALF;
    // two triangles, wound so their normals point up (+Y)
    let surfaces = [
        sm64::SM64Surface::grass([[cx - s, fy, cz - s], [cx + s, fy, cz + s], [cx + s, fy, cz - s]]),
        sm64::SM64Surface::grass([[cx + s, fy, cz + s], [cx - s, fy, cz - s], [cx - s, fy, cz + s]]),
    ];
    worker::call("flat floor", move |_| unsafe { sm64::sm64_static_surfaces_load(surfaces.as_ptr(), surfaces.len() as u32) });
}

/// The first collision hit between two world points (Lakitu's rays).
pub fn ray(from: Vec3, to: Vec3) -> Option<Vec3> {
    let tris = lock(&LOADED).as_ref().map(|(t, _, _)| t.clone())?;
    crate::havok::raycast(&tris, from, to)
}

/// Keeps the collision around Mario loaded, and the origin near him.
fn update_collision(mario: Vec3) {
    if !crate::havok::ready() {
        return;
    }
    let id = MARIO_ID.load(Ordering::Relaxed);
    // re-centre: keep SM64's numbers small
    let sm = to_sm64(mario.into());
    if sm.iter().any(|v| v.abs() > RECENTER) {
        *lock(&ORIGIN) = mario.into();
        worker::call("recentre", move |_| unsafe { sm64::sm64_set_mario_position(id, 0.0, 0.0, 0.0) });
        crate::log(format!("mario: origin moved to {mario:.2?}"));
        load_collision(mario);
        return;
    }
    let due = match lock(&LOADED).as_ref() {
        Some((_, c, at)) => c.distance(mario) > COL_RELOAD_DIST || at.elapsed().as_secs_f32() > 2.0,
        None => true,
    };
    if due {
        load_collision(mario);
    }
}

// ---- Mario ---------------------------------------------------------------------------------------

fn create_mario() {
    if MARIO_ID.load(Ordering::Relaxed) >= 0 {
        return;
    }
    let Some(pos) = crate::player::position() else { return };
    crate::havok::start();
    if !VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    // wait for the Havok world (at most 10 s in the world; then a flat floor until it's found)
    static WAITING: Mutex<Option<Instant>> = Mutex::new(None);
    let waited = lock(&WAITING).get_or_insert_with(Instant::now).elapsed();
    if !crate::havok::ready() && waited < Duration::from_secs(10) {
        return;
    }
    *lock(&ORIGIN) = pos;
    let center = Vec3::from(pos);
    let real = load_collision(center);
    if !real {
        crate::log("mario: no real collision here yet; a flat floor under the player");
        load_flat_floor(center);
    }
    let [x, y, z] = to_sm64(pos);
    let id = worker::call_timeout("mario_create", Duration::from_secs(5), move |_ctx| unsafe { sm64::sm64_mario_create(x, y + 60.0, z) });
    match id {
        Some(id) if id >= 0 => {
            MARIO_ID.store(id, Ordering::Relaxed);
            *lock(&PINNED) = Some(center);
            *lock(&LAST_GROUND) = Some(center.y);
            crate::log(format!(
                "mario: created id {id} at ({:.2}, {:.2}, {:.2}), {} collision",
                pos[0],
                pos[1],
                pos[2],
                if real { "real" } else { "flat" }
            ));
        }
        other => crate::log(format!("mario: sm64_mario_create failed ({other:?}); retrying")),
    }
}

/// Whether Sekiro's window has the keyboard focus (so typing elsewhere doesn't move Mario).
pub fn focused() -> bool {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

pub fn key_down(vk: i32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    focused() && (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
}

/// Pad and keyboard -> SM64 inputs. Pad: left stick, A jump, X punch, LT crouch (B is Sekiro's
/// grappling hook). Keyboard: WASD, Space jump, N punch, K crouch (F grapples). Movement is
/// relative to the camera.
fn read_inputs() -> sm64::SM64MarioInputs {
    let mut inputs = sm64::SM64MarioInputs { cam_look_x: 0.0, cam_look_z: -1.0, ..Default::default() };
    if let Some(p) = crate::pad::read().filter(|_| focused()) {
        inputs.stick_x = p.stick_x;
        inputs.stick_y = p.stick_y;
        inputs.button_a = p.a as u8;
        inputs.button_b = p.b as u8;
        inputs.button_z = p.z as u8;
    }
    if inputs.stick_x == 0.0 && inputs.stick_y == 0.0 {
        let axis = |neg: i32, pos: i32| (key_down(pos) as i32 - key_down(neg) as i32) as f32;
        // SM64: stick up (forward) is negative Y
        let (x, y) = (axis(0x41, 0x44), axis(0x57, 0x53)); // A/D, W/S
        let len = (x * x + y * y).sqrt().max(1.0);
        inputs.stick_x = x / len;
        inputs.stick_y = y / len;
    }
    inputs.button_a |= key_down(0x20) as u8; // Space
    inputs.button_b |= key_down(0x4E) as u8; // N
    inputs.button_z |= key_down(0x4B) as u8; // K
    // riding the grappling hook's pull: never crouching
    if lock(&GAME_DRIVES).as_ref().is_some_and(|d| d.why == "grapple") {
        inputs.button_z = 0;
    }

    // camera-relative: libsm64 takes the camera's look direction (in SM64 axes); kept as it was
    // while Mario swings a boss (the spin would turn the camera, and with it the stick)
    static LOOK: Mutex<Option<(f32, f32)>> = Mutex::new(None);
    if holding_boss() {
        if let Some((x, z)) = *lock(&LOOK) {
            inputs.cam_look_x = x;
            inputs.cam_look_z = z;
            return inputs;
        }
    }
    let player = crate::player::position().map(Vec3::from).unwrap_or(Vec3::from(world_pos()));
    let fwd = crate::lakitu::forward().or_else(|| crate::camera::active(player).map(|c| c.forward));
    if let Some(f) = fwd {
        let f = glam::Vec2::new(-f.x, f.z);
        if f.length_squared() > 1e-4 {
            let f = f.normalize();
            inputs.cam_look_x = f.x;
            inputs.cam_look_z = f.y;
        }
    }
    *lock(&LOOK) = Some((inputs.cam_look_x, inputs.cam_look_z));
    inputs
}

/// Whether Mario has a boss by the tail (SM64's Bowser grab, swing and throw).
fn holding_boss() -> bool {
    state_action().is_some_and(|a| matches!(a, crate::grab::ACT_PICKING_UP_BOWSER | crate::grab::ACT_HOLDING_BOWSER | crate::grab::ACT_RELEASING_BOWSER))
}

fn tick() {
    let id = MARIO_ID.load(Ordering::Relaxed);
    if id < 0 {
        return;
    }
    let inputs = read_inputs();
    let stick = (inputs.stick_x, inputs.stick_y);
    let camera_sounds = crate::lakitu::take_sounds();
    // locked on with Sekiro's camera: a punch turns Mario to the target first (aim.rs)
    static B_WAS: AtomicBool = AtomicBool::new(false);
    let b_edge = inputs.button_b != 0 && !B_WAS.swap(true, Ordering::Relaxed);
    if inputs.button_b == 0 {
        B_WAS.store(false, Ordering::Relaxed);
    }
    let aim = if b_edge {
        crate::aim::locked().and_then(|t| crate::aim::towards(Vec3::from(world_pos()), t)).map(crate::aim::face_angle)
    } else {
        None
    };
    let out = worker::call("sm64_mario_tick", move |ctx| {
        for s in camera_sounds {
            unsafe { sm64::sm64_play_sound_global(s) };
        }
        if let Some(a) = aim {
            unsafe { sm64::sm64_set_mario_faceangle(id, a) };
        }
        let mut state = sm64::SM64MarioState::default();
        {
            let mut buffers = ctx.geo.buffers();
            unsafe { sm64::sm64_mario_tick(id, &inputs, &mut state, &mut *buffers) };
        }
        // SM64's sound engine runs at the same 30 Hz as Mario
        let mut buf = [0i16; 544 * 2 * 2];
        let frames = unsafe { sm64::sm64_audio_tick(crate::audio::queued(), 1100, buf.as_mut_ptr()) } as usize;
        crate::audio::push(&buf[..(frames * 2 * 2).min(buf.len())]);
        let used = ctx.geo.used();
        let mesh = ctx.geo.position[..used * 9].to_vec();
        let color = ctx.geo.color[..used * 9].to_vec();
        let normal = ctx.geo.normal[..used * 9].to_vec();
        let uv = ctx.geo.uv[..used * 6].to_vec();
        (state, mesh, color, normal, uv)
    });
    let Some((state, mut mesh, color, mut normal, uv)) = out else { return };
    // to world space now (the origin may move later)
    for v in mesh.chunks_exact_mut(3) {
        let w = to_world([v[0], v[1], v[2]]);
        v.copy_from_slice(&w);
    }
    for n in normal.chunks_exact_mut(3) {
        n[0] = -n[0];
    }
    let mut pos = Vec3::from(to_world(state.position));
    // tunnelling: falling, Mario went down through a surface of the loaded collision (one SM64
    // didn't take as a floor): back on top of it
    let last = lock(&TICKS).as_ref().map(|t| (t.pos, t.state.action));
    if let Some((last, last_action)) = last {
        let falling = state.action & ACT_FLAG_AIR != 0 && last_action & ACT_FLAG_AIR != 0 && pos.y < last.y - 0.15;
        if falling && last.distance(pos) < 6.0 && lock(&GAME_DRIVES).is_none() {
            let hit = ray(last + Vec3::Y * 0.3, pos).filter(|h| h.y > pos.y + 0.05);
            // (a floor: straight down there it's at the same height too)
            let floor = hit.and_then(|h| ray(Vec3::new(h.x, last.y + 0.3, h.z), Vec3::new(h.x, pos.y - 0.1, h.z)).filter(|v| (v.y - h.y).abs() < 0.3));
            if let Some(f) = floor {
                static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if LOGGED.fetch_add(1, Ordering::Relaxed) < 20 {
                    crate::log(format!("collision: Mario fell through the floor at {f:.2} (from {last:.2} to {pos:.2}); back on top of it"));
                }
                let at = f + Vec3::Y * 0.05;
                let sm = to_sm64(at.into());
                worker::call("untunnel", move |_| unsafe {
                    sm64::sm64_set_mario_position(id, sm[0], sm[1], sm[2]);
                    sm64::sm64_set_mario_velocity(id, 0.0, 0.0, 0.0);
                });
                pos = at;
            }
        }
    }
    wall_report(stick, &state, pos);
    let tris = mesh.len() / 9;
    let before = state_action();
    // a boss in Mario's hands (or flying): grab.rs; the grab starts SM64's Bowser pickup
    crate::grab::tick(TICK, pos, state.face_angle, state.action);
    if crate::grab::take_start() {
        worker::call("grab", move |_| unsafe {
            sm64::sm64_set_mario_action(id, crate::grab::ACT_PICKING_UP_BOWSER);
            sm64::sm64_play_sound_global(crate::grab::SOUND_GRAB);
        });
    }
    if crate::hit::ready() {
        attacks(&state, pos);
    } else {
        // (no bullets yet: Sekiro's sword hits for Mario)
        // a ground pound landing is Sekiro's attack (the game's own hit where Mario lands)
        if state.action == ACT_GROUND_POUND_LAND && before != Some(ACT_GROUND_POUND_LAND) {
            crate::pad::pulse_attack(0.2);
        }
        // so are a dive (when it starts, and where it lands)
        if (state.action == ACT_DIVE || state.action == ACT_DIVE_SLIDE) && before != Some(state.action) {
            crate::pad::pulse_attack(0.2);
        }
    }
    {
        let mut guard = lock(&TICKS);
        let (prev_mesh, prev_pos) = match guard.take() {
            Some(t) => (t.mesh, t.pos),
            None => (Vec::new(), pos),
        };
        *guard = Some(Ticks { prev_mesh, mesh, color, normal, uv, prev_pos, pos, state });
    }
    // the last real ground (not the safety floor) Mario stood on, and the rewind history
    let swimming = state.action & ACT_GROUP_MASK == ACT_GROUP_SUBMERGED || state.action & 0x2000 != 0;
    let alive = crate::player::hp().is_some_and(|(hp, _)| hp > 0);
    if state.action & ACT_FLAG_AIR == 0 && !swimming && alive && !on_safety_floor(pos) && lock(&GAME_DRIVES).is_none() {
        *lock(&LAST_GROUND) = Some(pos.y);
        let mut h = lock(&HISTORY);
        if h.back().is_none_or(|(t, _)| t.elapsed().as_secs_f32() >= 0.5) {
            h.push_back((Instant::now(), pos));
            while h.front().is_some_and(|(t, _)| t.elapsed().as_secs_f32() > REWIND_STEP * (REWINDS as f32 + 1.0)) {
                h.pop_front();
            }
        }
    }
    // water: the water surface over Mario (Sekiro's water is collision on its own layers)
    // (SM64 takes it in its own coordinates, which move with the origin: compared as sent)
    let level = crate::havok::water_level_at(pos.x, pos.z).filter(|y| *y > pos.y - 3.0);
    let sm = level.map(|l| to_sm64([pos.x, l, pos.z])[1] as i32).unwrap_or(-100_000);
    let prev = WATER_SET.swap(sm as u32, Ordering::Relaxed) as i32;
    if prev != sm {
        worker::call("water", move |_| unsafe { sm64::sm64_set_mario_water_level(id, sm) });
        if let Some(l) = level.filter(|_| prev == -100_000 || (prev - sm).abs() > 20) {
            crate::log(format!("water: surface at {l:.2} (Mario at {:.2})", pos.y));
        }
    }
    crate::hud::set((state.health.max(0) as u16 >> 8) as u8);

    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if N.fetch_add(1, Ordering::Relaxed) % 150 == 0 {
        crate::log(format!(
            "mario: pos ({:.2}, {:.2}, {:.2}) action {:#x} anim {} health {:#x} tris {tris}",
            pos.x, pos.y, pos.z, state.action, state.anim_id, state.health
        ));
    }
}

// SM64's flags for the active frames of a punch, a kick and a sweep kick
const MARIO_PUNCHING: u32 = 0x0010_0000;
const MARIO_KICKING: u32 = 0x0020_0000;
const MARIO_TRIPPING: u32 = 0x0040_0000;

/// Mario's hits as the game's bullets (hit.rs): a punch or kick when its active frames start,
/// a dive when it starts and every 0.3 s while he dives or slides, a ground pound where he lands.
/// Called before this tick's state is stored (TICKS still holds the last one).
fn attacks(state: &sm64::SM64MarioState, pos: Vec3) {
    use crate::hit::{Attack, fire};
    static DIVE_HIT: Mutex<Option<Instant>> = Mutex::new(None);
    let last = lock(&TICKS).as_ref().map(|t| (t.state.flags, t.state.action));
    let (flags_before, action_before) = last.unwrap_or((0, 0));
    let facing = Vec3::new(-state.face_angle.sin(), 0.0, state.face_angle.cos());
    // locked on (Sekiro's camera): the hits go to the target
    let forward = crate::aim::locked().and_then(|t| crate::aim::towards(pos, t)).unwrap_or(facing);
    const HITTING: u32 = MARIO_PUNCHING | MARIO_KICKING | MARIO_TRIPPING;
    if state.flags & HITTING != 0 && flags_before & HITTING == 0 {
        let blocked = lock(&DEATHBLOW_BLOCKED).is_some_and(|t| Instant::now() < t);
        if !blocked && crate::player::deathblow_ready() {
            // the game offers a deathblow: Sekiro's attack does it (the game moves him for the
            // kill; Mario rides along)
            crate::log("deathblow: Mario's punch is Sekiro's attack");
            crate::pad::pulse_attack(0.25);
            if let Some(at) = crate::player::position().map(Vec3::from) {
                *lock(&PINNED) = None;
                hand_to_game("deathblow", at, 8.0);
                if let Some(d) = lock(&GAME_DRIVES).as_mut() {
                    d.last_moved = Instant::now() + std::time::Duration::from_millis(700);
                }
            }
        } else if !crate::grab::try_grab(pos, forward) {
            fire(Attack::Punch, pos + forward * 0.6 + Vec3::Y * 0.7, forward);
        }
    }
    let diving = state.action == ACT_DIVE || state.action == ACT_DIVE_SLIDE;
    if diving {
        let mut t = lock(&DIVE_HIT);
        let started = action_before != ACT_DIVE && action_before != ACT_DIVE_SLIDE;
        if started || t.is_none_or(|t| t.elapsed().as_secs_f32() >= 0.3) {
            *t = Some(Instant::now());
            fire(Attack::Dive, pos + forward * 0.5 + Vec3::Y * 0.4, forward);
        }
    }
    if state.action == ACT_GROUND_POUND_LAND && action_before != ACT_GROUND_POUND_LAND {
        fire(Attack::Pound, pos + Vec3::Y * 0.5, forward);
    }
}

/// What Mario runs into, in the log (to find invisible walls): when he pushes against something
/// for half a second, when he bonks into a wall, and on F11 (everything around him).
fn wall_report(stick: (f32, f32), state: &sm64::SM64MarioState, pos: Vec3) {
    const BONKS: [u32; 3] = [0x0002_0466, 0x0000_08A7, 0x0102_08B6];
    static STUCK: Mutex<Option<(Instant, Vec3)>> = Mutex::new(None);
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    static COUNT: AtomicU32 = AtomicU32::new(0);
    static F11: AtomicBool = AtomicBool::new(false);
    // SM64 moves Mario along (sin, cos) of his face angle; X is mirrored in the world
    let facing = Vec3::new(-state.face_angle.sin(), 0.0, state.face_angle.cos());
    let f11 = key_down(0x7A);
    if f11 && !F11.swap(true, Ordering::Relaxed) {
        let dirs: Vec<Vec3> = (0..8).map(|k| glam::Quat::from_rotation_y(k as f32 * std::f32::consts::FRAC_PI_4) * facing).collect();
        report_surroundings("F11 pressed", pos, &dirs, 1.5, true);
        crate::hud::toast("F11: collision around Mario logged");
        return;
    }
    if !f11 {
        F11.store(false, Ordering::Relaxed);
    }
    let bonk = BONKS.contains(&state.action) && state_action() != Some(state.action);
    let pushing = (stick.0 * stick.0 + stick.1 * stick.1).sqrt() > 0.6 && state.action & ACT_FLAG_AIR == 0;
    let mut stuck = lock(&STUCK);
    let stuck_long = if pushing {
        let (since, at) = *stuck.get_or_insert((Instant::now(), pos));
        if pos.distance(at) > 0.15 {
            *stuck = Some((Instant::now(), pos));
            false
        } else {
            since.elapsed().as_secs_f32() >= 0.5
        }
    } else {
        *stuck = None;
        false
    };
    drop(stuck);
    if !(bonk || stuck_long) || lock(&LAST).is_some_and(|t| t.elapsed().as_secs_f32() < 3.0) || COUNT.load(Ordering::Relaxed) >= 60 {
        return;
    }
    *lock(&LAST) = Some(Instant::now());
    COUNT.fetch_add(1, Ordering::Relaxed);
    let why = if bonk { format!("Mario bonked into a wall (action {:#x})", state.action) } else { format!("Mario is stuck pushing (action {:#x})", state.action) };
    // (a bonk knocks him back: look a little further)
    report_surroundings(&why, pos, &[facing], if bonk { 1.5 } else { 0.8 }, false);
}

/// Logs the collision around `pos` along `dirs` (at three heights) and the floor under it.
fn report_surroundings(why: &str, pos: Vec3, dirs: &[Vec3], reach: f32, ceiling: bool) {
    let Some(tris) = lock(&LOADED).as_ref().map(|(t, _, _)| t.clone()) else { return };
    crate::log(format!("wall: {why} at {pos:.2?}"));
    for dir in dirs {
        let mut any = false;
        for h in [0.3f32, 0.8, 1.3] {
            let from = pos + Vec3::Y * h;
            if let Some((hit, tri)) = crate::havok::raycast_tri(&tris, from, from + *dir * reach) {
                any = true;
                let n = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
                crate::log(format!(
                    "wall:   towards {dir:.2?}, {h:.1} m up, {:.2} m ahead: triangle {tri:.2?} (normal {n:.2}): {}",
                    hit.distance(from),
                    crate::havok::describe(&tri)
                ));
            }
        }
        if !any {
            crate::log(format!("wall:   towards {dir:.2?}: nothing within {reach:.1} m in the loaded collision"));
        }
    }
    if let Some((hit, tri)) = crate::havok::raycast_tri(&tris, pos + Vec3::Y * 0.3, pos - Vec3::Y * 0.5) {
        crate::log(format!("wall:   floor {:.2} m below: {}", (pos.y - hit.y).max(0.0), crate::havok::describe(&tri)));
    }
    if ceiling {
        if let Some((hit, tri)) = crate::havok::raycast_tri(&tris, pos + Vec3::Y * 0.3, pos + Vec3::Y * 4.0) {
            crate::log(format!("wall:   ceiling {:.2} m above: {}", hit.y - pos.y, crate::havok::describe(&tri)));
        }
    }
}

fn on_safety_floor(p: Vec3) -> bool {
    lock(&SAFETY_Y).is_some_and(|y| (p.y - y).abs() < 0.6)
}

// ---- Sekiro's character ----------------------------------------------------------------------------

/// F8: Mario mode (Sekiro hidden and pinned to Mario) or playing Sekiro (Mario hidden, frozen).
fn mode_toggle() {
    static WAS: AtomicBool = AtomicBool::new(false);
    let f8 = key_down(0x77);
    let back = crate::pad::take_back_press();
    let edge = f8 && !WAS.swap(true, Ordering::Relaxed);
    if !f8 {
        WAS.store(false, Ordering::Relaxed);
    }
    if !edge && !back {
        return;
    }
    set_mode(!in_mario_mode());
}

/// Mario mode on (Mario comes to where Sekiro is) or off (Sekiro back).
fn set_mode(on: bool) {
    MARIO_MODE.store(on, Ordering::Relaxed);
    *lock(&GAME_DRIVES) = None;
    if on {
        crate::log("mode: Mario (Sekiro hidden, Mario comes to where Sekiro is)");
        crate::hud::toast("F8: Mario");
        if let Some(p) = crate::player::position().map(Vec3::from) {
            teleport_mario(p);
            *lock(&PINNED) = Some(p);
        }
        // Mario's health from Sekiro's
        if let (Some((hp, max)), id) = (crate::player::hp(), MARIO_ID.load(Ordering::Relaxed)) {
            let health = (((hp as f32 / max as f32 * 8.0).ceil() as u16).clamp(1, 8) << 8) | 0x80;
            worker::call("mode health", move |_| unsafe { sm64::sm64_set_mario_health(id, health) });
        }
        *lock(&HP_PREV) = None;
    } else {
        crate::log("mode: playing Sekiro (Mario hidden)");
        crate::hud::toast("F8: Sekiro");
        crate::grab::reset();
        crate::player::set_no_gravity(false);
        crate::player::set_hidden(false);
        crate::lakitu::reset();
    }
}

/// Pins Sekiro's character to Mario: weightless, hidden, at Mario's feet. Run at several task
/// groups each frame (the game moves and shows the character in between). If the game moved it
/// far on its own (respawn, warp, load), Mario is taken there instead; if it must move it itself
/// (grappling hook, a long fall), it gets it for a while and Mario rides along.
pub fn pin() {
    if !in_mario_mode() || !VISIBLE.load(Ordering::Relaxed) || MARIO_ID.load(Ordering::Relaxed) < 0 || worker::hung() {
        return;
    }
    let Some(now) = crate::player::position().map(Vec3::from) else { return };
    crate::player::set_hidden(true);
    // the game moving the character itself: hands off until it stops
    {
        let mut drive = lock(&GAME_DRIVES);
        if let Some(d) = drive.as_mut() {
            let step = now.distance(d.last);
            if step > 0.02 {
                d.last_moved = Instant::now();
            }
            d.last = now;
            // the game dropping its character forever (it fell through the map, no kill plane):
            // back to where Mario last stood
            let ground = lock(&LAST_GROUND).unwrap_or(now.y);
            let alive = crate::player::hp().is_some_and(|(hp, _)| hp > 0);
            if d.why == "fall" && alive && now.y < ground - 150.0 {
                let back = lock(&HISTORY).back().map(|(_, p)| *p);
                if let Some(p) = back {
                    crate::log(format!("fall: Sekiro fell {:.0} m without dying (through the map?): both back to {p:.2?}", ground - now.y));
                    *drive = None;
                    drop(drive);
                    crate::player::set_no_gravity(false);
                    crate::player::set_position(p.into());
                    *lock(&PINNED) = Some(p);
                    teleport_mario(p);
                    return;
                }
            }
            // the grappling hook: Mario has control back as soon as he's on the ground (the game
            // still plays Sekiro's landing for a moment), not while the game still pulls (touching
            // the ground on the way)
            let landed = d.why == "grapple"
                && d.since.elapsed().as_secs_f32() > 0.5
                && step < GRAPPLE_LANDED_STEP
                && state_action().is_some_and(|a| a & ACT_FLAG_AIR == 0);
            if landed || d.last_moved.elapsed().as_secs_f32() > 0.3 || d.since.elapsed().as_secs_f32() > d.max_secs {
                let secs = d.since.elapsed().as_secs_f32();
                if d.why == "deathblow" && secs < DEATHBLOW_MIN {
                    crate::log(format!("deathblow: over after {secs:.1} s: not a deathblow; none for 3 s"));
                    *lock(&DEATHBLOW_BLOCKED) = Some(Instant::now() + Duration::from_secs(3));
                }
                if landed {
                    crate::log(format!("grapple: Mario landed after {:.1} s; Mario takes over", d.since.elapsed().as_secs_f32()));
                } else {
                    crate::log(format!("{}: the game stopped moving Sekiro after {:.1} s; Mario takes over", d.why, d.since.elapsed().as_secs_f32()));
                }
                *drive = None;
                drop(drive);
                *lock(&PINNED) = Some(now);
                *lock(&LAST_GROUND) = Some(now.y);
                let id = MARIO_ID.load(Ordering::Relaxed);
                let at = to_sm64(now.into());
                worker::call("handback", move |_| unsafe {
                    if !landed {
                        sm64::sm64_set_mario_position(id, at[0], at[1], at[2]);
                        sm64::sm64_set_mario_action(id, ACT_FREEFALL);
                    }
                    sm64::sm64_reset_mario_peak(id);
                });
            }
            return;
        }
    }
    let mut pinned = lock(&PINNED);
    if let Some(prev) = *pinned {
        let d = now - prev;
        // far: a respawn or warp
        if d.length() > TELEPORT {
            crate::log(format!("follow: the game moved Sekiro {:.1} m (respawn/warp): Mario goes there", d.length()));
            lock(&HISTORY).clear();
            *pinned = Some(now);
            drop(pinned);
            teleport_mario(now);
            return;
        }
        // just after the grappling hook was pressed, the game pulling the character away (not just
        // dropping it) means it's grappling
        // (not a knockback: an enemy's hit moves the character too)
        let grappling = lock(&GRAPPLE_PRESSED).is_some_and(|t| t.elapsed().as_secs_f32() < GRAPPLE_WINDOW)
            && lock(&HURT_AT).is_none_or(|t| t.elapsed().as_secs_f32() > 0.5);
        if grappling && d.length() > 0.25 && d.y > -0.3 {
            crate::log(format!("grapple: the game is pulling Sekiro ({:.2} m in a step); Mario rides along", d.length()));
            *pinned = None;
            drop(pinned);
            hand_to_game("grapple", now, 6.0);
            return;
        }
    }
    // a long fall, or out of the map onto the safety floor: the game drops its character itself,
    // so its fall deaths and kill triggers happen
    let mario = Vec3::from(world_pos());
    if let Some(s) = state() {
        let airborne = s.action & ACT_FLAG_AIR != 0;
        let fell = lock(&LAST_GROUND).map(|g| g - mario.y).unwrap_or(0.0);
        // (not while Sekiro is dead: the game moves its body about, then respawns it)
        let alive = crate::player::hp().is_some_and(|(hp, _)| hp > 0);
        let why = if !alive {
            None
        } else if !airborne && on_safety_floor(mario) {
            Some("out of the map")
        } else if airborne && fell > FALL_HANDOFF && crate::havok::water_level_at(mario.x, mario.z).is_none_or(|w| w > mario.y + 1.0) {
            Some("long fall")
        } else {
            None
        };
        if let Some(why) = why {
            crate::log(format!("fall: {why} ({fell:.1} m below the last ground); the game drops Sekiro, Mario rides along"));
            *pinned = None;
            drop(pinned);
            hand_to_game("fall", mario, 20.0);
            return;
        }
    }
    let st = state();
    let airborne = st.is_some_and(|s| s.action & ACT_FLAG_AIR != 0 || s.action & ACT_GROUP_MASK == ACT_GROUP_SUBMERGED);
    // weightless only in the air (or swimming): standing, the game must see its character on the
    // ground (doors, items and talking need that)
    crate::player::set_no_gravity(airborne);
    // facing where Mario faces (so Sekiro's attack and interactions go his way), also while he
    // stands still; not while Mario swings a boss (Sekiro's camera would spin along). An enemy
    // within a deathblow's reach comes first (the game offers the deathblow only to a Sekiro facing
    // it), then the locked-on one.
    if let Some(s) = st.filter(|_| !holding_boss()) {
        let face = crate::aim::deathblow_candidate(mario)
            .or_else(crate::aim::locked)
            .and_then(|t| crate::aim::towards(mario, t))
            .map(crate::aim::face_angle)
            .unwrap_or(s.face_angle);
        crate::player::set_facing(glam::Quat::from_rotation_y(std::f32::consts::PI - face));
    }
    // Mario standing still: the game keeps its character where it stands (not moved every frame),
    // so it settles on the ground and its action button (doors, items, talking) can come up
    let still = st.is_some_and(|s| s.action & ACT_FLAG_AIR == 0 && s.action & ACT_GROUP_MASK == 0);
    let flat = Vec3::new(now.x - mario.x, 0.0, now.z - mario.z).length();
    if still && flat < 0.35 && (now.y - mario.y).abs() < 0.5 {
        *pinned = Some(now);
        return;
    }
    if crate::player::set_position(mario.into()) {
        *pinned = Some(mario);
    }
}

/// The game moves its character itself (gravity on) for up to `max_secs`; Mario rides along.
fn hand_to_game(why: &'static str, at: Vec3, max_secs: f32) {
    crate::player::set_no_gravity(false);
    crate::player::set_position(at.into());
    *lock(&GAME_DRIVES) = Some(GameDrive { why, since: Instant::now(), last: at, last_moved: Instant::now(), max_secs });
}

/// Puts Mario at `pos` (world), standing, with the collision there.
fn teleport_mario(pos: Vec3) {
    let id = MARIO_ID.load(Ordering::Relaxed);
    if id < 0 {
        return;
    }
    *lock(&ORIGIN) = pos.into();
    *lock(&LOADED) = None;
    *lock(&SAFETY_Y) = None;
    *lock(&LAST_GROUND) = Some(pos.y);
    if !load_collision(pos) {
        load_flat_floor(pos);
    }
    worker::call("teleport", move |_| unsafe {
        sm64::sm64_set_mario_position(id, 0.0, 30.0, 0.0);
        sm64::sm64_set_mario_action(id, ACT_IDLE);
    });
    // no sliding across the map
    if let Some(t) = lock(&TICKS).as_mut() {
        t.prev_mesh.clear();
        t.prev_pos = pos;
        t.pos = pos;
    }
}

static HP_PREV: Mutex<Option<i32>> = Mutex::new(None);

/// Sekiro's HP is the truth: its losses hurt Mario (in SM64 wedges, with SM64's knockback), its
/// gains heal him, its death kills him, and coming back (respawn, resurrection) revives him.
fn sync_health() {
    let id = MARIO_ID.load(Ordering::Relaxed);
    let (Some((hp, max)), Some(state)) = (crate::player::hp(), state()) else { return };
    let mut prev = lock(&HP_PREV);
    let Some(before) = prev.replace(hp) else {
        crate::log(format!("health: Sekiro has {hp} / {max} HP"));
        return;
    };
    let wedges = |d: i32| ((d as f32 / max as f32 * 8.0).round() as u32).max(1);
    if hp < before {
        *lock(&HURT_AT) = Some(Instant::now());
        if hp == 0 {
            crate::log("health: Sekiro died; so does Mario");
            crate::grab::reset();
            worker::call("kill", move |_| unsafe { sm64::sm64_mario_kill(id) });
        } else {
            let w = wedges(before - hp);
            // knocked back from in front of him
            let (s, c) = state.face_angle.sin_cos();
            let src = [state.position[0] + s * 100.0, state.position[1], state.position[2] + c * 100.0];
            worker::call("damage", move |_| unsafe { sm64::sm64_mario_take_damage(id, w, 0, src[0], src[1], src[2]) });
        }
    } else if hp > before {
        if before == 0 {
            crate::log("health: Sekiro is back; so is Mario (where Sekiro is)");
            drop(prev);
            *lock(&GAME_DRIVES) = None;
            if let Some(p) = crate::player::position().map(Vec3::from) {
                teleport_mario(p);
                *lock(&PINNED) = Some(p);
            }
            worker::call("revive", move |_| unsafe {
                sm64::sm64_set_mario_health(id, 0x880);
                sm64::sm64_set_mario_action(id, ACT_IDLE);
            });
        } else {
            let w = wedges(hp - before);
            worker::call("heal", move |_| unsafe { sm64::sm64_mario_heal(id, (w * 4).min(255) as u8) });
        }
    } else if hp > 0 && (state.health as u16) < 0x100 {
        // SM64 killed him on its own (a long fall): Sekiro is alive, so is Mario
        let health = (((hp as f32 / max as f32 * 8.0).ceil() as u16).clamp(1, 8) << 8) | 0x80;
        worker::call("unkill", move |_| unsafe {
            sm64::sm64_set_mario_health(id, health);
            sm64::sm64_set_mario_action(id, ACT_IDLE);
        });
    }
}

/// F10: back to where Mario stood 10 s ago (again: 10 s before that, up to 5 times in a row).
fn rewind() {
    static WAS: AtomicBool = AtomicBool::new(false);
    let f10 = key_down(0x79);
    if !(f10 && !WAS.swap(true, Ordering::Relaxed)) {
        if !f10 {
            WAS.store(false, Ordering::Relaxed);
        }
        return;
    }
    let now = Instant::now();
    let mut chain = lock(&REWIND);
    // (a quick second press is a bounce, not another 10 s)
    if chain.is_some_and(|(_, at, _)| at.elapsed().as_secs_f32() < 0.5) {
        return;
    }
    // a chain continues while F10 is pressed again within 15 s
    let (count, reference) = match *chain {
        Some((n, at, back_to)) if at.elapsed().as_secs_f32() < 15.0 => (n, back_to),
        _ => (0, now),
    };
    if count >= REWINDS {
        crate::log(format!("F10 pressed: rewound {REWINDS} times already; the buffer is used up"));
        crate::hud::toast(format!("F10: rewound {REWINDS} times already"));
        worker::call("buzz", |_| unsafe { sm64::sm64_play_sound_global(0x700E_0081) });
        return;
    }
    let want = reference.checked_sub(Duration::from_secs_f32(REWIND_STEP)).unwrap_or(reference);
    let target = {
        let mut h = lock(&HISTORY);
        // (or as far back as there is: right after loading or a fall there's little)
        let found = h.iter().rev().find(|(t, _)| *t <= want).or_else(|| h.front().filter(|(t, _)| *t < reference)).copied();
        if let Some((t, _)) = found {
            // what comes after it is gone (rewinding again goes further back)
            while h.back().is_some_and(|(bt, _)| *bt > t) {
                h.pop_back();
            }
        }
        found
    };
    let Some((t, pos)) = target else {
        crate::log("F10 pressed: nothing to rewind to yet (Mario must have stood somewhere 10 s ago)");
        crate::hud::toast("F10: nothing to rewind to yet");
        worker::call("buzz", |_| unsafe { sm64::sm64_play_sound_global(0x700E_0081) });
        return;
    };
    *chain = Some((count + 1, now, t));
    drop(chain);
    crate::log(format!("F10 pressed: rewind {} of {REWINDS}, back {:.0} s to {pos:.2?}", count + 1, t.elapsed().as_secs_f32()));
    crate::hud::toast(format!("F10: back {:.0} s ({} of {REWINDS})", t.elapsed().as_secs_f32(), count + 1));
    *lock(&GAME_DRIVES) = None;
    crate::player::set_position(pos.into());
    *lock(&PINNED) = Some(pos);
    if in_mario_mode() {
        teleport_mario(pos);
        crate::lakitu::reset();
    }
}

/// F1: the key list; F6: boss grabs on / off.
fn f_keys() {
    static WAS: AtomicU8 = AtomicU8::new(0);
    let (f1, f6) = (key_down(0x70), key_down(0x75));
    let was = WAS.swap(f1 as u8 | (f6 as u8) << 1, Ordering::Relaxed);
    if f1 && was & 1 == 0 {
        let on = crate::hud::toggle_help();
        crate::log(format!("F1 pressed: key list {}", if on { "shown" } else { "hidden" }));
    }
    if f6 && was & 2 == 0 {
        let on = !crate::grab::ENABLED.fetch_xor(true, Ordering::Relaxed);
        if !on {
            crate::grab::reset();
        }
        crate::log(format!("F6 pressed: boss grabs {}", if on { "on" } else { "off" }));
        crate::hud::toast(if on { "F6: boss grabs on" } else { "F6: boss grabs off" });
    }
}

/// F7: a random SM64 song instead of Sekiro's music; again: Sekiro's music back.
fn music() {
    static WAS: AtomicBool = AtomicBool::new(false);
    let f7 = key_down(0x76);
    if !(f7 && !WAS.swap(true, Ordering::Relaxed)) {
        if !f7 {
            WAS.store(false, Ordering::Relaxed);
        }
        return;
    }
    let mut music = lock(&MUSIC);
    match music.take() {
        None => {
            let pick = SONGS[nanos_seed() % SONGS.len()];
            let args = (4u16 << 8) | pick as u16;
            worker::call("music", move |_| unsafe { sm64::sm64_play_music(0, args, 0) });
            let saved = set_game_music_volume(Some(0));
            crate::log(format!("F7 pressed: SM64 song {pick:#04x} (Sekiro's music volume {saved:?} -> 0)"));
            crate::hud::toast("F7: SM64 music");
            *music = Some((args, saved));
        }
        Some((args, saved)) => {
            worker::call("music off", move |_| unsafe { sm64::sm64_stop_background_music(args) });
            if let Some(v) = saved {
                set_game_music_volume(Some(v));
            }
            crate::log(format!("F7 pressed: SM64 music off (Sekiro's music volume back to {saved:?})"));
            crate::hud::toast("F7: Sekiro's music");
        }
    }
}

/// SM64's volumes follow Sekiro's sound options: its sound effects (Mario's voice, steps, hits)
/// at Sekiro's sound effects volume, its music 3 steps (of 10) below that.
fn sync_volume() {
    static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0xFF);
    let Some(fx) = game_option(5) else { return };
    let fx = fx.min(10);
    if LAST.swap(fx, Ordering::Relaxed) == fx {
        return;
    }
    let (sfx, music) = (fx as f32 / 10.0, fx.saturating_sub(3) as f32 / 10.0);
    crate::log(format!("volume: Sekiro's sound effects at {fx}: SM64 sound {sfx:.1}, music {music:.1}"));
    worker::call("volume", move |_| unsafe {
        sm64::sm64_set_sound_volume(sfx);
        sm64::sm64_set_music_volume(music);
    });
}

/// A byte of Sekiro's options (`OptionsData`: +4 music, +5 sound effects, +6 voice volume).
fn game_option(offset: usize) -> Option<u8> {
    use fromsoftware_shared::FromStatic;
    let gdm = sekiro::sprj::GameDataMan::instance_ptr().ok().filter(|p| !p.is_null())? as usize;
    let options = crate::mem::read_u64(gdm + 0x50)? as usize;
    (options > 0x10000 && crate::mem::readable(options + offset, 1)).then(|| unsafe { std::ptr::read_volatile((options + offset) as *const u8) })
}

fn nanos_seed() -> usize {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0)
}

/// Sets Sekiro's music volume option (0..10); returns what it was.
fn set_game_music_volume(v: Option<u8>) -> Option<u8> {
    use fromsoftware_shared::FromStatic;
    let gdm = sekiro::sprj::GameDataMan::instance_ptr().ok().filter(|p| !p.is_null())? as usize;
    // GameDataMan+0x50 -> OptionsData, +0x4 volume_music
    let options = crate::mem::read_u64(gdm + 0x50)? as usize;
    let at = options + 4;
    if !crate::mem::readable(at, 1) {
        return None;
    }
    let old = unsafe { std::ptr::read_volatile(at as *const u8) };
    if let Some(v) = v {
        unsafe { std::ptr::write_volatile(at as *mut u8, v) };
    }
    Some(old)
}

/// SM64's sound (music) keeps playing while Mario doesn't tick (playing Sekiro).
fn audio_only_tick() {
    if lock(&MUSIC).is_none() {
        return;
    }
    worker::call("audio", |_| {
        let mut buf = [0i16; 544 * 2 * 2];
        let frames = unsafe { sm64::sm64_audio_tick(crate::audio::queued(), 1100, buf.as_mut_ptr()) } as usize;
        crate::audio::push(&buf[..(frames * 2 * 2).min(buf.len())]);
    });
}

/// Lakitu's camera (F9 or L3 switches it with Sekiro's).
fn camera(dt: f32) {
    static WAS: AtomicBool = AtomicBool::new(false);
    // F9, or the left stick's click
    let f9 = key_down(0x78) || crate::pad::read().filter(|_| focused()).is_some_and(|p| p.l3);
    if f9 && !WAS.swap(true, Ordering::Relaxed) {
        let on = !crate::lakitu::ON.fetch_xor(true, Ordering::Relaxed);
        crate::log(format!("F9 / L3 pressed: camera {}", if on { "Lakitu (SM64)" } else { "Sekiro's" }));
        crate::hud::toast(if on { "F9: Lakitu's camera" } else { "F9: Sekiro's camera" });
    } else if !f9 {
        WAS.store(false, Ordering::Relaxed);
    }
    let Some(state) = state() else { return };
    if !crate::lakitu::ON.load(Ordering::Relaxed) {
        crate::lakitu::reset();
        return;
    }
    const T: f32 = 0.6;
    let right = crate::pad::read().filter(|_| focused()).map(|p| p.right).unwrap_or((0.0, 0.0));
    // C-left, C-right, C-up, C-down (right stick or arrow keys)
    let c = [right.0 < -T || key_down(0x25), right.0 > T || key_down(0x27), right.1 > T || key_down(0x26), right.1 < -T || key_down(0x28)];
    let mario = Vec3::from(world_pos());
    let game_cam = crate::camera::active(crate::player::position().map(Vec3::from).unwrap_or(mario)).map(|c| c.pos);
    let airborne = state.action & ACT_FLAG_AIR != 0;
    crate::lakitu::update(dt, mario, airborne, c, game_cam, ray);
}

/// Called every game frame from the task.
pub fn frame() {
    if !ENGINE_READY.load(Ordering::Relaxed) || worker::hung() {
        return;
    }
    if crate::player::update().is_none() {
        if !NO_PLAYER.swap(true, Ordering::Relaxed) {
            crate::log("world: no player character (title screen)");
        }
        return;
    }
    let in_world = update_in_world();
    create_mario();
    if MARIO_ID.load(Ordering::Relaxed) < 0 || !in_world {
        return;
    }
    update_menu();
    f_keys();
    // (popups and conversations are watched every frame: popup.rs)
    crate::popup::open();
    mode_toggle();
    rewind();
    music();
    sync_volume();
    crate::hit::update();
    crate::dmg::update();
    if !in_mario_mode() {
        // SM64's music goes on at 30 Hz on average: time is accumulated (waiting a whole tick
        // after the last one made it every third 60 fps frame now and then, and the music choppy)
        static AUDIO_ACC: Mutex<Option<(Instant, f32)>> = Mutex::new(None);
        let mut c = lock(&AUDIO_ACC);
        let now = Instant::now();
        let (last, acc) = c.get_or_insert((now, TICK));
        *acc += now.duration_since(*last).as_secs_f32().min(0.1);
        *last = now;
        let due = *acc >= TICK;
        if due {
            *acc = (*acc - TICK).min(TICK);
        }
        drop(c);
        if due {
            audio_only_tick();
        }
        return;
    }
    // the grappling hook (B / F): the press is noted, so pin() can tell the game pulling the
    // character
    static GRAPPLE_WAS: AtomicBool = AtomicBool::new(false);
    let grapple = crate::pad::read().filter(|_| focused()).is_some_and(|p| p.grapple) || key_down(0x46);
    if grapple && !GRAPPLE_WAS.swap(true, Ordering::Relaxed) {
        *lock(&GRAPPLE_PRESSED) = Some(Instant::now());
    } else if !grapple {
        GRAPPLE_WAS.store(false, Ordering::Relaxed);
    }
    // the action button (Y / E): the game does its action itself (a door, an item, talking)
    static ACTION_WAS: AtomicBool = AtomicBool::new(false);
    let action = focused() && (crate::pad::read().is_some_and(|p| p.y) || key_down(0x45));
    if action && !ACTION_WAS.swap(true, Ordering::Relaxed) && lock(&GAME_DRIVES).is_none() && !MENU.load(Ordering::Relaxed) && !crate::popup::open() {
        if let Some(at) = crate::player::position().map(Vec3::from) {
            let prompt = crate::prompt::current().map_or("none".to_string(), |id| id.to_string());
            let yaw = crate::player::physics_ptr().and_then(|p| crate::mem::read_f32(p + crate::player::PHYSICS_YAW)).unwrap_or(f32::NAN);
            crate::log(format!("action: Y / E pressed (prompt {prompt}, Sekiro's yaw {yaw:.2}); the game does Sekiro's action (Mario rides along)"));
            *lock(&PINNED) = None;
            hand_to_game("action", at, 10.0);
            // at least a second, even if the character doesn't move
            if let Some(d) = lock(&GAME_DRIVES).as_mut() {
                d.last_moved = Instant::now() + std::time::Duration::from_millis(700);
            }
        }
    }
    if !action {
        ACTION_WAS.store(false, Ordering::Relaxed);
    }
    // riding along while the game moves the character (falling pose). The fall's top stays at
    // his feet: moved up to the character every frame, SM64 would count each step as falling
    // (a landing after the grappling hook hurt him)
    let riding = lock(&GAME_DRIVES).as_ref().map(|d| (d.last, d.why));
    if let Some((at, why)) = riding {
        let id = MARIO_ID.load(Ordering::Relaxed);
        let sm = to_sm64(at.into());
        let first = why != "action" && why != "deathblow" && state().is_some_and(|s| s.action != ACT_FREEFALL && s.health >= 0x100);
        worker::call("ride", move |_| unsafe {
            sm64::sm64_set_mario_position(id, sm[0], sm[1], sm[2]);
            if first {
                sm64::sm64_set_mario_action(id, ACT_FREEFALL);
            }
            sm64::sm64_set_mario_velocity(id, 0.0, 0.0, 0.0);
            sm64::sm64_reset_mario_peak(id);
        });
    }

    // SM64 runs at 30 Hz on real time (at most one tick per game frame); this frame's
    // interpolation factor is how far into the next tick we are
    static CLOCK: Mutex<Option<(Instant, f32)>> = Mutex::new(None);
    let (due, dt, acc) = {
        let mut c = lock(&CLOCK);
        let now = Instant::now();
        let (last, acc) = c.get_or_insert((now, TICK));
        let dt = now.duration_since(*last).as_secs_f32().min(0.1);
        *acc += dt;
        *last = now;
        let due = *acc >= TICK;
        if due {
            *acc = (*acc - TICK).min(TICK);
        }
        (due, dt, *acc)
    };
    if due {
        update_collision(Vec3::from(world_pos()));
        tick();
    }
    ALPHA.store((acc / TICK).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    pin();
    sync_health();
    camera(dt);
}
