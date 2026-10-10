//! Dumps the running (Arxan-decrypted) game image to disk so it can be analysed in Ghidra.
//!
//! Import the result in Ghidra as a raw binary at base 0x140000000; virtual addresses in the dump
//! equal the game's runtime addresses.

use std::sync::atomic::{AtomicBool, Ordering};

use fromsoftware_shared::Program;
use pelite::pe64::Pe;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::log;
use crate::mem::read_bytes;

static DONE: AtomicBool = AtomicBool::new(false);

pub fn run() {
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let program = Program::current();
    let base = unsafe { GetModuleHandleW(None) }.map(|m| m.0 as usize).unwrap_or(0);
    if base == 0 {
        return;
    }
    let size = program.optional_header().SizeOfImage as usize;
    log(format!("dump: image {base:#x}..{:#x} ({size} bytes)", base + size));

    // Read in chunks so an unreadable page doesn't lose the whole image.
    const CHUNK: usize = 0x10000;
    let mut buf = vec![0u8; size];
    let mut ok = 0usize;
    let mut off = 0usize;
    while off < size {
        let len = CHUNK.min(size - off);
        if read_bytes(base + off, &mut buf[off..off + len]) {
            ok += len;
        }
        off += len;
    }
    log(format!("dump: read {ok}/{size} bytes"));

    let path = crate::paths::file("sek_dump.bin");
    match std::fs::write(&path, &buf) {
        Ok(()) => log(format!("dump: wrote {}", path.display())),
        Err(e) => log(format!("dump: write failed: {e}")),
    }
}
