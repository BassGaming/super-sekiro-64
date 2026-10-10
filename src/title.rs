//! The "Super Sekiro 64" title logo. Only the logo art ships, in the DLL (assets/title_logo.dds,
//! made by tools/title/export_logo.py). The game files it goes into are FromSoftware's, so they are
//! patched on the player's PC from their own install: on the first start (and when the logo
//! changes), this reads the title menu files out of Data1.bdt (never modified) and writes patched
//! copies to `sk_mario_title` next to the DLL, which `sk-mario.me3` loads as a package:
//!
//! ```text
//! menu/hi/01_common.tpf.dcx       SB_Title_02 (the S-E-K-I-R-O letters) becomes the logo
//! menu/hi/01_common.sblytbnd.dcx  the logo is MENU_Title_S; the other letters and the kanji
//!                                 point at an empty corner
//! menu/05_001_title_logo.gfx      MENU_Title_S is placed and sized as the whole logo; "TM" moves
//!                                 to its top right corner
//! ```
//! A port of tools/title/build_title.py (the same steps, without Python). The game may read the
//! files before they're written on the very first start; the logo shows from the next one.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::log;

static LOGO_DDS: &[u8] = include_bytes!("assets/title_logo.dds");
/// The logo's size in the texture (its top left corner)
const LOGO_W: u16 = 2048;
const LOGO_H: u16 = 773;
/// Where the logo goes on the 1920 x 1080 title stage (the letters spanned x 372..1541, y
/// 375..580; "SHADOWS DIE TWICE" stays below at y 587)
const LOGO_WIDTH: f64 = 1200.0;
const LOGO_BOTTOM: f64 = 580.0;
/// Transparent corners of SB_Title_02 and SB_Title
const EMPTY: &str = r#"x="2024" y="1000" width="16" height="16""#;
const EMPTY_TITLE: &str = r#"x="1290" y="90" width="16" height="16""#;
/// Written last: the package is up to date when this matches.
const STAMP: &str = "stamp.txt";

/// Data1.bhd's RSA public key (from UXM's ArchiveKeys.cs)
const DATA1_N: &str = "f7697e016c75695ee6cedfbaaf4d1b9bfaa77386fa347dd556bfefe14c4c71fceeb212fc8ec9fd6523f5b2cf79a2dfeaba4f1d389b29d3efdbbf11fe0bd0c47b3ccd2d2aaa0067768ead8f628b23ffa1616002a38cc94a8cdc5c54fb0a42e83465bcbe298b04362ae61177260357f396dac849fe9de8b281d541471cdf4d4642c29b789d1383eef7ce7690f1db88bd7812a7c30838309bb90b79076dde451b726761bbfe9dbcfe361c6c4198060b3450f78807bba8b30b305ce628a1d00f67d140aff8df5677c4f2ba269db8e40751d00c14615be75eafbff7bc0c5f40ba50abc067e6ffd72b4336ef2f66dfbe961274d528d37360388553f3ee0d4410ee7f4d";
const DATA1_E: u32 = 709425447;

/// Builds the package on its own thread if it's missing or out of date.
pub fn start() {
    if crate::paths::config("title").is_some_and(|v| v.eq_ignore_ascii_case("off")) {
        log("title: title = off (sk_mario.ini): no logo package");
        return;
    }
    let out = crate::paths::file("sk_mario_title");
    let stamp = stamp();
    if std::fs::read_to_string(out.join(STAMP)).is_ok_and(|s| s.trim() == stamp) {
        log("title: the logo package is up to date");
        return;
    }
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        match game_dir().and_then(|game| build(&game, &out)) {
            Ok(()) => {
                let _ = std::fs::write(out.join(STAMP), &stamp);
                log(format!("title: logo package written to {} in {:.1} s (shown from the next start if the title was already up)", out.display(), t.elapsed().as_secs_f32()));
            }
            Err(e) => log(format!("title: no logo package ({e}); the title keeps Sekiro's logo")),
        }
    });
}

fn stamp() -> String {
    let sum = LOGO_DDS.iter().fold(0u32, |h, &b| h.wrapping_mul(31).wrapping_add(b as u32));
    format!("logo {sum:08x} {LOGO_W}x{LOGO_H} {LOGO_WIDTH} {LOGO_BOTTOM}")
}

fn game_dir() -> Result<PathBuf, String> {
    use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
    let mut buf = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let exe = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    exe.parent().map(Path::to_path_buf).ok_or_else(|| "no game folder".into())
}

fn build(game: &Path, out: &Path) -> Result<(), String> {
    let game = game.to_path_buf();
    let arc = Archive::open(&game, "Data1")?;
    let tpf = undcx(&arc.read("/menu/hi/01_common.tpf.dcx")?, &game)?;
    let tpf = tpf_replace(&tpf, "SB_Title_02", LOGO_DDS, 5)?;

    let bnd = undcx(&arc.read("/menu/hi/01_common.sblytbnd.dcx")?, &game)?;
    let mut files = bnd_files(&bnd)?;
    let mut hit = 0;
    for f in files.iter_mut() {
        if let Some(new) = patch_layout(&f.1)? {
            f.1 = new;
            hit += 1;
        }
    }
    if hit != 2 {
        return Err(format!("{hit} of 2 title layouts found"));
    }
    let bnd = bnd_write(&bnd, &files)?;
    let gfx = patch_gfx(&arc.read("/menu/05_001_title_logo.gfx")?)?;

    std::fs::create_dir_all(out.join("menu").join("hi")).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(out.join(STAMP));
    for (rel, data) in [("menu/hi/01_common.tpf.dcx", dcx(&tpf)?), ("menu/hi/01_common.sblytbnd.dcx", dcx(&bnd)?), ("menu/05_001_title_logo.gfx", gfx)] {
        let path = out.join(rel);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &data).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

// ---- little-endian helpers ---------------------------------------------------------------------

fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("short read at {o:#x}"))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("short read at {o:#x}"))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| format!("short read at {o:#x}"))
}
fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

// ---- archives ----------------------------------------------------------------------------------

struct Archive {
    bhd: Vec<u8>,
    bdt: PathBuf,
}

impl Archive {
    /// The decrypted index (each 256-byte RSA block holds 255 bytes).
    fn open(game: &Path, name: &str) -> Result<Archive, String> {
        use num_bigint::BigUint;
        let raw = std::fs::read(game.join(format!("{name}.bhd"))).map_err(|e| format!("{name}.bhd: {e}"))?;
        let n = BigUint::parse_bytes(DATA1_N.as_bytes(), 16).ok_or("bad key")?;
        let e = BigUint::from(DATA1_E);
        let mut bhd = Vec::with_capacity(raw.len());
        for block in raw.chunks(256) {
            let m = BigUint::from_bytes_be(block).modpow(&e, &n).to_bytes_be();
            if m.len() > 255 {
                return Err(format!("{name}.bhd did not decrypt"));
            }
            bhd.extend(std::iter::repeat_n(0u8, 255 - m.len()));
            bhd.extend(m);
        }
        if !bhd.starts_with(b"BHD5") {
            return Err(format!("{name}.bhd did not decrypt"));
        }
        Ok(Archive { bhd, bdt: game.join(format!("{name}.bdt")) })
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let d = &self.bhd;
        let want = path.to_lowercase().bytes().fold(0u32, |h, c| h.wrapping_mul(37).wrapping_add(c as u32));
        let (buckets, at) = (u32_at(d, 0x10)? as usize, u32_at(d, 0x14)? as usize);
        for b in 0..buckets {
            let (count, headers) = (u32_at(d, at + b * 8)? as usize, u32_at(d, at + b * 8 + 4)? as usize);
            for f in 0..count {
                let h = headers + f * 0x28;
                if u32_at(d, h)? != want {
                    continue;
                }
                let padded = u32_at(d, h + 4)? as usize;
                let (off, aes, size) = (u64_at(d, h + 8)?, u64_at(d, h + 0x18)? as usize, u64_at(d, h + 0x20)? as i64);
                let mut data = vec![0u8; padded];
                let mut fh = std::fs::File::open(&self.bdt).map_err(|e| e.to_string())?;
                fh.seek(SeekFrom::Start(off)).and_then(|_| fh.read_exact(&mut data)).map_err(|e| format!("{path}: {e}"))?;
                if aes != 0 {
                    use aes::Aes128;
                    use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
                    let key = d.get(aes..aes + 16).ok_or("bad AES key")?;
                    let cipher = Aes128::new(GenericArray::from_slice(key));
                    for r in 0..u32_at(d, aes + 16)? as usize {
                        let (s, e) = (u64_at(d, aes + 20 + r * 16)? as i64, u64_at(d, aes + 28 + r * 16)? as i64);
                        if s < 0 || e <= s || e as usize > data.len() {
                            continue;
                        }
                        for block in data[s as usize..e as usize].chunks_exact_mut(16) {
                            cipher.decrypt_block(GenericArray::from_mut_slice(block));
                        }
                    }
                }
                if size > 0 {
                    data.truncate(size as usize);
                }
                return Ok(data);
            }
        }
        Err(format!("{path} not in the archive"))
    }
}

// ---- DCX ---------------------------------------------------------------------------------------

fn undcx(b: &[u8], game: &Path) -> Result<Vec<u8>, String> {
    if !b.starts_with(b"DCX\0") || b.len() < 0x4C {
        return Err("not a DCX file".into());
    }
    let be = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let (usize_, csize) = (be(0x1C), be(0x20));
    let body = b.get(0x4C..0x4C + csize).ok_or("short DCX")?;
    if &b[0x28..0x2C] == b"DFLT" {
        let mut out = Vec::with_capacity(usize_);
        flate2::read::ZlibDecoder::new(body).read_to_end(&mut out).map_err(|e| format!("DFLT: {e}"))?;
        return Ok(out);
    }
    // Kraken: the game's own Oodle
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{HSTRING, s};
    let dll = unsafe { LoadLibraryW(&HSTRING::from(game.join("oo2core_6_win64.dll").as_os_str())) }.map_err(|e| format!("Oodle: {e}"))?;
    let f = unsafe { GetProcAddress(dll, s!("OodleLZ_Decompress")) }.ok_or("no OodleLZ_Decompress")?;
    type Decompress = unsafe extern "system" fn(*const u8, i64, *mut u8, i64, i32, i32, i32, *mut u8, i64, *mut u8, *mut u8, *mut u8, i64, i32) -> i64;
    let f: Decompress = unsafe { std::mem::transmute(f) };
    let mut out = vec![0u8; usize_];
    let n = unsafe { f(body.as_ptr(), body.len() as i64, out.as_mut_ptr(), usize_ as i64, 0, 0, 0, std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0, 3) };
    if n as usize != usize_ {
        return Err("Oodle failed".into());
    }
    Ok(out)
}

/// DFLT: Sekiro reads it as well as Kraken.
fn dcx(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(data).map_err(|e| e.to_string())?;
    let z = z.finish().map_err(|e| e.to_string())?;
    let mut h = Vec::with_capacity(0x4C + z.len());
    h.extend(b"DCX\0");
    h.extend([0x00, 0x01, 0x10, 0x00]);
    for v in [0x18u32, 0x24, 0x44, 0x4C] {
        h.extend(v.to_be_bytes());
    }
    h.extend(b"DCS\0");
    h.extend((data.len() as u32).to_be_bytes());
    h.extend((z.len() as u32).to_be_bytes());
    h.extend(b"DCP\0DFLT");
    h.extend(0x20u32.to_be_bytes());
    h.extend([9, 0, 0, 0]);
    h.extend([0u8; 12]);
    h.extend(0x0001_0100u32.to_be_bytes());
    h.extend(b"DCA\0");
    h.extend(8u32.to_be_bytes());
    debug_assert_eq!(h.len(), 0x4C);
    h.extend(z);
    Ok(h)
}

// ---- TPF ---------------------------------------------------------------------------------------

/// (entry offset, data offset, size, name)
fn tpf_entries(t: &[u8]) -> Result<Vec<(usize, usize, usize, String)>, String> {
    let count = u32_at(t, 8)? as usize;
    (0..count)
        .map(|i| {
            let o = 0x10 + i * 20;
            let (off, size, name_at) = (u32_at(t, o)? as usize, u32_at(t, o + 4)? as usize, u32_at(t, o + 12)? as usize);
            let mut e = name_at;
            while t.get(e..e + 2).ok_or("TPF name")? != [0, 0] {
                e += 2;
            }
            let utf16: Vec<u16> = t[name_at..e].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok((o, off, size, String::from_utf16_lossy(&utf16)))
        })
        .collect()
}

fn tpf_replace(t: &[u8], name: &str, dds: &[u8], fmt: u8) -> Result<Vec<u8>, String> {
    let entries = tpf_entries(t)?;
    if !entries.iter().any(|e| e.3 == name) {
        return Err(format!("{name} not in the TPF"));
    }
    let mut head = t[..entries.first().ok_or("empty TPF")?.1].to_vec();
    let mut blob = Vec::new();
    for (o, off, size, n) in &entries {
        let data = if n == name { dds } else { t.get(*off..off + size).ok_or("TPF data")? };
        let at = (head.len() + blob.len()) as u32;
        put_u32(&mut head, *o, at);
        put_u32(&mut head, o + 4, data.len() as u32);
        if n == name {
            head[o + 8] = fmt;
            head[o + 10] = 1; // mip levels
        }
        blob.extend_from_slice(data);
    }
    let len = blob.len() as u32;
    put_u32(&mut head, 4, len);
    head.extend(blob);
    Ok(head)
}

// ---- BND4 --------------------------------------------------------------------------------------

/// (entry header offset, data)
fn bnd_files(b: &[u8]) -> Result<Vec<(usize, Vec<u8>)>, String> {
    let (count, size) = (u32_at(b, 0xC)? as usize, u64_at(b, 0x20)? as usize);
    (0..count)
        .map(|i| {
            let o = 0x40 + i * size;
            let (len, off) = (u64_at(b, o + 8)? as usize, u32_at(b, o + 24)? as usize);
            Ok((o, b.get(off..off + len).ok_or("BND data")?.to_vec()))
        })
        .collect()
}

fn bnd_write(b: &[u8], files: &[(usize, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let data_at = u64_at(b, 0x28)? as usize;
    let mut head = b.get(..data_at).ok_or("BND header")?.to_vec();
    let mut body = Vec::new();
    for (o, data) in files {
        let at = data_at + body.len();
        put_u64(&mut head, o + 8, data.len() as u64);
        put_u64(&mut head, o + 16, data.len() as u64);
        put_u32(&mut head, o + 24, at as u32);
        body.extend_from_slice(data);
        body.resize(body.len().div_ceil(16) * 16, 0);
    }
    head.extend(body);
    Ok(head)
}

// ---- layout ------------------------------------------------------------------------------------

/// Points every `<SubTexture name="{name}.png" .../>` at `attrs`.
fn sub(s: &mut String, name: &str, attrs: &str, half: &str) -> Result<(), String> {
    let start = format!(r#"<SubTexture name="{name}.png""#);
    let mut from = 0;
    let mut found = false;
    while let Some(i) = s[from..].find(&start).map(|i| i + from) {
        let Some(len) = s[i..].find("/>") else { break };
        if s[i..i + len].contains('/') {
            from = i + start.len();
            continue;
        }
        let new = format!(r#"<SubTexture name="{name}.png" {attrs} half="{half}"/>"#);
        s.replace_range(i..i + len + 2, &new);
        from = i + new.len();
        found = true;
    }
    if found { Ok(()) } else { Err(format!("{name} not in the layout")) }
}

fn patch_layout(xml: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let Ok(s) = std::str::from_utf8(xml) else { return Ok(None) };
    let mut s = s.to_string();
    if s.contains(r#"imagePath="SB_Title_02.png""#) {
        sub(&mut s, "MENU_Title_S", &format!(r#"x="0" y="0" width="{LOGO_W}" height="{LOGO_H}""#), "0")?;
        for c in ["E", "K", "I", "R", "O"] {
            sub(&mut s, &format!("MENU_Title_{c}"), EMPTY, "0")?;
        }
    } else if s.contains(r#"imagePath="SB_Title.png""#) {
        sub(&mut s, "MENU_Title_SEKIRO", EMPTY_TITLE, "1")?;
    } else {
        return Ok(None);
    }
    Ok(Some(s.into_bytes()))
}

// ---- GFX (Scaleform SWF) -----------------------------------------------------------------------

struct Bits<'a> {
    b: &'a [u8],
    p: usize,
}

impl Bits<'_> {
    fn u(&mut self, n: u32) -> Result<u64, String> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.b.get(self.p >> 3).ok_or("GFX bits")?;
            v = (v << 1) | ((byte >> (7 - (self.p & 7))) & 1) as u64;
            self.p += 1;
        }
        Ok(v)
    }
    fn end(&self) -> usize {
        self.p.div_ceil(8)
    }
}

fn skip_matrix(b: &[u8], p: usize) -> Result<usize, String> {
    let mut r = Bits { b, p: p * 8 };
    for _ in 0..2 {
        if r.u(1)? == 1 {
            let n = r.u(5)? as u32;
            r.u(2 * n)?;
        }
    }
    let n = r.u(5)? as u32;
    r.u(2 * n)?;
    Ok(r.end())
}

fn matrix(scale: f64, tx: f64, ty: f64) -> Vec<u8> {
    let sbits = |v: i64| -> u32 { (64 - (if v >= 0 { v } else { !v }).leading_zeros() + 1).max(1) };
    let s = (scale * 65536.0).round() as i64;
    let (x, y) = ((tx * 20.0).round() as i64, (ty * 20.0).round() as i64);
    let ns = sbits(s).max(2);
    let nt = sbits(x).max(sbits(y));
    let mask = |v: i64, n: u32| (v as u64) & ((1u64 << n) - 1);
    let vals = [(1, 1), (ns as u64, 5), (mask(s, ns), ns), (mask(s, ns), ns), (0, 1), (nt as u64, 5), (mask(x, nt), nt), (mask(y, nt), nt)];
    let mut bits: Vec<bool> = Vec::new();
    for (v, n) in vals {
        for k in (0..n).rev() {
            bits.push((v >> k) & 1 == 1);
        }
    }
    bits.resize(bits.len().div_ceil(8) * 8, false);
    bits.chunks(8).map(|c| c.iter().fold(0u8, |a, &bit| (a << 1) | bit as u8)).collect()
}

/// (code, tag start, body start, body length)
fn gfx_tags(b: &[u8]) -> Result<Vec<(u16, usize, usize, usize)>, String> {
    let mut r = Bits { b, p: 64 };
    let n = r.u(5)? as u32;
    r.u(4 * n)?;
    let mut p = r.end() + 4;
    let mut out = Vec::new();
    while p < b.len() {
        let h = u16_at(b, p)?;
        let (code, mut ln, mut q) = (h >> 6, (h & 63) as usize, p + 2);
        if ln == 63 {
            ln = u32_at(b, q)? as usize;
            q += 4;
        }
        out.push((code, p, q, ln));
        p = q + ln;
        if code == 0 {
            break;
        }
    }
    Ok(out)
}

fn tag(code: u16, body: &[u8]) -> Vec<u8> {
    let mut t = Vec::with_capacity(body.len() + 6);
    if body.len() < 63 {
        t.extend((code << 6 | body.len() as u16).to_le_bytes());
    } else {
        t.extend((code << 6 | 63).to_le_bytes());
        t.extend((body.len() as u32).to_le_bytes());
    }
    t.extend_from_slice(body);
    t
}

fn patch_gfx(src: &[u8]) -> Result<Vec<u8>, String> {
    let mut b = src.to_vec();
    let scale = LOGO_WIDTH / LOGO_W as f64;
    let x0 = 960.0 - LOGO_WIDTH / 2.0;
    let y0 = LOGO_BOTTOM - LOGO_H as f64 * scale;
    let mut s_id = None;
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for (code, p, q, ln) in gfx_tags(src)? {
        if code == 1009 {
            // DefineExternalImage2: id, format, width, height, export name
            let cid = u32_at(&b, q)?;
            let len = *b.get(q + 10).ok_or("GFX")? as usize;
            if b.get(q + 11..q + 11 + len) == Some(b"MENU_Title_S".as_slice()) {
                b[q + 6..q + 8].copy_from_slice(&LOGO_W.to_le_bytes());
                b[q + 8..q + 10].copy_from_slice(&LOGO_H.to_le_bytes());
                s_id = Some(cid);
            }
        } else if (code == 26 || code == 70) && b.get(q) == Some(&0x06) {
            // PlaceObject2/3 with a character and a matrix: flags (+flags2), depth
            let fixed = if code == 26 { 3 } else { 4 };
            if code == 70 && b[q + 1] & 0x08 != 0 {
                continue; // a class name before the character
            }
            let depth = u16_at(&b, q + fixed - 2)?;
            let cid = u16_at(&b, q + fixed)? as u32;
            let end = skip_matrix(&b, q + fixed + 2)?;
            let m = if code == 70 && Some(cid) == s_id {
                matrix(scale, x0, y0)
            } else if code == 26 && depth == 13 {
                matrix(1.0, x0 + LOGO_WIDTH - 20.0, y0 - 10.0) // the "TM" text
            } else {
                continue;
            };
            let mut body = b[q..q + fixed + 2].to_vec();
            body.extend(m);
            body.extend_from_slice(b.get(end..q + ln).ok_or("GFX tag")?);
            edits.push((p, q + ln, tag(code, &body)));
        }
    }
    if s_id.is_none() || edits.len() != 2 {
        return Err(format!("title gfx layout not recognised ({s_id:?}, {} edits)", edits.len()));
    }
    for (start, end, new) in edits.into_iter().rev() {
        b.splice(start..end, new);
    }
    let len = b.len() as u32;
    put_u32(&mut b, 4, len);
    Ok(b)
}

#[cfg(test)]
mod tests {
    /// Builds the package from a Sekiro install (SEKIRO_DIR) and compares it with the one
    /// tools/title/build_title.py wrote (TITLE_REF), decompressed.
    #[test]
    #[ignore]
    fn same_as_python() {
        let game = std::path::PathBuf::from(std::env::var("SEKIRO_DIR").unwrap());
        let reference = std::path::PathBuf::from(std::env::var("TITLE_REF").unwrap());
        let out = std::env::temp_dir().join("sk_mario_title_test");
        super::build(&game, &out).unwrap();
        for rel in ["menu/hi/01_common.tpf.dcx", "menu/hi/01_common.sblytbnd.dcx", "menu/05_001_title_logo.gfx"] {
            let (a, b) = (std::fs::read(out.join(rel)).unwrap(), std::fs::read(reference.join(rel)).unwrap());
            let un = |d: Vec<u8>| if d.starts_with(b"DCX ") { super::undcx(&d, &game).unwrap() } else { d };
            assert!(un(a) == un(b), "{rel} differs");
        }
    }
}
