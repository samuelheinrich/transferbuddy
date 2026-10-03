use super::*;
#[derive(PartialEq, Eq)]
struct LayoutKey {
    revision: u64,
    cli: bool,
    focused: bool,
    font: u32,
    size: Option<(u16, u16)>,
}

pub(super) struct ConsoleWindow {
    pub cli: bool,
    pub minimized: bool,
    pending: bool,
    focus_requested: bool,
    size_sent: Option<(u16, u16)>,
    cache_key: Option<LayoutKey>,
    text: String,
    job: Option<egui::text::LayoutJob>,
}
impl ConsoleWindow {
    pub fn new(cli: bool) -> Self {
        Self {
            cli,
            minimized: false,
            pending: cli,
            focus_requested: true,
            size_sent: None,
            cache_key: None,
            text: String::new(),
            job: None,
        }
    }
    pub(super) fn opened(&mut self, accepted: bool) {
        self.pending = false;
        if !accepted {
            self.cli = false;
            self.focus_requested = false;
            self.cache_key = None;
        }
    }
    fn layout(&mut self, _ui: &mut egui::Ui, d: &DeviceSnapshot, compact: bool, focused: bool) {
        let font = egui::FontId::monospace(if compact { 13.0 } else { 14.0 });
        let key = LayoutKey {
            revision: d.output_revision,
            cli: self.cli,
            focused,
            font: font.size.to_bits(),
            size: d.terminal.as_ref().map(|s| s.size()),
        };
        if self.cache_key.as_ref() == Some(&key) {
            return;
        }
        let mut job = egui::text::LayoutJob::default();
        if let Some(screen) = d.terminal.as_ref().filter(|_| self.cli) {
            let (rows, cols) = screen.size();
            for row in 0..rows {
                for col in 0..cols {
                    let Some(cell) = screen.cell(row, col) else {
                        continue;
                    };
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let content = cell.contents();
                    let mut fg = terminal_color(cell.fgcolor(), Color32::from_gray(230));
                    let mut bg = terminal_color(cell.bgcolor(), Color32::BLACK);
                    if cell.inverse() {
                        std::mem::swap(&mut fg, &mut bg);
                    }
                    if !screen.hide_cursor() && screen.cursor_position() == (row, col) && focused {
                        fg = Color32::BLACK;
                        bg = Color32::from_gray(230);
                    }
                    job.append(
                        if content.is_empty() { " " } else { &content },
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: fg,
                            background: bg,
                            italics: cell.italic(),
                            underline: if cell.underline() {
                                egui::Stroke::new(1.0, fg)
                            } else {
                                egui::Stroke::NONE
                            },
                            ..Default::default()
                        },
                    );
                }
                if row + 1 < rows {
                    job.append(
                        "\n",
                        0.0,
                        egui::TextFormat {
                            font_id: font.clone(),
                            ..Default::default()
                        },
                    );
                }
            }
        } else {
            for line in &d.transcript {
                let color = match line.kind {
                    transferbuddy_core::switch::LineKind::Error => RED,
                    transferbuddy_core::switch::LineKind::Sent => CYAN,
                    _ => Color32::from_gray(230),
                };
                job.append(
                    &format!("{}\n", line.text),
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color,
                        ..Default::default()
                    },
                );
            }
            job.append(
                &d.live,
                0.0,
                egui::TextFormat {
                    font_id: font,
                    color: Color32::from_gray(230),
                    ..Default::default()
                },
            );
        }
        job.wrap.max_width = f32::INFINITY;
        self.text = job.text.clone();
        self.job = Some(job);
        self.cache_key = Some(key);
    }
}

impl Desktop {
    pub(super) fn restore_console(&mut self, id: u64) {
        if let Some(window) = self.console_windows.get_mut(&id) {
            window.minimized = false;
            window.focus_requested = true;
            self.cli = window.cli.then_some(id);
            self.cli_focus = window.cli;
        }
    }
    fn minimize_console(&mut self, id: u64, ctx: &egui::Context) {
        if let Some(window) = self.console_windows.get_mut(&id) {
            window.minimized = true;
            window.focus_requested = false;
        }
        if self.cli == Some(id) {
            self.cli = None;
            self.cli_focus = false;
        }
        ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(("terminal_input", id))));
    }
    pub(super) fn close_console(&mut self, id: u64, ctx: &egui::Context) {
        if self
            .console_windows
            .remove(&id)
            .is_some_and(|window| window.cli)
        {
            self.send(Command::CloseCli(id));
        }
        self.minimize_console(id, ctx);
    }
    pub(super) fn console_taskbar(&mut self, parent: &mut egui::Ui) -> Option<egui::Rect> {
        if let Some(id) = self.console.take() {
            self.console_windows
                .entry(id)
                .or_insert_with(|| ConsoleWindow::new(false));
            self.restore_console(id);
        }
        if let Some(id) = self.cli {
            self.console_windows
                .entry(id)
                .or_insert_with(|| ConsoleWindow::new(true));
        }
        self.console_windows
            .retain(|id, _| self.snapshot.devices.iter().any(|d| d.id == *id));
        let ctx = parent.ctx().clone();
        if let Some(id) = self.cli.filter(|id| !self.console_windows.contains_key(id)) {
            self.minimize_console(id, &ctx);
        }
        if self.console_windows.is_empty() {
            return None;
        }
        let tabs: Vec<_> = self
            .console_windows
            .iter()
            .filter_map(|(id, window)| {
                self.snapshot.devices.iter().find(|d| d.id == *id).map(|d| {
                    (
                        *id,
                        format!("{} {}", if window.cli { "CLI" } else { "Console" }, d.name),
                        window.minimized,
                    )
                })
            })
            .collect();
        let mut selected = None;
        let response = egui::Panel::bottom("console_taskbar")
            .frame(
                egui::Frame::new()
                    .fill(parent.visuals().faint_bg_color)
                    .inner_margin(8),
            )
            .show(parent, |ui| {
                if self.obstructed() {
                    ui.disable();
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("CONSOLES").small().color(design::muted(ui)));
                    egui::ScrollArea::horizontal()
                        .id_salt("console_tabs")
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (id, label, minimized) in &tabs {
                                    ui.push_id(id, |ui| {
                                        if ui
                                            .selectable_label(!minimized, label)
                                            .on_hover_text(if *minimized {
                                                "Restore console · SSH stays connected"
                                            } else {
                                                "Bring console to front"
                                            })
                                            .clicked()
                                        {
                                            selected = Some(*id);
                                        }
                                    });
                                }
                            });
                        });
                });
            });
        if let Some(id) = selected {
            self.restore_console(id);
            ctx.move_to_top(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new(("console_window", id)),
            ));
        }
        Some(response.response.rect)
    }
    pub(super) fn console_windows(
        &mut self,
        ctx: &egui::Context,
        workspace: egui::Rect,
        taskbar: Option<egui::Rect>,
    ) {
        let bounds = workspace.shrink(20.0);
        let mut windows = std::mem::take(&mut self.console_windows);
        let restored = windows.values().any(|window| window.focus_requested);
        let mut visible_rects = Vec::new();
        let input_owner = self.cli.filter(|_| self.cli_focus);
        let mut input_selections = std::collections::BTreeMap::new();
        let events = ctx.input(|i| i.events.clone());
        let mut actions = Vec::new();
        for (index, (id, window)) in windows.iter_mut().enumerate() {
            let Some(d) = self.snapshot.devices.iter().find(|d| d.id == *id) else {
                continue;
            };
            if window.cli && d.terminal.is_none() && !window.pending {
                window.cli = false;
                window.cache_key = None;
                if self.cli == Some(*id) {
                    self.cli = None;
                    self.cli_focus = false;
                }
            }
            if d.terminal.is_some() {
                window.pending = false;
            }
            if window.minimized {
                continue;
            }
            let d = d.clone();
            let mut open = true;
            let mut minimize = false;
            let mut close = false;
            let mut launch = false;
            let mut terminal_focus = false;
            let mut selected_text = String::new();
            let mut selection_before = false;
            let mut paste_requested = false;
            let focused = self.cli == Some(*id) && self.cli_focus;
            let title = format!(
                "{} · {} · {}",
                if window.cli { "CLI" } else { "Console" },
                d.name,
                d.host
            );
            let result = egui::Window::new(&title)
                .id(egui::Id::new(("console_window", id)))
                .open(&mut open).title_bar(false).collapsible(false).constrain_to(bounds)
                .default_pos(bounds.min + egui::vec2(20.0 * index as f32, 20.0 * index as f32))
                .default_size(egui::vec2(900.0, 460.0).min(bounds.size() - egui::vec2(24.0, 24.0)))
                .min_size(egui::vec2(400.0, 160.0).min(bounds.size())).max_size(bounds.size())
                .frame(egui::Frame::new().fill(Color32::BLACK).stroke(egui::Stroke::new(1.0, Color32::from_gray(110))).inner_margin(10))
                .show(ctx, |ui| {
                    *ui.visuals_mut() = egui::Visuals::dark();
                    ui.visuals_mut().override_text_color = Some(Color32::from_gray(230));
                    ui.visuals_mut().extreme_bg_color = Color32::BLACK;
                    ui.visuals_mut().selection.bg_fill = Color32::from_rgb(40, 70, 110);
                    ui.visuals_mut().selection.stroke.color = Color32::WHITE;
                    ui.visuals_mut().text_cursor.stroke = egui::Stroke::NONE;
                    if self.obstructed() { ui.disable(); }
                    ui.add(egui::Label::new(RichText::new(&title).strong()).truncate().selectable(false)).on_hover_text("Drag the console window to move it");
                    ui.horizontal(|ui| {
                        let copy = ui.button("Copy transcript");
                        if copy.clicked() { copy.request_focus(); ui.ctx().copy_text(d.transcript.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n")); }
                        if window.cli && ui.button("Paste").clicked() {
                            paste_requested = true; window.focus_requested = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                        }
                        if !window.cli && ui.button("Connect to CLI").clicked() { launch = true; }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.button("Close").clicked(); minimize = ui.button("Minimize").clicked();
                        });
                    });
                    ui.add(egui::Label::new(RichText::new(if window.cli {
                        "ESC minimizes · select + Copy · Paste sends to switch · Ctrl+C interrupts"
                    } else { "Read-only transcript · select text to copy" }).small().color(Color32::from_gray(170))).truncate());
                    ui.separator();
                    if !window.cli {
                        egui::ScrollArea::both().id_salt(("transcript_scroll", id))
                            .auto_shrink([false, false]).stick_to_bottom(true)
                            .show_rows(ui, ui.text_style_height(&egui::TextStyle::Body),
                                d.transcript.len() + 1, |ui, range| {
                                for index in range {
                                    if let Some(line) = d.transcript.get(index) {
                                        let color = match line.kind {
                                            transferbuddy_core::switch::LineKind::Error => RED,
                                            transferbuddy_core::switch::LineKind::Sent => CYAN,
                                            _ => Color32::from_gray(230),
                                        };
                                        ui.add(egui::Label::new(RichText::new(&line.text).color(color))
                                            .wrap_mode(egui::TextWrapMode::Extend).selectable(true));
                                    } else { ui.add(egui::Label::new(&d.live).selectable(true)); }
                                }
                            });
                        return;
                    }
                    window.layout(ui, &d, self.prefs.compact, focused);
                    let terminal_id = egui::Id::new(("terminal_input", id));
                    selection_before = egui::text_edit::TextEditState::load(ctx, terminal_id)
                        .and_then(|s| s.cursor.char_range()).is_some_and(|r| !r.is_empty());
                    let font = egui::FontId::monospace(if self.prefs.compact { 13.0 } else { 14.0 });
                    if window.cli {
                        let (cw, ch) = ui.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
                        let size = ((ui.available_height() / ch).clamp(3.0, 100.0) as u16, (ui.available_width() / cw).clamp(20.0, 240.0) as u16);
                        if d.terminal.as_ref().is_some_and(|s| s.size() != size) && window.size_sent != Some(size) {
                            window.size_sent = Some(size); self.send(Command::ResizeCli(*id, size.0, size.1));
                        }
                    }
                    let mut text = window.text.as_str();
                    let galley = ui.fonts_mut(|fonts| fonts.layout_job(window.job.as_ref().unwrap().clone()));
                    let mut layouter = move |_: &egui::Ui, _: &dyn egui::TextBuffer, _: f32| galley.clone();
                    egui::ScrollArea::both().id_salt(("terminal_scroll", id)).auto_shrink([false, false]).show(ui, |ui| {
                        let output = egui::TextEdit::multiline(&mut text).id(terminal_id)
                            .font(font).code_editor().frame(egui::Frame::NONE).margin(0)
                            .desired_width(ui.available_width()).desired_rows(3).layouter(&mut layouter).show(ui);
                        output.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), format!("Terminal for {}", d.name)));
                        if window.focus_requested { output.response.request_focus(); }
                        terminal_focus = output.response.has_focus() || window.focus_requested;
                        if let Some(range) = output.cursor_range.filter(|r| !r.is_empty()) {
                            let range = range.as_sorted_char_range();
                            selected_text = window.text.chars().skip(range.start.0).take(range.end.0 - range.start.0).collect();
                        }
                        if window.cli && terminal_focus {
                            ui.memory_mut(|m| m.set_focus_lock_filter(terminal_id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, ..Default::default() }));
                        }
                    });
                });
            window.focus_requested = false;
            if let Some(result) = result {
                visible_rects.push(result.response.rect);
                if ctx.input(|i| i.pointer.any_pressed()) && result.response.contains_pointer() {
                    self.cli = window.cli.then_some(*id);
                    self.cli_focus = terminal_focus && window.cli;
                }
            }
            if paste_requested {
                self.cli = Some(*id);
                self.cli_focus = true;
            }
            input_selections.insert(*id, (selection_before, selected_text));
            if !open || close {
                actions.push((*id, 0));
            } else if minimize {
                actions.push((*id, 1));
            } else if launch {
                actions.push((*id, 2));
            }
        }
        let focused_cli = windows.iter().find_map(|(id, window)| {
            (window.cli
                && !window.minimized
                && ctx.memory(|m| m.has_focus(egui::Id::new(("terminal_input", id)))))
            .then_some(*id)
        });
        self.cli_focus = focused_cli.is_some();
        if let Some(id) = focused_cli {
            self.cli = Some(id);
        }
        self.console_windows = windows;
        self.cli_new = false;
        if let Some(id) = input_owner.filter(|_| !self.obstructed()) {
            if events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            }) {
                actions.push((id, 1));
            }
        }
        if let Some(id) = self.cli.filter(|id| {
            input_owner == Some(*id)
                && self.cli_focus
                && ctx.memory(|m| m.has_focus(egui::Id::new(("terminal_input", id))))
        }) {
            if !self.obstructed() {
                let (before, selected) = input_selections.remove(&id).unwrap_or_default();
                for event in &events {
                    if matches!(
                        event,
                        egui::Event::Key {
                            key: egui::Key::Escape,
                            pressed: true,
                            ..
                        }
                    ) {
                        actions.push((id, 1));
                        break;
                    }
                    if matches!(event, egui::Event::Key { key: egui::Key::A, pressed: true, modifiers, .. } if modifiers.command)
                    {
                        continue;
                    }
                    if matches!(event, egui::Event::Key { key, pressed: true, modifiers, .. }
                        if modifiers.shift && matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown
                            | egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::Home | egui::Key::End))
                    {
                        continue;
                    }
                    let copy = matches!(event, egui::Event::Copy)
                        || matches!(event,
                        egui::Event::Key { key: egui::Key::C, pressed: true, modifiers, .. }
                        if modifiers.command && (before || !selected.is_empty()));
                    if copy {
                        if !selected.is_empty() {
                            ctx.copy_text(selected.clone());
                        } else if !cfg!(target_os = "macos") {
                            self.send(Command::CliInput(id, vec![3]));
                        }
                        continue;
                    }
                    if let Some(bytes) = cli_bytes(event) {
                        self.send(Command::CliInput(id, bytes));
                    }
                }
            }
        }
        for (id, action) in actions {
            match action {
                0 => self.close_console(id, ctx),
                1 => self.minimize_console(id, ctx),
                _ => self.open_cli(id),
            }
        }
        if !restored && !self.obstructed() && ctx.input(|i| i.pointer.any_pressed()) {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                if !visible_rects.iter().any(|r| r.contains(pos))
                    && taskbar.is_none_or(|r| !r.contains(pos))
                {
                    let ids: Vec<_> = self.console_windows.keys().copied().collect();
                    for id in ids {
                        self.minimize_console(id, ctx);
                    }
                }
            }
        }
    }
}
