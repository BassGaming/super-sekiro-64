//! Mutes the Tarnished's own voice (grunts, pain, jump and death cries) in Mario mode: every row of
//! WwiseValueToStrParam_Switch_PlayerVoiceType (the player's voice type -> the sound engine's
//! switch state name) points at a state that doesn't exist, and back when Mario mode ends.

use std::sync::Mutex;

use eldenring::cs::{SoloParamRepository, WwiseValueToStrParam_Switch_PlayerVoiceType as VoiceType};
use fromsoftware_shared::FromStatic;

use crate::log;

const MUTED: &[u8] = b"ErMarioMuted";

/// (row id, original state name)
static ORIGINAL: Mutex<Vec<(u32, [u8; 32])>> = Mutex::new(Vec::new());

fn text(b: &[u8; 32]) -> String {
    String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(32)]).to_string()
}

pub fn mute(on: bool) {
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else { return };
    let mut original = ORIGINAL.lock().unwrap_or_else(|e| e.into_inner());
    if original.is_empty() {
        for id in 0..64u32 {
            if let Some(row) = repo.get::<VoiceType>(id) {
                original.push((id, *row.param_str()));
            }
        }
        log(format!(
            "voice: player voice types {:?}",
            original.iter().map(|(id, s)| format!("{id}={}", text(s))).collect::<Vec<_>>()
        ));
    }
    let mut muted = [0u8; 32];
    muted[..MUTED.len()].copy_from_slice(MUTED);
    for (id, s) in original.iter() {
        if let Some(row) = repo.get_mut::<VoiceType>(*id) {
            row.set_param_str(if on { muted } else { *s });
        }
    }
}
