//! Aiming like Sekiro: with Sekiro's camera, a locked-on enemy is where Mario's punches go (he
//! turns to it as he punches), and a deathblow within reach comes first whichever way he faces.
//!
//! The lock-on is read from the HUD: MenuMan's enemy gauges (the posture bars over enemies) carry
//! the character handle and whether that enemy is the locked one. Bosses have no bar over them
//! (theirs are at the top of the screen), so a boss lock-on is followed from the lock-on button
//! (R3 / Q / middle mouse): pressed with a boss in view and no enemy gauge locked, the boss is
//! locked; pressed again, or the camera off the boss for a while (the game let go), it isn't.

use std::sync::Mutex;
use std::time::Instant;

use glam::Vec3;

use crate::grab;
use crate::log;

/// A deathblow is looked for on enemies this close (m, flat), whichever way Mario faces.
pub const DEATHBLOW_REACH: f32 = 1.0;

/// The enemies with a gauge on screen: (handle, locked).
fn gauges() -> Vec<(u32, bool)> {
    use fromsoftware_shared::FromStatic;
    let Some(p) = sekiro::sprj::MenuMan::instance_ptr().ok().filter(|p| !p.is_null()) else { return Vec::new() };
    let menu = unsafe { &*p };
    menu.enemy_gauges().map(|g| (g.handle, g.is_lock)).filter(|(h, _)| *h != u32::MAX && *h != 0).collect()
}

/// The handles of the enemies with a gauge on screen.
pub fn gauge_handles() -> Vec<u32> {
    gauges().into_iter().map(|(h, _)| h).collect()
}

/// A living character's position.
fn alive_at(handle: u32) -> Option<Vec3> {
    let c = grab::chr(handle)?;
    if grab::hp(c).is_some_and(|hp| hp <= 0) {
        return None;
    }
    grab::position(grab::physics(c)?)
}

/// The locked-on enemy's position, with Sekiro's camera (Lakitu's keeps Mario's own controls).
pub fn locked() -> Option<Vec3> {
    if crate::lakitu::active() {
        return None;
    }
    let gauges = gauges();
    let target = gauges.iter().find(|(_, lock)| *lock).and_then(|&(h, _)| alive_at(h).map(|p| (h, p)));
    let target = target.or_else(|| boss_lock(gauges.iter().any(|(_, lock)| *lock)));
    static LAST: Mutex<Option<u32>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = target.map(|(h, _)| h);
    if *last != now {
        *last = now;
        log(match now {
            Some(h) => format!("aim: locked on {h:#x}: Mario's punches go there"),
            None => "aim: no lock-on".to_string(),
        });
    }
    target.map(|(_, p)| p)
}

/// Boss lock-on reach (m) when the button is pressed, and how centred the camera must be on him
/// (cosine, flat): to lock, and to stay locked
const BOSS_LOCK_REACH: f32 = 30.0;
const BOSS_LOCK_VIEW: f32 = 0.8;
const BOSS_LOCK_KEEP: f32 = 0.5;
/// the camera off the boss this long: the game let go of him
const BOSS_LOCK_LOST: f32 = 1.5;
/// the game's lock-on shows on the gauges this late after the button
const BOSS_LOCK_SETTLE: f32 = 0.15;

struct BossLock {
    button_was: bool,
    /// the button was pressed then (decided once the game had time to lock an enemy gauge)
    pressed: Option<Instant>,
    boss: Option<u32>,
    off_since: Option<Instant>,
}

static BOSS_LOCK: Mutex<BossLock> = Mutex::new(BossLock { button_was: false, pressed: None, boss: None, off_since: None });

/// How centred the camera is on `at` (flat cosine), and the camera's distance to it.
fn in_view(at: Vec3) -> Option<(f32, f32)> {
    let me = crate::player::position().map(Vec3::from)?;
    let cam = crate::camera::active(me)?;
    let f = Vec3::new(cam.forward.x, 0.0, cam.forward.z).normalize_or_zero();
    let to = towards(cam.pos, at)?;
    Some((f.dot(to), Vec3::new(at.x - me.x, 0.0, at.z - me.z).length()))
}

/// The locked-on boss (handle, position), followed from the lock-on button. `gauge_locked`: the
/// game locked an enemy with a gauge (that's the target then).
fn boss_lock(gauge_locked: bool) -> Option<(u32, Vec3)> {
    let mut b = BOSS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let button = crate::pad::read().filter(|_| crate::mario::focused()).is_some_and(|p| p.r3)
        || crate::mario::key_down(0x51)
        || crate::mario::key_down(0x04);
    if button && !b.button_was {
        if let Some(h) = b.boss.take() {
            log(format!("aim: lock-on button: boss {h:#x} let go"));
        } else {
            b.pressed = Some(Instant::now());
        }
    }
    b.button_was = button;
    let bosses = grab::bosses();
    if b.pressed.is_some_and(|t| t.elapsed().as_secs_f32() > BOSS_LOCK_SETTLE) {
        b.pressed = None;
        if !gauge_locked {
            // the boss most centred in view, within reach
            let pick = bosses
                .iter()
                .filter_map(|&h| alive_at(h).map(|p| (h, p)))
                .filter_map(|(h, p)| in_view(p).map(|(cos, d)| (h, cos, d)))
                .filter(|&(_, cos, d)| cos > BOSS_LOCK_VIEW && d < BOSS_LOCK_REACH)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((h, cos, d)) = pick {
                log(format!("aim: lock-on button with boss {h:#x} in view ({cos:.2}, {d:.1} m): locked on him"));
                b.boss = Some(h);
                b.off_since = None;
            }
        }
    }
    let h = b.boss?;
    let at = (bosses.contains(&h) && !gauge_locked).then(|| alive_at(h)).flatten();
    let Some(at) = at else {
        log(format!("aim: boss {h:#x} gone (bar off, dead, or another enemy locked): let go"));
        b.boss = None;
        return None;
    };
    if in_view(at).is_some_and(|(cos, _)| cos < BOSS_LOCK_KEEP) {
        let since = *b.off_since.get_or_insert_with(Instant::now);
        if since.elapsed().as_secs_f32() > BOSS_LOCK_LOST {
            log(format!("aim: camera off boss {h:#x} for {BOSS_LOCK_LOST} s: the game let go"));
            b.boss = None;
            return None;
        }
    } else {
        b.off_since = None;
    }
    Some((h, at))
}

/// The nearest enemy (with a gauge or a boss bar) within `DEATHBLOW_REACH` of Mario.
pub fn deathblow_candidate(mario: Vec3) -> Option<Vec3> {
    let mut handles: Vec<u32> = gauges().into_iter().map(|(h, _)| h).collect();
    handles.extend(grab::bosses());
    handles
        .into_iter()
        .filter_map(alive_at)
        .filter(|p| (p.y - mario.y).abs() < 1.5)
        .map(|p| (Vec3::new(p.x - mario.x, 0.0, p.z - mario.z).length(), p))
        .filter(|(d, _)| *d <= DEATHBLOW_REACH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
}

/// The flat direction from `from` to `to` (None when they're on top of each other).
pub fn towards(from: Vec3, to: Vec3) -> Option<Vec3> {
    let d = Vec3::new(to.x - from.x, 0.0, to.z - from.z);
    (d.length_squared() > 1e-4).then(|| d.normalize())
}

/// SM64's face angle (radians) for a world direction: Mario faces (-sin, cos) of it.
pub fn face_angle(dir: Vec3) -> f32 {
    (-dir.x).atan2(dir.z).clamp(-3.1415, 3.1415)
}
