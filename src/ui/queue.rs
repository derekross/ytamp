//! Builder C: the queue side panel.

use egui::RichText;

use crate::app::YtampApp;
use crate::model::PlayerCommand;

/// The queue panel: the engine's queue, click to jump.
pub fn panel(app: &mut YtampApp, ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Queue").strong().size(15.0));
        let count = app.state.queue.len();
        ui.label(RichText::new(format!("{count}")).weak());
        if count > 0 && ui.small_button("Clear").clicked() {
            app.send_cmd(PlayerCommand::QueueReplace(Vec::new(), None));
        }
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);

    let queue = app.state.queue.clone();
    let current = app.state.queue_index;
    if queue.is_empty() {
        ui.label(
            RichText::new("Nothing queued.\nSearch and press play — a radio follows.")
                .weak()
                .small(),
        );
        return;
    }

    let mut jump = None;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (index, track) in queue.iter().enumerate() {
                let is_current = current == Some(index);
                let marker = if is_current { "▶ " } else { "" };
                let label = RichText::new(format!("{marker}{}", track.display())).small();
                if ui.selectable_label(is_current, label).clicked() {
                    jump = Some(index);
                }
            }
        });
    if let Some(index) = jump {
        app.send_cmd(PlayerCommand::PlayAt(index));
    }
}
