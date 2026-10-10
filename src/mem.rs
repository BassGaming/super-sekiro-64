//! Safe game-memory reads.
//!
//! All reads go through `ReadProcessMemory`, which returns an error instead of raising an access
//! violation, so a stale/wild pointer can never crash the game.

use std::ffi::c_void;

use fromsoftware_shared::{Program, rtti::vftable_classname};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Threading::GetCurrentProcess;

pub fn read_bytes(addr: usize, buf: &mut [u8]) -> bool {
    let mut read: usize = 0;
    let res = unsafe {
        ReadProcessMemory(
            GetCurrentProcess(),
            addr as *const c_void,
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
            Some(&mut read),
        )
    };
    res.is_ok() && read == buf.len()
}

pub fn read_u64(addr: usize) -> Option<u64> {
    let mut b = [0u8; 8];
    read_bytes(addr, &mut b).then(|| u64::from_le_bytes(b))
}

pub fn read_u32(addr: usize) -> Option<u32> {
    let mut b = [0u8; 4];
    read_bytes(addr, &mut b).then(|| u32::from_le_bytes(b))
}

pub fn read_f32(addr: usize) -> Option<f32> {
    let mut b = [0u8; 4];
    read_bytes(addr, &mut b).then(|| f32::from_le_bytes(b))
}

/// Reads the RTTI class name of the object a pointer points at, if it is a valid object.
pub fn class_name_of(program: &Program, obj: usize) -> Option<String> {
    if obj < 0x10000 {
        return None;
    }
    let vt = read_u64(obj)?;
    vftable_classname(program, vt as usize)
}

/// True if `len` bytes at `addr` can be read (Windows probes a byte of every 64 KB step; cheap when
/// the memory is there). For hot loops over game structures that are read directly afterwards.
pub fn readable(addr: usize, len: usize) -> bool {
    use windows::Win32::System::Memory::IsBadReadPtr;
    if addr < 0x10000 || addr >= 0x8000_0000_0000 {
        return false;
    }
    let probe = |at: usize, n: usize| !unsafe { IsBadReadPtr(Some(at as *const c_void), n) }.as_bool();
    const STEP: usize = 0x10000;
    if len <= STEP {
        probe(addr, len.max(1))
    } else {
        let end = addr.saturating_add(len);
        (addr..end - 8).step_by(STEP).all(|at| probe(at, 8)) && probe(end - 8, 8)
    }
}
