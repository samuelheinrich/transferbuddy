//! Always-visible copy URL selection, using cached network facts.
use super::*;

pub(super) struct LinkSummary {
    pub(super) label: String,
    pub(super) warning: bool,
    protocol: Protocol,
    rows: Vec<(Protocol, Result<engine::CopyEndpoint, String>)>,
}
impl Desktop {
    pub(super) fn link_summary(&self) -> LinkSummary {
        let protocol = if self.tab == Tab::Upgrade {
            self.upgrade_protocol
        } else {
            self.snapshot
                .devices
                .iter()
                .find(|d| Some(d.id) == self.device)
                .map(|d| d.protocol)
                .unwrap_or(Protocol::Sftp)
        };
        let rows: Vec<_> = engine::PROTOCOL_PRIORITY
            .into_iter()
            .map(|p| {
                let bind = self
                    .snapshot
                    .services
                    .iter()
                    .find(|s| s.id == cisco::service_of(p))
                    .map(|s| s.settings.bind.as_str())
                    .unwrap_or("0.0.0.0");
                (
                    p,
                    engine::cached_copy_endpoint(
                        bind,
                        self.snapshot.advertise.as_deref(),
                        self.device,
                        &self.snapshot.network,
                    ),
                )
            })
            .collect();
        let endpoint = rows
            .iter()
            .find(|(p, _)| *p == protocol)
            .unwrap()
            .1
            .as_ref();
        let ips: std::collections::HashSet<_> = rows
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok().map(|e| e.ip))
            .collect();
        let pinned_unavailable = self.snapshot.advertise.as_deref().is_some_and(|choice| {
            engine::cached_copy_endpoint(
                "0.0.0.0",
                Some(choice),
                self.device,
                &self.snapshot.network,
            )
            .is_err()
        });
        let label =
            if endpoint.is_err() && (self.device.is_some() || self.snapshot.advertise.is_some()) {
                "Links: Unavailable".into()
            } else if ips.len() > 1 {
                format!(
                    "Links: Multiple IPs{}",
                    endpoint
                        .ok()
                        .map(|e| format!(" · {}", e.ip))
                        .unwrap_or_default()
                )
            } else if let Ok(endpoint) = endpoint {
                format!(
                    "Links: {} · {}{}",
                    endpoint.interface.as_deref().unwrap_or("IP"),
                    endpoint.ip,
                    if endpoint.fixed_bind {
                        " · fixed bind"
                    } else {
                        ""
                    }
                )
            } else if self.snapshot.advertise.is_none() && self.device.is_none() {
                "Links: Auto · per device".into()
            } else {
                "Links: Unavailable".into()
            };
        let warning = pinned_unavailable
            || (endpoint.is_err() && (self.device.is_some() || self.snapshot.advertise.is_some()));
        LinkSummary {
            label,
            warning,
            protocol,
            rows,
        }
    }

    pub(super) fn interface_quick_selector(&mut self, ui: &mut egui::Ui) {
        let summary = self.link_summary();
        let width: f32 = if ui.ctx().content_rect().width() < 700.0 {
            260.0
        } else {
            330.0
        };
        let text = RichText::new(format!("{} ▾", summary.label)).color(if summary.warning {
            design::semantic(ui, AMBER)
        } else {
            ui.visuals().text_color()
        });
        let response = ui.add_sized([width.min(ui.available_width()), ui.spacing().interact_size.y], egui::Button::new(text).truncate())
            .on_hover_text(format!("{}\nCopy URL address for {}. Fixed service binds take priority. Change the interface for new transfers and retries.", summary.label, summary.protocol.label()));
        if response.clicked() {
            self.interface = self.snapshot.advertise.clone().unwrap_or_default();
        }
        self.interface_anchor = Some(response);
    }

    pub(super) fn interface_popup(&mut self, ctx: &egui::Context) {
        if self.obstructed() {
            egui::Popup::close_id(ctx, egui::Id::new("global_interface_popup"));
            return;
        }
        let Some(response) = self.interface_anchor.clone() else {
            return;
        };
        let summary = self.link_summary();
        egui::Popup::menu(&response)
            .id(egui::Id::new("global_interface_popup"))
            .align(egui::RectAlign::BOTTOM_END)
            .layout(egui::Layout::top_down(egui::Align::Min))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .width((ctx.content_rect().width() - 32.0).min(440.0))
            .show(|ui| {
                egui::ScrollArea::vertical().id_salt("quick_interface_content").max_height((ui.ctx().content_rect().height() - response.rect.bottom() - 32.0).max(120.0)).show(ui, |ui| {
                ui.heading("Copy URL interface");
                let name = self.snapshot.devices.iter().find(|d| Some(d.id) == self.device)
                    .map(|d| format!("{} · {} · {}", d.name, d.host, summary.protocol.label()))
                    .unwrap_or_else(|| "No device selected · automatic addresses depend on the destination".into());
                ui.add(egui::Label::new(name).wrap());
                if summary.warning { ui.add(egui::Label::new(RichText::new("The selected interface or route is unavailable. Choose another interface or correct the service bind.").color(design::semantic(ui, AMBER))).wrap()); }
                if ui.selectable_label(self.snapshot.advertise.is_none(), "Automatic · per device").clicked() {
                    self.send(Command::Set(Setting::Advertise(None)));
                }
                egui::ScrollArea::vertical().id_salt("quick_interfaces").max_height(160.0).show(ui, |ui| {
                    let interfaces = self.snapshot.network.interfaces.clone();
                    for iface in interfaces.into_iter().filter(|i| i.ip.is_ipv4() && !i.ip.is_loopback()) {
                        let selected = self.snapshot.advertise.as_deref().is_some_and(|s| s.trim() == iface.name || s.trim() == iface.ip.to_string());
                        if ui.selectable_label(selected, format!("{} · {} · {}", iface.name, iface.ip, iface.kind.label())).clicked() {
                            self.send(Command::Set(Setting::Advertise(Some(iface.name))));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let label = ui.label("Interface or IP");
                    ui.add(egui::TextEdit::singleline(&mut self.interface).id_salt("quick_interface_input").desired_width(175.0)).labelled_by(label.id);
                    if ui.button("Apply address").clicked() {
                        self.send(Command::Set(Setting::Advertise((!self.interface.trim().is_empty()).then(|| self.interface.trim().to_owned()))));
                    }
                });
                ui.separator();
                ui.strong("Effective addresses by protocol");
                for (protocol, result) in &summary.rows {
                    ui.horizontal(|ui| {
                        ui.label(protocol.label());
                        match result {
                            Ok(endpoint) => {
                                ui.add(egui::Label::new(format!("{} · {}{}", endpoint.interface.as_deref().unwrap_or("IP"), endpoint.ip, if endpoint.fixed_bind { " · fixed service bind" } else { "" })).truncate())
                                    .on_hover_text("Fixed service binds override the interface selection");
                                if ui.small_button(format!("Copy IP · {}", protocol.label())).clicked() { ui.ctx().copy_text(endpoint.ip.to_string()); }
                            }
                            Err(message) => { ui.add(egui::Label::new(message).truncate()).on_hover_text(message); }
                        }
                    });
                }
                ui.add(egui::Label::new("Changes apply to new transfers and explicit retries. Running copy commands keep their original address.").wrap());
                });
            });
    }
}
