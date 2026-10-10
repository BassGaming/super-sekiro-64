//! BND4 binders (Elden Ring layout: 0x24-byte entries with ids and UTF-16 names, name hash table).

pub struct File {
    pub id: i32,
    pub name: String,
    pub data: Vec<u8>,
}

fn i32_at(d: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn i64_at(d: &[u8], o: usize) -> Option<i64> {
    Some(i64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

pub fn wstr(d: &[u8], o: usize) -> Option<String> {
    let mut e = o;
    while d.get(e..e + 2)? != [0, 0] {
        e += 2;
    }
    let units: Vec<u16> = d[o..e].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&units))
}

pub fn read(d: &[u8]) -> Result<Vec<File>, String> {
    let bad = || "damaged binder".to_string();
    if !d.starts_with(b"BND4") {
        return Err("not a BND4 binder".into());
    }
    let count = i32_at(d, 0x0C).ok_or_else(bad)? as usize;
    let unicode = d.get(0x30) == Some(&1);
    // Elden Ring stores the format byte bit-reversed
    let fmt = d.get(0x31).ok_or_else(bad)?.reverse_bits();
    let (has_ids, has_names) = (fmt & 0x02 != 0, fmt & 0x0C != 0);
    let (long_offsets, compressed) = (fmt & 0x10 != 0, fmt & 0x20 != 0);
    if !unicode && has_names {
        return Err("binder names are not UTF-16".into());
    }
    let mut p = 0x40;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        p += 8;
        let size = i64_at(d, p).ok_or_else(bad)? as usize;
        p += 8;
        if compressed {
            p += 8;
        }
        let offset = if long_offsets {
            p += 8;
            i64_at(d, p - 8).ok_or_else(bad)? as usize
        } else {
            p += 4;
            i32_at(d, p - 4).ok_or_else(bad)? as u32 as usize
        };
        let mut id = -1;
        if has_ids {
            id = i32_at(d, p).ok_or_else(bad)?;
            p += 4;
        }
        let mut name = String::new();
        if has_names {
            name = wstr(d, i32_at(d, p).ok_or_else(bad)? as usize).ok_or_else(bad)?;
            p += 4;
        }
        let data = d.get(offset..offset + size).ok_or_else(bad)?.to_vec();
        files.push(File { id, name, data });
    }
    Ok(files)
}

fn name_hash(name: &str) -> u32 {
    let mut p = name.to_lowercase().replace('\\', "/");
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    p.chars().fold(0u32, |h, c| h.wrapping_mul(37).wrapping_add(c as u32))
}

fn is_prime(p: usize) -> bool {
    p >= 2 && (2..).take_while(|k| k * k <= p).all(|k| p % k != 0)
}

/// A binder with `template`'s header and new `files` (header, entries, names, hash table, data).
pub fn build(template: &[u8], files: &[File]) -> Vec<u8> {
    let n = files.len();
    let mut out = template[..0x40].to_vec();
    out[0x0C..0x10].copy_from_slice(&(n as i32).to_le_bytes());
    let names_at = 0x40 + n * 0x24;
    let mut names = Vec::new();
    let mut name_offs = Vec::with_capacity(n);
    for f in files {
        name_offs.push(names_at + names.len());
        names.extend(f.name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    let hash_at = (names_at + names.len()).next_multiple_of(8);
    let groups = (n / 7..).find(|&p| is_prime(p)).unwrap();
    let mut hashes: Vec<(u32, u32, usize)> =
        files.iter().enumerate().map(|(i, f)| (name_hash(&f.name) % groups as u32, name_hash(&f.name), i)).collect();
    hashes.sort();
    let mut ht = Vec::new();
    ht.extend_from_slice(&((hash_at + 0x10 + groups * 8) as i64).to_le_bytes());
    ht.extend_from_slice(&(groups as i32).to_le_bytes());
    ht.extend_from_slice(&[0x10, 8, 8, 0]);
    let mut start = 0i32;
    for g in 0..groups as u32 {
        let count = hashes.iter().filter(|h| h.0 == g).count() as i32;
        ht.extend_from_slice(&count.to_le_bytes());
        ht.extend_from_slice(&start.to_le_bytes());
        start += count;
    }
    for &(_, h, i) in &hashes {
        ht.extend_from_slice(&h.to_le_bytes());
        ht.extend_from_slice(&(i as i32).to_le_bytes());
    }
    let headers_end = hash_at + ht.len();
    let data_start = headers_end.next_multiple_of(16);
    let mut offs = Vec::with_capacity(n);
    let mut pos = data_start;
    for f in files {
        pos = pos.next_multiple_of(16);
        offs.push(pos);
        pos += f.data.len();
    }
    for (i, f) in files.iter().enumerate() {
        out.push(0x40);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&(-1i32).to_le_bytes());
        out.extend_from_slice(&(f.data.len() as i64).to_le_bytes());
        out.extend_from_slice(&(f.data.len() as i64).to_le_bytes());
        out.extend_from_slice(&(offs[i] as u32).to_le_bytes());
        out.extend_from_slice(&f.id.to_le_bytes());
        out.extend_from_slice(&(name_offs[i] as i32).to_le_bytes());
    }
    out.extend_from_slice(&names);
    out.resize(hash_at, 0);
    out.extend_from_slice(&ht);
    for (i, f) in files.iter().enumerate() {
        out.resize(offs[i], 0);
        out.extend_from_slice(&f.data);
    }
    out[0x28..0x30].copy_from_slice(&(headers_end as i64).to_le_bytes());
    out[0x38..0x40].copy_from_slice(&(hash_at as i64).to_le_bytes());
    out
}
