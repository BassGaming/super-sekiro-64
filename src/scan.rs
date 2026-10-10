//! AOB (array-of-bytes) scanner.
//!
//! We reuse the byte patterns `fromsoftware-rs` uses to locate Elden Ring's `CSEzDraw` draw
//! functions and search Sekiro's `.text` for the same code. If the shared engine emitted similar
//! instructions, we get Sekiro's `draw_line`/`draw_triangle`/... addresses directly.
//!
//! Pattern syntax (as in fromsoftware-rs `mapper-profile.toml`):
//!   * `XX`  a concrete byte
//!   * `?`   any byte
//!   * `$`   a 4-byte little-endian operand we resolve: if it follows `e8`/`e9` (call/jmp rel32),
//!           the captured value is the target address.

use std::sync::atomic::{AtomicBool, Ordering};

use fromsoftware_shared::Program;
use pelite::pe64::Pe;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::log;
use crate::mem::read_bytes;

static DONE: AtomicBool = AtomicBool::new(false);

/// (name, pattern) — the Elden Ring patterns for the immediate debug-draw functions.
const PATTERNS: &[(&str, &str)] = &[
    (
        "cs_ez_draw_draw_line",
        "40 57 48 83 ec 40 48 c7 44 24 20 fe ff ff ff 48 89 5c 24 50 48 89 6c 24 58 48 89 74 24 60 49 8b f0 48 8b fa 48 8b d9 48 8d 69 28",
    ),
    (
        "cs_ez_draw_draw_capsule",
        "48 8b c4 55 48 8d 68 88 48 81 ec 70 01 00 00 0f 29 70 e8 0f 28 35 $ {} 0f 29 78 d8",
    ),
    (
        "cs_ez_draw_draw_sphere",
        "48 8b c4 55 48 8d 68 a1 48 81 ec b0 00 00 00 0f 28 2d $ {} 0f 28 25 $ {} 0f 28 1d $ {}",
    ),
    (
        "cs_ez_draw_draw_wedge",
        "f3 44 0f 11 6c 24 20 41 0f 28 df 4c 8d 45 e0 48 8d 55 90 48 8b cb e8 $ { ' }",
    ),
    (
        "cs_ez_draw_draw_triangle",
        "0f c6 c4 e8 0f c6 d0 d4 0f 29 55 20 ? 8d 55 00 ? 8b ce e8 $ { ' }",
    ),
    (
        "cs_ez_draw_draw_dodecadron",
        "0f 28 df ? 8d 45 07 ? 8d 55 f7 ? 8b cb e8 $ { ' }",
    ),
];

pub fn run() {
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    discover();
}

/// Compiled pattern: bytes (None = wildcard) plus the byte offsets of `$` captures.
struct Compiled {
    bytes: Vec<Option<u8>>,
    captures: Vec<usize>,
}

fn compile(pattern: &str) -> Compiled {
    let mut bytes = Vec::new();
    let mut captures = Vec::new();
    for tok in pattern.split_whitespace() {
        match tok {
            "?" => bytes.push(None),
            "$" => {
                captures.push(bytes.len());
                bytes.extend([None, None, None, None]);
            }
            "{" | "}" | "'" | "{}" | "{'" | "'}" => {}
            hex => {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    bytes.push(Some(b));
                }
            }
        }
    }
    Compiled { bytes, captures }
}

fn scan(text: &[u8], text_va: usize, pat: &Compiled, max: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if pat.bytes.is_empty() || text.len() < pat.bytes.len() {
        return out;
    }
    'outer: for i in 0..=(text.len() - pat.bytes.len()) {
        for (j, b) in pat.bytes.iter().enumerate() {
            if let Some(b) = b
                && text[i + j] != *b
            {
                continue 'outer;
            }
        }
        out.push(text_va + i);
        if out.len() >= max {
            break;
        }
    }
    out
}

fn discover() {
    let program = Program::current();
    let base = unsafe { GetModuleHandleW(None) }.map(|m| m.0 as usize).unwrap_or(0);
    if base == 0 {
        return;
    }
    let Some(text) = program.section_headers().by_name(".text") else { return };
    let range = text.virtual_range();
    let (start, end) = (range.start as usize, range.end as usize);
    let mut bytes = vec![0u8; end - start];
    if !read_bytes(base + start, &mut bytes) {
        log("scan: could not read .text");
        return;
    }
    log(format!("scan: .text {start:#x}..{end:#x} ({} bytes)", bytes.len()));

    for (name, pat) in PATTERNS {
        let compiled = compile(pat);
        let hits = scan(&bytes, base + start, &compiled, 4);
        log(format!("scan {name}: {} hit(s)", hits.len()));
        for va in hits {
            let off = va - (base + start);
            let mut line = format!("   {va:#x} (rva {:#x})", va - base);
            // resolve a call/jmp capture if present
            for &c in &compiled.captures {
                if c >= 1 && (bytes[off + c - 1] == 0xe8 || bytes[off + c - 1] == 0xe9) {
                    let rel = i32::from_le_bytes([
                        bytes[off + c],
                        bytes[off + c + 1],
                        bytes[off + c + 2],
                        bytes[off + c + 3],
                    ]);
                    let target = (va + c - 1) as i64 + 5 + rel as i64;
                    line += &format!("  -> target {:#x} (rva {:#x})", target, target as usize - base);
                }
            }
            log(line);
        }
    }

    DONE.store(true, Ordering::Relaxed);
    log("scan: done");
}
