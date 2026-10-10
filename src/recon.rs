//! Reconnaissance for rendering: disassemble Sekiro's `SprjEzDraw` virtual methods so we can
//! identify `draw_triangle` / `draw_line` / etc. (the game's immediate debug renderer).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use fromsoftware_shared::FromStatic;
use iced_x86::{Decoder, DecoderOptions, Formatter, NasmFormatter};

use crate::log;
use crate::mem::{read_bytes, read_u64};

#[repr(C)]
#[fromsoftware_shared::singleton("RendMan")]
struct RendMan {
    _vftable: usize,
}

static CALLS: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn run() {
    if CALLS.fetch_add(1, Ordering::Relaxed) % 120 != 0 || DONE.load(Ordering::Relaxed) {
        return;
    }
    discover();
}

fn disasm(addr: usize, count: usize) {
    let mut bytes = [0u8; 128];
    if !read_bytes(addr, &mut bytes) {
        return;
    }
    let mut dec = Decoder::with_ip(64, &bytes, addr as u64, DecoderOptions::NONE);
    let mut fmt = NasmFormatter::new();
    let mut out = String::new();
    for _ in 0..count {
        if !dec.can_decode() {
            break;
        }
        let instr = dec.decode();
        out.clear();
        fmt.format(&instr, &mut out);
        log(format!("       {:#x}: {out}", instr.ip()));
    }
}

fn discover() {
    let Ok(rm) = <RendMan as FromStatic>::instance_ptr() else { return };
    if rm.is_null() {
        return;
    }
    let rm = rm as usize;
    let Some(ez) = read_u64(rm + 0x30).map(|p| p as usize) else { return };
    if ez < 0x10000 {
        return;
    }
    log(format!("SprjEzDraw @ {ez:#x}"));
    let Some(vt) = read_u64(ez).map(|p| p as usize) else { return };
    log(format!("vtable @ {vt:#x}"));
    for i in 0..8usize {
        let Some(f) = read_u64(vt + i * 8).map(|p| p as usize) else { break };
        if f < 0x10000 {
            break;
        }
        log(format!("vt[{i}] {f:#x}:"));
        disasm(f, 16);
    }
    DONE.store(true, Ordering::Relaxed);
    log("recon: ez-draw disassembly done");
}
