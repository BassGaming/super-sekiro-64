//! Reads vanilla files out of Elden Ring's archives (DataN.bhd/.bdt, DLC): each .bhd index is RSA
//! encrypted with a public key the game keeps in memory, each file's sensitive ranges are AES-128
//! ECB encrypted with a key from the index. Paths are hashed (lowercase, "/" separated, x*0x85 + c).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aes::Aes128;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use base64::Engine;
use num_bigint::BigUint;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};

const ARCHIVES: [&str; 5] = ["Data0", "Data1", "Data2", "Data3", "DLC"];

struct RsaKey {
    n: BigUint,
    e: BigUint,
}

struct Entry {
    bdt: PathBuf,
    offset: u64,
    padded: usize,
    size: usize,
    aes: Option<([u8; 16], Vec<(i64, i64)>)>,
}

pub struct Archives {
    entries: std::collections::HashMap<u64, Entry>,
}

pub fn path_hash(path: &str) -> u64 {
    let mut p = path.to_lowercase().replace('\\', "/");
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    p.bytes().fold(0u64, |h, c| h.wrapping_mul(0x85).wrapping_add(c as u64))
}

/// The folder eldenring.exe runs from.
pub fn game_dir() -> PathBuf {
    let mut buf = [0u16; 1024];
    let len = unsafe { windows::Win32::System::LibraryLoader::GetModuleFileNameW(None, &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..len])).parent().map(Path::to_path_buf).unwrap_or_default()
}

/// The archive keys, found as PEM text in the running game's image.
fn find_keys() -> Vec<RsaKey> {
    const BEGIN: &[u8] = b"-----BEGIN RSA PUBLIC KEY-----";
    const END: &[u8] = b"-----END RSA PUBLIC KEY-----";
    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else { return Vec::new() };
    let base = module.0 as usize;
    let size = unsafe {
        let pe = base + *((base + 0x3C) as *const u32) as usize;
        *((pe + 0x18 + 0x38) as *const u32) as usize
    };
    let mut keys = Vec::new();
    let mut addr = base;
    while addr < base + size {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        if unsafe { VirtualQuery(Some(addr as *const _), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let (start, len) = (info.BaseAddress as usize, info.RegionSize);
        let readable = info.State == MEM_COMMIT && info.Protect.0 & (PAGE_NOACCESS.0 | PAGE_GUARD.0) == 0 && info.Protect.0 != 0;
        if readable {
            let region = unsafe { std::slice::from_raw_parts(start as *const u8, len) };
            let mut at = 0;
            while let Some(i) = find(&region[at..], BEGIN) {
                let begin = at + i;
                let Some(j) = find(&region[begin..], END) else { break };
                let pem = String::from_utf8_lossy(&region[begin + BEGIN.len()..begin + j]).to_string();
                if let Some(k) = parse_pem(&pem) {
                    keys.push(k);
                }
                at = begin + j;
            }
        }
        addr = start + len.max(0x1000);
    }
    keys
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// PKCS#1 RSAPublicKey: SEQUENCE { INTEGER n, INTEGER e }.
fn parse_pem(body: &str) -> Option<RsaKey> {
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let mut p = 0usize;
    let read_len = |der: &[u8], p: &mut usize| -> Option<usize> {
        let first = *der.get(*p)?;
        *p += 1;
        if first < 0x80 {
            return Some(first as usize);
        }
        let n = (first & 0x7F) as usize;
        let mut len = 0usize;
        for _ in 0..n {
            len = len << 8 | *der.get(*p)? as usize;
            *p += 1;
        }
        Some(len)
    };
    (der.get(p) == Some(&0x30)).then_some(())?;
    p += 1;
    read_len(&der, &mut p)?;
    let int = |der: &[u8], p: &mut usize| -> Option<BigUint> {
        (der.get(*p) == Some(&0x02)).then_some(())?;
        *p += 1;
        let len = read_len(der, p)?;
        let v = BigUint::from_bytes_be(der.get(*p..*p + len)?);
        *p += len;
        Some(v)
    };
    let n = int(&der, &mut p)?;
    let e = int(&der, &mut p)?;
    Some(RsaKey { n, e })
}

/// Raw RSA with the public key, block by block (the index was "encrypted" with the private key).
fn rsa_decrypt(key: &RsaKey, data: &[u8], only_first_block: bool) -> Vec<u8> {
    let k = (key.n.bits() as usize).div_ceil(8);
    let mut out = Vec::with_capacity(data.len());
    for block in data.chunks(k) {
        let m = BigUint::from_bytes_be(block).modpow(&key.e, &key.n).to_bytes_be();
        let width = k - 1;
        out.extend(std::iter::repeat_n(0u8, width.saturating_sub(m.len())));
        out.extend_from_slice(&m[m.len().saturating_sub(width)..]);
        if only_first_block {
            break;
        }
    }
    out
}

/// Decrypts in parallel (a few seconds for the big indexes).
fn rsa_decrypt_parallel(key: &RsaKey, data: &[u8]) -> Vec<u8> {
    let k = (key.n.bits() as usize).div_ceil(8);
    let blocks = data.len().div_ceil(k);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16);
    let per = blocks.div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let lo = (t * per * k).min(data.len());
                let hi = ((t + 1) * per * k).min(data.len());
                s.spawn(move || rsa_decrypt(key, &data[lo..hi], false))
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

fn i32_at(d: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn i64_at(d: &[u8], o: usize) -> Option<i64> {
    Some(i64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

impl Archives {
    pub fn open() -> Result<Self, String> {
        let keys = find_keys();
        if keys.is_empty() {
            return Err("archive keys not found in the game".into());
        }
        let dir = game_dir();
        let mut entries = std::collections::HashMap::new();
        for name in ARCHIVES {
            let bhd = dir.join(format!("{name}.bhd"));
            let Ok(data) = std::fs::read(&bhd) else { continue };
            let Some(key) = keys.iter().find(|k| rsa_decrypt(k, &data, true).starts_with(b"BHD5")) else {
                crate::log(format!("assets: no key opens {name}.bhd"));
                continue;
            };
            let index = rsa_decrypt_parallel(key, &data);
            Self::parse(&index, &dir.join(format!("{name}.bdt")), &mut entries).ok_or(format!("{name}.bhd is damaged"))?;
        }
        crate::log(format!("assets: {} archive entries", entries.len()));
        Ok(Self { entries })
    }

    fn parse(d: &[u8], bdt: &Path, entries: &mut std::collections::HashMap<u64, Entry>) -> Option<()> {
        let (bucket_count, buckets) = (i32_at(d, 16)? as usize, i32_at(d, 20)? as usize);
        for b in 0..bucket_count {
            let (n, off) = (i32_at(d, buckets + b * 8)? as usize, i32_at(d, buckets + b * 8 + 4)? as usize);
            for i in 0..n {
                let e = off + i * 40;
                let hash = i64_at(d, e)? as u64;
                let (padded, size) = (i32_at(d, e + 8)? as usize, i32_at(d, e + 12)? as usize);
                let (offset, aes_off) = (i64_at(d, e + 16)? as u64, i64_at(d, e + 32)? as usize);
                let aes = if aes_off != 0 {
                    let key: [u8; 16] = d.get(aes_off..aes_off + 16)?.try_into().ok()?;
                    let count = i32_at(d, aes_off + 16)? as usize;
                    let ranges = (0..count)
                        .map(|k| Some((i64_at(d, aes_off + 20 + k * 16)?, i64_at(d, aes_off + 28 + k * 16)?)))
                        .collect::<Option<Vec<_>>>()?;
                    Some((key, ranges))
                } else {
                    None
                };
                entries.insert(hash, Entry { bdt: bdt.to_path_buf(), offset, padded, size, aes });
            }
        }
        Some(())
    }

    /// The raw (usually DCX compressed) bytes of a game file, e.g. "/parts/bd_m_1280.partsbnd.dcx".
    pub fn read(&self, path: &str) -> Result<Vec<u8>, String> {
        let e = self.entries.get(&path_hash(path)).ok_or(format!("{path} not in the game archives"))?;
        let mut f = std::fs::File::open(&e.bdt).map_err(|err| format!("{}: {err}", e.bdt.display()))?;
        let mut data = vec![0u8; e.padded];
        f.seek(SeekFrom::Start(e.offset)).and_then(|_| f.read_exact(&mut data)).map_err(|err| format!("{path}: {err}"))?;
        if let Some((key, ranges)) = &e.aes {
            let cipher = Aes128::new(GenericArray::from_slice(key));
            for &(start, end) in ranges {
                if start < 0 || end <= start {
                    continue;
                }
                let (start, end) = (start as usize, (end as usize).min(data.len()));
                for block in data[start..end].chunks_exact_mut(16) {
                    cipher.decrypt_block(GenericArray::from_mut_slice(block));
                }
            }
        }
        if e.size != 0 {
            data.truncate(e.size);
        }
        Ok(data)
    }
}
