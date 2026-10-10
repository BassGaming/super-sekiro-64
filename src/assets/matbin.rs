//! MATBIN material edits.

fn i64_at(d: &[u8], o: usize) -> usize {
    i64::from_le_bytes(d[o..o + 8].try_into().unwrap()) as usize
}

/// Turns off the detail-blend grime: the detail texture slots ("AAT" paths) get the empty path
/// of an unused slot, so the shader falls back to its defaults.
pub fn clear_detail(b: &mut [u8]) {
    let (params, samplers) = (u32::from_le_bytes(b[0x1C..0x20].try_into().unwrap()) as usize, u32::from_le_bytes(b[0x20..0x24].try_into().unwrap()) as usize);
    let base = 0x38 + params * 0x28;
    let path = |b: &[u8], i: usize| i64_at(b, base + i * 0x30 + 8);
    let text = |b: &[u8], o: usize| super::bnd4::wstr(b, o).unwrap_or_default();
    let Some(empty) = (0..samplers).map(|i| path(b, i)).filter(|&o| text(b, o).is_empty()).last() else { return };
    for i in 0..samplers {
        if text(b, path(b, i)).contains("AAT") {
            let a = base + i * 0x30 + 8;
            b[a..a + 8].copy_from_slice(&(empty as i64).to_le_bytes());
        }
    }
}
