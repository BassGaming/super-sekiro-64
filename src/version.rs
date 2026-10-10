//! Game version check.
//!
//! Our RVAs (fromsoftware-rs) and, later, our byte patterns are for one Sekiro build. On any
//! other version the mod must stay completely out of the way (like er-mario does for Elden Ring).

use fromsoftware_shared::game_version::{GameVersion, LANG_ID_EN, LANG_ID_JP};
use pelite::pe64::PeView;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

/// Sekiro: Shadows Die Twice, app version 1.06 (executable 1.6.0.0), worldwide or Japanese.
pub struct Supported;

impl GameVersion for Supported {
    // `GameVersion::detect` compares against `normalize(product)`, which strips the trademark and
    // colon from "Sekiro™: Shadows Die Twice" -> "sekiro shadows die twice". It must be this
    // normalized form (no colon), or detection fails.
    const NAME: &'static str = "sekiro shadows die twice";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        matches!((lang_id, version), (LANG_ID_EN, "1.6.0.0") | (LANG_ID_JP, "1.6.0.0")).then_some(Supported)
    }
}

/// Ok, or why the mod can't run on this game.
pub fn check() -> Result<(), String> {
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| e.to_string())?;
    let view = unsafe { PeView::module(module.0 as *const u8) };
    std::panic::catch_unwind(|| Supported::detect(&view).map(|_| ()).map_err(|e| e.to_string()))
        .unwrap_or_else(|_| Err("could not read the game's version".into()))
}
