"""Exports the logo texture the DLL embeds (src/assets/title_logo.dds, DXT5, 2048 x 1024, the logo
in its top left corner) and prints the logo's size for title.rs (LOGO_W, LOGO_H).

Usage: python export_logo.py   (needs Pillow)
"""
import io, os, sys
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import logo as logo_art

TEX_W, TEX_H = 2048, 1024
OUT = os.path.join(HERE, '..', '..', 'src', 'assets', 'title_logo.dds')

art = logo_art.logo()
k = min(TEX_W / art.width, (TEX_H - 40) / art.height)
art = art.resize((int(art.width * k), int(art.height * k)), Image.LANCZOS)
tex = Image.new('RGBA', (TEX_W, TEX_H), (0, 0, 0, 0))
tex.paste(art, (0, 0))
buf = io.BytesIO()
tex.save(buf, 'DDS', pixel_format='DXT5')
open(OUT, 'wb').write(buf.getvalue())
print('LOGO_W', art.width, 'LOGO_H', art.height, 'bytes', len(buf.getvalue()))
