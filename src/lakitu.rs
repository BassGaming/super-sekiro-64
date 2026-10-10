//! SM64's Lakitu camera instead of Sekiro's (F9 switches; `camera = sekiro` in sk_mario.ini starts
//! with Sekiro's). Ported from er-mario's free-roam Lakitu:
//! - the camera stays where it is and only follows to keep its distance (a leash), so Mario runs
//!   around it the way he does in SM64
//! - C-left / C-right (right stick left / right, or the arrow keys) swing it 45 degrees around Mario
//! - C-down (right stick down, or down arrow) zooms out in three steps, C-up back in
//! - it doesn't bob with Mario's jumps (the height follows slowly in the air)
//! - walls in the way pull it in (rays against the collision Mario has loaded)
//! It writes the game's render camera at every step from the game's camera update to drawing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use glam::Vec3;

/// Mario's head height (m), what the camera looks at
const FOCUS_HEIGHT: f32 = 1.0;
/// distance (m) and height above the focus for the zoom steps (C-down steps out, C-up in)
const ZOOM: [(f32, f32); 3] = [(5.0, 1.6), (7.0, 2.2), (9.5, 3.0)];
/// one C-button press
const STEP: f32 = std::f32::consts::FRAC_PI_4;

/// SM64's camera sounds (SOUND_MENU_CAMERA_*)
const SOUND_TURN: i32 = 0x700F_0081;
const SOUND_ZOOM_IN: i32 = 0x7006_0081;
const SOUND_ZOOM_OUT: i32 = 0x7007_0081;
const SOUND_BUZZ: i32 = 0x700E_0081;

pub static ON: AtomicBool = AtomicBool::new(true);

pub fn load_setting() {
    if crate::paths::config("camera").is_some_and(|v| v.eq_ignore_ascii_case("sekiro")) {
        ON.store(false, Ordering::Relaxed);
    }
}

/// The last camera written: forward, position, when.
static LAST: Mutex<Option<(Vec3, Vec3, Instant)>> = Mutex::new(None);
static SOUNDS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Lakitu is on and steering the camera right now.
pub fn active() -> bool {
    ON.load(Ordering::Relaxed) && LAST.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|(_, _, at)| at.elapsed().as_secs_f32() < 0.2)
}

/// Lakitu's view direction (Mario steers relative to it), if active.
pub fn forward() -> Option<Vec3> {
    active().then(|| LAST.lock().unwrap_or_else(|e| e.into_inner()).map(|(f, _, _)| f)).flatten()
}

/// Zoomed out (for the HUD's camera icon).
pub fn far() -> bool {
    CAM.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|c| c.zoom > 0)
}

pub fn take_sounds() -> Vec<i32> {
    std::mem::take(&mut *SOUNDS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn sound(id: i32) {
    SOUNDS.lock().unwrap_or_else(|e| e.into_inner()).push(id);
}

/// Writes Lakitu's camera into the game's (run at every task group from the camera step to drawing).
pub fn reapply() {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let Some((fwd, pos, at)) = *LAST.lock().unwrap_or_else(|e| e.into_inner()) else { return };
    if at.elapsed().as_secs_f32() > 0.1 {
        return;
    }
    // never over the game's load-screen camera (that must stay visible: it's how Mario mode
    // knows the world is loading)
    if crate::camera::is_load_screen() {
        return;
    }
    crate::camera::write(fwd, pos);
}

struct Cam {
    dist: f32,
    lift: f32,
    look_up: f32,
    mario: Vec3,
    pos: Vec3,
    leash: Vec3,
    focus: Vec3,
    yaw: f32,
    target_yaw: Option<f32>,
    zoom: usize,
    held: [bool; 4],
}

static CAM: Mutex<Option<Cam>> = Mutex::new(None);

pub fn reset() {
    if CAM.lock().unwrap_or_else(|e| e.into_inner()).take().is_some() {
        crate::log("lakitu: off (Sekiro's camera takes over)");
    }
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn angle_diff(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::TAU);
    if d > std::f32::consts::PI { d - std::f32::consts::TAU } else { d }
}

/// Every frame while on: moves the camera. `mario` feet (world), `airborne`, C-buttons
/// [left, right, up, down], `hit` gives the first collision between two points.
pub fn update(dt: f32, mario: Vec3, airborne: bool, c: [bool; 4], game_cam: Option<Vec3>, hit: impl Fn(Vec3, Vec3) -> Option<Vec3>) {
    let mut guard = CAM.lock().unwrap_or_else(|e| e.into_inner());
    // start where Sekiro's camera is (or behind Mario)
    let cam = guard.get_or_insert_with(|| {
        let start = game_cam.filter(|p| p.distance(mario) < 30.0).unwrap_or(mario + Vec3::new(0.0, 2.0, -5.0));
        let d = start - mario;
        crate::log(format!("lakitu: start at {start:.2?}, mario {mario:.2?}"));
        Cam {
            dist: ZOOM[0].0,
            lift: 0.0,
            look_up: 0.0,
            mario,
            pos: start,
            leash: start,
            focus: mario + Vec3::Y * FOCUS_HEIGHT,
            yaw: d.z.atan2(d.x),
            target_yaw: None,
            zoom: 0,
            held: c,
        }
    });

    // C-buttons (edges)
    let pressed: Vec<bool> = (0..4).map(|i| c[i] && !cam.held[i]).collect();
    cam.held = c;
    let base = cam.target_yaw.unwrap_or(cam.yaw);
    if pressed[0] {
        cam.target_yaw = Some(base - STEP);
        sound(SOUND_TURN);
    }
    if pressed[1] {
        cam.target_yaw = Some(base + STEP);
        sound(SOUND_TURN);
    }
    if pressed[2] {
        if cam.zoom > 0 {
            cam.zoom -= 1;
            sound(SOUND_ZOOM_IN);
        } else {
            sound(SOUND_BUZZ);
        }
    } else if pressed[3] {
        if cam.zoom + 1 < ZOOM.len() {
            cam.zoom += 1;
            sound(SOUND_ZOOM_OUT);
        } else {
            sound(SOUND_BUZZ);
        }
    }

    // the focus: Mario's head; in the air the height follows slowly (no bobbing on jumps)
    let want = mario + Vec3::Y * FOCUS_HEIGHT;
    let jump = mario - cam.mario;
    cam.mario = mario;
    if Vec3::new(jump.x, 0.0, jump.z).length() > 4.0 || jump.y.abs() > 4.0 {
        // a teleport: move along instead of swinging round
        cam.pos += jump;
        cam.leash += jump;
        cam.focus += jump;
    }
    let rate = |r: f32| 1.0 - (-r * dt).exp();
    cam.focus.x = want.x;
    cam.focus.z = want.z;
    let vy = if airborne && want.y > cam.focus.y - 0.5 { 1.2 } else { 5.0 };
    cam.focus.y += (want.y - cam.focus.y) * rate(vy);
    let head = mario + Vec3::Y * FOCUS_HEIGHT;
    let ground = hit(head, mario - Vec3::Y * 30.0).map(|g| g.y).unwrap_or(mario.y);
    cam.focus.y = cam.focus.y.max(ground + 0.5);

    // yaw: a C-button swing, or the leash (the camera stays put, Mario runs around it)
    match cam.target_yaw {
        Some(t) => {
            let d = angle_diff(t, cam.yaw);
            cam.yaw += d * rate(10.0);
            if d.abs() < 0.01 {
                cam.yaw = t;
                cam.target_yaw = None;
            }
        }
        None => {
            let d = cam.leash - cam.focus;
            if d.x.abs() + d.z.abs() > 0.01 {
                cam.yaw = d.z.atan2(d.x);
            }
        }
    }
    let (dist, height) = ZOOM[cam.zoom];
    let flat = Vec3::new(cam.yaw.cos() * dist, 0.0, cam.yaw.sin() * dist);
    // uphill ahead: look up the slope, the camera a bit lower
    let ahead = mario - flat.normalize_or_zero() * 5.0;
    let probe = Vec3::new(ahead.x, mario.y + FOCUS_HEIGHT + 1.5, ahead.z);
    let open = hit(head, probe).is_none();
    let rise = if open { hit(probe, ahead - Vec3::Y * 8.0).map(|g| g.y - ground).unwrap_or(0.0) } else { 0.0 };
    cam.look_up += ((rise * 0.6).clamp(0.0, 3.0) - cam.look_up) * rate(3.0);
    let height = height - (cam.look_up * 0.5).min(height * 0.7);
    cam.leash = cam.focus + flat + Vec3::Y * height;
    // a hill behind Mario: fly up over it; only when that doesn't help does the camera pull in
    let clear = [0.0, 1.0, 2.0, 3.5, 5.0].into_iter().find(|&extra| hit(head, cam.focus + flat + Vec3::Y * (height + extra)).is_none());
    let want_lift = clear.unwrap_or(0.0);
    cam.lift += (want_lift - cam.lift) * if want_lift > cam.lift { rate(10.0) } else { rate(1.5) };
    let offset = flat + Vec3::Y * (height + cam.lift);
    let full = offset.length();
    let dir = offset / full;
    let want = if clear.is_some() {
        full
    } else {
        hit(head, cam.focus + offset).map(|h| ((h - cam.focus).length() - 0.3).max(0.6)).unwrap_or(full)
    };
    cam.dist += (want - cam.dist) * if want < cam.dist { rate(15.0) } else { rate(2.0) };
    let mut pos = cam.focus + dir * cam.dist;
    let top = Vec3::new(pos.x, pos.y.max(head.y) + 0.1, pos.z);
    if let Some(g) = hit(top, pos - Vec3::Y * 1.0) {
        pos.y = pos.y.max(g.y + 0.4);
    }
    cam.pos = pos;

    let fwd = (cam.focus + Vec3::Y * cam.look_up - cam.pos).normalize_or(Vec3::Z);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((fwd, cam.pos, Instant::now()));
}
