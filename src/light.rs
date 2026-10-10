//! Mario's shading from the scene around him. He is drawn with the game's debug triangles, after
//! its lighting, fog and volumetrics, so he'd look the same in a dark cave as in the sun. Instead,
//! every frame (at Present, before the overlay draws) a ring of small patches of the finished
//! image around Mario is copied out; their brightness and colour, read back a few frames later
//! (no GPU stall) and smoothed over about a second (no flicker), tint him, and far away he fades
//! towards the colour around him like the scene does into its fog.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use glam::Vec3;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_FORMAT_R10G10B10A2_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::Interface;

/// Patches sampled around Mario, and their size (pixels).
const POINTS: usize = 16;
const PATCH: u32 = 4;
/// Staging textures in flight: a copy is read back this many frames later.
const RING: usize = 3;
/// The scene brightness (sRGB luma) at which Mario is shown at his full colours.
const FULL: f32 = 0.42;
/// Darkest he gets (multiplier), and how much of the scene's colour cast he takes.
const DARKEST: f32 = 0.22;
const CAST: f32 = 0.45;
/// Smoothing time constant (s).
const SMOOTH: f32 = 0.8;
/// Fog: none up to NEAR m from the camera, up to FOG_MAX at FAR m.
const NEAR: f32 = 15.0;
const FAR: f32 = 90.0;
const FOG_MAX: f32 = 0.55;

/// Where Mario is on screen, from the game thread: camera-space tangents (x/z, y/z) of his centre,
/// the tangent radius of the ring, his distance from the camera, and when.
#[derive(Clone, Copy)]
struct Target {
    tx: f32,
    ty: f32,
    r: f32,
    dist: f32,
    tan_half_fov: f32,
    at: Instant,
}

static TARGET: Mutex<Option<Target>> = Mutex::new(None);
/// The smoothed result: tint (r, g, b), fog colour (r, g, b), fog amount. f32 bits.
static OUT: [AtomicU32; 7] = [const { AtomicU32::new(0) }; 7];
static HAVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called on the game thread each frame: Mario's centre and the camera.
pub fn update_target(mario: Vec3) {
    let player = crate::player::position().map(Vec3::from).unwrap_or(mario);
    let (Some(cam), Some(fov)) = (crate::camera::active(player), crate::camera::fov()) else {
        *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return;
    };
    let c = mario + Vec3::Y * 0.5;
    let d = c - cam.pos;
    let z = d.dot(cam.forward);
    let target = (z > 0.5).then(|| Target {
        tx: d.dot(cam.right) / z,
        ty: d.dot(cam.up) / z,
        // the ring: outside Mario (about 0.6 m around his middle)
        r: 1.1 / z,
        dist: d.length(),
        tan_half_fov: (fov * 0.5).tan(),
        at: Instant::now(),
    });
    *TARGET.lock().unwrap_or_else(|e| e.into_inner()) = target;
}

/// The shading for Mario's colours: (tint, fog colour, fog amount); neutral until measured.
pub fn shading() -> (Vec3, Vec3, f32) {
    if !HAVE.load(Ordering::Relaxed) {
        return (Vec3::ONE, Vec3::ZERO, 0.0);
    }
    let f = |i: usize| f32::from_bits(OUT[i].load(Ordering::Relaxed));
    (Vec3::new(f(0), f(1), f(2)), Vec3::new(f(3), f(4), f(5)), f(6))
}

struct Slot {
    tex: ID3D11Texture2D,
    /// copied and not read yet: (distance, when)
    pending: Option<f32>,
}

struct State {
    device: usize,
    format: DXGI_FORMAT,
    ctx: ID3D11DeviceContext,
    slots: Vec<Slot>,
    next: usize,
    /// smoothed: tint, fog colour, fog amount
    tint: Vec3,
    fog: Vec3,
    amount: f32,
    last: Option<Instant>,
    logged: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The overlay's Present callback (render thread).
pub fn on_present(swap_chain: *mut std::ffi::c_void) {
    let _span = crate::prof::span(crate::prof::PRESENT);
    let _ = std::panic::catch_unwind(|| unsafe { present(swap_chain) });
}

unsafe fn present(raw: *mut std::ffi::c_void) {
    let Some(sc) = (unsafe { IDXGISwapChain::from_raw_borrowed(&raw) }) else { return };
    let Ok(back) = (unsafe { sc.GetBuffer::<ID3D11Texture2D>(0) }) else { return };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { back.GetDesc(&mut desc) };
    let supported = [
        DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
        DXGI_FORMAT_R10G10B10A2_UNORM,
    ];
    if desc.SampleDesc.Count != 1 || !supported.contains(&desc.Format) {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            crate::log(format!("light: back buffer format {:?} x{} not supported; Mario isn't shaded", desc.Format, desc.SampleDesc.Count));
        }
        return;
    }
    let Ok(device) = (unsafe { back.GetDevice() }) else { return };
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let dev_raw = device.as_raw() as usize;
        if cell.as_ref().is_none_or(|s| s.device != dev_raw || s.format != desc.Format) {
            *cell = unsafe { create(&device, desc.Format) };
        }
        let Some(st) = cell.as_mut() else { return };
        unsafe { frame(st, &back, desc.Width, desc.Height) };
    });
}

unsafe fn create(device: &ID3D11Device, format: DXGI_FORMAT) -> Option<State> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: POINTS as u32 * PATCH,
        Height: PATCH,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut slots = Vec::new();
    for _ in 0..RING {
        let mut tex = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.ok()?;
        slots.push(Slot { tex: tex?, pending: None });
    }
    let ctx = unsafe { device.GetImmediateContext() }.ok()?;
    crate::log(format!("light: sampling the scene around Mario ({format:?})"));
    Some(State {
        device: device.as_raw() as usize,
        format,
        ctx,
        slots,
        next: 0,
        tint: Vec3::ONE,
        fog: Vec3::ZERO,
        amount: 0.0,
        last: None,
        logged: false,
    })
}

unsafe fn frame(st: &mut State, back: &ID3D11Texture2D, w: u32, h: u32) {
    // read back the oldest copy (if the GPU is done with it)
    let read = (st.next + 1) % RING;
    if let Some(dist) = st.slots[read].pending {
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        let ok = unsafe { st.ctx.Map(&st.slots[read].tex, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut m)) }.is_ok();
        if ok {
            let samples = unsafe { read_patches(&m, st.format) };
            unsafe { st.ctx.Unmap(&st.slots[read].tex, 0) };
            st.slots[read].pending = None;
            apply(st, &samples, dist);
        }
    }
    // copy this frame's patches
    let target = *TARGET.lock().unwrap_or_else(|e| e.into_inner());
    let Some(t) = target.filter(|t| t.at.elapsed().as_secs_f32() < 0.2 && crate::mario::shown()) else { return };
    let slot = st.next;
    if st.slots[slot].pending.is_some() {
        return; // still in flight: skip a frame
    }
    let aspect = w as f32 / h as f32;
    let to_px = |x: f32, y: f32| {
        (
            (0.5 + 0.5 * x / (t.tan_half_fov * aspect)) * w as f32,
            (0.5 - 0.5 * y / t.tan_half_fov) * h as f32,
        )
    };
    let (cx, cy) = to_px(t.tx, t.ty);
    let r_px = (t.r / t.tan_half_fov * 0.5 * h as f32).clamp(14.0, h as f32 * 0.25);
    let mut copied = 0;
    for i in 0..POINTS {
        let a = i as f32 / POINTS as f32 * std::f32::consts::TAU;
        let (x, y) = (cx + a.cos() * r_px, cy + a.sin() * r_px);
        let (x, y) = (x.clamp(0.0, (w - PATCH) as f32) as u32, y.clamp(0.0, (h - PATCH) as f32) as u32);
        let b = D3D11_BOX { left: x, top: y, front: 0, right: x + PATCH, bottom: y + PATCH, back: 1 };
        unsafe { st.ctx.CopySubresourceRegion(&st.slots[slot].tex, 0, i as u32 * PATCH, 0, 0, back, 0, Some(&b)) };
        copied += 1;
    }
    if copied > 0 && cx > -r_px && cx < w as f32 + r_px && cy > -r_px && cy < h as f32 + r_px {
        st.slots[slot].pending = Some(t.dist);
    }
    st.next = (st.next + 1) % RING;
}

/// Each patch's mean colour (sRGB-encoded, 0..1).
unsafe fn read_patches(m: &D3D11_MAPPED_SUBRESOURCE, format: DXGI_FORMAT) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(POINTS);
    let px = |x: u32, y: u32| -> Vec3 {
        let v = unsafe { *((m.pData as *const u8).add((y * m.RowPitch + x * 4) as usize) as *const u32) };
        match format {
            DXGI_FORMAT_R10G10B10A2_UNORM => {
                Vec3::new((v & 0x3ff) as f32, ((v >> 10) & 0x3ff) as f32, ((v >> 20) & 0x3ff) as f32) / 1023.0
            }
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => {
                Vec3::new(((v >> 16) & 0xff) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32) / 255.0
            }
            _ => Vec3::new((v & 0xff) as f32, ((v >> 8) & 0xff) as f32, ((v >> 16) & 0xff) as f32) / 255.0,
        }
    };
    for i in 0..POINTS as u32 {
        let mut sum = Vec3::ZERO;
        for y in 0..PATCH {
            for x in 0..PATCH {
                sum += px(i * PATCH + x, y);
            }
        }
        out.push(sum / (PATCH * PATCH) as f32);
    }
    out
}

fn luma(c: Vec3) -> f32 {
    c.dot(Vec3::new(0.299, 0.587, 0.114))
}

fn apply(st: &mut State, samples: &[Vec3], dist: f32) {
    if samples.is_empty() {
        return;
    }
    // robust: the middle half by brightness (the sky behind him, a lamp, the HUD don't count much)
    let mut s: Vec<Vec3> = samples.to_vec();
    s.sort_by(|a, b| luma(*a).total_cmp(&luma(*b)));
    let q = s.len() / 4;
    let mid = &s[q..s.len() - q];
    let mean = mid.iter().copied().sum::<Vec3>() / mid.len() as f32;
    let l = luma(mean);
    let level = (l / FULL).clamp(DARKEST, 1.0);
    let cast = if l > 0.02 { (mean / l).clamp(Vec3::splat(0.5), Vec3::splat(1.6)) } else { Vec3::ONE };
    let tint = Vec3::ONE.lerp(cast, CAST) * level;
    let amount = ((dist - NEAR) / (FAR - NEAR)).clamp(0.0, 1.0) * FOG_MAX;

    let now = Instant::now();
    let dt = st.last.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(10.0).min(10.0);
    st.last = Some(now);
    let k = 1.0 - (-dt / SMOOTH).exp();
    st.tint = st.tint.lerp(tint, k);
    st.fog = st.fog.lerp(mean, k);
    st.amount += (amount - st.amount) * k;
    let vals = [st.tint.x, st.tint.y, st.tint.z, st.fog.x, st.fog.y, st.fog.z, st.amount];
    for (o, v) in OUT.iter().zip(vals) {
        o.store(v.to_bits(), Ordering::Relaxed);
    }
    HAVE.store(true, Ordering::Relaxed);
    if !st.logged {
        st.logged = true;
        crate::log(format!("light: scene around Mario {mean:.2} -> tint {:.2}, fog {:.2}", st.tint, st.amount));
    }
}
