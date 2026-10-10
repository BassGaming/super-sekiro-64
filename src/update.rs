//! Update notice: asks GitHub for the latest release's version at launch and every 5 minutes after,
//! and lets the title screen (and a small note in game) say when there's a newer one. Only reads
//! that; nothing gets downloaded or replaced.
//! `update_check = off` in er_mario.ini skips it.

use std::sync::Mutex;

use windows::Win32::Networking::WinHttp::*;
use windows::core::{PCWSTR, w};

use crate::{log, paths};

static LATEST: Mutex<Option<String>> = Mutex::new(None);

/// The newer version on GitHub, if there is one.
pub fn newer() -> Option<String> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn start() {
    if paths::config("update_check").is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no")) {
        log("update check: off");
        return;
    }
    std::thread::spawn(|| {
        let mut first = true;
        loop {
            match latest_tag() {
                Some(tag) => {
                    let latest = tag.trim_start_matches('v').to_string();
                    let current = env!("CARGO_PKG_VERSION");
                    let mut known = LATEST.lock().unwrap_or_else(|e| e.into_inner());
                    if parse(&latest) > parse(current) {
                        if known.as_deref() != Some(latest.as_str()) {
                            log(format!("update check: {latest} is out (this is {current})"));
                            *known = Some(latest);
                        }
                    } else if first {
                        log(format!("update check: up to date ({current})"));
                    }
                }
                None if first => log("update check: GitHub not reachable"),
                None => {}
            }
            first = false;
            std::thread::sleep(std::time::Duration::from_secs(5 * 60));
        }
    });
}

fn parse(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

/// tag_name of the latest release, from GitHub's API.
fn latest_tag() -> Option<String> {
    let body = unsafe { get(w!("api.github.com"), w!("/repos/deltarooo/er-mario/releases/latest")) }?;
    let text = String::from_utf8_lossy(&body);
    let rest = &text[text.find("\"tag_name\"")? + 10..];
    let start = rest.find('"')? + 1;
    let end = start + rest[start..].find('"')?;
    Some(rest[start..end].to_string())
}

unsafe fn get(host: PCWSTR, path: PCWSTR) -> Option<Vec<u8>> {
    let agent = windows::core::HSTRING::from(concat!("er-mario/", env!("CARGO_PKG_VERSION")));
    let session = unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0) };
    if session.is_null() {
        return None;
    }
    let mut out = None;
    unsafe {
        let _ = WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000);
        let connect = WinHttpConnect(session, host, INTERNET_DEFAULT_HTTPS_PORT, 0);
        if !connect.is_null() {
            let request = WinHttpOpenRequest(connect, w!("GET"), path, PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE);
            if !request.is_null() {
                if WinHttpSendRequest(request, None, None, 0, 0, 0).is_ok() && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok() {
                    let mut body = Vec::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        let mut read = 0u32;
                        if WinHttpReadData(request, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read).is_err() || read == 0 {
                            break;
                        }
                        body.extend_from_slice(&buf[..read as usize]);
                        // a release is a few KB; don't keep reading something unexpected
                        if body.len() > 512 * 1024 {
                            break;
                        }
                    }
                    out = Some(body);
                }
                let _ = WinHttpCloseHandle(request);
            }
            let _ = WinHttpCloseHandle(connect);
        }
        let _ = WinHttpCloseHandle(session);
    }
    out
}
