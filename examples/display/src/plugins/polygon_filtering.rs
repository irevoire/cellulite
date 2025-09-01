use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use cellulite::FilteringStep;
use egui::{epaint::PathStroke, Color32, RichText, Ui, Vec2};
use egui_double_slider::DoubleSlider;
use geo::{GeodesicArea, Geometry};
use geo_types::Coord;
use h3o::Resolution;
use walkers::{Plugin, Position, Projector};

use crate::{
    runner::Runner,
    utils::{display_cell, draw_diagonal_cross, project_line_string},
};

use super::{insert_into_database::InsertMode, InsertIntoDatabase};

/// Plugin used to create or delete a polygon used to select a subset of points
#[derive(Clone)]
pub struct PolygonFiltering {
    pub in_creation: Arc<AtomicBool>,
    pub display_filtering_details: Arc<AtomicUsize>,
    pub display_details_min_res: Arc<AtomicUsize>,
    pub display_details_max_res: Arc<AtomicUsize>,
    runner: Runner,
    pub insert_into_database: InsertIntoDatabase,
}

impl PolygonFiltering {
    pub fn new(runner: Runner, insert_into_database: InsertIntoDatabase) -> Self {
        PolygonFiltering {
            runner,
            in_creation: insert_into_database.filtering.clone(),
            display_filtering_details: Arc::default(),
            display_details_min_res: Arc::new(AtomicUsize::new(0)),
            display_details_max_res: Arc::new(AtomicUsize::new(16)),
            insert_into_database,
        }
    }

    pub fn ui(&self, ui: &mut Ui) {
        ui.collapsing(RichText::new("Filter").heading(), |ui| {
            let in_creation = self.in_creation.load(Ordering::Relaxed);
            let no_polygon = self.runner.polygon_filter.lock().len() <= 2;
            #[allow(clippy::collapsible_if)]
            if !in_creation && no_polygon {
                if ui.button("Create polygon").clicked() {
                    self.in_creation.store(true, Ordering::Relaxed);
                    *self.insert_into_database.insert_mode.lock() = InsertMode::Disable;
                }
            } else if !in_creation && !no_polygon {
                if ui.button("Delete polygon").clicked() {
                    self.runner.polygon_filter.lock().clear();
                    *self.runner.filter_stats.lock() = None;
                }
            } else if in_creation && no_polygon {
                if ui.button("Cancel").clicked() {
                    self.in_creation.store(false, Ordering::Relaxed);
                    self.runner.polygon_filter.lock().clear();
                    *self.runner.filter_stats.lock() = None;
                }
                if ui.button("Remove last point").clicked() {
                    self.runner.polygon_filter.lock().pop();
                }
            } else if in_creation {
                if ui.button("Finish").clicked() {
                    self.in_creation.store(false, Ordering::Relaxed);
                    let mut polygon = self.runner.polygon_filter.lock();
                    let first = *polygon.first().unwrap();
                    polygon.push(first);
                    self.runner.wake_up.signal();
                }
                if ui.button("Remove last point").clicked() {
                    self.runner.polygon_filter.lock().pop();
                }
            }
            let polygon = self.runner.polygon_filter.lock().clone();
            let polygon = geo::geometry::Polygon::new(polygon.into(), vec![]);
            display_polygon_stats(ui, &polygon);
            let stats = self.runner.filter_stats.lock();
            if let Some(stats) = stats.as_ref() {
                ui.heading("Result");
                ui.horizontal(|ui| {
                    ui.label("Matched ");
                    ui.strong(format!("{} items", stats.nb_points_matched));
                });

                ui.horizontal(|ui| {
                    ui.label(RichText::new("[COLD]").strong().color(Color32::CYAN));
                    ui.label(" Processed in ");
                    ui.strong(format!("{:.2?}", stats.processed_in_cold));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("[HOT]").strong().color(Color32::LIGHT_RED));
                    ui.label(" Processed in ");
                    ui.strong(format!("{:.2?}", stats.processed_in_hot));
                });
                let mut display_filtering_details =
                    self.display_filtering_details.load(Ordering::Acquire);
                ui.add(
                    egui::Slider::new(
                        &mut display_filtering_details,
                        0..=stats.cell_explored.len(),
                    )
                    .text("Filtering details")
                    .smart_aim(false),
                );
                ui.vertical(|ui| {
                    if ui.small_button("+").clicked() {
                        display_filtering_details += 1;
                    }
                    if ui.small_button("-").clicked() {
                        display_filtering_details -= 1;
                    }
                });
                self.display_filtering_details
                    .store(display_filtering_details, Ordering::Release);

                let mut display_details_min = self.display_details_min_res.load(Ordering::Relaxed);
                let mut display_details_max = self.display_details_max_res.load(Ordering::Relaxed);
                ui.label(format!(
                    "Cells resolution between {display_details_min} and {display_details_max}"
                ));
                ui.add(DoubleSlider::new(
                    &mut display_details_min,
                    &mut display_details_max,
                    Resolution::Zero as usize..=Resolution::Fifteen as usize,
                ));
                self.display_details_min_res
                    .store(display_details_min, Ordering::Relaxed);
                self.display_details_max_res
                    .store(display_details_max, Ordering::Relaxed);
                let polygon = self.runner.polygon_filter.lock();
                ui.label(format!("Coords: {:?}", *polygon));
            }
        });
    }
}

impl Plugin for PolygonFiltering {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &walkers::Projector,
    ) {
        let painter = ui.painter();
        let mut line = self.runner.polygon_filter.lock();
        let in_creation = self.in_creation.load(Ordering::Relaxed);
        let mut to_display = line.clone();

        let color = if in_creation {
            if let Some(pos) = response.hover_pos() {
                let pos = projector.unproject(Vec2::new(pos.x, pos.y));
                let coord = Coord {
                    x: pos.x() as f32,
                    y: pos.y() as f32,
                };
                if response.secondary_clicked() {
                    line.push(Coord {
                        x: coord.x as f64,
                        y: coord.y as f64,
                    });
                }
                to_display.push(Coord {
                    x: coord.x as f64,
                    y: coord.y as f64,
                });
            } else if line.len() >= 2 {
                let first = *line.first().unwrap();
                to_display.push(first);
            }

            Color32::YELLOW
        } else {
            Color32::GREEN
        };

        let line = project_line_string(projector, &to_display);

        painter.line(line, PathStroke::new(8.0, color));

        // If we have a polygon + it's finished we retrieve the points it contains and display them
        if to_display.len() >= 3 && !in_creation {
            for shape in self.runner.points_matched.lock().iter() {
                match Geometry::try_from(shape.clone()).unwrap() {
                    Geometry::Point(coords) => {
                        let pos = projector.project(Position::new(coords.x(), coords.y()));
                        draw_diagonal_cross(painter, pos.to_pos2(), Color32::DARK_GREEN);
                    }
                    Geometry::MultiPoint(coords) => {
                        for coord in coords {
                            let pos = projector.project(Position::new(coord.x(), coord.y()));
                            draw_diagonal_cross(painter, pos.to_pos2(), Color32::DARK_GREEN);
                        }
                    }
                    Geometry::Polygon(polygon) => {
                        project_and_draw_line(painter, projector, polygon.exterior().points());
                    }
                    Geometry::MultiPolygon(coords) => {
                        for polygon in coords {
                            project_and_draw_line(painter, projector, polygon.exterior().points());
                        }
                    }
                    Geometry::Line(line) => {
                        project_and_draw_line(
                            painter,
                            projector,
                            [line.start_point(), line.end_point()].into_iter(),
                        );
                    }
                    Geometry::LineString(line_string) => {
                        project_and_draw_line(painter, projector, line_string.points());
                    }
                    Geometry::MultiLineString(multi_line_string) => {
                        for line_string in multi_line_string {
                            project_and_draw_line(painter, projector, line_string.points());
                        }
                    }
                    Geometry::GeometryCollection(_geometry_collection) => todo!(),
                    Geometry::Rect(rect) => {
                        project_and_draw_line(
                            painter,
                            projector,
                            rect.to_polygon().exterior().points(),
                        );
                    }
                    Geometry::Triangle(triangle) => {
                        project_and_draw_line(
                            painter,
                            projector,
                            triangle.to_polygon().exterior().points(),
                        );
                    }
                }
            }

            let display_filtering_details = self.display_filtering_details.load(Ordering::Relaxed);
            let min = self.display_details_min_res.load(Ordering::Relaxed);
            let max = self.display_details_max_res.load(Ordering::Relaxed);
            if display_filtering_details > 0 {
                if let Some(stats) = self.runner.filter_stats.lock().as_ref() {
                    for (action, cell) in stats
                        .cell_explored
                        .iter()
                        .take(display_filtering_details)
                        .filter(|(_, cell)| (min..max).contains(&(cell.resolution() as usize)))
                        .copied()
                    {
                        let color = match action {
                            FilteringStep::NotPresentInDB => Color32::BLACK,
                            FilteringStep::OutsideOfShape => Color32::RED,
                            FilteringStep::Returned => Color32::GREEN,
                            FilteringStep::RequireDoubleCheck => Color32::YELLOW,
                            FilteringStep::DeepDive => Color32::BLUE,
                        };
                        display_cell(projector, painter, cell, color);
                    }
                }
            }
        }
    }
}

fn project_and_draw_line(
    painter: &egui::Painter,
    projector: &Projector,
    points: impl Iterator<Item = geo_types::Point>,
) {
    let points = points
        .map(|point| {
            let pos = projector.project(Position::new(point.x(), point.y()));
            pos.to_pos2()
        })
        .collect();
    painter.line(points, PathStroke::new(4.0, Color32::DARK_GREEN));
}

fn display_polygon_stats(ui: &mut Ui, polygon: &geo::geometry::Polygon) {
    let points = polygon.exterior().points().len();
    let mut area = polygon.geodesic_area_signed().abs();
    let area_unit = if area < 1000.0 * 1000.0 {
        "m"
    } else {
        area /= 1000.0 * 1000.0;
        "km"
    };
    let mut perimeter = polygon.geodesic_perimeter();
    let perimeter_unit = if perimeter < 1000.0 {
        "m"
    } else {
        perimeter /= 1000.0;
        "km"
    };
    ui.horizontal_wrapped(|ui| {
        ui.label("Polygon contains ");
        ui.strong(format!("{points} points"));
        ui.label(" for a total surface of ");
        ui.strong(format!("{area:.2}{area_unit}²"));
        ui.label(" and a perimeter of ");
        ui.strong(format!("{perimeter:.2}{perimeter_unit}"));
    });
}
