//! Framework-agnostic Share Mode toolbar state + an egui example.
//!
//! Copy-paste recipe (not compiled as part of the crate). The helper below
//! decides *what to show*; your UI toolkit decides *how*. It only needs
//! `simple_sync`.

use simple_sync::{ShareLinks, ShareRole, SyncEngine};

/// What the toolbar should render this frame.
#[derive(Debug, PartialEq)]
pub enum ToolbarView {
    /// `[Share Document]` button + `[Join via Link]` input.
    Offline,
    /// `🟢 Live (N Peers)`, `[Copy Edit Link]`, `[Copy View-Only Link]`, red `[Stop Sharing]`.
    Hosting { label: String },
    /// `🟢 Live (N Peers)` + `[Leave]`. Local mutation broadcasting stays enabled.
    JoinedCanEdit { label: String },
    /// `🔒 View Only` badge. Local mutations must NOT be broadcast.
    JoinedViewOnly { label: String },
}

pub struct ShareToolbar {
    pub join_input: String,
    pub links: Option<ShareLinks>,
    pub error: Option<String>,
}

impl ShareToolbar {
    pub fn new() -> Self {
        Self { join_input: String::new(), links: None, error: None }
    }

    pub fn view(&self, sync: &SyncEngine) -> ToolbarView {
        let n = sync.connected_peers_count();
        let live = format!("🟢 Live ({n} Peer{})", if n == 1 { "" } else { "s" });
        match sync.role() {
            ShareRole::Offline => ToolbarView::Offline,
            ShareRole::Hosting => ToolbarView::Hosting { label: live },
            ShareRole::JoinedCanEdit => ToolbarView::JoinedCanEdit { label: live },
            ShareRole::JoinedViewOnly => ToolbarView::JoinedViewOnly { label: "🔒 View Only".into() },
        }
    }

    /// `[Share Document]`
    pub fn on_share(&mut self, sync: &mut SyncEngine, filename: &str, current_file_bytes: Vec<u8>) {
        match sync.enable_share_mode(filename, current_file_bytes) {
            Ok(links) => { self.links = Some(links); self.error = None; }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// `[Join via Link]`. The document itself arrives later as
    /// `SyncEvent::BaseDocumentReady`; load it in your poll loop.
    pub fn on_join(&mut self, sync: &mut SyncEngine) {
        match sync.join_via_ticket(self.join_input.trim()) {
            Ok(_) => { self.join_input.clear(); self.error = None; }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// `[Stop Sharing]` (host) or `[Leave]` (joiner): the kill switch.
    pub fn on_stop(&mut self, sync: &mut SyncEngine) {
        if let Err(e) = sync.disable_share_mode() {
            self.error = Some(e.to_string());
        }
        self.links = None; // old links are dead; never show them again
    }

    pub fn edit_link(&self) -> Option<&str> { self.links.as_ref().map(|l| l.edit_uri.as_str()) }
    pub fn view_only_link(&self) -> Option<&str> { self.links.as_ref().map(|l| l.view_only_uri.as_str()) }
}

/// Gate for every local edit path in the app (menu items, tools, shortcuts).
/// Disable editing tools entirely when this is false.
pub fn local_editing_allowed(sync: &SyncEngine) -> bool {
    sync.role() != ShareRole::JoinedViewOnly
}

// ---------------------------------------------------------------------------
// egui example (add `eframe`/`egui` to the *target app*, not to simple-sync)
// ---------------------------------------------------------------------------
#[cfg(feature = "egui-example")]
pub fn show_toolbar(
    ui: &mut egui::Ui,
    tb: &mut ShareToolbar,
    sync: &mut SyncEngine,
    filename: &str,
    current_file_bytes: impl Fn() -> Vec<u8>,
) {
    ui.horizontal(|ui| match tb.view(sync) {
        ToolbarView::Offline => {
            if ui.button("Share Document").clicked() {
                tb.on_share(sync, filename, current_file_bytes());
            }
            ui.add(egui::TextEdit::singleline(&mut tb.join_input).hint_text("Paste ssync:// link"));
            if ui.button("Join via Link").clicked() {
                tb.on_join(sync);
            }
        }
        ToolbarView::Hosting { label } => {
            ui.label(label);
            if ui.button("Copy Edit Link").clicked() {
                if let Some(l) = tb.edit_link() { ui.ctx().copy_text(l.to_owned()); }
            }
            if ui.button("Copy View-Only Link").clicked() {
                if let Some(l) = tb.view_only_link() { ui.ctx().copy_text(l.to_owned()); }
            }
            let stop = egui::Button::new(egui::RichText::new("Stop Sharing").color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(200, 40, 40));
            if ui.add(stop).clicked() {
                tb.on_stop(sync);
            }
        }
        ToolbarView::JoinedCanEdit { label } => {
            ui.label(label);
            if ui.button("Leave").clicked() { tb.on_stop(sync); }
        }
        ToolbarView::JoinedViewOnly { label } => {
            ui.label(label);
            if ui.button("Leave").clicked() { tb.on_stop(sync); }
        }
    });
    if let Some(e) = &tb.error {
        ui.colored_label(egui::Color32::RED, e);
    }
}
