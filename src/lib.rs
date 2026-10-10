//! sk-mario — Super Mario 64 movement inside Sekiro, in-process (libsm64).
//!
//! Ported from deltarooo/er-mario (MIT); see LICENSE and README for attribution. The original
//! Elden Ring modules live in git history (commit dd21f0e) and are ported back one at a time,
//! replacing `eldenring::*` with our Sekiro layer.
//!
//! Milestone 0: prove me3 loads this DLL into Sekiro and we can find the game module.
//! Milestone 1: run our own code on every frame via Sekiro's task system.
//! Milestone 2: read the local player's world position.
//! Milestone 3 (current): run libsm64 (Mario) on a flat floor under the player.

mod audio;
mod camera;
mod havok;
mod hit;
mod dmg;
mod hud;
mod light;
mod lakitu;
mod mario;
mod pad;
mod prompt;
mod aim;
mod popup;
mod prof;
mod grab;
mod mem;
mod paths;
mod player;
mod draw;
mod dump;
mod sm64;
mod title;
mod version;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fromsoftware_shared::SharedTaskImpExt;
use sekiro::sprj::{SprjTaskGroupIndex, SprjTaskImp};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW, GetModuleHandleW,
};
use windows::core::PCWSTR;

static MODULE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// The folder this DLL was loaded from (the mod folder).
pub(crate) fn mod_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let mut module = HMODULE::default();
        let mut buf = [0u16; 1024];
        let len = unsafe {
            let anchor = mod_dir as *const () as *const u16;
            let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
            if GetModuleHandleExW(flags, PCWSTR(anchor), &mut module).is_err() {
                return PathBuf::from(".");
            }
            GetModuleFileNameW(Some(module), &mut buf) as usize
        };
        let dll = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
        dll.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
    })
}

/// Append a line to logs/sk_mario.log next to the DLL, with the local time. The first line of a
/// session moves the previous session's log to sk_mario.prev.log.
pub(crate) fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);
    let mut guard = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let dir = mod_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let (log, prev) = (dir.join("sk_mario.log"), dir.join("sk_mario.prev.log"));
        let _ = std::fs::rename(&log, &prev);
        *guard = std::fs::OpenOptions::new().create(true).append(true).open(log).ok();
    }
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(
            file,
            "[{:02}:{:02}:{:02}.{:03}] {}",
            t.wHour,
            t.wMinute,
            t.wSecond,
            t.wMilliseconds,
            msg.as_ref()
        );
        let _ = file.flush();
    }
}

/// Everything after `DllMain` (on its own thread, so the loader can return).
fn start() {
    log(format!("sk-mario {} loaded (DLL base {:#x})", env!("CARGO_PKG_VERSION"), MODULE.load(Ordering::Relaxed)));
    log(format!("mod dir: {}", mod_dir().display()));
    match unsafe { GetModuleHandleW(None) } {
        Ok(main) => log(format!("game main module base: {:#x}", main.0 as usize)),
        Err(e) => log(format!("could not resolve the game main module: {e}")),
    }

    if let Err(e) = version::check() {
        log(format!("unsupported game version ({e}); sk-mario stays off"));
        return;
    }
    log("game version supported: 1.6.0.0");
    title::start();

    std::panic::set_hook(Box::new(|info| log(format!("PANIC: {info}"))));

    // Wait for Sekiro's task system, then register a recurring task. This is the base for
    // everything: Mario, collision, camera and input all run at one of these task points.
    let started = std::time::Instant::now();
    let task = loop {
        match std::panic::catch_unwind(|| SprjTaskImp::wait_for_instance(Duration::from_secs(5))) {
            Ok(Ok(task)) => break task,
            Ok(Err(e)) => log(format!("waiting for the task system ({:.0}s): {e}", started.elapsed().as_secs_f32())),
            Err(_) => {
                log("the task system is unavailable on this build; sk-mario stays off");
                return;
            }
        }
    };
    log("task system ready");

    std::thread::sleep(Duration::from_millis(500));

    // `mod = off` in sk_mario.ini: only the frame counter (the frame rate with the DLL loaded and
    // nothing else of the mod running)
    let off = paths::config("mod").is_some_and(|v| v.eq_ignore_ascii_case("off"));
    if off {
        log("mod = off (sk_mario.ini): nothing runs but the frame counter");
    }

    let handle = task.run_recurring(
        move |_data: &usize| {
            let n = FRAMES.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                log("first frame task fired — we are running code every frame inside Sekiro");
            } else if n % 600 == 0 {
                log(format!("heartbeat: {n} frames"));
                log(prof::report(600));
            }
            if off {
                return;
            }
            let _span = prof::span(prof::FRAME);
            // keep the player pointer fresh, then run Mario (SM64 tick, collision, camera, follow)
            let _ = std::panic::catch_unwind(mario::frame);
        },
        SprjTaskGroupIndex::FrameEnd,
    );
    // Keep the task alive for the process lifetime (dropping the handle would cancel it).
    std::mem::forget(handle);
    if off {
        return;
    }

    // Milestone 6: draw Mario with the game's debug renderer, BEFORE the draw step (like
    // er-mario), so the commands are consumed this frame instead of piling up.
    let draw_handle = task.run_recurring(
        |_data: &usize| {
            let _span = prof::span(prof::DRAW);
            let _ = std::panic::catch_unwind(draw::draw);
        },
        SprjTaskGroupIndex::Draw_Pre,
    );
    std::mem::forget(draw_handle);
    log("frame task registered");

    // Lakitu's camera into the game's at every step from its camera update to drawing (it copies
    // its own back in between), and Sekiro pinned to Mario (weightless, hidden) around the game's
    // physics and drawing.
    for group in [
        SprjTaskGroupIndex::ChrIns_PreBehavior,
        SprjTaskGroupIndex::ChrIns_PrePhysics,
        SprjTaskGroupIndex::ChrIns_PostPhysicsSafe,
        SprjTaskGroupIndex::WorldChrMan_PostPhysics,
        SprjTaskGroupIndex::ChrIns_UpdateDraw_Begin,
        SprjTaskGroupIndex::CameraStep,
        SprjTaskGroupIndex::DrawParamUpdate,
        SprjTaskGroupIndex::Draw_Pre,
    ] {
        let handle = task.run_recurring(
            |_data: &usize| {
                let _span = prof::span(prof::PIN);
                let _ = std::panic::catch_unwind(|| {
                    lakitu::reapply();
                    mario::pin();
                    grab::apply();
                });
            },
            group,
        );
        std::mem::forget(handle);
    }

    // initialise libsm64 from the SM64 ROM (its own worker thread), with its sound and HUD
    lakitu::load_setting();
    mario::start();

    // make queued triangles rasterise filled (see draw.rs)
    draw::install_hook();
    // controller, and the HUD overlay
    pad::install();
    // tutorial / item popups (Y confirms them)
    popup::install();
    // (`hud = off` in sk_mario.ini, in case another overlay - ReShade, SpecialK - conflicts)
    if paths::config("hud").is_some_and(|v| v.eq_ignore_ascii_case("off")) {
        log("hud: off (sk_mario.ini)");
    } else {
        hud::install(MODULE.load(Ordering::Relaxed));
    }

    // a copy of the decrypted game image for offline analysis (only with `dump = on` in the ini,
    // and only when there's none yet)
    let dump = paths::config("dump").is_some_and(|v| v.eq_ignore_ascii_case("on"));
    std::thread::spawn(move || {
        if !dump {
            return;
        }
        std::thread::sleep(Duration::from_secs(5));
        if !mod_dir().join("sek_dump.bin").exists() {
            dump::run();
        }
    });
}

/// # Safety
/// Called by the Windows loader only. `reason == 1` is `DLL_PROCESS_ATTACH`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DllMain(hmodule: usize, reason: u32) -> bool {
    if reason != 1 {
        return true;
    }
    MODULE.store(hmodule, Ordering::Relaxed);
    std::thread::spawn(start);
    true
}
