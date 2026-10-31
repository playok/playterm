#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod boxdraw;
mod connect_dialog;
mod fonts;
mod input;
mod session;
mod view;

use eframe::egui;

use crate::connect_dialog::ConnectRequest;

const USAGE: &str = "사용법: playterm [--cp949] [ssh://[user@]host[:port] | telnet://host[:port]]...";

fn main() -> eframe::Result {
    env_logger::init();

    let initial = match parse_args(std::env::args().skip(1)) {
        Ok(reqs) => reqs,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            std::process::exit(2);
        }
    };

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("playterm")
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([420.0, 260.0]),
        ..Default::default()
    };
    // DX12 is always present on Windows 10+; probing Vulkan first is slow and noisy on some
    // drivers (notably ARM64). WGPU_BACKEND still overrides this.
    if cfg!(windows)
        && std::env::var_os("WGPU_BACKEND").is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }

    eframe::run_native(
        "playterm",
        options,
        Box::new(|cc| Ok(Box::new(app::PlaytermApp::new(cc, initial)?))),
    )
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Vec<ConnectRequest>, String> {
    let mut charset = playterm_term::Charset::Utf8;
    let mut requests = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--cp949" | "--euc-kr" => charset = playterm_term::Charset::Cp949,
            "-h" | "--help" => return Err(String::new()),
            url => {
                let mut req = ConnectRequest::from_url(url)?;
                req.charset = charset;
                requests.push(req);
            }
        }
    }
    Ok(requests)
}
