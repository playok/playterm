//! Connection form shared by the quick-connect dialog and the session editor,
//! plus parsing of `ssh://` / `telnet://` URLs from the command line.

use std::path::PathBuf;

use eframe::egui;
use playterm_config::{AutoLogin, LoginStep, SessionProfile, TabColor};
use playterm_i18n::{t, tr};
use playterm_net::{
    ForwardKind, ForwardRule, KeepAlive, KeepAliveMode, Protocol, SerialFlow, SerialParity, SerialPortEntry, SerialSettings,
    decode_escapes, list_ports,
};
use playterm_term::Charset;
use zeroize::Zeroizing;

use crate::icon;

/// Parses `ssh://[user@]host[:port]` or `telnet://host[:port]` (`user@host` alone means SSH).
/// IPv6 hosts are written in brackets: `ssh://user@[::1]:2222`.
pub fn parse_url(url: &str) -> Result<SessionProfile, String> {
    let (protocol, rest) = match url.split_once("://") {
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "ssh" => (Protocol::Ssh, rest),
            "telnet" => (Protocol::Telnet, rest),
            "serial" | "com" => (Protocol::Serial, rest),
            "rdp" => (Protocol::Rdp, rest),
            other => return Err(tr!("profile_form.unsupported_protocol", other = other)),
        },
        None => (Protocol::Ssh, url),
    };
    let rest = rest.trim_end_matches('/');
    if protocol == Protocol::Serial {
        let (port, baud) = match rest.split_once(':') {
            Some((port, baud)) => (port, baud.parse().ok().filter(|&b: &u32| b > 0).ok_or(tr!("profile_form.invalid_speed", baud = baud))?),
            None => (rest, SerialSettings::default().baud),
        };
        if port.is_empty() {
            return Err(tr!("profile_form.missing_serial_port", url = url));
        }
        let serial = SerialSettings { port: port.to_owned(), baud, ..Default::default() };
        return Ok(SessionProfile {
            protocol,
            host: port.to_owned(),
            port: 0,
            serial,
            ..SessionProfile::new().map_err(|e| e.to_string())?
        });
    }
    let (username, hostport) = match rest.rsplit_once('@') {
        Some((user, hp)) => (user.to_owned(), hp),
        None => (String::new(), rest),
    };

    let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
        let (host, tail) = after.split_once(']').ok_or_else(|| tr!("profile_form.invalid_address", url = url))?;
        (host, tail.strip_prefix(':'))
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(tr!("profile_form.missing_host", url = url));
    }
    let port = match port {
        Some(p) => p.parse().ok().filter(|&p: &u16| p > 0).ok_or_else(|| tr!("profile_form.invalid_port", p = p))?,
        None => protocol.default_port(),
    };
    if protocol == Protocol::Ssh && username.is_empty() {
        return Err(tr!("profile_form.an_ssh_address_needs_a_user_name", url = url));
    }

    Ok(SessionProfile { protocol, host: host.to_owned(), port, username, ..SessionProfile::new().map_err(|e| e.to_string())? })
}

/// Expands a leading `~` to the user's home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => match directories::BaseDirs::new() {
            Some(dirs) => dirs.home_dir().join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
    }
}

/// Native file dialog for choosing a private key, starting in `~/.ssh`.
fn pick_key_file(current: &str) -> Option<PathBuf> {
    let start = match current.trim() {
        "" => expand_home("~/.ssh"),
        c => expand_home(c).parent().map(PathBuf::from).unwrap_or_default(),
    };
    crate::picker::Picker::new(crate::picker::Kind::File, t!("profile_form.choose_an_ssh_private_key"))
        .directory(start)
        .filter(t!("profile_form.all_files"), &["*"])
        .filter(t!("profile_form.putty_keys_ppk"), &["ppk"])
        .pick_one()
}

/// Serial line fields. Returns `true` if Enter was pressed in a text field.
fn serial_rows(
    ui: &mut egui::Ui,
    s: &mut SerialSettings,
    baud: &mut String,
    ports: &mut Vec<SerialPortEntry>,
    focus_requested: &mut bool,
) -> bool {
    let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut submit = false;

    ui.label(t!("profile_form.serial_port"));
    ui.horizontal(|ui| {
        let port = ui.add(egui::TextEdit::singleline(&mut s.port).hint_text("COM3").desired_width(90.0));
        if !*focus_requested {
            port.request_focus();
            *focus_requested = true;
        }
        submit |= enter(ui, &port);
        egui::ComboBox::from_id_salt("serial-ports").selected_text(t!("profile_form.list")).show_ui(ui, |ui| {
            if ports.is_empty() {
                ui.weak(t!("profile_form.no_com_ports"));
            }
            for entry in ports.iter() {
                let label =
                    if entry.description.is_empty() { entry.name.clone() } else { format!("{}  {}", entry.name, entry.description) };
                if ui.selectable_label(s.port == entry.name, label).clicked() {
                    s.port = entry.name.clone();
                }
            }
        });
        if ui.add(icon::button(icon::REFRESH, t!("common.refresh"))).clicked() {
            *ports = list_ports();
        }
    });
    ui.end_row();

    ui.label(t!("profile_form.speed_baud"));
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(baud).desired_width(90.0));
        submit |= enter(ui, &r);
        ui.menu_button(t!("profile_form.choose"), |ui| {
            for rate in SerialSettings::COMMON_BAUD_RATES {
                if ui.button(rate.to_string()).clicked() {
                    *baud = rate.to_string();
                    ui.close();
                }
            }
        });
    });
    ui.end_row();

    ui.label(t!("profile_form.data_bits"));
    egui::ComboBox::from_id_salt("data-bits").selected_text(s.data_bits.to_string()).show_ui(ui, |ui| {
        for bits in [8u8, 7, 6, 5] {
            ui.selectable_value(&mut s.data_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label(t!("profile_form.parity"));
    egui::ComboBox::from_id_salt("parity").selected_text(s.parity.to_string()).show_ui(ui, |ui| {
        for parity in SerialParity::ALL {
            ui.selectable_value(&mut s.parity, parity, parity.to_string());
        }
    });
    ui.end_row();

    ui.label(t!("profile_form.stop_bits"));
    egui::ComboBox::from_id_salt("stop-bits").selected_text(s.stop_bits.to_string()).show_ui(ui, |ui| {
        for bits in [1u8, 2] {
            ui.selectable_value(&mut s.stop_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label(t!("profile_form.flow_control"));
    egui::ComboBox::from_id_salt("flow").selected_text(s.flow.to_string()).show_ui(ui, |ui| {
        for flow in SerialFlow::ALL {
            ui.selectable_value(&mut s.flow, flow, flow.to_string());
        }
    });
    ui.end_row();
    submit
}

fn log_ui(ui: &mut egui::Ui, log: &mut playterm_config::LogSettings) {
    egui::CollapsingHeader::new(t!("profile_form.session_log")).id_salt("log-settings").default_open(log.auto).show(ui, |ui| {
        ui.checkbox(&mut log.auto, t!("profile_form.start_logging_automatically_on_connect"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut log.raw, false, t!("profile_form.text_control_characters_removed"));
            ui.radio_value(&mut log.raw, true, t!("profile_form.raw_bytes_as_received"));
        });
        ui.add_enabled(!log.raw, egui::Checkbox::new(&mut log.timestamps, t!("profile_form.timestamp_each_line")));
        ui.horizontal(|ui| {
            ui.label(t!("common.folder"));
            let default_dir = crate::logging::default_log_dir().display().to_string();
            ui.add(egui::TextEdit::singleline(&mut log.dir).hint_text(default_dir).desired_width(240.0));
            if ui.add(icon::button(icon::BROWSE, t!("profile_form.browse"))).clicked()
                && let Some(dir) = crate::picker::Picker::new(crate::picker::Kind::Folder, t!("profile_form.log_folder")).pick_one()
            {
                log.dir = dir.display().to_string();
            }
        });
    });
}

fn size_label(size: (u16, u16)) -> String {
    if size.0 == 0 || size.1 == 0 {
        t!("profile_form.fit_the_tab_follows_the_window_size").into()
    } else {
        format!("{} × {}", size.0, size.1)
    }
}

/// Desktop size and clipboard of an RDP site.
fn rdp_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t!("profile_form.screen_size"));
    let current = (rdp.width, rdp.height);
    egui::ComboBox::from_id_salt("rdp-size").width(260.0).selected_text(size_label(current)).show_ui(ui, |ui| {
        for size in playterm_net::RdpSettings::SIZES {
            if ui.selectable_label(current == size, size_label(size)).clicked() {
                (rdp.width, rdp.height) = size;
            }
        }
    });
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.clipboard, t!("profile_form.share_the_clipboard_copy_and_paste"));
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.sound, t!("profile_form.play_the_remote_computer_s_sound_on_this"));
    ui.end_row();

    // The device redirection backend is Windows-only.
    if cfg!(windows) {
        rdp_device_rows(ui, rdp);
    }

    rdp_gateway_rows(ui, rdp);
}

/// The RD Gateway to connect through, and its own user when it does not take the site's.
fn rdp_gateway_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t!("profile_form.rd_gateway"));
    ui.add(egui::TextEdit::singleline(&mut rdp.gateway).hint_text(t!("profile_form.none_connect_directly")).desired_width(220.0))
        .on_hover_text(t!("profile_form.gateway_example_com_or_gateway_example_com_8443"));
    ui.end_row();

    ui.label(t!("profile_form.gateway_user"));
    ui.add_enabled(
        rdp.gateway().is_some(),
        egui::TextEdit::singleline(&mut rdp.gateway_user)
            .hint_text(t!("profile_form.same_user_and_password_as_the_site"))
            .desired_width(220.0),
    )
    .on_hover_text(t!("profile_form.only_for_logging_on_to_the_gateway_with"));
    ui.end_row();
}

/// The folder shown as a drive in the remote desktop, and the printer.
fn rdp_device_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t!("profile_form.shared_folder"));
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut rdp.drive_folder).hint_text(t!("profile_form.not_shared")).desired_width(220.0))
            .on_hover_text(t!("profile_form.shown_as_a_drive_in_this_pc_of"));
        if ui.add(icon::button(icon::BROWSE, t!("profile_form.browse"))).clicked()
            && let Some(dir) = crate::picker::Picker::new(crate::picker::Kind::Folder, t!("profile_form.shared_folder")).pick_one()
        {
            rdp.drive_folder = dir.display().to_string();
        }
    });
    ui.end_row();

    ui.label("");
    ui.add_enabled(
        rdp.shared_folder().is_some(),
        egui::Checkbox::new(&mut rdp.drive_read_only, t!("profile_form.read_only_cannot_be_changed_remotely")),
    );
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.printer, t!("profile_form.print_on_this_pc_s_default_printer"))
        .on_hover_text(t!("profile_form.the_printer_appears_only_if_the_remote_computer"));
    ui.end_row();
}

/// Swatches to pick the site's tab color, "none" first.
fn tab_color_row(ui: &mut egui::Ui, color: &mut Option<TabColor>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let none = ui.add(egui::Button::selectable(color.is_none(), t!("common.none")));
        if none.clicked() {
            *color = None;
        }
        for c in TabColor::ALL {
            let fill = crate::menu::color32(c);
            let selected = *color == Some(c);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect.shrink(2.0), 3.0, fill);
            if selected || response.hovered() {
                let stroke = egui::Stroke::new(2.0, ui.visuals().strong_text_color());
                painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);
            }
            if response.on_hover_text(c.name()).clicked() {
                *color = Some(c);
            }
        }
    });
}

fn keepalive_ui(ui: &mut egui::Ui, k: &mut KeepAlive, protocol: Protocol) {
    let mode_name = |m: KeepAliveMode| match m {
        KeepAliveMode::Off => t!("common.off"),
        KeepAliveMode::Protocol => t!("profile_form.protocol_keep_alive"),
        KeepAliveMode::Text => t!("profile_form.type_text"),
    };
    let title = match k.mode {
        KeepAliveMode::Off => t!("profile_form.keep_alive_off").to_owned(),
        m => tr!("profile_form.keep_alive_s", arg0 = mode_name(m), arg1 = k.interval),
    };
    egui::CollapsingHeader::new(title).id_salt("keepalive").show(ui, |ui| {
        ui.horizontal(|ui| {
            for m in KeepAliveMode::ALL {
                ui.radio_value(&mut k.mode, m, mode_name(m));
            }
        });
        ui.weak(match k.mode {
            KeepAliveMode::Off => t!("profile_form.nothing_is_sent"),
            KeepAliveMode::Protocol => t!("profile_form.sends_ssh_keepalives_telnet_nop_stops_firewalls_routers"),
            KeepAliveMode::Text => t!("profile_form.types_the_text_below_to_stop_devices_and"),
        });
        ui.add_enabled_ui(k.mode != KeepAliveMode::Off, |ui| {
            ui.horizontal(|ui| {
                ui.label(t!("profile_form.interval"));
                ui.add(
                    egui::DragValue::new(&mut k.interval)
                        .range(KeepAlive::MIN_INTERVAL..=KeepAlive::MAX_INTERVAL)
                        .speed(1)
                        .suffix(t!("common.s")),
                );
                ui.weak(t!("profile_form.after_this_long_with_nothing_sent"));
            });
            if k.mode == KeepAliveMode::Text {
                ui.horizontal(|ui| {
                    ui.label(t!("profile_form.text"));
                    ui.add(egui::TextEdit::singleline(&mut k.text).hint_text(KeepAlive::DEFAULT_TEXT).desired_width(160.0));
                    if ui.add(icon::button(icon::RESET, t!("profile_form.default")).small()).clicked() {
                        k.text = KeepAlive::DEFAULT_TEXT.into();
                    }
                });
                if let Err(e) = decode_escapes(&k.text) {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            if protocol == Protocol::Serial && k.mode == KeepAliveMode::Protocol {
                ui.colored_label(ui.visuals().warn_fg_color, t!("profile_form.serial_lines_have_no_protocol_keep_alive_use"));
            }
        });
    });
}

fn validate_keepalive(k: &KeepAlive) -> Result<KeepAlive, String> {
    let mut k = k.clone();
    k.interval = k.interval.clamp(KeepAlive::MIN_INTERVAL, KeepAlive::MAX_INTERVAL);
    if k.mode == KeepAliveMode::Text {
        let bytes = decode_escapes(&k.text).map_err(|e| tr!("profile_form.keep_alive_text", e = e))?;
        if bytes.is_empty() {
            return Err(t!("profile_form.enter_the_keep_alive_text_to_send").into());
        }
    }
    Ok(k)
}

/// The macro a site runs once connected (and logged in automatically, if it does).
fn after_connect_row(ui: &mut egui::Ui, p: &mut SessionProfile, macros: &[String]) {
    ui.label(t!("profile_form.macro_after_connecting"));
    let none = t!("common.none");
    let current = p.after_connect_macro.clone();
    let shown = if current.is_empty() {
        none.to_owned()
    } else if macros.contains(&current) {
        current.clone()
    } else {
        tr!("profile_form.no_such_macro", current = current)
    };
    egui::ComboBox::from_id_salt("after-connect-macro")
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut p.after_connect_macro, String::new(), none);
            for name in macros {
                ui.selectable_value(&mut p.after_connect_macro, name.clone(), name);
            }
        })
        .response
        .on_hover_text(t!("profile_form.runs_this_macro_once_connected_and_the_automatic"));
    ui.end_row();
}

/// Which optional parts of the form to show.
/// The rows most connections leave as they are: key file and agent (SSH), the remote desktop
/// settings, encoding and terminal type. Returns whether Enter was pressed in one of them.
fn advanced_rows(ui: &mut egui::Ui, p: &mut SessionProfile) -> bool {
    let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let ssh = p.protocol == Protocol::Ssh;
    let rdp = p.protocol == Protocol::Rdp;
    let mut submit = false;
    if ssh {
        ui.label(t!("profile_form.key_file"));
        ui.horizontal(|ui| {
            let key = ui.add(
                egui::TextEdit::singleline(&mut p.identity_file)
                    .hint_text(t!("profile_form.none_agent_and_default_keys"))
                    .desired_width(220.0),
            );
            submit |= enter(ui, &key);
            if ui.add(icon::button(icon::BROWSE, t!("profile_form.browse"))).clicked()
                && let Some(path) = pick_key_file(&p.identity_file)
            {
                p.identity_file = path.display().to_string();
            }
        });
        ui.end_row();

        ui.label(t!("profile_form.user_certificate"));
        ui.horizontal(|ui| {
            let cert = ui.add(
                egui::TextEdit::singleline(&mut p.certificate_file)
                    .hint_text(t!("profile_form.auto_key_path_cert_pub"))
                    .desired_width(220.0),
            );
            submit |= enter(ui, &cert);
            if ui.add(icon::button(icon::BROWSE, t!("profile_form.browse"))).clicked()
                && let Some(path) = crate::picker::Picker::new(crate::picker::Kind::File, t!("profile_form.choose_an_ssh_user_certificate"))
                    .directory(expand_home("~/.ssh"))
                    .filter("OpenSSH (*.pub)", &["pub"])
                    .pick_one()
            {
                p.certificate_file = path.display().to_string();
            }
        })
        .response
        .on_hover_text(t!("profile_form.signed_with_the_matching_private_key_or_ssh"));
        ui.end_row();

        ui.label(t!("profile_form.jump_host"));
        let jump = ui
            .add(
                egui::TextEdit::singleline(&mut p.jump_host)
                    .hint_text(t!("profile_form.none_a_site_name_or_user_host_port"))
                    .desired_width(260.0),
            )
            .on_hover_text(t!("profile_form.connects_through_this_server_openssh_proxyjump_j_with"));
        submit |= enter(ui, &jump);
        ui.end_row();

        ui.label(t!("profile_form.x11_forwarding"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut p.x11_forwarding, "").on_hover_text(t!("profile_form.shows_x_programs_of_the_server_xterm_installers"));
            let hint = tr!("profile_form.display_default", arg0 = playterm_net::x11::default_display());
            let display =
                ui.add_enabled(p.x11_forwarding, egui::TextEdit::singleline(&mut p.x11_display).hint_text(hint).desired_width(180.0));
            submit |= enter(ui, &display);
            ui.add_enabled(p.x11_forwarding, egui::Checkbox::new(&mut p.x11_trusted, t!("profile_form.trusted_y")))
                .on_hover_text(t!("profile_form.on_the_server_s_x_programs_can_reach"));
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut p.use_agent, tr!("profile_form.use_the_ssh_agent", arg0 = crate::platform::ssh_agents()))
            .on_hover_text(t!("profile_form.use_agent_keys_and_certificates_to_log_in"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut p.agent_forwarding, t!("profile_form.ssh_agent_forwarding_a"));
        ui.end_row();
        if p.agent_forwarding {
            ui.label("");
            ui.vertical(|ui| {
                ui.set_max_width(400.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(t!("profile_form.the_remote_server_can_request_authentication_signatures_from"))
                            .color(egui::Color32::YELLOW),
                    )
                    .wrap(),
                );
            });
            ui.end_row();
        }

        ui.label("");
        ui.checkbox(&mut p.ssh_legacy_algorithms, t!("profile_form.legacy_ssh_equipment_compatibility"));
        ui.end_row();
        if p.ssh_legacy_algorithms {
            ui.label("");
            ui.colored_label(egui::Color32::YELLOW, t!("profile_form.allows_sha_1_key_exchange_mac_and_aes"));
            ui.end_row();
        }
    }

    if rdp {
        rdp_rows(ui, &mut p.rdp);
    } else {
        ui.label(t!("profile_form.encoding")).on_hover_text(t!("profile_form.auto_detect_distinguishes_utf_8_and_cp949_select"));
        egui::ComboBox::from_id_salt("charset").selected_text(p.charset.to_string()).show_ui(ui, |ui| {
            for c in Charset::ALL {
                ui.selectable_value(&mut p.charset, c, c.to_string());
            }
        });
        ui.end_row();

        ui.label(t!("profile_form.terminal_type"));
        ui.text_edit_singleline(&mut p.term_type);
        ui.end_row();
    }
    submit
}

/// What `user@host:port` typed or pasted into the host field holds. A bare IPv6 address
/// (`fe80::1`) is a host, not host and port; `[fe80::1]:2222` has a port.
#[derive(Debug, PartialEq, Eq)]
pub struct Address {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

pub fn split_address(text: &str) -> Address {
    let text = text.trim();
    let (user, rest) = match text.rsplit_once('@') {
        Some((u, h)) if !u.is_empty() => (Some(u.to_owned()), h),
        _ => (None, text),
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        // [v6] or [v6]:port
        match inner.split_once(']') {
            Some((h, after)) => (h.to_owned(), after.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (rest.to_owned(), None),
        }
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':').unwrap_or((rest, ""));
        match p.parse::<u16>() {
            Ok(port) if port > 0 => (h.to_owned(), Some(port)),
            _ => (rest.to_owned(), None),
        }
    } else {
        (rest.to_owned(), None)
    };
    Address { user, host, port }
}

pub struct FormOptions<'a> {
    /// Name and folder fields (session editor).
    pub meta: bool,
    /// "Save password" checkbox (session editor).
    pub save_password: bool,
    /// Existing folders offered in the folder picker.
    pub folders: &'a [String],
    /// Names of the macros a site can run once connected.
    pub macros: &'a [String],
}

/// Editable text form of a [`ForwardRule`] (ports stay strings while typing).
#[derive(Clone, PartialEq)]
struct ForwardRow {
    kind: ForwardKind,
    bind_host: String,
    bind_port: String,
    dest_host: String,
    dest_port: String,
}

impl ForwardRow {
    fn from_rule(r: &ForwardRule) -> Self {
        Self {
            kind: r.kind,
            bind_host: r.bind_host.clone(),
            bind_port: r.bind_port.to_string(),
            dest_host: r.dest_host.clone(),
            dest_port: if r.kind == ForwardKind::Dynamic { String::new() } else { r.dest_port.to_string() },
        }
    }

    fn to_rule(&self, n: usize) -> Result<ForwardRule, String> {
        let port = |s: &str, what: &str| {
            s.trim()
                .parse::<u16>()
                .ok()
                .filter(|&p| p > 0)
                .ok_or_else(|| tr!("profile_form.port_forwarding_invalid_port", n = n, what = what))
        };
        let mut rule = ForwardRule {
            kind: self.kind,
            bind_host: self.bind_host.trim().to_owned(),
            bind_port: port(&self.bind_port, t!("profile_form.listening"))?,
            ..Default::default()
        };
        if self.kind != ForwardKind::Dynamic {
            rule.dest_host = self.dest_host.trim().to_owned();
            if rule.dest_host.is_empty() {
                return Err(tr!("profile_form.port_forwarding_enter_the_destination_host", n = n));
            }
            rule.dest_port = port(&self.dest_port, t!("profile_form.destination"))?;
        }
        Ok(rule)
    }
}

pub struct ProfileForm {
    pub profile: SessionProfile,
    port: String,
    forwards: Vec<ForwardRow>,
    password: Zeroizing<String>,
    /// The vault already holds a password for this profile.
    has_saved_password: bool,
    baud: String,
    /// COM ports found on this machine; scanned when the serial fields are first shown.
    serial_ports: Option<Vec<SerialPortEntry>>,
    error: Option<String>,
    focus_requested: bool,
    /// Editable state when the form was opened, to detect unsaved changes.
    initial: (SessionProfile, String, String, Vec<ForwardRow>),
}

impl ProfileForm {
    pub fn new(mut profile: SessionProfile, has_saved_password: bool) -> Self {
        let error = profile.ensure_id().err().map(|e| e.to_string());
        let port = profile.port.to_string();
        let baud = profile.serial.baud.to_string();
        let forwards: Vec<ForwardRow> = profile.forwards.iter().map(ForwardRow::from_rule).collect();
        Self {
            initial: (profile.clone(), port.clone(), baud.clone(), forwards.clone()),
            port,
            forwards,
            profile,
            password: Zeroizing::default(),
            has_saved_password,
            baud,
            serial_ports: None,
            error,
            focus_requested: false,
        }
    }

    /// The profile as it was when the form was opened.
    pub fn initial_profile(&self) -> &SessionProfile {
        &self.initial.0
    }

    /// Whether anything was changed since the form was opened.
    pub fn is_dirty(&self) -> bool {
        let (profile, port, baud, forwards) = &self.initial;
        !self.password.is_empty() || (&self.profile, &self.port, &self.baud, &self.forwards) != (profile, port, baud, forwards)
    }

    /// Draws the form. Returns `true` if Enter was pressed in one of its text fields.
    pub fn ui(&mut self, ui: &mut egui::Ui, opts: &FormOptions<'_>) -> bool {
        let mut submit = false;
        let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let p = &mut self.profile;

        egui::Grid::new("profile-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            if opts.meta {
                ui.label(t!("common.name"));
                let name = ui.add(egui::TextEdit::singleline(&mut p.name).hint_text(t!("profile_form.user_host_if_empty")));
                if !self.focus_requested {
                    name.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &name);
                ui.end_row();

                ui.label(t!("common.folder"));
                ui.horizontal(|ui| {
                    let folder = ui.add(egui::TextEdit::singleline(&mut p.folder).hint_text(t!("common.none")).desired_width(150.0));
                    submit |= enter(ui, &folder);
                    let existing: Vec<&String> = opts.folders.iter().filter(|f| !f.is_empty()).collect();
                    if !existing.is_empty() {
                        ui.menu_button(t!("profile_form.choose"), |ui| {
                            for f in existing {
                                if ui.button(f).clicked() {
                                    p.folder = f.clone();
                                    ui.close();
                                }
                            }
                        });
                    }
                });
                ui.end_row();

                ui.label(t!("common.tab_color"));
                ui.horizontal(|ui| {
                    tab_color_row(ui, &mut p.tab_color);
                    // Named, so it reads as the label even when filled in (#151).
                    ui.add_space(8.0);
                    ui.label(t!("profile_form.label"));
                    ui.add(
                        egui::TextEdit::singleline(&mut p.tab_label)
                            .hint_text(t!("profile_form.label_e_g_prod"))
                            .char_limit(12)
                            .desired_width(110.0),
                    )
                    .on_hover_text(t!("profile_form.shown_with_the_tab_the_connection_line_and"));
                });
                ui.end_row();
            }

            ui.label(t!("profile_form.protocol"));
            ui.horizontal(|ui| {
                for proto in Protocol::ALL {
                    let old = p.protocol;
                    if ui.radio_value(&mut p.protocol, proto, proto.to_string()).changed() {
                        // Focus the first field of the newly shown fields (host or COM port).
                        self.focus_requested = opts.meta;
                        if self.port == old.default_port().to_string() {
                            self.port = proto.default_port().to_string();
                        }
                    }
                }
            });
            ui.end_row();

            if p.protocol == Protocol::Serial {
                let ports = self.serial_ports.get_or_insert_with(list_ports);
                submit |= serial_rows(ui, &mut p.serial, &mut self.baud, ports, &mut self.focus_requested);
            } else {
                ui.label(t!("profile_form.host"));
                let host = ui.add(egui::TextEdit::singleline(&mut p.host).hint_text(t!("profile_form.example_com_ip_or_user_host_port")));
                if host.changed() {
                    // user@host:port pasted or typed: fill in the user and port fields.
                    let address = split_address(&p.host);
                    if let Some(user) = address.user {
                        p.username = user;
                    }
                    if let Some(port) = address.port {
                        self.port = port.to_string();
                    }
                    if address.host != p.host {
                        p.host = address.host;
                    }
                }
                if !self.focus_requested {
                    host.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &host);
                ui.end_row();

                ui.label(t!("profile_form.port"));
                let port = ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                submit |= enter(ui, &port);
                ui.end_row();
            }

            // Telnet and serial consoles log in at a prompt: the site manager keeps a user name
            // and password for the automatic login.
            let ssh = p.protocol == Protocol::Ssh;
            let rdp = p.protocol == Protocol::Rdp;
            if ssh || rdp || opts.meta {
                ui.label(t!("common.user"));
                let hint = if ssh {
                    ""
                } else if rdp {
                    t!("profile_form.user_or_domain_user")
                } else {
                    t!("profile_form.in_the_automatic_login")
                };
                let user = ui.add(egui::TextEdit::singleline(&mut p.username).hint_text(hint));
                submit |= enter(ui, &user);
                ui.end_row();

                ui.label(t!("common.password"));
                let hint = if self.has_saved_password && p.save_password {
                    t!("profile_form.saved_password_kept_type_to_change")
                } else if ssh || rdp {
                    t!("profile_form.asked_on_connect_if_empty")
                } else {
                    t!("profile_form.in_the_automatic_login_2")
                };
                let pw = ui.add(egui::TextEdit::singleline(&mut *self.password).password(true).hint_text(hint));
                crate::dialogs::secret_field(ui, &pw);
                submit |= enter(ui, &pw);
                ui.end_row();

                if opts.save_password {
                    ui.label("");
                    ui.checkbox(&mut p.save_password, t!("profile_form.save_password_encrypted_with_the_master_password"));
                    ui.end_row();
                }
            }

            if opts.meta {
                submit |= advanced_rows(ui, p);
                if p.protocol != Protocol::Rdp {
                    after_connect_row(ui, p, opts.macros);
                }
            }
        });

        if opts.meta {
            if self.profile.protocol == Protocol::Ssh {
                ui.add_space(4.0);
                self.forwards_ui(ui);
            }
        } else {
            // Quick connect: what most connections leave as it is, folded away.
            ui.add_space(4.0);
            egui::CollapsingHeader::new(t!("profile_form.advanced")).id_salt("quick-advanced").show(ui, |ui| {
                egui::Grid::new("profile-form-advanced").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    submit |= advanced_rows(ui, &mut self.profile);
                });
                if self.profile.protocol == Protocol::Ssh {
                    ui.add_space(4.0);
                    self.forwards_ui(ui);
                }
            });
        }
        // A remote desktop has no text to log or prompts to answer.
        if opts.meta && self.profile.protocol != Protocol::Rdp {
            self.auto_login_ui(ui);
            keepalive_ui(ui, &mut self.profile.keepalive, self.profile.protocol);
            log_ui(ui, &mut self.profile.log);
        }

        if let Some(err) = &self.error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        submit
    }

    fn auto_login_ui(&mut self, ui: &mut egui::Ui) {
        let save_password = self.profile.save_password;
        let a = &mut self.profile.auto_login;
        let title = if a.enabled {
            tr!("profile_form.automatic_login_steps", arg0 = a.steps.len())
        } else {
            t!("profile_form.automatic_login").to_owned()
        };
        egui::CollapsingHeader::new(title).id_salt("auto-login").default_open(a.enabled).show(ui, |ui| {
            let label = t!("profile_form.type_at_the_prompts_automatically_on_connect");
            if ui.checkbox(&mut a.enabled, label).changed() && a.enabled && a.steps.is_empty() {
                a.steps = AutoLogin::standard_steps();
            }
            ui.colored_label(ui.visuals().warn_fg_color, t!("profile_form.send_text_is_saved_as_plain_text_in"));
            ui.add_enabled_ui(a.enabled, |ui| {
                ui.weak(t!("profile_form.when_a_screen_line_ends_with_wait_for"));
                let mut remove = None;
                egui::Grid::new("login-steps").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                    ui.weak("");
                    ui.weak(t!("profile_form.wait_for"));
                    ui.weak(t!("profile_form.send"));
                    ui.end_row();
                    for (i, step) in a.steps.iter_mut().enumerate() {
                        ui.label(format!("{}.", i + 1));
                        ui.add(egui::TextEdit::singleline(&mut step.expect).hint_text("login:").desired_width(150.0));
                        ui.add(egui::TextEdit::singleline(&mut step.send).hint_text("{user}").desired_width(170.0));
                        if ui.add(icon::icon_button(icon::DELETE).small()).on_hover_text(t!("common.delete")).clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    a.steps.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui.add(icon::button(icon::ADD, t!("profile_form.add_step"))).clicked() {
                        a.steps.push(LoginStep::default());
                    }
                    if ui.add(icon::button(icon::RESET, t!("profile_form.defaults_login_password"))).clicked() {
                        a.steps = AutoLogin::standard_steps();
                    }
                });
                if a.uses_password() && !save_password {
                    ui.colored_label(ui.visuals().warn_fg_color, t!("profile_form.turn_on_save_password_above_to_use"));
                }
            });
        });
    }

    fn forwards_ui(&mut self, ui: &mut egui::Ui) {
        let title = tr!("profile_form.port_forwarding", arg0 = self.forwards.len());
        egui::CollapsingHeader::new(title).id_salt("forwards").default_open(!self.forwards.is_empty()).show(ui, |ui| {
            let mut remove = None;
            egui::Grid::new("forward-rows").num_columns(5).spacing([6.0, 4.0]).show(ui, |ui| {
                for (i, row) in self.forwards.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("fwd-kind", i)).width(110.0).selected_text(row.kind.to_string()).show_ui(ui, |ui| {
                        for k in ForwardKind::ALL {
                            ui.selectable_value(&mut row.kind, k, k.to_string());
                        }
                    });
                    ui.horizontal(|ui| {
                        let hint = if row.kind == ForwardKind::Remote { t!("profile_form.server") } else { "127.0.0.1" };
                        ui.add(egui::TextEdit::singleline(&mut row.bind_host).hint_text(hint).desired_width(80.0));
                        ui.label(":");
                        ui.add(egui::TextEdit::singleline(&mut row.bind_port).hint_text(t!("common.port")).desired_width(45.0));
                    });
                    if row.kind == ForwardKind::Dynamic {
                        ui.label("SOCKS5");
                        ui.label("");
                    } else {
                        ui.label("→");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut row.dest_host).hint_text(t!("profile_form.host_2")).desired_width(90.0));
                            ui.label(":");
                            ui.add(egui::TextEdit::singleline(&mut row.dest_port).hint_text(t!("common.port")).desired_width(45.0));
                        });
                    }
                    if ui.add(icon::icon_button(icon::DELETE).small()).on_hover_text(t!("common.delete")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                self.forwards.remove(i);
            }
            if ui.add(icon::button(icon::ADD, t!("profile_form.add_forwarding"))).clicked() {
                self.forwards.push(ForwardRow {
                    kind: ForwardKind::Local,
                    bind_host: String::new(),
                    bind_port: String::new(),
                    dest_host: "localhost".into(),
                    dest_port: String::new(),
                });
            }
        });
    }

    /// Checks the input. On success returns the profile and the typed password, if any.
    pub fn validate(&mut self) -> Option<(SessionProfile, Option<Zeroizing<String>>)> {
        match self.try_validate() {
            Ok(v) => Some(v),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    fn try_validate(&self) -> Result<(SessionProfile, Option<Zeroizing<String>>), String> {
        let mut p = self.profile.clone();
        p.ensure_id().map_err(|e| e.to_string())?;
        p.name = p.name.trim().to_owned();
        p.folder = p.folder.trim().to_owned();
        p.host = p.host.trim().to_owned();
        p.username = p.username.trim().to_owned();
        p.identity_file = p.identity_file.trim().trim_matches('"').to_owned();
        p.certificate_file = p.certificate_file.trim().trim_matches('"').to_owned();
        if p.protocol != Protocol::Ssh {
            p.certificate_file.clear();
            p.agent_forwarding = false;
        }
        p.log.dir = p.log.dir.trim().trim_matches('"').to_owned();
        p.auto_login = validate_auto_login(&p.auto_login)?;
        p.keepalive = validate_keepalive(&p.keepalive)?;
        let password = (!self.password.is_empty()).then(|| self.password.clone());
        if p.protocol == Protocol::Serial {
            p.serial.port = p.serial.port.trim().to_uppercase();
            if p.serial.port.is_empty() {
                return Err(t!("profile_form.enter_a_serial_port_e_g_com3").into());
            }
            p.serial.baud = self
                .baud
                .trim()
                .parse()
                .ok()
                .filter(|&b: &u32| b > 0)
                .ok_or(t!("profile_form.the_speed_baud_must_be_a_positive_integer"))?;
            p.host = p.serial.port.clone();
            p.port = 0;
            p.identity_file.clear();
            p.forwards.clear();
            if p.term_type.trim().is_empty() {
                p.term_type = "xterm-256color".into();
            }
            // The user name and password are for the automatic login at the console prompt.
            return Ok((p, password));
        }
        if p.host.is_empty() {
            return Err(t!("profile_form.enter_a_host").into());
        }
        p.port = self.port.trim().parse().ok().filter(|&port: &u16| port > 0).ok_or(t!("profile_form.the_port_must_be_a_number_from_1"))?;
        if p.term_type.trim().is_empty() {
            p.term_type = "xterm-256color".into();
        }
        match p.protocol {
            Protocol::Ssh => {
                if p.username.is_empty() {
                    return Err(t!("profile_form.enter_the_ssh_user_name").into());
                }
                p.forwards = self.forwards.iter().enumerate().map(|(i, r)| r.to_rule(i + 1)).collect::<Result<_, _>>()?;
            }
            Protocol::Serial => unreachable!("handled above"),
            Protocol::Rdp => {
                p.identity_file.clear();
                p.forwards.clear();
            }
            Protocol::Telnet => {
                // The user name and password are for the automatic login at the remote prompt.
                p.identity_file.clear();
                p.forwards.clear();
            }
        }
        Ok((p, password))
    }
}

/// Drops empty rows; a step that types something must say what to wait for.
fn validate_auto_login(a: &AutoLogin) -> Result<AutoLogin, String> {
    let steps: Vec<LoginStep> = a
        .steps
        .iter()
        .map(|s| LoginStep { expect: s.expect.trim().to_owned(), send: s.send.clone() })
        .filter(|s| !s.expect.is_empty() || !s.send.trim().is_empty())
        .collect();
    if let Some(n) = steps.iter().position(|s| s.expect.is_empty()) {
        return Err(tr!("profile_form.automatic_login_step_enter_the_text_to_wait", arg0 = n + 1));
    }
    if a.enabled && steps.is_empty() {
        return Err(t!("profile_form.add_at_least_one_automatic_login_step_or").into());
    }
    Ok(AutoLogin { enabled: a.enabled, steps })
}

#[cfg(test)]
mod tests {

    #[test]
    fn splits_pasted_addresses() {
        let a = |user: Option<&str>, host: &str, port: Option<u16>| Address { user: user.map(str::to_owned), host: host.to_owned(), port };
        assert_eq!(split_address("web01"), a(None, "web01", None));
        assert_eq!(split_address(" deploy@web01 "), a(Some("deploy"), "web01", None));
        assert_eq!(split_address("web01:2222"), a(None, "web01", Some(2222)));
        assert_eq!(split_address("deploy@10.0.0.5:2222"), a(Some("deploy"), "10.0.0.5", Some(2222)));
        assert_eq!(split_address("fe80::1"), a(None, "fe80::1", None), "bare IPv6: no port");
        assert_eq!(split_address("root@[fe80::1]:2222"), a(Some("root"), "fe80::1", Some(2222)));
        assert_eq!(split_address("[::1]"), a(None, "::1", None));
        assert_eq!(split_address("host:abc"), a(None, "host:abc", None), "not a port: left as typed");
        assert_eq!(split_address("@host"), a(None, "@host", None));
    }
    use super::*;

    #[test]
    fn parses_urls() {
        let r = parse_url("ssh://root@10.0.0.1:2222").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Ssh, "10.0.0.1", 2222, "root"));

        let r = parse_url("telnet://router").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Telnet, "router", 23));

        let r = parse_url("serial://COM3:115200").unwrap();
        assert_eq!((r.protocol, r.serial.port.as_str(), r.serial.baud), (Protocol::Serial, "COM3", 115200));
        let r = parse_url("rdp://admin@win01").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Rdp, "win01", 3389, "admin"));
        assert_eq!(parse_url("rdp://10.0.0.9:3390").unwrap().port, 3390);
        assert_eq!(parse_url("serial://COM1").unwrap().serial.baud, 9600);
        assert!(parse_url("serial://COM1:fast").is_err());

        let r = parse_url("admin@[::1]").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Ssh, "::1", 22));
    }

    #[test]
    fn rejects_bad_urls() {
        assert!(parse_url("ssh://host").is_err());
        assert!(parse_url("ftp://host").is_err());
        assert!(parse_url("telnet://host:0").is_err());
        assert!(parse_url("telnet://:23").is_err());
    }

    #[test]
    fn home_expansion() {
        let home = directories::BaseDirs::new().unwrap().home_dir().to_owned();
        assert_eq!(expand_home("~/.ssh/id_rsa"), home.join(".ssh").join("id_rsa"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home(r"C:\keys.ppk"), PathBuf::from(r"C:\keys.ppk"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn validation() {
        let mut form = ProfileForm::new(SessionProfile { host: " web ".into(), ..SessionProfile::new().unwrap() }, false);
        assert!(form.validate().is_none(), "SSH needs a user name");

        form.profile.username = "me".into();
        *form.password = "pw".into();
        let (p, pw) = form.validate().unwrap();
        assert_eq!((p.host.as_str(), p.port), ("web", 22));
        assert_eq!(pw.as_deref().map(|s| s.as_str()), Some("pw"));

        form.profile.protocol = Protocol::Telnet;
        form.profile.save_password = true;
        form.profile.identity_file = "key".into();
        let (p, pw) = form.validate().unwrap();
        // Telnet keeps the user name and password for the automatic login; keys are SSH only.
        assert_eq!(pw.as_deref().map(|s| s.as_str()), Some("pw"));
        assert!(p.save_password && p.username == "me" && p.identity_file.is_empty());

        form.port = "70000".into();
        assert!(form.validate().is_none());
    }

    #[test]
    fn keepalive_validation() {
        let k = |mode, interval, text: &str| KeepAlive { mode, interval, text: text.into() };
        assert_eq!(validate_keepalive(&k(KeepAliveMode::Protocol, 1, "")).unwrap().interval, KeepAlive::MIN_INTERVAL);
        assert_eq!(validate_keepalive(&k(KeepAliveMode::Protocol, 99999, "")).unwrap().interval, KeepAlive::MAX_INTERVAL);
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "\\r")).is_ok());
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "")).is_err());
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "\\x1")).is_err());
        assert!(validate_keepalive(&k(KeepAliveMode::Off, 60, "\\x1")).is_ok(), "the text only matters when used");
    }

    #[test]
    fn dirty_tracking() {
        let profile = SessionProfile { host: "h".into(), username: "u".into(), ..SessionProfile::new().unwrap() };
        let mut form = ProfileForm::new(profile, false);
        assert!(!form.is_dirty());
        form.port = "2222".into();
        assert!(form.is_dirty());
        form.port = "22".into();
        assert!(!form.is_dirty(), "changing a field back is not a change");
        *form.password = "pw".into();
        assert!(form.is_dirty(), "a typed password is a change");
        form.password.clear();
        form.profile.protocol = Protocol::Telnet;
        assert!(form.is_dirty());
    }

    #[test]
    fn serial_validation() {
        let profile = SessionProfile {
            protocol: Protocol::Serial,
            username: "root".into(),
            serial: SerialSettings { port: " com4 ".into(), ..Default::default() },
            ..SessionProfile::new().unwrap()
        };
        let mut form = ProfileForm::new(profile, false);
        form.baud = "115200".into();
        let (p, pw) = form.validate().unwrap();
        assert_eq!((p.serial.port.as_str(), p.serial.baud, p.host.as_str()), ("COM4", 115200, "COM4"));
        assert!(pw.is_none() && p.username == "root", "kept for the console login");

        form.baud = "0".into();
        assert!(form.validate().is_none());
        form.baud = "9600".into();
        form.profile.serial.port.clear();
        assert!(form.validate().is_none());
    }

    #[test]
    fn auto_login_validation() {
        let step = |e: &str, s: &str| LoginStep { expect: e.into(), send: s.into() };
        let mut a = AutoLogin { enabled: true, steps: vec![step(" login: ", "{user}"), step("", "  "), step("Password:", "{password}")] };
        let v = validate_auto_login(&a).unwrap();
        assert_eq!(v.steps, vec![step("login:", "{user}"), step("Password:", "{password}")], "trimmed, blank row dropped");

        a.steps.push(step("", "show run"));
        assert!(validate_auto_login(&a).unwrap_err().contains("3단계"));

        let empty = AutoLogin { enabled: true, steps: vec![] };
        assert!(validate_auto_login(&empty).is_err());
        assert!(validate_auto_login(&AutoLogin::default()).is_ok(), "off and empty is fine");
    }

    #[test]
    fn forward_rows_validate() {
        let rule = ForwardRule::parse(ForwardKind::Local, "8080:db:5432").unwrap();
        let profile = SessionProfile {
            host: "h".into(),
            username: "u".into(),
            forwards: vec![rule.clone(), ForwardRule::parse(ForwardKind::Dynamic, "1080").unwrap()],
            ..SessionProfile::new().unwrap()
        };
        let mut form = ProfileForm::new(profile, false);
        assert_eq!(form.validate().unwrap().0.forwards[0], rule);

        form.forwards[0].dest_port = "x".into();
        assert!(form.validate().is_none());
        form.forwards[0].dest_port = "5432".into();
        form.forwards[0].dest_host = " ".into();
        assert!(form.validate().is_none());
    }
}
