//! Xbox controller (XInput) for Mario.
//!
//! Sekiro reads pads through `XINPUT1_3.dll` `XInputGetState` (imported by ordinal 2). We swap
//! that import table entry for our own function: the real state is kept for Mario, and the game
//! gets it back with the right stick hidden while Lakitu's camera owns it.
//!
//! Mapping in Mario mode (like er-mario / SM64 on a pad): left stick moves, A jumps, X punches, LT
//! crouches (Z: crouch, ground pound; the game doesn't get it), B is Sekiro's grappling hook (the
//! game gets it as LT), LB is only Sekiro's deflect, Y is the game's action button, right stick is
//! Lakitu's C-buttons. Playing Sekiro, the pad is the game's own.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::XboxController::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK, XINPUT_GAMEPAD_LEFT_THUMB, XINPUT_GAMEPAD_RIGHT_SHOULDER,
    XINPUT_GAMEPAD_RIGHT_THUMB,
    XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y, XINPUT_STATE,
};

type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XINPUT_STATE) -> u32;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static PAD: Mutex<Option<(XINPUT_STATE, Instant)>> = Mutex::new(None);

/// What Mario reads from the pad this tick.
#[derive(Clone, Copy, Default)]
pub struct Pad {
    pub stick_x: f32,
    /// SM64 convention: up (forward) is negative
    pub stick_y: f32,
    pub a: bool,
    pub b: bool,
    pub z: bool,
    /// B: Sekiro's grappling hook (the game gets it as LT)
    pub grapple: bool,
    /// Y: Sekiro's action button (doors, items, talking)
    pub y: bool,
    /// left stick click: Lakitu's camera / Sekiro's
    pub l3: bool,
    /// right stick click: Sekiro's lock-on
    pub r3: bool,
    /// right stick, -1..1 each (x right, y up)
    pub right: (f32, f32),
}

/// Controller 0, if it reported in the last quarter second.
pub fn read() -> Option<Pad> {
    let (s, at) = (*PAD.lock().unwrap_or_else(|e| e.into_inner()))?;
    if at.elapsed().as_secs_f32() > 0.25 {
        return None;
    }
    let g = s.Gamepad;
    let axis = |v: i16| {
        let f = v as f32 / 32767.0;
        if f.abs() < 0.2 { 0.0 } else { f.clamp(-1.0, 1.0) }
    };
    let b = g.wButtons;
    Some(Pad {
        stick_x: axis(g.sThumbLX),
        stick_y: -axis(g.sThumbLY),
        a: b.contains(XINPUT_GAMEPAD_A),
        b: b.contains(XINPUT_GAMEPAD_X),
        // (LB is Sekiro's deflect only: crouching and deflecting can't share a button; nor can
        // crouching and the grappling hook: a ground pound grappled)
        z: g.bLeftTrigger > 100,
        grapple: b.contains(XINPUT_GAMEPAD_B),
        y: b.contains(XINPUT_GAMEPAD_Y),
        l3: b.contains(XINPUT_GAMEPAD_LEFT_THUMB),
        r3: b.contains(XINPUT_GAMEPAD_RIGHT_THUMB),
        right: (axis(g.sThumbRX), axis(g.sThumbRY)),
    })
}

/// Select was pressed (an edge): Mario mode / playing Sekiro, like F8.
static BACK_EDGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Sekiro's attack (RB) is held for the game until then (Mario's ground pound).
static ATTACK_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Select was pressed since the last call.
pub fn take_back_press() -> bool {
    BACK_EDGE.swap(false, Ordering::Relaxed)
}

/// Holds Sekiro's attack for the game for `secs` (Mario's ground pound lands).
pub fn pulse_attack(secs: f32) {
    *ATTACK_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + std::time::Duration::from_secs_f32(secs));
}

/// Mario attacks right now: X/B on the pad, N on the keyboard, or a pulse.
fn mario_attacks(pad: Option<&XINPUT_STATE>) -> bool {
    let pulse = ATTACK_UNTIL.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| Instant::now() < t);
    // Mario's hits are his own (the game's bullets, hit.rs): the sword stays in its sheath, but
    // for a pulse (a deathblow: mario.rs)
    if crate::hit::ready() {
        return pulse;
    }
    let b = pad.map(|s| s.Gamepad.wButtons);
    b.is_some_and(|b| b.contains(XINPUT_GAMEPAD_X) || b.contains(XINPUT_GAMEPAD_B))
        || crate::mario::key_down(0x4E)
        || pulse
}

/// The game's action button (interact) on the pad: X (`action_button = x`, the default) or Y.
fn action_is_x() -> bool {
    static X: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *X.get_or_init(|| !crate::paths::config("action_button").is_some_and(|v| v.trim().eq_ignore_ascii_case("y")))
}

unsafe extern "system" fn get_state(index: u32, state: *mut XINPUT_STATE) -> u32 {
    let _span = crate::prof::span(crate::prof::PAD);
    let original: XInputGetStateFn = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Relaxed)) };
    let rc = unsafe { original(index, state) };
    if state.is_null() || index > 3 {
        return rc;
    }
    let controls = crate::mario::controls_game();
    if rc != 0 {
        // no pad: Mario's punches still reach the game as its attack, through a virtual pad 0
        if index == 0 && controls && mario_attacks(None) {
            let s = unsafe { &mut *state };
            *s = XINPUT_STATE::default();
            s.Gamepad.wButtons = XINPUT_GAMEPAD_RIGHT_SHOULDER;
            return 0;
        }
        return rc;
    }
    let s = unsafe { &mut *state };
    if index == 0 {
        let mut pad = PAD.lock().unwrap_or_else(|e| e.into_inner());
        if pad.is_none() {
            crate::log("pad: controller 0 connected");
        }
        let was_back = pad.is_some_and(|(p, _)| p.Gamepad.wButtons.contains(XINPUT_GAMEPAD_BACK));
        if s.Gamepad.wButtons.contains(XINPUT_GAMEPAD_BACK) && !was_back {
            BACK_EDGE.store(true, Ordering::Relaxed);
        }
        *pad = Some((*s, Instant::now()));
    }
    // Lakitu's camera owns the right stick (the game's camera would fight it)
    if crate::lakitu::active() {
        s.Gamepad.sThumbRX = 0;
        s.Gamepad.sThumbRY = 0;
    }
    if crate::mario::in_world_no_menu() {
        // Select is F8 (Mario mode / playing Sekiro)
        s.Gamepad.wButtons &= !XINPUT_GAMEPAD_BACK;
    }
    if controls {
        // Mario's buttons are his: Sekiro mustn't jump (A), dodge (B), use items (X) or grapple
        // (LT: Mario's crouch) with them. B is the grappling hook instead (the game's LT).
        // Sekiro's attack (RB) comes with Mario's deathblows (or his punches while the bullets
        // aren't ready). LB (deflect), RT (prosthetic) and RB itself still reach the game.
        let attack = mario_attacks(Some(s));
        let grapple = s.Gamepad.wButtons.contains(XINPUT_GAMEPAD_B);
        s.Gamepad.bLeftTrigger = if grapple { 255 } else { 0 };
        // (L3 is Mario's camera switch, not Sekiro's crouch; Y is Sekiro's action button, which
        // is X in the game's own layout)
        let y = s.Gamepad.wButtons.contains(XINPUT_GAMEPAD_Y);
        s.Gamepad.wButtons &= !(XINPUT_GAMEPAD_A | XINPUT_GAMEPAD_B | XINPUT_GAMEPAD_X | XINPUT_GAMEPAD_LEFT_THUMB);
        if y && crate::popup::open() {
            // a tutorial or item popup: Y answers it, as the game's confirm
            s.Gamepad.wButtons = (s.Gamepad.wButtons & !XINPUT_GAMEPAD_Y) | XINPUT_GAMEPAD_A;
        } else if y && action_is_x() {
            s.Gamepad.wButtons = (s.Gamepad.wButtons & !XINPUT_GAMEPAD_Y) | XINPUT_GAMEPAD_X;
        }
        if attack {
            s.Gamepad.wButtons |= XINPUT_GAMEPAD_RIGHT_SHOULDER;
        }
    }
    rc
}

/// Swaps the game's `XInputGetState` import (ordinal 2 of any xinput DLL) for ours.
pub fn install() {
    match unsafe { patch_import(2, get_state as *const () as usize) } {
        Some((dll, previous)) => {
            ORIGINAL.store(previous, Ordering::Relaxed);
            crate::log(format!("pad: hooked XInputGetState through the import table ({dll})"));
        }
        None => crate::log("pad: XInputGetState import not found; controller support is off"),
    }
}

/// Replaces the game executable's import of `ordinal` from any xinput DLL. Returns (dll, previous).
unsafe fn patch_import(ordinal: u16, replacement: usize) -> Option<(String, usize)> {
    use windows::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
    let base = unsafe { GetModuleHandleW(None) }.ok()?.0 as usize;
    let u32_at = |a: usize| unsafe { (a as *const u32).read_unaligned() };
    let nt = base + u32_at(base + 0x3C) as usize;
    if u32_at(nt) != 0x4550 {
        return None;
    }
    let imports = u32_at(nt + 0x18 + 0x70 + 8) as usize;
    if imports == 0 {
        return None;
    }
    let mut desc = base + imports;
    loop {
        let (lookup, name, iat) = (u32_at(desc) as usize, u32_at(desc + 12) as usize, u32_at(desc + 16) as usize);
        if name == 0 {
            return None;
        }
        let dll = unsafe { std::ffi::CStr::from_ptr((base + name) as *const std::ffi::c_char) }.to_string_lossy().to_string();
        if dll.to_ascii_lowercase().starts_with("xinput") {
            let names = if lookup != 0 { lookup } else { iat };
            for k in 0.. {
                let entry = unsafe { ((base + names + k * 8) as *const u64).read_unaligned() };
                if entry == 0 {
                    break;
                }
                let hit = if entry & (1 << 63) != 0 {
                    (entry & 0xFFFF) as u16 == ordinal
                } else {
                    let n = unsafe { std::ffi::CStr::from_ptr((base + entry as usize + 2) as *const std::ffi::c_char) };
                    n.to_bytes() == b"XInputGetState"
                };
                if hit {
                    let slot = (base + iat + k * 8) as *mut usize;
                    let mut old = PAGE_PROTECTION_FLAGS(0);
                    unsafe { VirtualProtect(slot as *const _, 8, PAGE_READWRITE, &mut old) }.ok()?;
                    let previous = unsafe { slot.read() };
                    unsafe { slot.write(replacement) };
                    let mut back = PAGE_PROTECTION_FLAGS(0);
                    let _ = unsafe { VirtualProtect(slot as *const _, 8, old, &mut back) };
                    return Some((dll, previous));
                }
            }
        }
        desc += 20;
    }
}
