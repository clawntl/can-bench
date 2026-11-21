//! Device page UI: a dashboard of cards in columns. Cards and their widgets come from the profile
//! (`[[card]]`); built-in cards (@nodes, @signals, …) are always available. "Edit layout" lets you
//! drag cards between columns, resize columns, hide/add cards and save the layout to the profile.

use super::devtab::{Confirm, Edit, BUILTINS, HISTORY_SECS};
use super::{th, App, Kind, C_ERR, C_INFO, C_RX, C_TIME, C_TX, C_WARN};
use crate::profile::{placeholders, Card, NodeSel, Profile, WidgetDef};
use eframe::egui::{self, Color32, RichText, Sense};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

/// Where a widget is drawn: which card, for which node.
#[derive(Clone)]
struct Wc {
    card: String,
    node: i64,
    color: Option<Color32>,
    /// card that sequences started here report node changes to (the opener for popups)
    owner: String,
}

enum Act {
    Load(PathBuf),
    Reload,
    Unload,
    Command(usize, i64),
    Sequence(usize, i64, String),
    Open(String, i64, String),
    StopAll,
    Abort,
    StopOutput(usize),
    Output(usize, i64, Option<Vec<f64>>, Option<bool>),
    ApplyBus,
    SaveLayout,
}

pub(super) fn parse_color(s: &str) -> Option<Color32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let v = u32::from_str_radix(h, 16).ok()?;
        return match h.len() {
            6 => Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
            3 => Some(Color32::from_rgb(((v >> 8) & 0xF) as u8 * 17, ((v >> 4) & 0xF) as u8 * 17, (v & 0xF) as u8 * 17)),
            _ => None,
        };
    }
    Some(match s.to_ascii_lowercase().as_str() {
        "green" => th((60, 170, 80), (80, 190, 100)),
        "red" => th((210, 60, 55), (225, 85, 80)),
        "orange" => th((230, 140, 30), (240, 160, 60)),
        "yellow" => th((215, 185, 30), (225, 200, 70)),
        "blue" => th((50, 120, 210), (90, 150, 235)),
        "purple" => th((140, 80, 190), (170, 120, 220)),
        "gray" | "grey" => th((150, 150, 158), (110, 112, 120)),
        _ => return None,
    })
}

const AUTO_COLORS: [(u8, u8, u8); 6] = [(38, 139, 210), (211, 54, 130), (133, 153, 0), (203, 75, 22), (108, 113, 196), (42, 161, 152)];

fn auto_color(i: usize) -> Color32 {
    let c = AUTO_COLORS[i % AUTO_COLORS.len()];
    Color32::from_rgb(c.0, c.1, c.2)
}

fn text_on(bg: Color32) -> Color32 {
    let l = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if l > 150.0 { Color32::from_rgb(20, 20, 20) } else { Color32::WHITE }
}

fn badge(ui: &mut egui::Ui, text: &str, bg: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).color(text_on(bg)).strong()))
        .response
}

impl App {
    fn sig_text(&self, key: &str, node: i64) -> Option<(String, bool, Duration, Option<f64>)> {
        let s = self.dev.signals.get(&(Some(node), key.to_string())).or_else(|| self.dev.signals.get(&(None, key.to_string())))?;
        Some((s.text.clone(), s.alarm, s.last.elapsed(), s.num))
    }

    /// Fills `{key}` placeholders. Returns None when `stale_ms` is set and a value is missing/old.
    fn fill(&self, text: &str, node: i64, stale_ms: Option<u64>) -> Option<String> {
        let mut out = text.to_string();
        for ph in placeholders(text) {
            let v = match ph.split_once(':') {
                Some(("count", m)) => self.dev.counts.get(&(Some(node), m.to_string())).or_else(|| self.dev.counts.get(&(None, m.to_string()))).copied().unwrap_or(0).to_string(),
                Some(("raw", k)) => match self.sig_text(k, node) {
                    Some((_, _, age, num)) if stale_ms.is_none_or(|s| age.as_millis() as u64 <= s) => num.map(|x| format!("{x}")).unwrap_or_else(|| "–".into()),
                    _ if stale_ms.is_some() => return None,
                    _ => "–".into(),
                },
                Some(("hex", k)) => match self.sig_text(k, node) {
                    Some((_, _, _, Some(x))) => format!("0x{:X}", x as i64),
                    _ => "–".into(),
                },
                _ => match ph.as_str() {
                    "n" => node.to_string(),
                    "bus_errors" => self.dev.bus_errors.to_string(),
                    "rx" => self.rx_count.to_string(),
                    "tx" => self.tx_count.to_string(),
                    k => match self.sig_text(k, node) {
                        Some((t, _, age, _)) => {
                            if stale_ms.is_some_and(|s| age.as_millis() as u64 > s) {
                                return None;
                            }
                            t
                        }
                        None if stale_ms.is_some() => return None,
                        None => "–".into(),
                    },
                },
            };
            out = out.replacen(&format!("{{{ph}}}"), &v, 1);
        }
        Some(out)
    }

    pub(super) fn device_ui(&mut self, ui: &mut egui::Ui) {
        let mut acts = Vec::new();
        self.header_ui(ui, &mut acts);
        if self.dev.edit.is_some() {
            self.edit_bar(ui, &mut acts);
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.columns_ui(ui, &mut acts);
        });
        self.popups_ui(ui.ctx(), &mut acts);
        self.confirm_ui(ui.ctx(), &mut acts);
        self.apply(acts);
    }

    fn header_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        egui::Panel::top("dev_profile").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).fill(th((236, 240, 247), (30, 33, 40)))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Device profile").strong().color(th((40, 70, 140), (130, 170, 240))));
                let name = self.dev.profile.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "none loaded".into());
                ui.label(RichText::new(name).strong());
                if ui.button("📂 Load…").on_hover_text("Open a .toml device profile").clicked() {
                    let mut d = rfd::FileDialog::new().add_filter("Device profile", &["toml"]);
                    if let Some(dir) = self.dev.path.as_ref().and_then(|p| p.parent()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(p) = d.pick_file() {
                        acts.push(Act::Load(p));
                    }
                }
                if let Some(p) = &self.dev.path {
                    let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(RichText::new(file).small().color(C_TIME())).on_hover_text(p.display().to_string());
                    if ui.button("⟳ Reload").on_hover_text("Read the file again after editing it").clicked() {
                        acts.push(Act::Reload);
                    }
                    if ui.button("✖").on_hover_text("Unload the profile").clicked() {
                        acts.push(Act::Unload);
                    }
                }
                ui.separator();
                ui.checkbox(&mut self.dev.decode_trace, "Decode in trace").on_hover_text("Add the decoded values to frames on the CAN Terminal page");
                if ui.button("Clear values").on_hover_text("Forget decoded values, nodes and statistics").clicked() {
                    self.dev.reset_seen();
                }
                let editing = self.dev.edit.is_some();
                if ui.selectable_label(editing, "✏ Edit layout").on_hover_text("Move, resize, hide and add cards").clicked() && !editing {
                    self.dev.edit = Some(Edit { before: (self.dev.layout.clone(), self.dev.widths.clone()), drag: None });
                }
                ui.separator();
                if let Some(b) = self.dev.profile.as_ref().and_then(|p| p.bus.clone()) {
                    let txt = format!("Bus {} · {:.1} %{}", crate::timing::fmt_baud(b.bitrate as f64), b.sample_point, b.data_bitrate.map(|d| format!(" · FD {}", crate::timing::fmt_baud(d as f64))).unwrap_or_default());
                    let matches = self.baud(false) == b.bitrate && (self.nom_sp - b.sample_point).abs() < 0.05 && self.fd == b.data_bitrate.is_some();
                    ui.label(RichText::new(txt).small().color(if matches { C_TIME() } else { C_WARN() }));
                    if !matches && ui.add_enabled(self.link.is_none(), egui::Button::new("Apply")).on_hover_text("Use the profile's bus settings (adapter must be closed)").clicked() {
                        acts.push(Act::ApplyBus);
                    }
                }
                let (txt, col) = if self.is_open { ("● adapter open", C_RX()) } else { ("● adapter closed – open it on the CAN Terminal page", C_WARN()) };
                ui.label(RichText::new(txt).color(col));
                if let Some(r) = &self.dev.running {
                    ui.separator();
                    ui.label(RichText::new(format!("⏳ {}", r.name)).color(C_INFO()).strong());
                    if ui.button("Abort").clicked() {
                        acts.push(Act::Abort);
                    }
                }
            });
            if let Some(e) = &self.dev.load_err {
                ui.label(RichText::new(format!("Cannot load the profile: {e}")).color(C_ERR()));
            }
        });
    }

    fn edit_bar(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let p = self.dev.profile.clone();
        egui::Panel::top("dev_edit").frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).fill(th((255, 246, 220), (55, 48, 30)))).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Editing layout").strong());
                ui.label(RichText::new("Drag cards by ⠿ · drag the bars between columns to resize").color(C_TIME()));
                ui.separator();
                if ui.button("➕ Column").clicked() {
                    self.dev.layout.push(Vec::new());
                    self.dev.widths.push(1.0);
                }
                ui.separator();
                // hidden cards
                let shown: BTreeSet<String> = self.dev.layout.iter().flatten().cloned().collect();
                let mut hidden: Vec<(String, String)> = Vec::new();
                if let Some(p) = &p {
                    for c in p.cards.iter().filter(|c| !c.popup) {
                        if !shown.contains(&c.title) {
                            hidden.push((c.title.clone(), c.title.clone()));
                        }
                    }
                }
                for (id, name) in BUILTINS {
                    if !shown.contains(id) {
                        hidden.push((id.to_string(), format!("{name} (built-in)")));
                    }
                }
                if !hidden.is_empty() {
                    ui.label("Add:");
                }
                for (id, name) in hidden {
                    if ui.small_button(format!("+ {name}")).clicked() {
                        let ci = (0..self.dev.layout.len()).min_by_key(|i| self.dev.layout[*i].len()).unwrap_or(0);
                        self.dev.layout[ci].push(id);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        if let Some(e) = self.dev.edit.take() {
                            (self.dev.layout, self.dev.widths) = e.before;
                        }
                    }
                    if ui.button("Reset").on_hover_text("Back to the layout in the profile").clicked() {
                        self.dev.default_layout();
                    }
                    let can_save = self.dev.path.is_some() && self.dev.profile.is_some();
                    if ui.add_enabled(can_save, egui::Button::new(RichText::new("💾 Save to profile").strong())).on_hover_text("Write this layout into the profile's [layout] section").clicked() {
                        acts.push(Act::SaveLayout);
                    }
                    if ui.button("Done").on_hover_text("Keep this layout for now without saving it").clicked() {
                        self.dev.edit = None;
                    }
                });
            });
        });
    }

    fn columns_ui(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        if self.dev.profile.is_none() && self.dev.layout.iter().flatten().all(|c| c.starts_with('@')) && self.dev.edit.is_none() {
            ui.add_space(10.0);
            ui.label(RichText::new("Load a device profile to decode your own protocol.").size(16.0));
            ui.label(
                "A profile is a .toml file that names your CAN messages and signals, the node numbering, the commands your \
                 boards understand, the values you want to send with sliders, test sequences and the cards on this page. \
                 See profiles/README.md in the repository; profiles/bldc-driver.toml is a complete example.",
            );
            ui.add_space(10.0);
        }
        let editing = self.dev.edit.is_some();
        let full = ui.available_rect_before_wrap();
        let ncols = self.dev.layout.len().max(1);
        while self.dev.widths.len() < ncols {
            self.dev.widths.push(1.0);
        }
        let gap = if editing { 14.0 } else { 10.0 };
        let total: f32 = self.dev.widths[..ncols].iter().sum::<f32>().max(0.01);
        let usable = (full.width() - gap * (ncols as f32 - 1.0)).max(100.0);
        let mut x = full.left();
        let mut col_rects = Vec::new();
        let mut card_rects: Vec<(usize, String, egui::Rect)> = Vec::new();
        let mut removed_col: Option<usize> = None;
        for ci in 0..ncols {
            let w = usable * self.dev.widths[ci] / total;
            let rect = egui::Rect::from_min_size(egui::pos2(x, full.top()), egui::vec2(w, full.height()));
            col_rects.push(rect);
            x += w + gap;
            let cards = self.dev.layout[ci].clone();
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                egui::ScrollArea::vertical().id_salt(("dash_col", ci)).auto_shrink([false, false]).show(ui, |ui| {
                    if editing {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Column {}", ci + 1)).small().color(C_TIME()));
                            if ncols > 1 && ui.small_button("✖ remove column").clicked() {
                                let moved = std::mem::take(&mut self.dev.layout[ci]);
                                let to = if ci > 0 { ci - 1 } else { 1 };
                                self.dev.layout[to].extend(moved);
                                removed_col = Some(ci);
                            }
                        });
                    }
                    for title in &cards {
                        let r = self.card_ui(ui, title, acts);
                        card_rects.push((ci, title.clone(), r));
                        ui.add_space(8.0);
                    }
                    if editing && cards.is_empty() {
                        ui.label(RichText::new("Empty column — drop cards here").color(C_TIME()));
                    }
                });
            });
        }
        // drop columns emptied by "remove column"
        if editing && self.dev.layout.len() > 1 {
            let mut i = 0;
            while i < self.dev.layout.len() && self.dev.layout.len() > 1 {
                if self.dev.layout[i].is_empty() && self.dev.edit.as_ref().is_some_and(|e| e.drag.is_none()) && removed_col == Some(i) {
                    self.dev.layout.remove(i);
                    self.dev.widths.remove(i);
                } else {
                    i += 1;
                }
            }
        }
        if !editing {
            return;
        }
        // column resize bars
        for ci in 0..ncols.saturating_sub(1) {
            let r = col_rects[ci];
            let bar = egui::Rect::from_min_max(egui::pos2(r.right() + 2.0, r.top()), egui::pos2(r.right() + gap - 2.0, r.bottom()));
            let resp = ui.interact(bar, ui.scope_id().with(("colbar", ci)), Sense::drag());
            let col = if resp.hovered() || resp.dragged() { C_INFO() } else { th((210, 200, 170), (90, 80, 60)) };
            ui.painter().rect_filled(bar, 3.0, col);
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.dragged() {
                let du = resp.drag_delta().x / usable * total;
                let (a, b) = (self.dev.widths[ci] + du, self.dev.widths[ci + 1] - du);
                if a > 0.25 && b > 0.25 {
                    self.dev.widths[ci] = a;
                    self.dev.widths[ci + 1] = b;
                }
            }
        }
        // drag and drop of cards
        let Some(dragging) = self.dev.edit.as_ref().and_then(|e| e.drag.clone()) else { return };
        let Some(pos) = ui.ctx().pointer_latest_pos() else { return };
        let (ci, idx) = drop_target(&col_rects, &card_rects, &dragging, pos, gap);
        let others: Vec<&(usize, String, egui::Rect)> = card_rects.iter().filter(|(c, t, _)| *c == ci && *t != dragging).collect();
        let y = if idx == 0 { others.first().map(|o| o.2.top() - 4.0).unwrap_or(col_rects[ci].top() + 24.0) } else { others[idx - 1].2.bottom() + 4.0 };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("can_bench_dnd")));
        let cr = col_rects[ci];
        painter.line_segment([egui::pos2(cr.left(), y), egui::pos2(cr.right(), y)], egui::Stroke::new(3.0, C_INFO()));
        let label = self.card_label(&dragging);
        let galley = painter.layout_no_wrap(label, egui::FontId::proportional(14.0), Color32::WHITE);
        let r = egui::Rect::from_min_size(pos + egui::vec2(12.0, 4.0), galley.size() + egui::vec2(16.0, 8.0));
        painter.rect_filled(r, 4.0, C_INFO());
        painter.galley(r.min + egui::vec2(8.0, 4.0), galley, Color32::WHITE);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if ui.input(|i| i.pointer.any_released()) {
            move_card(&mut self.dev.layout, &dragging, ci, idx);
            if let Some(e) = &mut self.dev.edit {
                e.drag = None;
            }
        }
    }

    fn card_label(&self, id: &str) -> String {
        BUILTINS.iter().find(|(i, _)| *i == id).map(|(_, n)| n.to_string()).unwrap_or_else(|| id.to_string())
    }

    /// Draws one card; returns its rectangle.
    fn card_ui(&mut self, ui: &mut egui::Ui, id: &str, acts: &mut Vec<Act>) -> egui::Rect {
        let p = self.dev.profile.clone();
        let card: Option<Card> = p.as_ref().and_then(|p| p.cards.iter().find(|c| c.title == id).cloned());
        let builtin = id.starts_with('@');
        if card.is_none() && !builtin {
            return ui.label(RichText::new(format!("(no card \"{id}\")")).color(C_ERR())).rect;
        }
        let editing = self.dev.edit.is_some();
        let color = card.as_ref().and_then(|c| c.color.as_deref()).and_then(parse_color);
        let node = self.dev.card_node(id);
        let wc = Wc { card: id.to_string(), node, color, owner: id.to_string() };
        let collapsed = self.dev.collapsed.contains(id);
        let resp = egui::Frame::new()
            .fill(th((255, 255, 255), (32, 35, 42)))
            .stroke(egui::Stroke::new(1.0, th((205, 212, 225), (60, 66, 78))))
            .corner_radius(6.0)
            .inner_margin(egui::Margin { left: 12, right: 8, top: 6, bottom: 8 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if editing {
                        let h = ui.add(egui::Label::new(RichText::new("⠿").size(18.0)).sense(Sense::drag())).on_hover_text("Drag to move");
                        if h.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        if h.drag_started() {
                            if let Some(e) = &mut self.dev.edit {
                                e.drag = Some(id.to_string());
                            }
                        }
                    } else if ui.small_button(if collapsed { "▶" } else { "▼" }).on_hover_text(if collapsed { "Expand" } else { "Collapse" }).clicked() {
                        if collapsed {
                            self.dev.collapsed.remove(id);
                        } else {
                            self.dev.collapsed.insert(id.to_string());
                        }
                    }
                    ui.label(RichText::new(self.card_label(id)).strong().size(15.0).color(color.unwrap_or(th((40, 70, 140), (130, 170, 240)))));
                    if let (Some(c), Some(p)) = (&card, &p) {
                        if c.node_select {
                            if let Some(nd) = &p.nodes {
                                ui.label(RichText::new(nd.label.to_lowercase()).color(C_TIME()));
                                let mut n = node;
                                if ui.add(egui::DragValue::new(&mut n).range(nd.min..=nd.max).speed(0.1)).changed() {
                                    self.dev.card_nodes.insert(id.to_string(), n);
                                }
                            }
                        } else if c.node.is_some() && p.nodes.is_some() {
                            ui.label(RichText::new(format!("node {node}")).color(C_TIME()));
                        }
                    }
                    if editing {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✖").on_hover_text("Hide this card (add it back from the bar above)").clicked() {
                                for col in &mut self.dev.layout {
                                    col.retain(|t| t != id);
                                }
                            }
                        });
                    }
                });
                if collapsed && !editing {
                    return;
                }
                match id {
                    "@nodes" => self.nodes_card(ui),
                    "@signals" => self.signals_card(ui),
                    "@plot" => self.ticked_plot(ui),
                    "@outputs" => self.outputs_card(ui, acts),
                    "@commands" => self.commands_card(ui, acts),
                    "@sequences" => self.sequences_card(ui, acts, &wc),
                    "@log" => self.log_widget(ui, 220.0),
                    "@bus" => self.bus_widget(ui),
                    _ => {
                        if let Some(c) = &card {
                            ui.add_space(2.0);
                            for w in &c.widgets {
                                self.widget_ui(ui, w, &wc, acts);
                            }
                        }
                    }
                }
            })
            .response;
        if let Some(c) = color {
            let r = resp.rect;
            ui.painter().rect_filled(egui::Rect::from_min_max(r.left_top() + egui::vec2(1.0, 6.0), egui::pos2(r.left() + 5.0, r.bottom() - 6.0)), 2.0, c);
        }
        resp.rect
    }

    fn widget_ui(&mut self, ui: &mut egui::Ui, w: &WidgetDef, wc: &Wc, acts: &mut Vec<Act>) {
        let p = self.dev.profile.clone().unwrap();
        match w {
            WidgetDef::Text { text, large, small, color, stale_ms, stale_text } => {
                let (t, stale) = match self.fill(text, wc.node, *stale_ms) {
                    Some(t) => (t, false),
                    None => (stale_text.clone().unwrap_or_else(|| self.fill(text, wc.node, None).unwrap_or_default()), true),
                };
                let mut rt = RichText::new(t);
                if *large {
                    rt = rt.size(22.0).strong();
                }
                if *small {
                    rt = rt.small();
                }
                rt = if stale { rt.color(C_TIME()) } else if let Some(c) = color.as_deref().and_then(parse_color) { rt.color(c) } else { rt };
                ui.label(rt);
            }
            WidgetDef::Value { signal, label, large, stale_ms } => {
                let (t, alarm, fresh) = match self.sig_text(signal, wc.node) {
                    Some((t, a, age, _)) => (t, a, stale_ms.is_none_or(|s| age.as_millis() as u64 <= s)),
                    None => ("–".into(), false, false),
                };
                ui.horizontal(|ui| {
                    if let Some(l) = label {
                        ui.label(RichText::new(l).color(C_TIME()));
                    }
                    let mut rt = RichText::new(if fresh { t } else { "–".into() }).monospace();
                    if *large {
                        rt = rt.size(24.0).strong();
                    }
                    rt = if !fresh { rt.color(C_TIME()) } else if alarm { rt.color(C_ERR()) } else { rt };
                    ui.label(rt);
                });
            }
            WidgetDef::Badge { signal, label, colors, stale_ms } => {
                ui.horizontal(|ui| {
                    if let Some(l) = label {
                        ui.label(RichText::new(l).color(C_TIME()));
                    }
                    match self.sig_text(signal, wc.node) {
                        Some((t, alarm, age, _)) if stale_ms.is_none_or(|s| age.as_millis() as u64 <= s) => {
                            let bg = colors.get(&t).and_then(|c| parse_color(c)).unwrap_or(if alarm { parse_color("red").unwrap() } else { th((225, 228, 235), (60, 64, 72)) });
                            badge(ui, &t, bg);
                        }
                        _ => {
                            badge(ui, "–", th((235, 236, 240), (50, 53, 60)));
                        }
                    }
                });
            }
            WidgetDef::Flags { signal, stale_ms } => {
                let def = p.find_signal(signal);
                let cur = self.sig_text(signal, wc.node).filter(|(_, _, age, _)| stale_ms.is_none_or(|s| age.as_millis() as u64 <= s));
                let set: Vec<String> = cur.as_ref().map(|c| c.0.split('|').map(|s| s.to_string()).collect()).unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    for (name, alarm) in def.map(|d| d.flag_list()).unwrap_or_default() {
                        let on = set.contains(&name);
                        let bg = if !on { th((238, 239, 243), (48, 51, 58)) } else if alarm { parse_color("red").unwrap() } else { parse_color("blue").unwrap() };
                        if on {
                            badge(ui, &name, bg);
                        } else {
                            egui::Frame::new().fill(bg).corner_radius(4.0).inner_margin(egui::Margin::symmetric(8, 2)).show(ui, |ui| ui.label(RichText::new(&name).color(C_TIME())));
                        }
                    }
                });
            }
            WidgetDef::Online { online, offline } => {
                let alive = self.dev.is_alive(wc.node);
                let (t, c) = if alive { (online.clone().unwrap_or_else(|| "online".into()), "green") } else { (offline.clone().unwrap_or_else(|| "no status".into()), "red") };
                badge(ui, &t, parse_color(c).unwrap());
            }
            WidgetDef::Ids { messages } => {
                let parts: Vec<String> = messages
                    .iter()
                    .filter_map(|m| p.messages.iter().find(|x| &x.name == m).map(|x| (m, x)))
                    .map(|(m, x)| match x.id_for(wc.node) {
                        Ok(id) if x.ext => format!("{m} 0x{id:08X}"),
                        Ok(id) => format!("{m} 0x{id:03X}"),
                        Err(_) => format!("{m} ?"),
                    })
                    .collect();
                ui.label(RichText::new(parts.join(" · ")).small().color(C_TIME()));
            }
            WidgetDef::Slider { output, signal, label, presets, send_toggle } => {
                let Some(oi) = p.outputs.iter().position(|o| &o.name == output) else { return };
                let m = &p.messages[p.outputs[oi].message];
                let n = if m.per_node { wc.node } else { 0 };
                let si = signal.as_ref().and_then(|s| m.signals.iter().position(|x| &x.name == s)).unwrap_or(0);
                let sdef = &m.signals[si];
                let mut vals = self.dev.values_for(oi, n);
                let mut driven = self.dev.outputs[oi].nodes.contains(&n);
                let period = p.outputs[oi].period_ms;
                let is_open = self.is_open;
                let mut body = |ui: &mut egui::Ui| {
                    if *send_toggle && ui.add_enabled(is_open || driven, egui::Checkbox::new(&mut driven, RichText::new("Send").strong())).on_hover_text(format!("Send {output} to this node every {period} ms")).changed() {
                        acts.push(Act::Output(oi, n, None, Some(driven)));
                    }
                    let (lo, hi) = (sdef.min.unwrap_or(-100.0), sdef.max.unwrap_or(100.0));
                    ui.spacing_mut().slider_width = (ui.available_width() - 150.0).clamp(120.0, 360.0);
                    let text = label.clone().unwrap_or_else(|| format!("{} {}", sdef.name, sdef.unit));
                    if ui.add(egui::Slider::new(&mut vals[si], lo..=hi).text(text).fixed_decimals(sdef.decimals().min(4)).clamping(egui::SliderClamping::Always)).changed() {
                        acts.push(Act::Output(oi, n, Some(vals.clone()), None));
                    }
                    for v in presets {
                        let t = if *v == 0.0 { "0".to_string() } else { format!("{v:+}") };
                        if ui.small_button(t).on_hover_text(format!("Set to {v}")).clicked() {
                            let mut nv = vals.clone();
                            nv[si] = *v;
                            acts.push(Act::Output(oi, n, Some(nv), None));
                        }
                    }
                };
                // inside a row: add straight to it; on its own: a wrapping row of its own
                if ui.layout().is_horizontal() {
                    body(ui);
                } else {
                    ui.horizontal_wrapped(|ui| body(ui));
                }
            }
            WidgetDef::Button { label, command, sequence, open, action, output, confirm, inputs, danger } => {
                ui.horizontal(|ui| {
                    let text = label.clone().or_else(|| command.clone()).or_else(|| sequence.clone()).or_else(|| open.clone().map(|o| format!("{o}…"))).unwrap_or_else(|| action.clone().unwrap_or_default());
                    let running_this = sequence.as_ref().is_some_and(|s| self.dev.running.as_ref().is_some_and(|r| &r.name == s));
                    let enabled = match (command, sequence, open, action) {
                        (Some(_), _, _, _) => self.is_open,
                        (_, Some(_), _, _) => self.is_open && self.dev.running.is_none(),
                        (_, _, _, Some(a)) if a == "abort" => self.dev.running.is_some(),
                        _ => true,
                    };
                    let mut b = egui::Button::new(RichText::new(if running_this { format!("⏳ {text}") } else { text.clone() }).strong().color(if *danger { Color32::WHITE } else { ui.visuals().text_color() }));
                    if *danger {
                        b = b.fill(parse_color("red").unwrap());
                    }
                    let mut r = ui.add_enabled(enabled, b);
                    let help = command.as_ref().and_then(|c| p.commands.iter().find(|x| &x.name == c)).map(|c| c.help.clone()).or_else(|| sequence.as_ref().and_then(|s| p.sequences.iter().find(|x| &x.name == s)).map(|s| s.help.clone())).unwrap_or_default();
                    if !help.is_empty() {
                        r = r.on_hover_text(help);
                    }
                    if r.clicked() {
                        let act = if let Some(c) = command {
                            let ci = p.commands.iter().position(|x| &x.name == c).unwrap();
                            if confirm.is_none() && p.commands[ci].confirm {
                                self.dev.confirm = Some(Confirm::Command(ci, wc.node));
                                None
                            } else {
                                Some(Confirm::Command(ci, wc.node))
                            }
                        } else if let Some(s) = sequence {
                            Some(Confirm::Sequence(p.sequences.iter().position(|x| &x.name == s).unwrap(), wc.node, Some(wc.owner.clone())))
                        } else {
                            None
                        };
                        match (act, confirm) {
                            (Some(a), Some(t)) => self.dev.confirm = Some(Confirm::Text(self.fill(t, wc.node, None).unwrap_or_default(), Box::new(a))),
                            (Some(Confirm::Command(ci, n)), None) => acts.push(Act::Command(ci, n)),
                            (Some(Confirm::Sequence(si, n, c)), None) => acts.push(Act::Sequence(si, n, c.unwrap_or_default())),
                            _ => {}
                        }
                        if let Some(o) = open {
                            acts.push(Act::Open(o.clone(), wc.node, wc.owner.clone()));
                        }
                        match action.as_deref() {
                            Some("stop") => acts.push(Act::StopAll),
                            Some("abort") => acts.push(Act::Abort),
                            Some("stop_output") => {
                                if let Some(oi) = output.as_ref().and_then(|o| p.outputs.iter().position(|x| &x.name == o)) {
                                    acts.push(Act::StopOutput(oi));
                                }
                            }
                            _ => {}
                        }
                    }
                    if *inputs {
                        if let Some(ci) = command.as_ref().and_then(|c| p.commands.iter().position(|x| &x.name == c)) {
                            for (ai, a) in p.commands[ci].args.iter().enumerate() {
                                ui.label(RichText::new(&a.0).color(C_TIME()));
                                arg_input(ui, &mut self.dev.args[ci][ai], a);
                            }
                        }
                        if let Some(si) = sequence.as_ref().and_then(|s| p.sequences.iter().position(|x| &x.name == s)) {
                            for (ai, a) in p.sequences[si].args.iter().enumerate() {
                                ui.label(RichText::new(&a.0).color(C_TIME()));
                                arg_input(ui, &mut self.dev.seq_args[si][ai], a);
                            }
                        }
                    }
                });
            }
            WidgetDef::Link { output, label } => {
                if let Some(oi) = p.outputs.iter().position(|o| &o.name == output) {
                    let mut on = self.dev.linked.contains(&oi);
                    if ui.checkbox(&mut on, label.clone().unwrap_or_else(|| format!("Link {output} sliders + Send"))).on_hover_text("Moving one slider (or Send) moves them all").changed() {
                        if on {
                            self.dev.linked.insert(oi);
                        } else {
                            self.dev.linked.remove(&oi);
                        }
                    }
                }
            }
            WidgetDef::Plot { title, series, seconds, height, min_range, gap_ms } => {
                if let Some(t) = title {
                    ui.label(RichText::new(t).color(C_TIME()));
                }
                let lines = self.expand_series(&p, series, wc);
                let id = format!("plot_{}_{}", wc.card, title.clone().unwrap_or_default());
                self.draw_plot(ui, &id, &lines, *seconds, *height, *min_range, *gap_ms);
            }
            WidgetDef::Log { height } => self.log_widget(ui, *height),
            WidgetDef::Rate { message, label, gap_warn_ms } => {
                let node = p.messages.iter().find(|m| &m.name == message || m.variants.iter().any(|v| &v.name == message)).is_some_and(|m| m.per_node).then_some(wc.node);
                let r = self.dev.rates.get(&(node, message.clone()));
                let mut t = match r {
                    Some(r) if r.last.is_some_and(|l| l.elapsed() < Duration::from_secs(2)) => {
                        format!("{} {}/s, longest gap {:.0} ms", label.clone().unwrap_or_else(|| message.clone()), r.times.len(), r.max_gap_ms)
                    }
                    _ => format!("{}: none", label.clone().unwrap_or_else(|| message.clone())),
                };
                let mut warn = false;
                if let (Some(g), Some(r)) = (gap_warn_ms, r) {
                    let late = r.gaps.iter().filter(|x| **x > *g as f64).count();
                    t += &format!(", gaps > {g} ms: {late}");
                    warn = late > 0;
                }
                ui.label(RichText::new(t).small().color(if warn { C_WARN() } else { C_TIME() }));
            }
            WidgetDef::Row { widgets } => {
                ui.horizontal_wrapped(|ui| {
                    for w in widgets {
                        self.widget_ui(ui, w, wc, acts);
                    }
                });
            }
            WidgetDef::Separator => {
                ui.separator();
            }
            WidgetDef::Bus => self.bus_widget(ui),
            WidgetDef::Nodes => self.nodes_card(ui),
            WidgetDef::Signals => self.signals_card(ui),
            WidgetDef::Outputs => self.outputs_card(ui, acts),
            WidgetDef::Commands => self.commands_card(ui, acts),
            WidgetDef::Sequences => self.sequences_card(ui, acts, wc),
            WidgetDef::PlotTicked => self.ticked_plot(ui),
        }
    }

    /// (label, node, key, colour) per plotted line.
    fn expand_series(&self, p: &Profile, series: &[crate::profile::SeriesDef], wc: &Wc) -> Vec<(String, Option<i64>, String, Color32)> {
        let mut out = Vec::new();
        let short = |k: &str| k.rsplit('.').next().unwrap_or(k).to_string();
        for (si, s) in series.iter().enumerate() {
            let per_node = p.nodes.is_some();
            let base = s.label.clone().unwrap_or_else(|| short(&s.signal));
            let col = s.color.as_deref().and_then(parse_color);
            match &s.node {
                Some(NodeSel::Node(n)) => out.push((format!("{base} n{n}"), Some(*n), s.signal.clone(), col.unwrap_or(auto_color(si)))),
                Some(NodeSel::Word(w)) if w == "cards" => {
                    for (i, c) in p.cards.iter().filter(|c| !c.popup && (c.node.is_some() || c.node_select)).enumerate() {
                        let n = self.dev.card_node(&c.title);
                        let cc = c.color.as_deref().and_then(parse_color).or(col).unwrap_or(auto_color(i));
                        out.push((format!("{base} {}", c.title), Some(n), s.signal.clone(), cc));
                    }
                }
                Some(NodeSel::Word(w)) if w == "all" => {
                    let mut nodes: Vec<Option<i64>> = self.dev.signals.keys().filter(|(_, k)| k == &s.signal).map(|(n, _)| *n).collect();
                    nodes.dedup();
                    for (i, n) in nodes.into_iter().enumerate() {
                        out.push((format!("{base}{}", n.map(|n| format!(" n{n}")).unwrap_or_default()), n, s.signal.clone(), col.unwrap_or(auto_color(si + i))));
                    }
                }
                _ => {
                    let n = if per_node { Some(wc.node) } else { None };
                    out.push((base, n, s.signal.clone(), col.or(wc.color).unwrap_or(auto_color(si))));
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_plot(&mut self, ui: &mut egui::Ui, id: &str, lines: &[(String, Option<i64>, String, Color32)], secs: f64, height: f32, min_range: Option<f64>, gap_ms: u64) {
        let now = self.dev.t0.elapsed().as_secs_f64();
        let gap = gap_ms as f64 / 1000.0;
        let mut plot = egui_plot::Plot::new(id.to_string())
            .height(height)
            .legend(egui_plot::Legend::default().position(egui_plot::Corner::LeftTop))
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_x(false)
            .y_axis_min_width(40.0)
            .y_grid_spacer(egui_plot::log_grid_spacer(5))
            .x_axis_formatter(move |m, _| format!("{:.0} s", m.value - now));
        if let Some(r) = min_range {
            plot = plot.include_y(r).include_y(-r);
        }
        let mut any = false;
        plot.show(ui, |pui| {
            for (name, n, key, col) in lines {
                let Some(s) = self.dev.signals.get(&(*n, key.clone())).or_else(|| if n.is_some() { None } else { self.dev.signals.get(&(None, key.clone())) }) else {
                    // an empty line keeps the legend entry
                    pui.line(egui_plot::Line::new(name.clone(), Vec::<[f64; 2]>::new()).color(*col));
                    continue;
                };
                let start = now - secs - 0.5;
                let mut seg: Vec<[f64; 2]> = Vec::new();
                let mut first = true;
                let mut prev_t = f64::NEG_INFINITY;
                for p in s.hist.iter().filter(|p| p[0] >= start) {
                    if p[0] - prev_t > gap && !seg.is_empty() {
                        pui.line(egui_plot::Line::new(name.clone(), std::mem::take(&mut seg)).color(*col).width(2.0));
                        first = false;
                    }
                    seg.push(*p);
                    prev_t = p[0];
                    any = true;
                }
                // a value that stopped coming ends where it stopped
                if !seg.is_empty() || first {
                    pui.line(egui_plot::Line::new(name.clone(), seg).color(*col).width(2.0));
                }
            }
            pui.set_auto_bounds([false, true]);
            pui.set_plot_bounds_x(now - secs..=now);
        });
        // smooth scrolling: repaint every frame while the plot is moving
        if self.is_open || any {
            ui.ctx().request_repaint();
        }
    }

    fn log_widget(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.horizontal(|ui| {
            if ui.small_button("Clear log").clicked() {
                self.dev.log.clear();
            }
            ui.label(RichText::new(format!("{} lines", self.dev.log.len())).small().color(C_TIME()));
        });
        egui::Frame::new().fill(th((250, 251, 253), (22, 24, 29))).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 3.0;
            let n = self.dev.log.len();
            egui::ScrollArea::vertical().id_salt(ui.scope_id().with("log")).max_height(height).min_scrolled_height(height).stick_to_bottom(true).auto_shrink([false, false]).show_rows(ui, row_h, n, |ui, range| {
                for l in &self.dev.log[range] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&l.t).monospace().small().color(C_TIME()));
                        let mut t = RichText::new(&l.text).monospace().color(l.kind.color());
                        if l.kind == Kind::Note {
                            t = t.strong();
                        }
                        ui.label(t);
                    });
                }
            });
        });
    }

    fn bus_widget(&mut self, ui: &mut egui::Ui) {
        let heard: Vec<String> = self.dev.heard.keys().filter(|n| self.dev.is_alive(**n)).map(|n| n.to_string()).collect();
        let mut t = format!("Rx {} · Tx {} · bus errors {}", self.rx_count, self.tx_count, self.dev.bus_errors);
        if !heard.is_empty() {
            t += &format!(" · nodes heard: {}", heard.join(", "));
        }
        if let Some(r) = &self.dev.running {
            t += &format!(" · running: {}", r.name);
        }
        ui.label(RichText::new(t).small().color(C_TIME()));
        let fighting: Vec<String> = self.dev.foreign.iter().filter(|(_, t)| t.elapsed() < Duration::from_secs(2)).map(|(id, _)| format!("0x{id:X}")).collect();
        if !fighting.is_empty() {
            ui.label(RichText::new(format!("⚠ Another node also sends {} — it fights your output.", fighting.join(", "))).color(C_ERR()));
        }
    }

    // ------------------------------------------------------------------ built-in cards

    fn nodes_card(&mut self, ui: &mut egui::Ui) {
        let Some(p) = self.dev.profile.clone() else { return };
        let Some(nodes) = &p.nodes else {
            ui.label(RichText::new("The profile has no [nodes] section.").color(C_TIME()));
            return;
        };
        if self.dev.heard.is_empty() {
            ui.label(RichText::new(if self.is_open { "No node heard yet." } else { "Open the adapter to see the nodes on the bus." }).color(C_TIME()));
            return;
        }
        let mut select = None;
        egui::ScrollArea::horizontal().id_salt(ui.scope_id().with("nodes_h")).show(ui, |ui| {
            egui::Grid::new(ui.scope_id().with("nodes")).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label(RichText::new(&nodes.label).strong());
                ui.label(RichText::new("Seen").strong());
                for c in &nodes.columns {
                    ui.label(RichText::new(c.rsplit('.').next().unwrap_or(c)).strong()).on_hover_text(c);
                }
                ui.end_row();
                for (&n, &last) in &self.dev.heard {
                    let alive = self.dev.is_alive(n);
                    if ui.selectable_label(self.dev.sel_node == n, RichText::new(format!("{n}")).strong()).on_hover_text("Select for commands").clicked() {
                        select = Some(n);
                    }
                    let (t, col) = if alive { ("● alive".to_string(), C_RX()) } else { (format!("● missing {:.1} s", last.elapsed().as_secs_f32()), C_ERR()) };
                    ui.label(RichText::new(t).color(col));
                    for c in &nodes.columns {
                        match self.dev.signals.get(&(Some(n), c.clone())) {
                            Some(s) => ui.label(RichText::new(&s.text).monospace().color(if s.alarm { C_ERR() } else if alive { th((20, 20, 20), (225, 225, 225)) } else { C_TIME() })),
                            None => ui.label(RichText::new("–").color(C_TIME())),
                        };
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(n) = select {
            self.dev.sel_node = n;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    fn outputs_card(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let Some(p) = self.dev.profile.clone() else { return };
        if p.outputs.is_empty() {
            ui.label(RichText::new("The profile has no [[output]].").color(C_TIME()));
            return;
        }
        ui.label(RichText::new("Tick Send to transmit every period. Unticking or Esc sends each signal's stop value once.").small().color(C_TIME()));
        let range = p.node_range();
        for (oi, o) in p.outputs.iter().enumerate() {
            let m = &p.messages[o.message];
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&o.name).strong());
                ui.label(RichText::new(format!("every {} ms", o.period_ms)).color(C_TIME()));
                ui.label(RichText::new(format!("sent {}", self.dev.outputs[oi].sent)).monospace().color(C_TX()));
                if ui.add_enabled(!self.dev.outputs[oi].nodes.is_empty(), egui::Button::new("⏹ Stop")).clicked() {
                    acts.push(Act::StopOutput(oi));
                }
            });
            let show: Vec<i64> = if m.per_node {
                let mut v: BTreeSet<i64> = self.dev.heard.keys().copied().chain(self.dev.outputs[oi].nodes.iter().copied()).collect();
                v.insert(self.dev.sel_node);
                v.into_iter().filter(|n| range.contains(n)).collect()
            } else {
                vec![0]
            };
            egui::Grid::new(ui.scope_id().with(("out", oi))).spacing([10.0, 4.0]).show(ui, |ui| {
                for n in show {
                    let mut drive = self.dev.outputs[oi].nodes.contains(&n);
                    let label = if m.per_node { format!("Send node {n}") } else { "Send".to_string() };
                    if ui.add_enabled(self.is_open || drive, egui::Checkbox::new(&mut drive, label)).changed() {
                        acts.push(Act::Output(oi, n, None, Some(drive)));
                    }
                    let mut vals = self.dev.values_for(oi, n);
                    let mut changed = false;
                    ui.spacing_mut().slider_width = 220.0;
                    for (s, v) in m.signals.iter().zip(vals.iter_mut()) {
                        let (lo, hi) = (s.min.unwrap_or(-100.0), s.max.unwrap_or(100.0));
                        changed |= ui.add(egui::Slider::new(v, lo..=hi).text(format!("{} {}", s.name, s.unit)).fixed_decimals(s.decimals().min(4)).clamping(egui::SliderClamping::Always)).changed();
                    }
                    if changed {
                        acts.push(Act::Output(oi, n, Some(vals), None));
                    }
                    ui.end_row();
                }
            });
        }
    }

    fn commands_card(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
        let Some(p) = self.dev.profile.clone() else { return };
        if p.commands.is_empty() {
            ui.label(RichText::new("The profile has no [[command]].").color(C_TIME()));
            return;
        }
        if let Some(nodes) = &p.nodes {
            ui.horizontal(|ui| {
                ui.label(format!("Send to {}:", nodes.label.to_lowercase()));
                let heard = &self.dev.heard;
                egui::ComboBox::from_id_salt(ui.scope_id().with("cmd_node")).selected_text(self.dev.sel_node.to_string()).show_ui(ui, |ui| {
                    for n in nodes.min..=nodes.max {
                        let t = if heard.contains_key(&n) { format!("{n}  ●") } else { n.to_string() };
                        ui.selectable_value(&mut self.dev.sel_node, n, t);
                    }
                });
            });
        }
        let n = self.dev.sel_node;
        egui::Grid::new(ui.scope_id().with("commands")).spacing([10.0, 4.0]).show(ui, |ui| {
            for (ci, c) in p.commands.iter().enumerate() {
                let mut b = ui.add_enabled(self.is_open, egui::Button::new(&c.name).min_size(egui::vec2(150.0, 0.0)));
                if !c.help.is_empty() {
                    b = b.on_hover_text(&c.help);
                }
                if b.clicked() {
                    if c.confirm {
                        self.dev.confirm = Some(Confirm::Command(ci, n));
                    } else {
                        acts.push(Act::Command(ci, n));
                    }
                }
                ui.horizontal(|ui| {
                    for (ai, a) in c.args.iter().enumerate() {
                        ui.label(&a.0);
                        arg_input(ui, &mut self.dev.args[ci][ai], a);
                    }
                    if c.confirm {
                        ui.label(RichText::new("asks first").small().color(C_TIME()));
                    }
                });
                ui.end_row();
            }
        });
    }

    fn sequences_card(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Act>, wc: &Wc) {
        let Some(p) = self.dev.profile.clone() else { return };
        if p.sequences.is_empty() {
            ui.label(RichText::new("The profile has no [[sequence]].").color(C_TIME()));
            return;
        }
        let n = if wc.card.starts_with('@') { self.dev.sel_node } else { wc.node };
        ui.label(RichText::new(format!("Runs on node {n} (choose it under Commands)")).small().color(C_TIME()));
        for (si, s) in p.sequences.iter().enumerate() {
            ui.horizontal(|ui| {
                let running = self.dev.running.as_ref().is_some_and(|r| r.name == s.name);
                let mut b = ui.add_enabled(self.is_open && self.dev.running.is_none(), egui::Button::new(if running { format!("⏳ {}", s.name) } else { s.name.clone() }).min_size(egui::vec2(150.0, 0.0)));
                if !s.help.is_empty() {
                    b = b.on_hover_text(&s.help);
                }
                if b.clicked() {
                    acts.push(Act::Sequence(si, n, wc.owner.clone()));
                }
                for (ai, a) in s.args.iter().enumerate() {
                    ui.label(&a.0);
                    arg_input(ui, &mut self.dev.seq_args[si][ai], a);
                }
            });
        }
        if ui.add_enabled(self.dev.running.is_some(), egui::Button::new("Abort")).clicked() {
            acts.push(Act::Abort);
        }
    }

    fn signals_card(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.dev.filter).desired_width(160.0).hint_text("filter"));
            ui.label(RichText::new("Tick a value to plot it in the Plot card.").small().color(C_TIME()));
        });
        let filter = self.dev.filter.to_lowercase();
        let mut toggles = Vec::new();
        egui::ScrollArea::vertical().id_salt(ui.scope_id().with("sigs")).max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new(ui.scope_id().with("sigs_grid")).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                for h in ["", "Node", "Signal", "Value", "Age"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for (key, s) in &self.dev.signals {
                    if !filter.is_empty() && !key.1.to_lowercase().contains(&filter) {
                        continue;
                    }
                    let mut on = self.dev.plot.contains(key);
                    if ui.add_enabled(s.num.is_some() || !s.hist.is_empty(), egui::Checkbox::without_text(&mut on)).changed() {
                        toggles.push((key.clone(), on));
                    }
                    ui.label(key.0.map(|n| n.to_string()).unwrap_or_else(|| "–".into()));
                    let name = RichText::new(&key.1).monospace();
                    ui.label(if s.tx { name.color(C_TX()) } else { name });
                    ui.label(RichText::new(&s.text).monospace().color(if s.alarm { C_ERR() } else if s.tx { C_TX() } else { C_INFO() }));
                    ui.label(RichText::new(format!("{:.1} s", s.last.elapsed().as_secs_f32())).small().color(C_TIME()));
                    ui.end_row();
                }
            });
            if self.dev.signals.is_empty() {
                ui.label(RichText::new("No decoded frames yet.").color(C_TIME()));
            }
        });
        for (k, on) in toggles {
            if on {
                self.dev.plot.insert(k);
            } else {
                self.dev.plot.remove(&k);
            }
        }
    }

    fn ticked_plot(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.dev.follow, "Follow").on_hover_text("Off: drag and zoom freely");
            ui.add_enabled(self.dev.follow, egui::DragValue::new(&mut self.dev.plot_secs).range(1.0..=HISTORY_SECS).suffix(" s").speed(0.5)).on_hover_text("Time window");
            if ui.button("Clear plot").clicked() {
                self.dev.plot.clear();
            }
            if ui.button("💾 CSV").on_hover_text("Save the plotted signals' history").clicked() {
                self.save_csv();
            }
        });
        if self.dev.plot.is_empty() {
            ui.label(RichText::new("Tick values in the Signals card to plot them here.").small().color(C_TIME()));
        }
        let lines: Vec<(String, Option<i64>, String, Color32)> = self
            .dev
            .plot
            .iter()
            .enumerate()
            .map(|(i, (n, k))| (n.map(|n| format!("{k} n{n}")).unwrap_or_else(|| k.clone()), *n, k.clone(), auto_color(i)))
            .collect();
        if self.dev.follow {
            let secs = self.dev.plot_secs;
            self.draw_plot(ui, "ticked_plot", &lines, secs, 260.0, None, 1000);
        } else {
            egui_plot::Plot::new("ticked_plot_free").height(260.0).legend(egui_plot::Legend::default()).show(ui, |pui| {
                for (name, n, k, col) in &lines {
                    if let Some(s) = self.dev.signals.get(&(*n, k.clone())) {
                        pui.line(egui_plot::Line::new(name.clone(), s.hist.iter().copied().collect::<Vec<_>>()).color(*col));
                    }
                }
            });
        }
    }

    fn save_csv(&mut self) {
        if self.dev.plot.is_empty() {
            self.note(Kind::Warn, "Tick some signals to plot first.");
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_file_name("signals.csv").save_file() else { return };
        let mut out = String::from("time_s,node,signal,value\n");
        for key in &self.dev.plot {
            if let Some(s) = self.dev.signals.get(key) {
                for p in &s.hist {
                    out += &format!("{:.6},{},{},{}\n", p[0], key.0.map(|n| n.to_string()).unwrap_or_default(), key.1, p[1]);
                }
            }
        }
        if let Err(e) = std::fs::write(&path, out) {
            self.note(Kind::Err, &format!("Cannot save: {e}"));
        }
    }

    // ------------------------------------------------------------------ popups, confirmations, actions

    fn popups_ui(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        let Some(p) = self.dev.profile.clone() else { return };
        let popups = self.dev.popups.clone();
        let mut close = Vec::new();
        for (i, (title, node, owner)) in popups.iter().enumerate() {
            let Some(card) = p.cards.iter().find(|c| &c.title == title) else { continue };
            let mut open = true;
            let wc = Wc { card: title.clone(), node: *node, color: card.color.as_deref().and_then(parse_color), owner: owner.clone().unwrap_or_else(|| title.clone()) };
            egui::Window::new(format!("{title} — node {node}")).id(egui::Id::unique(("can_bench_popup", title))).open(&mut open).resizable(true).default_width(460.0).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
                for w in &card.widgets {
                    self.widget_ui(ui, w, &wc, acts);
                }
            });
            if !open {
                close.push(i);
            }
        }
        for i in close.into_iter().rev() {
            self.dev.popups.remove(i);
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, acts: &mut Vec<Act>) {
        let Some(c) = self.dev.confirm.take() else { return };
        let p = self.dev.profile.clone();
        let (title, body) = match &c {
            Confirm::Command(ci, n) => match p.as_ref().and_then(|p| p.commands.get(*ci)) {
                Some(cmd) => (format!("Send \"{}\" to node {n}?", cmd.name), cmd.help.clone()),
                None => return,
            },
            Confirm::Sequence(si, n, _) => match p.as_ref().and_then(|p| p.sequences.get(*si)) {
                Some(s) => (format!("Run \"{}\" on node {n}?", s.name), s.help.clone()),
                None => return,
            },
            Confirm::Script(t, _) => (t.clone(), String::new()),
            Confirm::Text(t, _) => (t.clone(), String::new()),
        };
        let mut answer = None;
        egui::Window::new("Confirm").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(RichText::new(&title).strong());
            if !body.is_empty() {
                ui.label(body);
            }
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    answer = Some(false);
                }
            });
        });
        match answer {
            None => self.dev.confirm = Some(c),
            Some(yes) => {
                let mut c = c;
                if let Confirm::Text(_, inner) = c {
                    if !yes {
                        return;
                    }
                    c = *inner;
                    if let Confirm::Command(ci, n) = c {
                        acts.push(Act::Command(ci, n));
                        return;
                    }
                }
                match c {
                    Confirm::Command(ci, n) if yes => acts.push(Act::Command(ci, n)),
                    Confirm::Sequence(si, n, card) if yes => acts.push(Act::Sequence(si, n, card.unwrap_or_default())),
                    Confirm::Script(_, tx) => {
                        let _ = tx.send(yes);
                    }
                    _ => {}
                }
            }
        }
    }

    fn apply(&mut self, acts: Vec<Act>) {
        for a in acts {
            match a {
                Act::Load(p) => {
                    self.stop_everything();
                    self.dev.load(p);
                    self.after_load();
                }
                Act::Reload => {
                    if let Some(p) = self.dev.path.clone() {
                        self.stop_everything();
                        self.dev.load(p);
                        self.after_load();
                    }
                }
                Act::Unload => {
                    self.stop_everything();
                    self.dev.unload();
                }
                Act::Command(ci, n) => self.send_command(ci, n),
                Act::Sequence(si, n, card) => self.run_sequence(si, n, Some(card)),
                Act::Open(title, n, owner) => {
                    self.dev.popups.retain(|p| p.0 != title);
                    self.dev.popups.push((title, n, Some(owner)));
                }
                Act::StopAll => self.stop_everything(),
                Act::Abort => self.abort_sequence(),
                Act::StopOutput(oi) => self.stop_outputs(Some(oi)),
                Act::Output(oi, n, v, d) => self.set_output(oi, n, v, d),
                Act::ApplyBus => {
                    if let Some(b) = self.dev.profile.as_ref().and_then(|p| p.bus.clone()) {
                        self.apply_bus(&b);
                    }
                }
                Act::SaveLayout => match self.save_layout() {
                    Ok(()) => {
                        self.dev.edit = None;
                        self.note(Kind::Note, "Layout saved to the profile.");
                    }
                    Err(e) => self.note(Kind::Err, &format!("Cannot save the layout: {e}")),
                },
            }
        }
    }

    fn after_load(&mut self) {
        match (&self.dev.profile, &self.dev.load_err) {
            (Some(p), _) => {
                let msg = format!(
                    "Profile loaded: {} ({} messages, {} commands, {} outputs, {} sequences, {} cards)",
                    p.name,
                    p.messages.len(),
                    p.commands.len(),
                    p.outputs.len(),
                    p.sequences.len(),
                    p.cards.len()
                );
                self.note(Kind::Note, &msg);
            }
            (None, Some(e)) => {
                let e = e.clone();
                self.note(Kind::Err, &format!("Profile: {e}"));
            }
            _ => {}
        }
    }

    /// Writes the current layout into the profile's [layout] table, keeping the rest of the file.
    fn save_layout(&mut self) -> Result<(), String> {
        let path = self.dev.path.clone().ok_or("no profile file")?;
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let out = layout_into_toml(&text, &self.dev.layout, &self.dev.widths)?;
        std::fs::write(&path, out).map_err(|e| e.to_string())?;
        // re-read so the in-memory profile matches the file
        let keep = (self.dev.layout.clone(), self.dev.widths.clone());
        self.dev.load(path);
        if self.dev.load_err.is_some() {
            (self.dev.layout, self.dev.widths) = keep;
        }
        Ok(())
    }
}

/// Replaces (or adds) the [layout] table of a profile, keeping everything else as written.
fn layout_into_toml(text: &str, layout: &[Vec<String>], widths: &[f32]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut cols = toml_edit::Array::new();
    for c in layout {
        let mut a = toml_edit::Array::new();
        for t in c {
            a.push(t.as_str());
        }
        cols.push(a);
    }
    let mut ws = toml_edit::Array::new();
    for i in 0..layout.len() {
        ws.push(((widths.get(i).copied().unwrap_or(1.0) as f64) * 100.0).round() / 100.0);
    }
    let mut t = toml_edit::Table::new();
    t.insert("columns", toml_edit::value(cols));
    t.insert("widths", toml_edit::value(ws));
    doc.insert("layout", toml_edit::Item::Table(t));
    Ok(doc.to_string())
}

/// Column and position a dragged card lands at, from the pointer position.
fn drop_target(cols: &[egui::Rect], cards: &[(usize, String, egui::Rect)], dragging: &str, pos: egui::Pos2, gap: f32) -> (usize, usize) {
    let n = cols.len().max(1);
    let ci = cols
        .iter()
        .position(|r| pos.x >= r.left() - gap / 2.0 && pos.x <= r.right() + gap / 2.0)
        .unwrap_or(if cols.first().is_some_and(|r| pos.x < r.left()) { 0 } else { n - 1 });
    let idx = cards.iter().filter(|(c, t, r)| *c == ci && t != dragging && r.center().y < pos.y).count();
    (ci, idx)
}

/// Moves a card to column `ci`, position `idx` (counted without the card itself).
fn move_card(layout: &mut [Vec<String>], card: &str, ci: usize, idx: usize) {
    for col in layout.iter_mut() {
        col.retain(|t| t != card);
    }
    if let Some(col) = layout.get_mut(ci) {
        let idx = idx.min(col.len());
        col.insert(idx, card.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn drag_and_drop_positions() {
        let cols = [rect(0.0, 0.0, 300.0, 800.0), rect(310.0, 0.0, 300.0, 800.0)];
        let cards = vec![
            (0, "A".to_string(), rect(0.0, 0.0, 300.0, 200.0)),
            (0, "B".to_string(), rect(0.0, 210.0, 300.0, 200.0)),
            (1, "C".to_string(), rect(310.0, 0.0, 300.0, 300.0)),
        ];
        // drop A below B
        assert_eq!(drop_target(&cols, &cards, "A", egui::pos2(100.0, 500.0), 10.0), (0, 1));
        // drop A into the right column, above C
        assert_eq!(drop_target(&cols, &cards, "A", egui::pos2(400.0, 50.0), 10.0), (1, 0));
        // in the gap between the columns: the nearer one; outside: the edge columns
        assert_eq!(drop_target(&cols, &cards, "A", egui::pos2(306.0, 50.0), 10.0).0, 1);
        assert_eq!(drop_target(&cols, &cards, "A", egui::pos2(-50.0, 50.0), 10.0).0, 0);
        assert_eq!(drop_target(&cols, &cards, "A", egui::pos2(900.0, 50.0), 10.0).0, 1);

        let mut layout = vec![vec!["A".to_string(), "B".to_string()], vec!["C".to_string()]];
        move_card(&mut layout, "A", 0, 1);
        assert_eq!(layout, vec![vec!["B".to_string(), "A".to_string()], vec!["C".to_string()]]);
        move_card(&mut layout, "A", 1, 0);
        assert_eq!(layout, vec![vec!["B".to_string()], vec!["A".to_string(), "C".to_string()]]);
        move_card(&mut layout, "@log", 1, 99);
        assert_eq!(layout[1].last().map(String::as_str), Some("@log"));
    }

    #[test]
    fn layout_saved_into_profile_keeps_the_rest() {
        let text = include_str!("../../../profiles/bldc-driver.toml");
        let layout = vec![vec!["Right motor".to_string(), "@log".to_string()], vec!["Left motor".to_string(), "Both".to_string()]];
        let out = layout_into_toml(text, &layout, &[1.4, 1.0]).unwrap();
        assert!(out.contains("# SAFETY: the motors WILL spin."), "comments are kept");
        assert_eq!(out.matches("[layout]").count(), 1);
        let p = crate::profile::Profile::parse(&out, "x").unwrap();
        let l = p.layout.unwrap();
        assert_eq!(l.columns, layout);
        assert_eq!(l.widths, vec![1.4, 1.0]);
        // a profile without [layout] gets one added
        let out = layout_into_toml("name = \"x\"
", &layout, &[1.0, 1.0]).unwrap();
        assert!(out.contains("[layout]"));
    }
}

fn arg_input(ui: &mut egui::Ui, v: &mut f64, a: &(String, f64, Option<f64>, Option<f64>)) {
    let whole = [Some(a.1), a.2, a.3].iter().flatten().all(|x| x.fract() == 0.0);
    let mut d = egui::DragValue::new(v).speed(if whole { 0.2 } else { 0.1 });
    if whole {
        d = d.fixed_decimals(0);
    }
    if let (Some(lo), Some(hi)) = (a.2, a.3) {
        d = d.range(lo..=hi);
    }
    ui.add(d);
}
