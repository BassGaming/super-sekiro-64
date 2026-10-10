//! What the mod costs the game per frame: each hook on the game's threads times itself with
//! `span`, and the heartbeat logs the frame rate with every part's average and slowest run.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const FRAME: usize = 0;
pub const DRAW: usize = 1;
pub const PIN: usize = 2;
pub const PRESENT: usize = 3;
pub const OVERLAY: usize = 4;
pub const PAD: usize = 5;
pub const WORKER_WAIT: usize = 6;
const NAMES: [&str; 7] = ["frame task", "draw task", "pin tasks (8 a frame)", "light at Present", "overlay", "pad hook", "waiting for libsm64"];

static TOTAL: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static WORST: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static COUNT: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

pub struct Span(usize, Instant);

pub fn span(part: usize) -> Span {
    Span(part, Instant::now())
}

impl Drop for Span {
    fn drop(&mut self) {
        let ns = self.1.elapsed().as_nanos() as u64;
        TOTAL[self.0].fetch_add(ns, Ordering::Relaxed);
        WORST[self.0].fetch_max(ns, Ordering::Relaxed);
        COUNT[self.0].fetch_add(1, Ordering::Relaxed);
    }
}

/// "44.1 fps (22.7 ms a frame); per frame: frame task 0.31 ms (worst 2.1), ..." since the last call.
pub fn report(frames: u64) -> String {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let secs = last.replace(now).map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0);
    let fps = if secs > 0.0 { frames as f32 / secs } else { 0.0 };
    let parts: Vec<String> = (0..NAMES.len())
        .map(|i| {
            let total = TOTAL[i].swap(0, Ordering::Relaxed) as f32 / 1e6;
            let worst = WORST[i].swap(0, Ordering::Relaxed) as f32 / 1e6;
            let n = COUNT[i].swap(0, Ordering::Relaxed);
            format!("{} {:.2} ms (worst {worst:.1}, {:.1} runs)", NAMES[i], total / frames.max(1) as f32, n as f32 / frames.max(1) as f32)
        })
        .collect();
    format!("perf: {fps:.1} fps ({:.1} ms a frame); per frame: {}", if fps > 0.0 { 1000.0 / fps } else { 0.0 }, parts.join(", "))
}
