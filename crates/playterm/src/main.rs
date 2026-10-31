#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod boxdraw;
mod clipboard;
mod dialogs;
mod fonts;
mod input;
mod profile_form;
mod session;
mod sftp_view;
mod sidebar;
mod view;

use eframe::egui;

use playterm_config::SessionProfile;
use playterm_net::{ForwardKind, ForwardRule};

const USAGE: &str = "사용법: playterm [--cp949] [-i 키파일] [-L|-R 포트:호스트:포트] [-D 포트] [ssh://[user@]host[:port] | telnet://host[:port]]...";

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
