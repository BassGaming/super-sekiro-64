"""Builds the "Super Sekiro 64" title logo as an me3 package (dist/sk_mario_title).

The game's own files are read from your Sekiro install (Data1.bdt, never modified) and patched
copies are written next to the mod:
  menu/hi/01_common.tpf.dcx       SB_Title_02 (the S-E-K-I-R-O letters) becomes the new logo
  menu/hi/01_common.sblytbnd.dcx  the logo is MENU_Title_S; the other letters and the kanji
                                  point at an empty corner
  menu/05_001_title_logo.gfx      MENU_Title_S is placed and sized as the whole logo; "TM" moves
                                  to its top right corner

Usage: python build_title.py [sekiro dir] [out dir]   (needs Pillow)
"""
import base64, ctypes, os, re, struct, sys, zlib
from ctypes import wintypes

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import logo as logo_art

SEKIRO = sys.argv[1] if len(sys.argv) > 1 else r'G:\SteamLibrary\steamapps\common\Sekiro'
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, '..', '..', 'dist', 'sk_mario_title')

# Data1.bhd's RSA public key (from UXM's ArchiveKeys.cs)
DATA1_KEY = '''
MIIBCwKCAQEA92l+AWx1aV7mzt+6r00bm/qnc4b6NH3VVr/v4UxMcfzushL8jsn9
ZSP1ss95ot/quk8dOJsp0+/bvxH+C9DEezzNLSqqAGd2jq2PYosj/6FhYAKjjMlK
jNxcVPsKQug0Zby+KYsENirmEXcmA1fzltrISf6d6LKB1UFHHN9NRkLCm3idE4Pu
9852kPHbiL14EqfDCDgwm7kLeQdt3kUbcmdhu/6dvP42HGxBmAYLNFD3iAe7qLML
MFzmKKHQD2fRQK/431Z3xPK6Jp245AdR0AwUYVvnXq+/97wMX0C6UKvAZ+b/1ytD
Nu8vZt++lhJ01SjTc2A4hVPz7g1EEO5/TQIEKkj5Jw=='''

# where the logo goes on the 1920x1080 title stage (the letters spanned x 372..1541, y 375..580;
# "SHADOWS DIE TWICE" stays below at y 587)
LOGO_WIDTH = 1200.0
LOGO_BOTTOM = 580.0
TEX_W, TEX_H = 2048, 1024
EMPTY = 'x="2024" y="1000" width="16" height="16"'          # transparent corner of SB_Title_02
EMPTY_TITLE = 'x="1290" y="90" width="16" height="16"'       # transparent spot in SB_Title


# --- archives ---------------------------------------------------------------------------------

def rsa_key():
    der = base64.b64decode(''.join(DATA1_KEY.split()))
    def item(d, i):
        n = d[i + 1]; i += 2
        if n & 0x80:
            k = n & 0x7F; n = int.from_bytes(d[i:i + k], 'big'); i += k
        return d[i:i + n], i + n
    seq, _ = item(der, 0)
    n, j = item(seq, 0)
    e, _ = item(seq, j)
    return int.from_bytes(n, 'big'), int.from_bytes(e, 'big')


def aes_ecb_decrypt(key, data):
    bc = ctypes.WinDLL('bcrypt')
    alg = ctypes.c_void_p()
    assert bc.BCryptOpenAlgorithmProvider(ctypes.byref(alg), 'AES', None, 0) == 0
    mode = 'ChainingModeECB'
    bc.BCryptSetProperty(alg, 'ChainingMode', ctypes.create_unicode_buffer(mode), (len(mode) + 1) * 2, 0)
    k = ctypes.c_void_p()
    assert bc.BCryptGenerateSymmetricKey(alg, ctypes.byref(k), None, 0, key, 16, 0) == 0
    buf = ctypes.create_string_buffer(bytes(data), len(data))
    n = wintypes.ULONG()
    assert bc.BCryptDecrypt(k, buf, len(data), None, None, 0, buf, len(data), ctypes.byref(n), 0) == 0
    bc.BCryptDestroyKey(k); bc.BCryptCloseAlgorithmProvider(alg, 0)
    return buf.raw[:len(data)]


def path_hash(p):
    h = 0
    for c in p.lower():
        h = (h * 37 + ord(c)) & 0xFFFFFFFF
    return h


class Archive:
    def __init__(self, name):
        n, e = rsa_key()
        raw = open(os.path.join(SEKIRO, name + '.bhd'), 'rb').read()
        self.bhd = b''.join(pow(int.from_bytes(raw[i:i + 256], 'big'), e, n).to_bytes(255, 'big')
                            for i in range(0, len(raw), 256))
        assert self.bhd[:4] == b'BHD5', 'Data1.bhd did not decrypt'
        self.bdt = os.path.join(SEKIRO, name + '.bdt')

    def read(self, path):
        d, want = self.bhd, path_hash(path)
        buckets, at = struct.unpack_from('<II', d, 0x10)
        for b in range(buckets):
            count, headers = struct.unpack_from('<II', d, at + b * 8)
            for f in range(count):
                h, padded, off, _sha, aes, size = struct.unpack_from('<IIqqqq', d, headers + f * 0x28)
                if h != want:
                    continue
                with open(self.bdt, 'rb') as fh:
                    fh.seek(off); data = bytearray(fh.read(padded))
                if aes:
                    key = d[aes:aes + 16]
                    for r in range(struct.unpack_from('<i', d, aes + 16)[0]):
                        s, e = struct.unpack_from('<qq', d, aes + 20 + r * 16)
                        if s != -1 and e != -1 and s != e:
                            data[s:e] = aes_ecb_decrypt(key, data[s:e])
                return bytes(data[:size] if size > 0 else data)
        raise FileNotFoundError(path)


# --- DCX --------------------------------------------------------------------------------------

def undcx(b):
    assert b[:4] == b'DCX\0'
    usize, csize = struct.unpack('>II', b[0x1C:0x24])
    body = b[0x4C:0x4C + csize]
    if b[0x28:0x2C] == b'DFLT':
        return zlib.decompress(body)
    oodle = ctypes.WinDLL(os.path.join(SEKIRO, 'oo2core_6_win64.dll'))
    f = oodle.OodleLZ_Decompress
    f.restype = ctypes.c_int64
    dst = ctypes.create_string_buffer(usize)
    n = f(body, ctypes.c_int64(len(body)), dst, ctypes.c_int64(usize), 0, 0, 0, None, 0, None, None, None, 0, 3)
    assert n == usize, 'Oodle failed'
    return dst.raw


def dcx(data):
    # DFLT: Sekiro reads it as well as Kraken (Sekiro Resurrection ships its menus this way)
    z = zlib.compress(data, 9)
    head = bytearray(b'DCX\0' + bytes.fromhex('00011000') + struct.pack('>IIII', 0x18, 0x24, 0x44, 0x4C)
                     + b'DCS\0' + struct.pack('>II', len(data), len(z))
                     + b'DCP\0DFLT' + struct.pack('>I', 0x20) + bytes([9, 0, 0, 0]) + bytes(12)
                     + struct.pack('>I', 0x00010100) + b'DCA\0' + struct.pack('>I', 8))
    assert len(head) == 0x4C
    return bytes(head) + z


# --- TPF --------------------------------------------------------------------------------------

def tpf_entries(t):
    count = struct.unpack_from('<I', t, 8)[0]
    out = []
    for i in range(count):
        o = 0x10 + i * 20
        off, size, fmt = struct.unpack_from('<IIB', t, o)
        name_at = struct.unpack_from('<I', t, o + 12)[0]
        e = name_at
        while t[e:e + 2] != b'\0\0':
            e += 2
        out.append((o, off, size, t[name_at:e].decode('utf-16le')))
    return out


def tpf_replace(t, name, dds, fmt):
    entries = tpf_entries(t)
    head = bytearray(t[:entries[0][1]])
    blob = bytearray()
    for o, off, size, n in entries:
        data = dds if n == name else t[off:off + size]
        struct.pack_into('<II', head, o, len(head) + len(blob), len(data))
        if n == name:
            head[o + 8] = fmt
            head[o + 10] = 1   # mip levels
        blob += data
    struct.pack_into('<I', head, 4, len(blob))
    return bytes(head + blob)


def dxt5(img):
    import io
    buf = io.BytesIO()
    img.save(buf, 'DDS', pixel_format='DXT5')
    return buf.getvalue()


# --- BND4 -------------------------------------------------------------------------------------

def bnd_files(b):
    count, = struct.unpack_from('<I', b, 0xC)
    size, = struct.unpack_from('<Q', b, 0x20)
    out = []
    for i in range(count):
        o = 0x40 + i * size
        length, = struct.unpack_from('<Q', b, o + 8)
        off, = struct.unpack_from('<I', b, o + 24)
        out.append([o, b[off:off + length]])
    return out


def bnd_write(b, files):
    data_at, = struct.unpack_from('<Q', b, 0x28)
    head = bytearray(b[:data_at])
    body = bytearray()
    for o, data in files:
        at = data_at + len(body)
        struct.pack_into('<QQI', head, o + 8, len(data), len(data), at)
        body += data
        body += bytes(-len(body) % 16)
    return bytes(head + body)


# --- GFX (Scaleform SWF) ----------------------------------------------------------------------

class Bits:
    def __init__(self, b, p): self.b, self.p = b, p * 8
    def u(self, n):
        v = 0
        for _ in range(n):
            v = (v << 1) | ((self.b[self.p >> 3] >> (7 - (self.p & 7))) & 1); self.p += 1
        return v
    def end(self): return (self.p + 7) >> 3


def skip_matrix(b, p):
    r = Bits(b, p)
    for _ in range(2):
        if r.u(1):
            n = r.u(5); r.u(2 * n)
    n = r.u(5); r.u(2 * n)
    return r.end()


def matrix(scale, tx, ty):
    vals = []
    def sbits(v):
        return max(1, (v if v >= 0 else ~v).bit_length() + 1)
    s = int(round(scale * 65536))
    x, y = int(round(tx * 20)), int(round(ty * 20))
    ns = max(sbits(s), 2); nt = max(sbits(x), sbits(y))
    vals += [(1, 1), (ns, 5), (s & ((1 << ns) - 1), ns), (s & ((1 << ns) - 1), ns), (0, 1),
             (nt, 5), (x & ((1 << nt) - 1), nt), (y & ((1 << nt) - 1), nt)]
    bits = ''.join(format(v, '0%db' % n) for v, n in vals)
    bits += '0' * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def gfx_tags(b):
    r = Bits(b, 8); n = r.u(5); r.u(4 * n)
    p = r.end() + 4
    while p < len(b):
        h, = struct.unpack_from('<H', b, p)
        code, ln, q = h >> 6, h & 63, p + 2
        if ln == 63:
            ln, = struct.unpack_from('<I', b, q); q += 4
        yield code, p, q, ln
        p = q + ln
        if code == 0:
            break


def tag(code, body):
    if len(body) < 63:
        return struct.pack('<H', code << 6 | len(body)) + body
    return struct.pack('<HI', code << 6 | 63, len(body)) + body


def patch_gfx(b, logo_w, logo_h):
    b = bytearray(b)
    scale = LOGO_WIDTH / logo_w
    x0 = 960 - LOGO_WIDTH / 2
    y0 = LOGO_BOTTOM - logo_h * scale
    s_id = None
    edits = []
    for code, p, q, ln in gfx_tags(bytes(b)):
        if code == 1009:   # DefineExternalImage2: id, format, width, height, export name
            cid, = struct.unpack_from('<I', b, q)
            name = b[q + 11:q + 11 + b[q + 10]]
            if name == b'MENU_Title_S':
                struct.pack_into('<HH', b, q + 6, logo_w, logo_h)
                s_id = cid
        elif code in (26, 70) and b[q] == 0x06:   # PlaceObject2/3 with a character and a matrix
            fixed = 3 if code == 26 else 4           # flags (+flags2), depth
            if code == 70 and b[q + 1] & 0x08:       # a class name before the character
                continue
            depth, = struct.unpack_from('<H', b, q + fixed - 2)
            cid, = struct.unpack_from('<H', b, q + fixed)
            end = skip_matrix(b, q + fixed + 2)
            if code == 70 and cid == s_id:
                m = matrix(scale, x0, y0)
            elif code == 26 and depth == 13:      # the "TM" text
                m = matrix(1.0, x0 + LOGO_WIDTH - 20, y0 - 10)
            else:
                continue
            body = bytes(b[q:q + fixed + 2]) + m + bytes(b[end:q + ln])
            edits.append((p, q + ln, tag(code, body)))
    assert s_id is not None and len(edits) == 2, ('gfx layout not recognised', s_id, len(edits))
    for start, end, new in reversed(edits):
        b[start:end] = new
    struct.pack_into('<I', b, 4, len(b))
    return bytes(b)


# --- layout -----------------------------------------------------------------------------------

def patch_layout(xml, logo_w, logo_h):
    s = xml.decode('utf-8')
    def sub(name, attrs, half='0'):
        nonlocal s
        pat = r'<SubTexture name="%s\.png"[^/]*/>' % re.escape(name)
        assert re.search(pat, s), name
        s = re.sub(pat, '<SubTexture name="%s.png" %s half="%s"/>' % (name, attrs, half), s)
    if 'imagePath="SB_Title_02.png"' in s:
        sub('MENU_Title_S', 'x="0" y="0" width="%d" height="%d"' % (logo_w, logo_h))
        for c in 'EKIRO':
            sub('MENU_Title_' + c, EMPTY)
    elif 'imagePath="SB_Title.png"' in s:
        sub('MENU_Title_SEKIRO', EMPTY_TITLE, '1')
    else:
        return None
    return s.encode('utf-8')


def main():
    from PIL import Image
    arc = Archive('Data1')
    art = logo_art.logo()
    k = min(TEX_W / art.width, (TEX_H - 40) / art.height)
    art = art.resize((int(art.width * k), int(art.height * k)), Image.LANCZOS)
    tex = Image.new('RGBA', (TEX_W, TEX_H), (0, 0, 0, 0))
    tex.paste(art, (0, 0))
    lw, lh = art.size
    print('logo', lw, 'x', lh)

    t = undcx(arc.read('/menu/hi/01_common.tpf.dcx'))
    t = tpf_replace(t, 'SB_Title_02', dxt5(tex), 5)

    bnd = undcx(arc.read('/menu/hi/01_common.sblytbnd.dcx'))
    files = bnd_files(bnd)
    hit = 0
    for f in files:
        new = patch_layout(f[1], lw, lh)
        if new is not None:
            f[1] = new; hit += 1
    assert hit == 2, hit
    bnd = bnd_write(bnd, files)

    gfx = patch_gfx(arc.read('/menu/05_001_title_logo.gfx'), lw, lh)

    os.makedirs(os.path.join(OUT, 'menu', 'hi'), exist_ok=True)
    for rel, data in (('menu/hi/01_common.tpf.dcx', dcx(t)), ('menu/hi/01_common.sblytbnd.dcx', dcx(bnd)),
                      ('menu/05_001_title_logo.gfx', gfx)):
        with open(os.path.join(OUT, rel), 'wb') as fh:
            fh.write(data)
        print('wrote', rel, len(data))
    tex.save(os.path.join(OUT, 'logo_preview.png'))


if __name__ == '__main__':
    main()
