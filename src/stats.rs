//! SM64's counters for a playthrough, per character (by name): deaths (the HUD's lives), coins
//! and stars (bosses). Kept in er_mario_stats.txt in the mod folder.

use std::sync::Mutex;

use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;

const FILE: &str = "er_mario_stats.txt";

#[derive(Clone, Default)]
pub struct Stats {
    pub deaths: u32,
    pub coins: u32,
    pub stars: u32,
}

static CURRENT: Mutex<Option<(String, Stats)>> = Mutex::new(None);

fn character() -> Option<String> {
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let name = &gdm.main_player_game_data.character_name;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let s = String::from_utf16_lossy(&name[..len]).replace(['\t', '\n', '\r'], " ");
    (!s.is_empty()).then_some(s)
}

fn read_all() -> Vec<(String, Stats)> {
    let text = std::fs::read_to_string(crate::paths::file(FILE)).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let n = |i: usize| f.get(i).and_then(|v| v.trim().parse().ok());
            Some((f.first()?.to_string(), Stats { deaths: n(1)?, coins: n(2)?, stars: n(3)? }))
        })
        .collect()
}

fn save(name: &str, stats: &Stats) {
    let mut all = read_all();
    match all.iter_mut().find(|(n, _)| n == name) {
        Some((_, s)) => *s = stats.clone(),
        None => all.push((name.to_string(), stats.clone())),
    }
    let text: String = all.iter().map(|(n, s)| format!("{n}\t{}\t{}\t{}\n", s.deaths, s.coins, s.stars)).collect();
    let _ = std::fs::write(crate::paths::file(FILE), text);
}

/// The current character's counters (loaded when the character changes).
pub fn get() -> Stats {
    let Some(name) = character() else { return Stats::default() };
    let mut cur = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if cur.as_ref().is_none_or(|(n, _)| *n != name) {
        let stats = read_all().into_iter().find(|(n, _)| *n == name).map(|(_, s)| s).unwrap_or_default();
        *cur = Some((name, stats));
    }
    cur.as_ref().map(|(_, s)| s.clone()).unwrap_or_default()
}

/// Changes and saves the current character's counters.
pub fn update(f: impl FnOnce(&mut Stats)) {
    get();
    let mut cur = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((name, stats)) = cur.as_mut() {
        f(stats);
        save(name, stats);
    }
}
