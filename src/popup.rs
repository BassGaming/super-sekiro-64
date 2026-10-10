//! The game's popups (tutorials and "item obtained" boxes) and conversations, which wait for the
//! confirm button.
//!
//! They aren't menu mode (MenuMan's flags stay as they are), so nothing in the menus tells they
//! are up. Instead every virtual function of their dialog classes (`GetItemDialog`,
//! `TutorialMsgDialog`, ... by RTTI) is routed through a stub that counts the call and jumps on to
//! the game's own function: a dialog on screen is updated every frame, so a count that keeps
//! going up means one is open. Then Y is the game's confirm (A) instead of its action button (pad.rs);
//! nothing else changes, so a wrong guess costs no more than Y. A class only counts once it has
//! been quiet (some are called every frame with nothing on screen, `AppMenu::CSTutorialDialog`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use fromsoftware_shared::{Program, rtti::find_rtti_classes};
use pelite::pe64::Pe;
use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualAlloc, VirtualProtect};

use crate::log;
use crate::mem::{read_u64, readable};

/// The dialogs that wait for the confirm button, and the conversation window.
const CLASSES: &[&str] = &[
    "GetItemDialog",
    "TutorialMsgDialog",
    "FullScrMsgDialog",
    "AppMenu::TalkMenuWindow",
    "AppMenu::TalkMenuWindow::TalkComponent",
];
/// Virtual functions patched per class at most.
const MAX_SLOTS: usize = 80;
/// A dialog counts as open this long after its last call.
const OPEN_FOR: f32 = 0.25;
/// A class must be quiet this long before it counts.
const QUIET_FIRST: f32 = 1.0;
/// A class busy this long without a break is something else (a dialog kept alive off screen): it
/// doesn't count until it is quiet again.
const NEVER_THAT_LONG: f32 = 180.0;

/// The block holding the counters (one per class) and, after them, the stubs.
static BLOCK: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicBool = AtomicBool::new(false);

struct Watch {
    /// last count seen, when it last changed, since when it keeps changing
    last: u64,
    changed: Option<Instant>,
    busy_since: Option<Instant>,
    open: bool,
    /// it has been quiet since it was patched (or since it was busy for too long)
    trusted: bool,
    /// when it was patched / last distrusted
    since: Instant,
}

static WATCH: Mutex<Vec<Watch>> = Mutex::new(Vec::new());

fn counter(i: usize) -> &'static AtomicU64 {
    unsafe { &*((BLOCK.load(Ordering::Relaxed) + i * 8) as *const AtomicU64) }
}

/// Patches the dialogs' virtual tables (on a thread: the RTTI scan takes a moment).
pub fn install() {
    if crate::paths::config("popups").is_some_and(|v| v.eq_ignore_ascii_case("off")) {
        log("popup: off (sk_mario.ini)");
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Err(e) = patch() {
            log(format!("popup: not watching the game's popups ({e})"));
        }
    });
}

fn patch() -> Result<(), String> {
    let program = Program::current();
    let base = program.optional_header().ImageBase as usize;
    // a table's functions point into the executable sections; what follows its last one (the next
    // table's type info, in .rdata) points into the image too, and patching that broke the game's
    // type checks: every frame slower (40 fps)
    let exec: Vec<(usize, usize)> = program
        .section_headers()
        .iter()
        .filter(|s| s.Characteristics & 0x2000_0000 != 0)
        .map(|s| (base + s.VirtualAddress as usize, base + s.VirtualAddress as usize + s.VirtualSize as usize))
        .collect();
    let code = |p: u64| exec.iter().any(|&(a, b)| (p as usize) >= a && (p as usize) < b);
    let mut tables = Vec::new();
    for class in find_rtti_classes(&program) {
        if let Some(i) = CLASSES.iter().position(|&c| c == class.name) {
            if let Ok(vt) = program.rva_to_va(class.vftable) {
                tables.push((i, vt as usize));
            }
        }
    }
    if tables.is_empty() {
        return Err("none of the dialog classes found".into());
    }
    // counters, then (on the next page: writing to a page the CPU runs code from makes it throw
    // away its work in flight, on every call) 24-byte stubs: lock inc qword [counter]; jmp qword
    // [original]; (8 bytes original)
    let block = unsafe { VirtualAlloc(None, 0x10000, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) } as usize;
    if block == 0 {
        return Err("VirtualAlloc failed".into());
    }
    BLOCK.store(block, Ordering::Relaxed);
    let mut stub = block + 0x1000;
    let mut done = Vec::new();
    for (class, vt) in tables {
        let mut slots = 0;
        for s in 0..MAX_SLOTS {
            let entry = vt + s * 8;
            let Some(f) = read_u64(entry).filter(|&f| code(f)) else { break };
            if stub + 24 > block + 0x10000 {
                break;
            }
            let ctr = block + class * 8;
            let mut b = [0u8; 24];
            // F0 48 FF 05 rel32: lock inc qword ptr [rip+rel32]
            b[..4].copy_from_slice(&[0xF0, 0x48, 0xFF, 0x05]);
            b[4..8].copy_from_slice(&((ctr as i64 - (stub + 8) as i64) as i32).to_le_bytes());
            // FF 25 rel32: jmp qword ptr [rip+rel32] (the original, at stub+16)
            b[8..10].copy_from_slice(&[0xFF, 0x25]);
            b[10..14].copy_from_slice(&((stub + 16) as i64 - (stub + 14) as i64).to_le_bytes()[..4]);
            b[16..24].copy_from_slice(&f.to_le_bytes());
            unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), stub as *mut u8, 24) };
            if !write_ptr(entry, stub as u64) {
                break;
            }
            stub += 24;
            slots += 1;
        }
        done.push(format!("{} ({vt:#x}, {slots} functions)", CLASSES[class]));
    }
    *WATCH.lock().unwrap_or_else(|e| e.into_inner()) =
        CLASSES.iter().map(|_| Watch { last: 0, changed: None, busy_since: None, open: false, trusted: false, since: Instant::now() }).collect();
    READY.store(true, Ordering::Relaxed);
    log(format!("popup: executable sections {:x?}", exec));
    let missing: Vec<&str> = CLASSES.iter().copied().filter(|c| !done.iter().any(|d| d.starts_with(&format!("{c} (")))).collect();
    log(format!("popup: watching {}; not found: {missing:?}", done.join(", ")));
    Ok(())
}

fn write_ptr(at: usize, v: u64) -> bool {
    if !readable(at, 8) {
        return false;
    }
    let mut old = PAGE_PROTECTION_FLAGS::default();
    if unsafe { VirtualProtect(at as *const _, 8, PAGE_EXECUTE_READWRITE, &mut old) }.is_err() {
        return false;
    }
    unsafe { (at as *mut u64).write_volatile(v) };
    let mut back = PAGE_PROTECTION_FLAGS::default();
    let _ = unsafe { VirtualProtect(at as *const _, 8, old, &mut back) };
    true
}

/// A tutorial or item popup is on screen.
pub fn open() -> bool {
    if !READY.load(Ordering::Relaxed) {
        return false;
    }
    let now = Instant::now();
    let mut watch = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    let mut any = false;
    for (i, w) in watch.iter_mut().enumerate() {
        let n = counter(i).load(Ordering::Relaxed);
        if n != w.last {
            w.last = n;
            if w.changed.is_none_or(|t| now.duration_since(t).as_secs_f32() > OPEN_FOR) {
                w.busy_since = Some(now);
            }
            w.changed = Some(now);
        }
        let busy = w.changed.is_some_and(|t| now.duration_since(t).as_secs_f32() < OPEN_FOR);
        let quiet_for = now.duration_since(w.changed.unwrap_or(w.since).max(w.since)).as_secs_f32();
        if !w.trusted && !busy && quiet_for > QUIET_FIRST {
            w.trusted = true;
            log(format!("popup: {} quiet: watched", CLASSES[i]));
        }
        if w.trusted && busy && w.busy_since.is_some_and(|t| now.duration_since(t).as_secs_f32() > NEVER_THAT_LONG) {
            w.trusted = false;
            w.since = now;
            log(format!("popup: {} busy for {NEVER_THAT_LONG:.0} s: not a popup, ignored until it's quiet", CLASSES[i]));
        }
        let open = busy && w.trusted;
        if open != w.open {
            w.open = open;
            log(format!("popup: {} {}", CLASSES[i], if open { "open: Y confirms" } else { "closed" }));
        }
        any |= open;
    }
    any
}
