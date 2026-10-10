//! Bowser's swing on Sekiro's bosses: Mario's punch from behind on a boss (anyone with the big
//! health bar at the bottom of the screen, mini-bosses too) grabs him like Bowser by the tail.
//! SM64's own grab, swing and throw follow: spin the stick to wind up, B / X to let go. The boss
//! is held at arm's length while Mario swings, then flies off where Mario faces, and where he
//! crashes Mario's throw hits (hit.rs: a bullet around him, so the damage is the game's).
//!
//! Found in Sekiro 1.6.0.0:
//! ```text
//! [0x143D67408] + 0x2E10 + i * 0xA8 (i < 3)   the boss health bars (SetBossGauge, 0x14067C7C0):
//!                                             +0 its name (-1: off), +4 the character's handle
//! 0x14080CFF0(handle) -> ChrIns               the game's lookup by handle
//! ChrIns +0x1FF8 modules: +0x18 data (HP +0x130, flags +0x228), +0x68 physics (position +0x80,
//!                yaw +0x74, NoGravity +0x92A)
//! ```

use std::sync::Mutex;
use std::time::Instant;

use glam::Vec3;

use crate::log;
use crate::mem::{read_f32, read_u32, read_u64, readable};
use crate::player::{NO_DAMAGE, NO_DEAD, PHYSICS_NO_GRAVITY, PHYSICS_POSITION, PHYSICS_YAW, PLAYER_MODULES, set_data_flag};

pub const ACT_PICKING_UP_BOWSER: u32 = 0x390;
pub const ACT_HOLDING_BOWSER: u32 = 0x391;
pub const ACT_RELEASING_BOWSER: u32 = 0x392;
/// SOUND_OBJ_BOWSER_TAIL_PICKUP
pub const SOUND_GRAB: i32 = 0x5005_0081;

const BOSS_BARS: usize = 0x143D67408;
const BOSS_BAR_SLOTS: [usize; 2] = [0x2E10, 0x2C08];
const BOSS_BAR_SIZE: usize = 0xA8;
const BY_HANDLE: usize = 0x14080CFF0;

/// Mario's reach for the grab (m, horizontally from the boss's middle) and how far behind him
/// he must be (the cosine between the boss's facing and the way to Mario)
const GRAB_REACH: f32 = 3.0;
const BEHIND: f32 = -0.2;
const GRAVITY: f32 = 16.0;
/// After the crash: no death for the boss this long (the game's fall death would take him)
const LANDED_FOR: f32 = 1.5;
/// ...and held where he landed this long
const SETTLE_FOR: f32 = 0.3;
/// The flight ends here at the latest (s; m from where he was thrown, horizontally / down):
/// within the collision loaded around Mario, so he can't fly through terrain he doesn't know
const FLIGHT_MAX: f32 = 1.2;
const FLIGHT_REACH: f32 = 8.0;
const FLIGHT_DROP: f32 = 3.0;
/// A boss that flew too far is put back this far from Mario
const BACK_AT: f32 = 2.0;

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    /// Mario has him: (handle, arm's length in m)
    Held { boss: u32, reach: f32 },
    /// `from` where Mario threw him
    Flying { boss: u32, pos: Vec3, vel: Vec3, since: Instant, from: Vec3 },
    /// flew out of the known collision: held out of the way at `at` until `until`, then put back
    /// next to Mario
    Away { boss: u32, until: Instant, at: Vec3, vel: Vec3 },
    /// crashed (at `at`, held there a moment while the game takes him back): protected a
    /// moment, then back to the fight
    Landed { boss: u32, until: Instant, at: Vec3, since: Instant },
}

struct State {
    phase: Phase,
    /// where the boss is put this frame (applied at every task point)
    target: Option<(Vec3, f32)>,
    last_face: Option<f32>,
    spin: f32,
    /// the grab to start on the next SM64 tick
    start: bool,
}

static STATE: Mutex<State> = Mutex::new(State { phase: Phase::Idle, target: None, last_face: None, spin: 0.0, start: false });

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The handles of the characters with a boss health bar on screen.
pub fn bosses() -> Vec<u32> {
    let Some(base) = read_u64(BOSS_BARS).map(|b| b as usize).filter(|&b| b >= 0x10000) else { return Vec::new() };
    let mut out = Vec::new();
    for slots in BOSS_BAR_SLOTS {
        for i in 0..3 {
            let bar = base + slots + i * BOSS_BAR_SIZE;
            let (Some(name), Some(handle)) = (read_u32(bar), read_u32(bar + 4)) else { continue };
            if name as i32 != -1 && handle != u32::MAX && handle >> 28 == 1 && !out.contains(&handle) {
                out.push(handle);
            }
        }
    }
    static LAST: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last != out {
        log(format!("grab: boss bars on screen for {:x?}", out));
        *last = out.clone();
    }
    out
}

/// The ChrIns of a handle (the game's own lookup).
pub(crate) fn chr(handle: u32) -> Option<usize> {
    if !readable(BY_HANDLE, 16) {
        return None;
    }
    let f: unsafe extern "system" fn(u32) -> usize = unsafe { std::mem::transmute(BY_HANDLE) };
    let c = unsafe { f(handle) };
    (c >= 0x10000 && read_u32(c + 8) == Some(handle)).then_some(c)
}

pub(crate) fn module(chr: usize, off: usize) -> Option<usize> {
    let container = read_u64(chr + PLAYER_MODULES).map(|c| c as usize).filter(|&c| c >= 0x10000)?;
    read_u64(container + off).map(|m| m as usize).filter(|&m| m >= 0x10000)
}

pub(crate) fn physics(chr: usize) -> Option<usize> {
    module(chr, 0x68)
}

pub(crate) fn hp(chr: usize) -> Option<i32> {
    module(chr, 0x18).and_then(|d| read_u32(d + 0x130)).map(|v| v as i32)
}

pub(crate) fn position(physics: usize) -> Option<Vec3> {
    Some(Vec3::new(read_f32(physics + PHYSICS_POSITION)?, read_f32(physics + PHYSICS_POSITION + 4)?, read_f32(physics + PHYSICS_POSITION + 8)?))
}

/// Puts the character at `p`, facing `forward`, weightless (or not).
fn place(physics: usize, p: Vec3, forward: Option<Vec3>, weightless: bool) {
    if !p.is_finite() || !readable(physics + PHYSICS_POSITION, 12) {
        return;
    }
    unsafe {
        let at = (physics + PHYSICS_POSITION) as *mut f32;
        std::ptr::write_volatile(at, p.x);
        std::ptr::write_volatile(at.add(1), p.y);
        std::ptr::write_volatile(at.add(2), p.z);
        if let Some(f) = forward.filter(|f| f.length_squared() > 1e-4) {
            // faces -(sin yaw, cos yaw)
            std::ptr::write_volatile((physics + PHYSICS_YAW) as *mut f32, (-f.x).atan2(-f.z));
        }
        std::ptr::write_volatile((physics + PHYSICS_NO_GRAVITY) as *mut u8, weightless as u8);
    }
}

/// Mario punched at `mario` (feet) facing `forward`: a boss within reach with Mario behind him
/// is grabbed (true: no punch).
/// Boss grabs on (F6 switches).
pub static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn try_grab(mario: Vec3, forward: Vec3) -> bool {
    if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let mut st = lock();
    if !matches!(st.phase, Phase::Idle) {
        return false;
    }
    let any_side = crate::paths::config("grab_from_any_side").is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    for boss in bosses() {
        let Some(c) = chr(boss) else { continue };
        let Some(ph) = physics(c) else { continue };
        let (Some(p), Some(hp)) = (position(ph), hp(c)) else { continue };
        if hp <= 0 {
            continue;
        }
        let d = Vec3::new(mario.x - p.x, 0.0, mario.z - p.z);
        let dist = d.length();
        if dist > GRAB_REACH || (mario.y - p.y).abs() > 2.0 {
            continue;
        }
        let facing = crate::player::facing_of(ph).unwrap_or(Vec3::Z);
        let behind = facing.dot(d.normalize_or_zero());
        log(format!(
            "grab: boss {boss:x} at {p:.2}, facing {facing:.2}; Mario at {mario:.2} facing {forward:.2}: {dist:.1} m, behind him {behind:.2} (needs < {BEHIND})"
        ));
        if behind > BEHIND && !any_side {
            continue;
        }
        let chance = crate::dmg::setting("grab_chance", 0.2).clamp(0.0, 1.0);
        let roll = crate::dmg::rand();
        if roll >= chance {
            log(format!("grab: the boss slipped out of Mario's hands ({roll:.2}, needs < {chance:.2}): a punch instead"));
            return false;
        }
        st.phase = Phase::Held { boss, reach: dist.clamp(1.2, 2.5) };
        st.last_face = None;
        st.spin = 0.0;
        st.start = true;
        set_data_flag(c, NO_DEAD, true);
        let me = crate::player::ptr();
        if me != 0 {
            set_data_flag(me, NO_DAMAGE, true);
        }
        log("grab: Mario has the boss by the tail");
        return true;
    }
    false
}

/// SM64 tick: whether to start the grab now (Mario goes into SM64's Bowser pickup).
pub fn take_start() -> bool {
    std::mem::take(&mut lock().start)
}

/// Every SM64 tick (30 Hz): hold, throw and fly the boss. `mario` feet (world), `face` SM64 face
/// angle, `action` Mario's SM64 action.
pub fn tick(dt: f32, mario: Vec3, face: f32, action: u32) {
    let mut st = lock();
    let fwd = Vec3::new(-face.sin(), 0.0, face.cos());
    if let Some(l) = st.last_face {
        let d = (face - l + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        st.spin = st.spin * 0.7 + (d / dt.max(1e-3)) * 0.3;
    }
    st.last_face = Some(face);
    match st.phase {
        Phase::Idle => st.target = None,
        Phase::Held { boss, reach } => {
            let Some(c) = chr(boss).filter(|&c| hp(c).is_some_and(|h| h > 0)) else {
                log("grab: the boss is gone; Mario lets go");
                drop(st);
                reset();
                return;
            };
            let holding = matches!(action, ACT_PICKING_UP_BOWSER | ACT_HOLDING_BOWSER) || st.start;
            if holding {
                // at arm's length where Mario faces, a little off the ground, facing away
                let start = mario + Vec3::Y * 0.3;
                let want = start + fwd * reach;
                let p = crate::mario::ray(start, want + fwd * 0.5).map(|h| h - fwd * 0.6).unwrap_or(want);
                st.target = Some((p, (-fwd.x).atan2(-fwd.z)));
                let _ = c;
            } else if action == ACT_RELEASING_BOWSER {
                let speed = (st.spin.abs() * reach * 1.5).clamp(8.0, 30.0);
                let pos = st.target.map(|t| t.0).unwrap_or(mario + fwd * reach);
                log(format!("grab: thrown at {speed:.1} m/s (spin {:.1} rad/s)", st.spin));
                st.phase = Phase::Flying { boss, pos, vel: fwd * speed + Vec3::Y * 7.0, since: Instant::now(), from: mario };
            } else {
                log(format!("grab: let go without a throw (action {action:#x})"));
                drop(st);
                reset();
            }
        }
        Phase::Flying { boss, pos, vel, since, from } => {
            let mut vel = vel;
            vel.y -= GRAVITY * dt;
            let next = pos + vel * dt;
            let lead = (next - pos).normalize_or_zero() * 0.8;
            let mid = Vec3::Y * 0.8;
            let impact = crate::mario::ray(pos + mid, next + lead + mid).or_else(|| crate::mario::ray(pos + mid, next - Vec3::Y * 0.1));
            let away = Vec3::new(next.x - from.x, 0.0, next.z - from.z).length();
            let gone = since.elapsed().as_secs_f32() > FLIGHT_MAX || away > FLIGHT_REACH || next.y < from.y - FLIGHT_DROP;
            if impact.is_some() || gone {
                let speed = vel.length();
                // where he crashed, if there's ground under it; else (and after a flight out of
                // the known collision) back on solid ground next to Mario
                let crash = impact.map(|h| h - lead - mid).and_then(|at| ground_at(at, 1.5));
                let me = crate::player::ptr();
                if me != 0 {
                    set_data_flag(me, NO_DAMAGE, false);
                }
                let Some(rest) = crash else {
                    // out of the known collision: kept up there out of the way a while, then
                    // back next to Mario
                    let delay = crate::dmg::setting("throw_return_delay", 3.0).clamp(0.0, 30.0);
                    log(format!("grab: the boss flew too far at {speed:.1} m/s, {away:.1} m out: back next to Mario in {delay:.1} s"));
                    st.target = Some((next, f32::NAN));
                    st.phase = Phase::Away { boss, until: Instant::now() + std::time::Duration::from_secs_f32(delay), at: next, vel };
                    return;
                };
                log(format!("grab: the boss crashed at {speed:.1} m/s, {away:.1} m out; now at {rest:.2}"));
                drop(st);
                land(boss, rest, vel);
                return;
            }
            st.target = Some((next, (-vel.x).atan2(-vel.z)));
            st.phase = Phase::Flying { boss, pos: next, vel, since, from };
        }
        Phase::Away { boss, until, at, vel } => {
            st.target = Some((at, f32::NAN));
            if Instant::now() >= until {
                let rest = safe_spot(mario, fwd);
                log(format!("grab: the boss is back next to Mario at {rest:.2}"));
                drop(st);
                land(boss, rest, vel);
            }
        }
        Phase::Landed { boss, until, at, since } => {
            st.target = (since.elapsed().as_secs_f32() < SETTLE_FOR).then_some((at, f32::NAN));
            if Instant::now() >= until {
                if let Some(c) = chr(boss) {
                    set_data_flag(c, NO_DEAD, false);
                }
                log("grab: the boss is back in the fight");
                st.phase = Phase::Idle;
            }
        }
    }
}

/// The thrown boss comes down at `rest`: held there a moment, then the throw's hit (a bullet
/// around him) and its posture damage.
fn land(boss: u32, rest: Vec3, vel: Vec3) {
    if let Some(ph) = chr(boss).and_then(physics) {
        place(ph, rest, None, false);
    }
    {
        let mut st = lock();
        st.target = None;
        let now = Instant::now();
        st.phase = Phase::Landed { boss, until: now + std::time::Duration::from_secs_f32(LANDED_FOR), at: rest, since: now };
    }
    crate::hit::fire(crate::hit::Attack::Throw, rest + Vec3::Y * 0.8, vel);
    let share = crate::dmg::setting("throw_posture", 20.0).clamp(0.0, 100.0) / 100.0;
    crate::dmg::add_posture(boss, share);
}

/// At the game's task points: the held / flying boss where he belongs (and a landed one held
/// on the ground a moment).
pub fn apply() {
    let st = lock();
    let (boss, (p, yaw), weightless) = match (st.phase, st.target) {
        (Phase::Held { boss, .. } | Phase::Flying { boss, .. } | Phase::Away { boss, .. }, Some(t)) => (boss, t, true),
        (Phase::Landed { boss, .. }, Some(t)) => (boss, t, false),
        _ => return,
    };
    drop(st);
    if let Some(ph) = chr(boss).and_then(physics) {
        let facing = yaw.is_finite().then(|| Vec3::new(-yaw.sin(), 0.0, -yaw.cos()));
        place(ph, p, facing, weightless);
    }
}

/// The ground under `p` (within `depth` below, from a little above), if there is any.
fn ground_at(p: Vec3, depth: f32) -> Option<Vec3> {
    crate::mario::ray(p + Vec3::Y * 1.0, p - Vec3::Y * depth).map(|g| g + Vec3::Y * 0.05)
}

/// Solid ground BACK_AT from Mario with nothing between them: ahead of him first, then around
/// him; Mario's own spot if there's none.
fn safe_spot(mario: Vec3, fwd: Vec3) -> Vec3 {
    let chest = Vec3::Y * 0.8;
    (0..8)
        .map(|k| glam::Quat::from_rotation_y(k as f32 * std::f32::consts::FRAC_PI_4) * fwd)
        .filter_map(|d| {
            let p = mario + d * BACK_AT;
            if crate::mario::ray(mario + chest, p + chest).is_some() {
                return None;
            }
            crate::mario::ray(p + Vec3::Y * 2.0, p - Vec3::Y * 3.0).map(|g| g + Vec3::Y * 0.05)
        })
        .next()
        .unwrap_or(mario + Vec3::Y * 0.05)
}

/// Mario died or left Mario mode: let go of the boss (his gravity and death back).
pub fn reset() {
    let mut st = lock();
    let boss = match st.phase {
        Phase::Held { boss, .. } | Phase::Flying { boss, .. } | Phase::Away { boss, .. } | Phase::Landed { boss, .. } => Some(boss),
        Phase::Idle => None,
    };
    if let Some(c) = boss.and_then(chr) {
        set_data_flag(c, NO_DEAD, false);
        if let Some(ph) = physics(c) {
            unsafe { std::ptr::write_volatile((ph + PHYSICS_NO_GRAVITY) as *mut u8, 0) };
        }
    }
    if boss.is_some() {
        let me = crate::player::ptr();
        if me != 0 {
            set_data_flag(me, NO_DAMAGE, false);
        }
    }
    st.phase = Phase::Idle;
    st.target = None;
    st.start = false;
}
