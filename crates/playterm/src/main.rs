#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod boxdraw;
mod clipboard;
mod dialogs;
mod fonts;
mod input;
mod logging;
mod menu;
mod mouse;
mod profile_form;
mod session;
mod sftp_view;
mod sidebar;
mod site_manager;
mod view;

use std::sync::Arc;

use eframe::egui;
use playterm_config::SessionProfile;
use playterm_net::{ForwardKind, ForwardRule};

const USAGE: &str = "사용법: playterm [--cp949] [-i 키파일] [-L|-R 포트:호스트:포트] [-D 포트] [ssh://[user@]host[:port] | telnet://host[:port] | serial://COM3[:속도]]...";

fn main() -> eframe::Result {
    env_logger::init();

    let initial = match parse_args(std::env::args().skip(1)) {
        Ok(reqs) => reqs,
        Err(e) => {
            let text = if e.is_empty() { USAGE.to_owned() } else { format!("{e}\n\n{USAGE}") };
            show_message(&text, e.is_empty());
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    };
    let icon = Arc::new(
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/playterm.png"))
            .expect("embedded icon is a valid PNG"),
    );

    let mut last_error = None;
    for renderer in renderers() {
        let initial = initial.clone();
        let options = native_options(renderer, icon.clone());
        // Set once the app exists: after that a failure must not silently restart the window
        // (and drop open sessions) on another renderer.
        let started = std::cell::Cell::new(false);
        // eframe panics (rather than returning an error) when no OpenGL pixel format matches.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            eframe::run_native(
                "playterm",
                options,
                Box::new(|cc| {
                    started.set(true);
                    Ok(Box::new(app::PlaytermApp::new(cc, initial)?))
                }),
            )
        }));
        let result = match result {
            Ok(result) => result,
            Err(_) if renderer == eframe::Renderer::Glow && !started.get() => {
                log::warn!("OpenGL renderer panicked during start-up; trying the next renderer");
                continue;
            }
            Err(panic) => std::panic::resume_unwind(panic),
        };
        match result {
            // OpenGL could not start (no driver, remote session, VM): try the next renderer.
            Err(e @ (eframe::Error::Glutin(_) | eframe::Error::NoGlutinConfigs(..) | eframe::Error::OpenGL(_)))
                if !started.get() =>
            {
                log::warn!("{renderer:?} renderer unavailable: {e}");
                last_error = Some(e);
            }
            Err(e) => {
                show_message(&format!("playterm 을 시작할 수 없습니다:\n{e}"), false);
                return Err(e);
            }
            Ok(()) => return Ok(()),
        }
    }
    let error = last_error.expect("at least one renderer was tried");
    show_message(&format!("화면 렌더러를 초기화할 수 없습니다:\n{error}"), false);
    Err(error)
}

/// Release builds have no console, so messages go to a dialog.
fn show_message(text: &str, info: bool) {
    rfd::MessageDialog::new()
        .set_title("playterm")
        .set_description(text)
        .set_level(if info { rfd::MessageLevel::Info } else { rfd::MessageLevel::Error })
        .show();
}

/// OpenGL (glow) first: it needs about a third of the memory of wgpu/DX12. wgpu is the
/// fallback where OpenGL is missing. `PLAYTERM_RENDERER=wgpu|glow` forces one.
fn renderers() -> Vec<eframe::Renderer> {
    match std::env::var("PLAYTERM_RENDERER").as_deref() {
        Ok("wgpu") => vec![eframe::Renderer::Wgpu],
        Ok("glow") => vec![eframe::Renderer::Glow],
        _ => vec![eframe::Renderer::Glow, eframe::Renderer::Wgpu],
    }
}

fn native_options(renderer: eframe::Renderer, icon: Arc<egui::IconData>) -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("playterm")
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([420.0, 260.0])
            .with_icon(icon),
        ..Default::default()
    };
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        // DX12 is always present on Windows 10+; probing Vulkan first is slow and noisy on some
        // drivers (notably ARM64). WGPU_BACKEND still overrides this.
        if cfg!(windows) && std::env::var_os("WGPU_BACKEND").is_none() {
            setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        }
        // The default (Performance) reserves large GPU heaps up front; a terminal draws
        // little, and this roughly halves the process memory.
        let base = setup.device_descriptor.clone();
        setup.device_descriptor = Arc::new(move |adapter| eframe::wgpu::DeviceDescriptor {
            memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
            ..base(adapter)
        });
    }
    options
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Vec<SessionProfile>, String> {
    let mut args = args;
    let mut charset = playterm_term::Charset::Utf8;
    let mut identity_file = String::new();
    let mut forwards = Vec::new();
    let mut requests = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cp949" | "--euc-kr" => charset = playterm_term::Charset::Cp949,
            "-i" => identity_file = args.next().ok_or("-i 다음에 키 파일 경로가 필요합니다")?,
            "-L" | "-R" | "-D" => {
                let kind = match arg.as_str() {
                    "-L" => ForwardKind::Local,
                    "-R" => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                let spec = args.next().ok_or_else(|| format!("{arg} 다음에 포워딩 설정이 필요합니다"))?;
                forwards.push(ForwardRule::parse(kind, &spec)?);
            }
            "-h" | "--help" => return Err(String::new()),
            url => {
                let mut profile = profile_form::parse_url(url)?;
                profile.charset = charset;
                profile.identity_file = identity_file.clone();
                profile.forwards = forwards.clone();
                requests.push(profile);
            }
        }
    }
    Ok(requests)
}
