//! SM64's HUD, from the player's ROM: the power meter (the "POWER" frame with the 8-wedge health
//! pie), the counters in SM64's own font (Mario's head x deaths, coins, stars) and the camera
//! status. Drawn as a 2D overlay on the game's final image (hudhook: a DirectX 11 overlay, Sekiro
//! renders with DirectX 11). Ported from er-mario's hud.rs (trimmed to what sk-mario has yet).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::Instant;

use hudhook::imgui::{self, TextureId};
use hudhook::{ImguiRenderLoop, RenderContext};

use crate::log;

/// MIO0 block with the power meter (actors/power_meter in the decomp)
const BLOCK: usize = 0x201410;
const LEFT: usize = 0x233E0;
const RIGHT: usize = 0x243E0;
/// pies for 1..=8 wedges
const PIES: [usize; 8] = [0x28BE0, 0x283E0, 0x27BE0, 0x273E0, 0x26BE0, 0x263E0, 0x25BE0, 0x253E0];
/// MIO0 block with the HUD font (segment 2): digits 0-9 at 0x200 each, then these
const SEGMENT2: usize = 0x108A40;
const GLYPH_MULTIPLY: usize = 0x4200;
const GLYPH_COIN: usize = 0x4400;
const GLYPH_HEAD: usize = 0x4600;
const GLYPH_STAR: usize = 0x4800;

/// HUD size relative to SM64's, and the meter centre's height on SM64's 240-line screen
const SIZE: f32 = 0.65;
const HEIGHT: f32 = 184.0;
/// where the meter rests while Mario is hurt (under the boss posture bar), after sliding up
const HEIGHT_TOP: f32 = 196.0;
/// above the top edge: where it slides to when Mario is back to full health
const HEIGHT_GONE: f32 = 270.0;
const METER_RISE_AFTER: f32 = 2.0;
const METER_RISE_TIME: f32 = 0.4;
/// seconds the meter stays up after Mario is back to full health
const LINGER: f32 = 1.5;
/// the coin counter's left edge, as a share of the screen width
const COINS_X: f32 = 0.56;
/// textures are uploaded this many times bigger (nearest neighbour), so the overlay's smooth
/// filtering keeps SM64's crisp pixels
const UPSCALE: usize = 4;

struct Image {
    rgba: Vec<u8>,
    w: usize,
    h: usize,
}

impl Image {
    fn new(w: usize, h: usize, pixel: impl Fn(usize, usize) -> [u8; 4]) -> Self {
        let (bw, bh) = (w * UPSCALE, h * UPSCALE);
        let mut rgba = Vec::with_capacity(bw * bh * 4);
        for y in 0..bh {
            for x in 0..bw {
                rgba.extend_from_slice(&pixel(x / UPSCALE, y / UPSCALE));
            }
        }
        Image { rgba, w, h }
    }
}

/// RGBA16 (5-5-5-1) pixel
fn rgba16_px(t: &[u8], i: usize) -> [u8; 4] {
    let v = u16::from_be_bytes([t[i * 2], t[i * 2 + 1]]);
    let c = |shift: u16| (((v >> shift) & 31) as u32 * 255 / 31) as u8;
    [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
}

fn rgba16(tex: &[u8], w: usize, h: usize) -> Image {
    Image::new(w, h, |x, y| rgba16_px(tex, y * w + x))
}

static IMAGES: Mutex<Option<Vec<Image>>> = Mutex::new(None);
const IMG_BASE: usize = 0;
const IMG_PIES: usize = 1; // 8
const IMG_DIGITS: usize = 9; // 10
const IMG_MULTIPLY: usize = 19;
const IMG_COIN: usize = 20;
const IMG_HEAD: usize = 21;
const IMG_STAR: usize = 22;
const IMG_CAMERA: usize = 23;
const IMG_LAKITU: usize = 24;
const IMG_ARROW_DOWN: usize = 25;

/// SM64's MIO0 decompression (its ROM's compressed blocks).
pub fn mio0(data: &[u8], start: usize) -> Option<Vec<u8>> {
    let be32 = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize);
    if data.get(start..start + 4)? != b"MIO0" {
        return None;
    }
    let (size, comp_off, raw_off) = (be32(start + 4)?, be32(start + 8)?, be32(start + 12)?);
    let (mut layout, mut comp, mut raw) = (start + 16, start + comp_off, start + raw_off);
    let mut out = Vec::with_capacity(size);
    let (mut flags, mut bit) = (0u32, 0u32);
    while out.len() < size {
        if bit == 0 {
            flags = be32(layout)? as u32;
            layout += 4;
            bit = 32;
        }
        bit -= 1;
        if flags & (1 << bit) != 0 {
            out.push(*data.get(raw)?);
            raw += 1;
        } else {
            let v = u16::from_be_bytes(data.get(comp..comp + 2)?.try_into().ok()?) as usize;
            comp += 2;
            let (length, back) = ((v >> 12) + 3, (v & 0xFFF) + 1);
            for _ in 0..length {
                let b = *out.get(out.len().checked_sub(back)?)?;
                out.push(b);
            }
        }
    }
    Some(out)
}

/// Reads the HUD's textures from the ROM.
pub fn load(rom: &[u8]) {
    let (Some(d), Some(seg2)) = (mio0(rom, BLOCK), mio0(rom, SEGMENT2)) else {
        log("hud: HUD textures not found in the ROM (not a US ROM?)");
        return;
    };
    let tex = |d: &[u8], off: usize, len: usize| d.get(off..off + len).map(<[u8]>::to_vec);
    let load = || -> Option<Vec<Image>> {
        let (left, right) = (tex(&d, LEFT, 4096)?, tex(&d, RIGHT, 4096)?);
        // the frame: two 32x64 halves side by side
        let base = Image::new(64, 64, |x, y| if x < 32 { rgba16_px(&left, y * 32 + x) } else { rgba16_px(&right, y * 32 + x - 32) });
        let mut out = vec![base];
        for &o in &PIES {
            out.push(rgba16(&tex(&d, o, 2048)?, 32, 32));
        }
        let glyph = |off: usize| tex(&seg2, off, 512).map(|t| rgba16(&t, 16, 16));
        for i in 0..10 {
            out.push(glyph(i * 0x200)?);
        }
        for off in [GLYPH_MULTIPLY, GLYPH_COIN, GLYPH_HEAD, GLYPH_STAR] {
            out.push(glyph(off)?);
        }
        out.push(glyph(0x7000)?); // camera
        out.push(glyph(0x7200)?); // Lakitu
        out.push(tex(&seg2, 0x7680, 128).map(|t| rgba16(&t, 8, 8))?); // arrow down
        Some(out)
    };
    match load() {
        Some(images) => {
            log(format!("hud: SM64 HUD loaded ({} textures)", images.len()));
            *IMAGES.lock().unwrap_or_else(|e| e.into_inner()) = Some(images);
        }
        None => log("hud: HUD textures incomplete in the ROM"),
    }
}

// ---- what to show (set by the Mario frame) ------------------------------------------------------

/// wedges 0..=8, or 0xFF: no HUD
static WEDGES: AtomicU8 = AtomicU8::new(0xFF);
static DEATHS: AtomicU32 = AtomicU32::new(0);
static COINS: AtomicU32 = AtomicU32::new(0);
static STARS: AtomicU32 = AtomicU32::new(0);
/// when the Mario frame last reported (the HUD goes when it stops, e.g. on a loading screen)
static LAST_SET: Mutex<Option<Instant>> = Mutex::new(None);

/// Mario's health in wedges (0..=8), every frame while Mario is in the world.
pub fn set(wedges: u8) {
    WEDGES.store(wedges.min(8), Ordering::Relaxed);
    *LAST_SET.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// A short message (an F-key's effect) for a few seconds.
static TOAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);
const TOAST_SECS: f32 = 3.0;
/// F1's list of the mod's keys, shown until F1 again.
static HELP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const HELP_LINES: &[&str] = &[
    "Super Sekiro 64 controls",
    "",
    "Mario mode (pad / keyboard):",
    "Left stick / WASD   move",
    "A / Space   jump        X / N   punch, kick",
    "LT / K   crouch, ground pound (in the air)",
    "B / F   grappling hook        Y / E   action",
    "LB   deflect        Right stick   camera",
    "",
    "F1    this list (F1 again closes it)",
    "Shift+F2 / F3   Mario's damage lower / higher",
    "F6    boss grabs on / off",
    "F7    SM64 music / Sekiro's music",
    "F8    Mario / Sekiro (pad: Select)",
    "F9    Lakitu's camera / Sekiro's (pad: L3)",
    "F10   back to where Mario stood 10 s ago (again: further)",
    "F11   log the collision around Mario (invisible walls)",
];

/// The main menu's credits, bottom right.
const CREDITS: &[&str] = &["Made by BassGaming.", "Shoutout to deltarooo for er-mario and a lot of open source code <3"];

/// A big message in the middle of the screen (the F1 hint after a savegame loads).
static BANNER: Mutex<Option<(String, Instant)>> = Mutex::new(None);
const BANNER_SECS: f32 = 3.0;

/// Shows `text` big in the middle of the screen for 3 s.
pub fn banner(text: impl Into<String>) {
    *BANNER.lock().unwrap_or_else(|e| e.into_inner()) = Some((text.into(), Instant::now()));
}

/// Shows `text` for 3 s.
pub fn toast(text: impl Into<String>) {
    *TOAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((text.into(), Instant::now()));
}

/// F1: the key list on or off; true if it's on now.
pub fn toggle_help() -> bool {
    !HELP.fetch_xor(true, Ordering::Relaxed)
}

pub fn set_counters(deaths: u32, coins: u32, stars: u32) {
    DEATHS.store(deaths, Ordering::Relaxed);
    COINS.store(coins, Ordering::Relaxed);
    STARS.store(stars, Ordering::Relaxed);
}

// ---- the overlay ---------------------------------------------------------------------------------

pub struct Overlay {
    textures: Vec<TextureId>,
    /// the text font (its index in the atlas): a Windows font at a readable size (imgui's own is
    /// 13 px). (An index: the overlay must be Send, imgui's FontId isn't.)
    font: Option<usize>,
    /// the same font at twice the size (the banner)
    big: Option<usize>,
    meter: Option<Meter>,
}

/// The power meter's motion (SM64's: damage brings it down, healing fills it where it is).
struct Meter {
    wedges: u8,
    hit: Instant,
    full: Option<(Instant, f32)>,
}

/// Starts the overlay (hooks the game's DirectX 11 presentation). True if it's hooked.
pub fn install(module: usize) -> bool {
    use hudhook::hooks::dx11::ImguiDx11Hooks;
    let hmodule = hudhook::windows::Win32::Foundation::HINSTANCE(module as _);
    let overlay = Overlay { textures: Vec::new(), font: None, big: None, meter: None };
    // the finished frame, for Mario's shading (light.rs)
    let _ = hudhook::hooks::dx11::ON_PRESENT.set(crate::light::on_present);
    match hudhook::Hudhook::builder().with::<ImguiDx11Hooks>(overlay).with_hmodule(hmodule).build().apply() {
        Ok(()) => {
            log("hud: overlay hooked (DirectX 11)");
            true
        }
        Err(e) => {
            log(format!("hud: overlay hook failed: {e:?}"));
            false
        }
    }
}

impl ImguiRenderLoop for Overlay {
    fn initialize<'a>(&'a mut self, ctx: &mut imgui::Context, _render_context: &'a mut dyn RenderContext) {
        // no cursor, no settings file
        ctx.io_mut().mouse_draw_cursor = false;
        ctx.set_ini_filename(None);
        // (hudhook builds the font atlas after this)
        let ttf = ["segoeuib.ttf", "arialbd.ttf", "tahomabd.ttf"]
            .iter()
            .find_map(|f| std::fs::read(format!(r"C:\Windows\Fonts\{f}")).ok());
        if let Some(ttf) = ttf {
            ctx.fonts().add_font(&[imgui::FontSource::DefaultFontData { config: None }]);
            let data: &'static [u8] = Box::leak(ttf.into_boxed_slice());
            ctx.fonts().add_font(&[imgui::FontSource::TtfData { data, size_pixels: 26.0, config: None }]);
            self.font = Some(ctx.fonts().fonts().len() - 1);
            ctx.fonts().add_font(&[imgui::FontSource::TtfData { data, size_pixels: 56.0, config: None }]);
            self.big = Some(ctx.fonts().fonts().len() - 1);
        }
    }

    fn before_render<'a>(&'a mut self, _ctx: &mut imgui::Context, render_context: &'a mut dyn RenderContext) {
        if !self.textures.is_empty() {
            return;
        }
        let images = IMAGES.lock().unwrap_or_else(|e| e.into_inner());
        let Some(images) = images.as_ref() else { return };
        let ids: Option<Vec<TextureId>> = images
            .iter()
            .map(|i| render_context.load_texture(&i.rgba, (i.w * UPSCALE) as u32, (i.h * UPSCALE) as u32).ok())
            .collect();
        match ids {
            Some(ids) => {
                log("hud: overlay textures uploaded");
                self.textures = ids;
            }
            None => log("hud: overlay texture upload failed"),
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        let _span = crate::prof::span(crate::prof::OVERLAY);
        text_overlay(ui, self.font, self.big);
        if self.textures.len() <= IMG_ARROW_DOWN {
            return;
        }
        // nothing when the Mario frame stopped (loading, menus that pause the world)
        let wedges = WEDGES.load(Ordering::Relaxed);
        let alive = LAST_SET.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed().as_secs_f32() < 0.5);
        if wedges == 0xFF || !alive {
            return;
        }
        let size = ui.io().display_size;
        let textures = &self.textures;
        let dl = ui.get_foreground_draw_list();
        let image = |id: usize, x: f32, y: f32, w: f32, h: f32| {
            dl.add_image(textures[id], [x, y], [x + w, y + h]).build();
        };
        // one of SM64's pixels (its screen is 240 lines high), at HUD size
        let px = size[1] / 240.0 * SIZE;

        // the power meter's height (None: out of the screen). Damage brings it down to HEIGHT,
        // a moment later it slides up to HEIGHT_TOP; back to full health it lingers, then leaves.
        let now = Instant::now();
        let ease = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let hurt_height = |hit: Instant| {
            HEIGHT + (HEIGHT_TOP - HEIGHT) * ease((now.duration_since(hit).as_secs_f32() - METER_RISE_AFTER) / METER_RISE_TIME)
        };
        let long_ago = now.checked_sub(std::time::Duration::from_secs(60)).unwrap_or(now);
        let m = self.meter.get_or_insert(Meter { wedges, hit: long_ago, full: (wedges >= 8).then_some((long_ago, HEIGHT_GONE)) });
        if wedges < m.wedges {
            m.hit = now;
        }
        if wedges >= 8 && m.wedges < 8 {
            m.full = Some((now, hurt_height(m.hit)));
        } else if wedges < 8 {
            m.full = None;
        }
        m.wedges = wedges;
        let meter_height = match m.full {
            None => Some(hurt_height(m.hit)),
            Some((t, h0)) => {
                let out = (now.duration_since(t).as_secs_f32() - LINGER) / METER_RISE_TIME;
                (out < 1.0).then(|| h0 + (HEIGHT_GONE - h0) * ease(out))
            }
        };

        // the counters (SM64's layout: icon, x, digits 12 px apart, 15 px from the top)
        {
            let top = 15.0 * px;
            let counter = |icon: usize, x: f32, n: u32| {
                image(icon, x, top, 16.0 * px, 16.0 * px);
                image(IMG_MULTIPLY, x + 16.0 * px, top, 16.0 * px, 16.0 * px);
                for (k, ch) in n.to_string().bytes().enumerate() {
                    image(IMG_DIGITS + (ch - b'0') as usize, x + (32.0 + 12.0 * k as f32) * px, top, 16.0 * px, 16.0 * px);
                }
            };
            counter(IMG_HEAD, 22.0 * px, DEATHS.load(Ordering::Relaxed));
            counter(IMG_COIN, size[0] * COINS_X, COINS.load(Ordering::Relaxed));
            counter(IMG_STAR, size[0] - 78.0 * px, STARS.load(Ordering::Relaxed));
        }

        // the camera status, bottom right like SM64's: camera + Lakitu, the arrow when zoomed out
        if crate::lakitu::active() {
            let (x, y) = (size[0] - 54.0 * px, size[1] - (240.0 - 205.0) * px);
            image(IMG_CAMERA, x, y, 16.0 * px, 16.0 * px);
            image(IMG_LAKITU, x + 16.0 * px, y, 16.0 * px, 16.0 * px);
            if crate::lakitu::far() {
                image(IMG_ARROW_DOWN, x + 4.0 * px, y + 16.0 * px, 8.0 * px, 8.0 * px);
            }
        }

        // the power meter, centred at the top
        if let Some(height) = meter_height {
            let (cx, cy) = (size[0] * 0.5, size[1] * (1.0 - height / 240.0));
            image(IMG_BASE, cx - 32.0 * px, cy - 32.0 * px, 64.0 * px, 64.0 * px);
            if wedges > 0 {
                image(IMG_PIES + (wedges.min(8) - 1) as usize, cx - 16.0 * px, cy - 16.0 * px, 32.0 * px, 32.0 * px);
            }
        }
    }
}

/// The F-key message and F1's list (shown in Mario mode and playing Sekiro alike).
fn text_overlay(ui: &imgui::Ui, font: Option<usize>, big: Option<usize>) {
    let toast = {
        let mut t = TOAST.lock().unwrap_or_else(|e| e.into_inner());
        if t.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() > TOAST_SECS) {
            *t = None;
        }
        t.clone()
    };
    let banner = {
        let mut b = BANNER.lock().unwrap_or_else(|e| e.into_inner());
        if b.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() > BANNER_SECS) {
            *b = None;
        }
        b.clone()
    };
    let help = HELP.load(Ordering::Relaxed);
    let credits = crate::mario::at_title();
    if toast.is_none() && !help && banner.is_none() && !credits {
        return;
    }
    let _font = font.and_then(|i| ui.fonts().fonts().get(i).copied()).map(|f| ui.push_font(f));
    let size = ui.io().display_size;
    let dl = ui.get_foreground_draw_list();
    if credits {
        // gray, right-aligned in the bottom right corner of the main menu
        let line_h = ui.text_line_height_with_spacing();
        let margin = size[1] * 0.03;
        for (k, l) in CREDITS.iter().enumerate() {
            let w = ui.calc_text_size(l)[0];
            let y = size[1] - margin - (CREDITS.len() - k) as f32 * line_h;
            dl.add_text([size[0] - margin - w + 1.5, y + 1.5], [0.0, 0.0, 0.0, 0.6], l);
            dl.add_text([size[0] - margin - w, y], [0.62, 0.62, 0.62, 0.9], l);
        }
    }
    let pad = 12.0;
    // a dark box with white text, its corners rounded
    let panel = |lines: &[&str], center_x: f32, top: f32, alpha: f32| {
        let line_h = ui.text_line_height_with_spacing();
        let w = lines.iter().map(|l| ui.calc_text_size(l)[0]).fold(0.0f32, f32::max);
        let h = line_h * lines.len() as f32;
        let x = center_x - w / 2.0;
        dl.add_rect([x - pad, top - pad], [x + w + pad, top + h + pad], [0.0, 0.0, 0.0, 0.7 * alpha]).filled(true).rounding(8.0).build();
        for (k, l) in lines.iter().enumerate() {
            let y = top + k as f32 * line_h;
            dl.add_text([x + 2.0, y + 2.0], [0.0, 0.0, 0.0, alpha], l);
            dl.add_text([x, y], [1.0, 1.0, 1.0, alpha], l);
        }
    };
    if help {
        panel(HELP_LINES, size[0] * 0.5, size[1] * 0.22, 1.0);
    }
    if let Some((text, at)) = toast {
        // fading out over the last half second
        let left = TOAST_SECS - at.elapsed().as_secs_f32();
        panel(&[text.as_str()], size[0] * 0.5, size[1] * 0.80, (left / 0.5).clamp(0.0, 1.0));
    }
    if let Some((text, at)) = banner {
        drop(_font);
        let _big = big.or(font).and_then(|i| ui.fonts().fonts().get(i).copied()).map(|f| ui.push_font(f));
        let left = BANNER_SECS - at.elapsed().as_secs_f32();
        let h = ui.text_line_height_with_spacing();
        panel(&[text.as_str()], size[0] * 0.5, size[1] * 0.5 - h / 2.0, (left / 0.5).clamp(0.0, 1.0));
    }
}
