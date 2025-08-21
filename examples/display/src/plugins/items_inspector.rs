use cellulite::roaring::RoaringBitmapCodec;
use egui::{Color32, Response, RichText, Ui};
use egui_double_slider::DoubleSlider;
use egui_extras::syntax_highlighting::CodeTheme;
use fst::{
    automaton::{Levenshtein, Str},
    Automaton, IntoStreamer, Streamer,
};
use geo::{Intersects, MultiPolygon};
use geojson::GeoJson;
use h3o::{CellIndex, Resolution};
use std::ops::RangeInclusive;
use walkers::{Plugin, Projector};

use crate::{
    runner::Runner,
    utils::{display_cell, draw_geometry_on_map, extract_displayed_rect},
};

#[derive(Clone)]
pub struct ItemsInspector {
    pub query: String,
    pub runner: Runner,
    #[allow(clippy::type_complexity)]
    pub selected: Option<(u32, String, GeoJson, Vec<CellIndex>, Vec<CellIndex>)>,
    pub search_result: Option<Vec<(String, u32)>>,
    pub resolution_range: RangeInclusive<Resolution>,
}

impl ItemsInspector {
    pub fn new(runner: Runner) -> Self {
        Self {
            query: String::new(),
            runner,
            selected: None,
            search_result: Some(Vec::new()),
            resolution_range: Resolution::Zero..=Resolution::Fifteen,
        }
    }

    fn search_fst<A: Automaton>(
        &self,
        fst: &fst::Map<Vec<u8>>,
        automaton: A,
        result: &mut Vec<(String, u32)>,
    ) {
        if result.len() >= result.capacity() {
            return;
        }
        let rtxn = self.runner.env.read_txn().unwrap();
        let mut stream = fst.search(&automaton).into_stream();
        while let Some((s, bitmap_id)) = stream.next() {
            let name = String::from_utf8(s.to_vec()).unwrap();
            // Get the bitmap, we might miss it because there is a race condition in the runner where the fst is updated before the bitmaps are commited
            let Some(bitmap) = self
                .runner
                .metadata
                .remap_data_type::<RoaringBitmapCodec>()
                .get(&rtxn, &format!("bitmap_{bitmap_id:010}"))
                .unwrap()
            else {
                continue;
            };
            result.extend(bitmap.iter().map(|id| (name.clone(), id)));
            if result.len() >= result.capacity() {
                break;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.collapsing(RichText::new("Inspect item").heading(), |ui| {
            if ui.text_edit_singleline(&mut self.query).changed() {
                self.selected = None;
                self.search_result = None;
                self.search();
            }
            ui.separator();
            if let Some((item, name, geometry, cells, inner_shape_cells)) = &self.selected {
                let response = ui.selectable_label(true, format!("{name}: {item}"));
                ui.collapsing(RichText::new("Geojson").heading(), |ui| {
                    display_geojson_as_codeblock(ui, geometry);
                });
                ui.label(format!("Made of {} cells", cells.len()));
                ui.label(format!(
                    "Made of {} inner shape cells",
                    inner_shape_cells.len()
                ));

                // Add resolution range slider
                ui.horizontal(|ui| {
                    ui.label("Resolution range:");
                    let mut min = *self.resolution_range.start() as u8;
                    let mut max = *self.resolution_range.end() as u8;
                    let response = ui.add(DoubleSlider::new(&mut min, &mut max, 0..=15));

                    if response.changed() {
                        self.resolution_range = Resolution::try_from(min)
                            .unwrap_or(Resolution::Zero)
                            ..=Resolution::try_from(max).unwrap_or(Resolution::Fifteen);
                    }
                });

                // Handle deselection using the original label's response
                if response.clicked() {
                    self.selected = None;
                }
            } else if let Some(result) = self.search_result.as_ref() {
                for (name, item) in result {
                    let response = ui.selectable_label(
                        self.selected.as_ref().map(|(id, _, _, _, _)| *id) == Some(*item),
                        format!("{name}: {item}"),
                    );
                    if response.clicked() {
                        let geometry = self.runner.all_items.lock()[item].clone();
                        // Get the cells containing this document from the runner's all_db_cells
                        let cells = self
                            .runner
                            .all_db_cells
                            .lock()
                            .iter()
                            .filter(|(_, bitmap)| bitmap.contains(*item))
                            .map(|(cell, _)| *cell)
                            .collect();
                        let inner_shape_cells = self
                            .runner
                            .inner_shape_cell_db
                            .lock()
                            .iter()
                            .filter(|(_, bitmap)| bitmap.contains(*item))
                            .map(|(cell, _)| *cell)
                            .collect();
                        self.selected =
                            Some((*item, name.clone(), geometry, cells, inner_shape_cells));
                    }
                }
            } else {
                ui.spinner();
            }
        });
    }

    fn search(&mut self) -> Vec<(String, u32)> {
        let fst = self.runner.fst.lock();
        let mut result = Vec::with_capacity(50);
        let exact = Str::new(&self.query);
        self.search_fst(&fst, exact.clone(), &mut result);
        let prefix = exact
            .clone()
            .starts_with()
            .intersection(exact.clone().complement());
        self.search_fst(&fst, prefix.clone(), &mut result);
        let lev = Levenshtein::new(&self.query, self.query.len() as u32 / 3).unwrap();
        let base = lev
            .starts_with()
            .intersection(exact.clone().starts_with().complement());
        self.search_fst(&fst, base, &mut result);
        result
    }
}

fn display_geojson_as_codeblock(ui: &mut Ui, geometry: &GeoJson) {
    let geojson_obj = geometry.clone();
    let pretty_geometry = serde_json::to_string_pretty(&geojson_obj).unwrap();
    let geojson_url = format!(
        "http://geojson.io/#data=data:application/json,{}",
        &geometry
    );

    let mut code_str = pretty_geometry.clone();
    let mut layouter = |ui: &egui::Ui, buf: &str, wrap_width: f32| {
        let mut layout_job = egui_extras::syntax_highlighting::highlight(
            ui.ctx(),
            ui.style(),
            &CodeTheme::from_style(ui.style()),
            buf,
            "json",
        );
        layout_job.wrap.max_width = wrap_width;
        ui.fonts(|f| f.layout_job(layout_job))
    };

    let text_edit = egui::TextEdit::multiline(&mut code_str)
        .code_editor()
        .desired_width(f32::INFINITY)
        .layouter(&mut layouter)
        .interactive(true);

    ui.add(text_edit);

    ui.horizontal(|ui| {
        if ui.button("Copy GeoJSON").clicked() {
            ui.ctx().copy_text(pretty_geometry.clone());
        }
        ui.hyperlink_to("View on geojson.io", geojson_url);
    });
}

impl Plugin for ItemsInspector {
    fn run(self: Box<Self>, ui: &mut Ui, _response: &Response, projector: &Projector) {
        if let Some((_, _, geometry, cells, inner_shape_cells)) = self.selected {
            let displayed_rect = extract_displayed_rect(ui, projector);
            let painter = ui.painter();
            draw_geometry_on_map(projector, displayed_rect, painter, &geometry);

            // Get the cells containing this document from the runner's all_db_cells
            for cell in cells.iter() {
                let resolution = cell.resolution();
                if self.resolution_range.contains(&resolution) {
                    let cell_polygon = MultiPolygon::from(*cell);

                    if cell_polygon.intersects(&displayed_rect) {
                        display_cell(projector, painter, *cell, Color32::DARK_BLUE);
                    }
                }
            }
            for cell in inner_shape_cells.iter() {
                let resolution = cell.resolution();
                if self.resolution_range.contains(&resolution) {
                    display_cell(projector, painter, *cell, Color32::DARK_GREEN);
                }
            }
        }
    }
}
