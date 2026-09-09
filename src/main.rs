//! ytamp — YouTube Music, native and skinned.
//!
//! Skeleton main; Builder C replaces this with the real app shell.

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_title("ytamp"),
        ..Default::default()
    };
    eframe::run_native(
        "ytamp",
        options,
        Box::new(|_cc| Ok(Box::new(YtampSkeleton::default()))),
    )
}

#[derive(Default)]
struct YtampSkeleton;

impl eframe::App for YtampSkeleton {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("ytamp");
        ui.label("YouTube Music, native and skinned — builders are wiring the rooms. ⭐");
    }
}
