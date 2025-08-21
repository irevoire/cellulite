use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use egui::{Color32, RichText, Ui};
use egui_double_slider::DoubleSlider;
use geo::{Contains, Intersects, MultiPolygon};
use h3o::Resolution;
use walkers::Plugin;

use crate::{
    runner::Runner,
    utils::{display_cell, draw_geometry_on_map, extract_displayed_rect},
};

/// Plugin used to display the cells
#[derive(Clone)]
pub struct DisplayDbContent {
    pub display_db_cells: Arc<AtomicBool>,
    pub display_db_cells_min_res: Arc<AtomicUsize>,
    pub display_db_cells_max_res: Arc<AtomicUsize>,
    pub display_items: Arc<AtomicBool>,
    runner: Runner,
}

impl DisplayDbContent {
    pub fn new(runner: Runner) -> Self {
        DisplayDbContent {
            display_db_cells: Arc::default(),
            display_db_cells_min_res: Arc::new(AtomicUsize::from(0)),
            display_db_cells_max_res: Arc::new(AtomicUsize::from(16)),
            display_items: Arc::default(),
            runner,
        }
    }

    pub fn ui(&self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            let nb_items = self.runner.all_items.lock().len();
            let mut display_items = self.display_items.load(Ordering::Relaxed);
            if ui
                .toggle_value(&mut display_items, "Display items:")
                .clicked()
            {
                self.display_items.store(display_items, Ordering::Relaxed);
            }
            ui.label(RichText::new(format!("{nb_items}")).strong().color(
                Color32::WHITE.lerp_to_gamma(Color32::RED, 1.0_f32.min(nb_items as f32 / 100000.0)),
            ));
        });
        let mut display_db_cells = self.display_db_cells.load(Ordering::Relaxed);
        ui.horizontal_wrapped(|ui| {
            let nb_cells = self.runner.all_db_cells.lock().len();
            if ui
                .toggle_value(&mut display_db_cells, "Display DB cells")
                .clicked()
            {
                self.display_db_cells
                    .store(display_db_cells, Ordering::Relaxed);
            }
            ui.label(RichText::new(format!("{nb_cells}")).strong().color(
                Color32::WHITE.lerp_to_gamma(Color32::RED, 1.0_f32.min(nb_cells as f32 / 10000.0)),
            ));
        });
        if display_db_cells {
            let mut display_db_cells_min = self.display_db_cells_min_res.load(Ordering::Relaxed);
            let mut display_db_cells_max = self.display_db_cells_max_res.load(Ordering::Relaxed);
            ui.label(format!(
                "Cells resolution between {display_db_cells_min} and {display_db_cells_max}"
            ));
            ui.add(DoubleSlider::new(
                &mut display_db_cells_min,
                &mut display_db_cells_max,
                Resolution::Zero as usize..=Resolution::Fifteen as usize,
            ));
            self.display_db_cells_min_res
                .store(display_db_cells_min, Ordering::Relaxed);
            self.display_db_cells_max_res
                .store(display_db_cells_max, Ordering::Relaxed);
        }
    }
}

impl Plugin for DisplayDbContent {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        _response: &egui::Response,
        projector: &walkers::Projector,
    ) {
        let displayed_rect = extract_displayed_rect(ui, projector);
        let painter = ui.painter();

        if self.display_db_cells.load(Ordering::Relaxed) {
            let min = self.display_db_cells_min_res.load(Ordering::Relaxed);
            let max = self.display_db_cells_max_res.load(Ordering::Relaxed);

            for (cell, bitmap) in self.runner.all_db_cells.lock().iter() {
                if (min..max).contains(&(cell.resolution() as usize)) {
                    let cell_polygon = MultiPolygon::from(*cell);

                    if cell_polygon.intersects(&displayed_rect)
                        || displayed_rect.contains(&cell_polygon)
                    {
                        display_cell(
                            projector,
                            painter,
                            *cell,
                            Color32::BLUE.lerp_to_gamma(
                                Color32::RED,
                                bitmap.len() as f32 / self.runner.db.threshold as f32,
                            ),
                        );
                    }
                }
            }
        }

        if self.display_items.load(Ordering::Relaxed) {
            for value in self.runner.all_items.lock().values() {
                draw_geometry_on_map(projector, displayed_rect, painter, value)
            }
        }
    }
}
