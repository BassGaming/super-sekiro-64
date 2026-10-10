//! What Mario's hits do to HP, on top of the game's own damage (hit.rs's bullets):
//!   * the damage multiplier (Shift+F2 lower, Shift+F3 higher, 0.1 steps; ini `damage`): each
//!     enemy's HP loss from Mario's hit is scaled by it (posture stays the game's);
//!   * bosses guard: the game turns Mario's hits on a guarding boss into posture damage only.
//!     Like Sekiro's sword, some of those hits get through anyway: a blocked hit lands with
//!     `boss_hit_chance` (ini, default 0.4) and always after `boss_hit_blocks` (3) blocked in a
//!     row, for a share of the boss's max HP (`boss_hit_punch` and co., in %). The last HP point
//!     is the game's (a deathblow);
//!   * a thrown boss takes posture damage on landing (grab.rs, `throw_posture`, % of his max).
//!
//! Each hit takes the HP of the enemies around it when it's fired and looks again a moment later
//! (the bullet lives 0.1 s).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use glam::Vec3;

use crate::grab;
use crate::hit::Attack;
use crate::log;
use crate::mem::read_u32;

const MULT_MIN: f32 = 0.1;
const MULT_MAX: f32 = 10.0;
/// The hit is looked at again after this long (s)
const SETTLE: f32 = 0.25;
/// Enemies this much further than the hit's radius count (m; their middle is up to a body away)
const REACH_EXTRA: f32 = 1.5;

/// A number from the ini (`key = 1.5`), or `default`.
pub fn setting(key: &str, default: f32) -> f32 {
    crate::paths::config(key).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(default)
}

/// The multiplier (f32 bits); 0: not read from the ini yet.
static MULT: AtomicU32 = AtomicU32::new(0);

pub fn multiplier() -> f32 {
    let bits = MULT.load(Ordering::Relaxed);
    if bits != 0 {
        return f32::from_bits(bits);
    }
    let m = crate::paths::config("damage").and_then(|v| v.parse::<f32>().ok()).filter(|m| m.is_finite()).unwrap_or(1.0);
    let m = round(m.clamp(MULT_MIN, MULT_MAX));
    MULT.store(m.to_bits(), Ordering::Relaxed);
    m
}

fn round(m: f32) -> f32 {
    (m * 10.0).round() / 10.0
}

/// Share of a boss's max HP a blocked hit that gets through takes (before the multiplier).
fn boss_share(attack: Attack) -> f32 {
    let (key, pct) = match attack {
        Attack::Punch => ("boss_hit_punch", 3.0),
        Attack::Dive => ("boss_hit_dive", 3.0),
        Attack::Pound => ("boss_hit_pound", 6.0),
        Attack::Throw => ("boss_hit_throw", 8.0),
    };
    setting(key, pct).clamp(0.0, 100.0) / 100.0
}

struct Victim {
    handle: u32,
    hp: i32,
    max: i32,
    boss: bool,
    /// (bosses: the posture value, to see which way a blocked hit moves it in the log)
    posture: Option<i32>,
}

fn posture_of(chr: usize) -> Option<i32> {
    grab::module(chr, 0x18).and_then(|d| read_u32(d + POSTURE)).map(|v| v as i32)
}

struct Pending {
    attack: Attack,
    at: Instant,
    victims: Vec<Victim>,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static BLOCKS: AtomicU32 = AtomicU32::new(0);

fn hp_of(chr: usize) -> Option<(i32, i32)> {
    let data = grab::module(chr, 0x18)?;
    Some((read_u32(data + 0x130)? as i32, read_u32(data + 0x134)? as i32))
}

fn set_hp(chr: usize, hp: i32) {
    let Some(data) = grab::module(chr, 0x18) else { return };
    if crate::mem::readable(data + 0x130, 4) {
        unsafe { std::ptr::write_volatile((data + 0x130) as *mut i32, hp) };
    }
}

/// Game thread: Mario's hit `attack` (radius `radius` m) was fired at `at`.
pub fn fired(attack: Attack, at: Vec3, radius: f32) {
    let bosses = grab::bosses();
    let mut handles = crate::aim::gauge_handles();
    handles.extend(bosses.iter().copied());
    handles.sort_unstable();
    handles.dedup();
    let victims: Vec<Victim> = handles
        .into_iter()
        .filter_map(|handle| {
            let c = grab::chr(handle)?;
            let p = grab::position(grab::physics(c)?)?;
            let (hp, max) = hp_of(c)?;
            (hp > 0 && max > 0 && p.distance(at) <= radius + REACH_EXTRA).then_some(Victim { handle, hp, max, boss: bosses.contains(&handle), posture: posture_of(c) })
        })
        .collect();
    if !victims.is_empty() {
        PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(Pending { attack, at: Instant::now(), victims });
    }
}

/// Game thread, every frame: the keys, and the hits fired a moment ago.
pub fn update() {
    keys();
    let due: Vec<Pending> = {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let (due, wait) = p.drain(..).partition(|h| h.at.elapsed().as_secs_f32() >= SETTLE);
        *p = wait;
        due
    };
    let mult = multiplier();
    for hit in due {
        for v in hit.victims {
            let Some(c) = grab::chr(v.handle) else { continue };
            let Some((hp, _)) = hp_of(c) else { continue };
            let lost = v.hp - hp;
            if hp <= 0 {
                continue;
            }
            if lost > 0 {
                // the game's damage, scaled
                let want = ((lost as f32) * mult).round() as i32;
                let now = (v.hp - want).clamp(1, v.max);
                if now != hp {
                    set_hp(c, now);
                    log(format!("dmg: {:?} on {:#x}: the game took {lost} HP, x{mult:.1} = {} ({now} of {} left)", hit.attack, v.handle, v.hp - now, v.max));
                }
                if v.boss {
                    BLOCKS.store(0, Ordering::Relaxed);
                }
            } else if v.boss {
                // blocked: posture only, unless this one gets through
                let blocks = BLOCKS.fetch_add(1, Ordering::Relaxed) + 1;
                let lands = blocks > setting("boss_hit_blocks", 3.0).max(0.0) as u32 || rand() < setting("boss_hit_chance", 0.4).clamp(0.0, 1.0);
                if !lands {
                    let p = posture_of(c);
                    log(format!("dmg: {:?} on boss {:#x} blocked ({blocks} in a row): posture only ({:?} -> {p:?})", hit.attack, v.handle, v.posture));
                    continue;
                }
                BLOCKS.store(0, Ordering::Relaxed);
                let dmg = ((v.max as f32 * boss_share(hit.attack) * mult).round() as i32).max(1);
                let now = (hp - dmg).max(1);
                set_hp(c, now);
                log(format!("dmg: {:?} on boss {:#x} got through the guard: {} HP ({now} of {} left)", hit.attack, v.handle, hp - now, v.max));
            }
        }
    }
}

/// ChrDataModule posture: the game's "stamina", which runs down as the posture bar fills (at 0 it
/// breaks), and its max.
const POSTURE: usize = 0x148;
const POSTURE_MAX: usize = 0x14C;

/// Adds `share` of the character's max posture to his posture bar (never quite breaking it: the
/// break is the game's, from the next hit).
pub fn add_posture(handle: u32, share: f32) {
    let Some(data) = grab::chr(handle).and_then(|c| grab::module(c, 0x18)) else { return };
    let (Some(cur), Some(max)) = (read_u32(data + POSTURE).map(|v| v as i32), read_u32(data + POSTURE_MAX).map(|v| v as i32)) else { return };
    if !(1..=100_000).contains(&max) || !(0..=max).contains(&cur) {
        let mut raw = [0u8; 0x30];
        crate::mem::read_bytes(data + 0x130, &mut raw);
        log(format!("dmg: no posture at +{POSTURE:#x} for {handle:#x} ({cur} of {max}); data +0x130: {raw:02x?}"));
        return;
    }
    let now = (cur - (max as f32 * share).round() as i32).max(1);
    if now < cur {
        unsafe { std::ptr::write_volatile((data + POSTURE) as *mut i32, now) };
    }
    log(format!("dmg: posture damage on {handle:#x}: {:.0}% of {max} (posture left {cur} -> {now})", share * 100.0));
}

/// 0..1, good enough for a dice roll.
pub fn rand() -> f32 {
    static STATE: AtomicU32 = AtomicU32::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1) ^ std::process::id().wrapping_mul(0x9E37_79B9) | 1;
    }
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    STATE.store(x, Ordering::Relaxed);
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Shift+F2: lower; Shift+F3: higher (0.1 steps).
fn keys() {
    use crate::mario::key_down;
    static WAS: AtomicU32 = AtomicU32::new(0);
    let shift = key_down(0x10);
    let (f2, f3) = (shift && key_down(0x71), shift && key_down(0x72));
    let was = WAS.swap(f2 as u32 | (f3 as u32) << 1, Ordering::Relaxed);
    let step = if f2 && was & 1 == 0 {
        -0.1
    } else if f3 && was & 2 == 0 {
        0.1
    } else {
        return;
    };
    let m = round((multiplier() + step).clamp(MULT_MIN, MULT_MAX));
    MULT.store(m.to_bits(), Ordering::Relaxed);
    log(format!("Shift+{} pressed: Mario's damage x{m:.1}", if step < 0.0 { "F2" } else { "F3" }));
    crate::hud::toast(format!("Mario's damage x{m:.1}"));
}
