//! Sekiro's action prompts ("Pick up", "Open", "Talk"...): which one is on screen (logged with
//! Mario's action presses).
//!
//! `SprjActionButtonMan` (FD4Singleton, instance at [0x143D8D338]) keeps its entries in three
//! lists, from its debug menu (0x140EA41A0 / 0x140EA5480: "ENABLE", "DISABLE", "CURRENT"):
//! +0x10 enabled, +0x18 disabled after use, +0x20 current (the prompt shown). Nodes link through
//! +0x00; +0x0C is the ActionButtonParam row, +0x10 its unique id.

use std::sync::atomic::{AtomicI32, Ordering};

use crate::mem::{read_u32, read_u64};

const ACTION_BUTTON_MAN: usize = 0x143D8D338;
const CURRENT: usize = 0x20;
const NODE_PARAM: usize = 0x0C;

/// The ActionButtonParam row of the prompt on screen, if there is one.
pub fn current() -> Option<i32> {
    let man = read_u64(ACTION_BUTTON_MAN).filter(|&m| m >= 0x10000)? as usize;
    let node = read_u64(man + CURRENT).filter(|&n| n >= 0x10000)? as usize;
    let id = read_u32(node + NODE_PARAM)? as i32;
    static LAST: AtomicI32 = AtomicI32::new(-1);
    static LOGGED: AtomicI32 = AtomicI32::new(0);
    if LAST.swap(id, Ordering::Relaxed) != id && LOGGED.fetch_add(1, Ordering::Relaxed) < 40 {
        crate::log(format!("prompt: action button {id} on screen"));
    }
    Some(id)
}
