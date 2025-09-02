use std::sync::atomic::{AtomicU64, Ordering};

use egui::{epaint::PathStroke, Color32, Painter, Pos2, Ui, Vec2};
use geo::{
    line_measures::Densifiable, Contains, Densify, Geometry, Haversine, Intersects, MultiPolygon,
    Point, Rect,
};
use geo_types::Coord;
use geojson::GeoJson;
use h3o::CellIndex;
use walkers::{Position, Projector};

pub struct AtomicF64 {
    storage: AtomicU64,
}
impl AtomicF64 {
    pub fn new(value: f64) -> Self {
        let as_u64 = value.to_bits();
        Self {
            storage: AtomicU64::new(as_u64),
        }
    }
    pub fn store(&self, value: f64, ordering: Ordering) {
        let as_u64 = value.to_bits();
        self.storage.store(as_u64, ordering)
    }
    pub fn load(&self, ordering: Ordering) -> f64 {
        let as_u64 = self.storage.load(ordering);
        f64::from_bits(as_u64)
    }
}

pub fn project_line_string(projector: &Projector, line: &[Coord]) -> Vec<Pos2> {
    let line = geo::LineString::from(line.to_vec());
    let line = Haversine.densify(&line, 1_000.0);
    line.into_points()
        .into_iter()
        .map(|point| {
            projector
                .project(Position::new(point.x(), point.y()))
                .to_pos2()
        })
        .collect()
}

pub fn display_cell(projector: &Projector, painter: &Painter, cell: CellIndex, color: Color32) {
    let cell_polygon = MultiPolygon::from(cell);
    for polygon in cell_polygon.into_iter() {
        let line = project_line_string(projector, &polygon.exterior().0);

        // Check if cell is at least 1 pixel in size by comparing projected points

        painter.line(
            line,
            PathStroke::new(16.0 - cell.resolution() as u8 as f32, color),
        );
    }
}

/// Draw a cross at the given center position with the specified size and color
pub fn draw_diagonal_cross(painter: &Painter, point: Pos2, color: Color32) {
    let size: f32 = 8.0;
    let stroke_width: f32 = 4.0;
    painter.line(
        vec![point - Vec2::splat(size), point + Vec2::splat(size)],
        PathStroke::new(stroke_width, color),
    );
    painter.line(
        vec![
            point + Vec2::new(size, -size),
            point + Vec2::new(-size, size),
        ],
        PathStroke::new(stroke_width, color),
    );
}

/// Draw an orthogonal cross (plus sign) at the given center position with the specified size and color
pub fn draw_orthogonal_cross(painter: &Painter, point: Pos2, color: Color32) {
    let size: f32 = 8.0;
    let stroke_width: f32 = 4.0;
    // Draw the horizontal line
    painter.line(
        vec![point + Vec2::new(-size, 0.0), point + Vec2::new(size, 0.0)],
        PathStroke::new(stroke_width, color),
    );

    // Draw the vertical line
    painter.line(
        vec![point + Vec2::new(0.0, -size), point + Vec2::new(0.0, size)],
        PathStroke::new(stroke_width, color),
    );
}

/// Draw geometrical shape on map
pub fn draw_geometry_on_map(
    projector: &walkers::Projector,
    displayed_rect: Rect,
    painter: &egui::Painter,
    value: &GeoJson,
) {
    let geom = Geometry::try_from(value.clone()).unwrap();
    match geom {
        Geometry::Point(coords) => {
            let coord = h3o::LatLng::new(coords.y(), coords.x()).unwrap();
            if !displayed_rect.contains(&Point::new(coord.lng(), coord.lat())) {
                return;
            }
            let center = projector.project(Position::new(coord.lng(), coord.lat()));
            draw_orthogonal_cross(painter, center.to_pos2(), Color32::BLACK);
        }
        Geometry::MultiPoint(coords) => {
            for coord in coords {
                let coord = h3o::LatLng::new(coord.y(), coord.x()).unwrap();
                if !displayed_rect.contains(&Point::new(coord.lng(), coord.lat())) {
                    continue;
                }
                let center = projector.project(Position::new(coord.lng(), coord.lat()));
                draw_orthogonal_cross(painter, center.to_pos2(), Color32::BLACK);
            }
        }
        Geometry::Polygon(coords) => {
            let polygon: geo::Polygon = coords.clone();

            if polygon.intersects(&displayed_rect) {
                let points: Vec<_> = polygon
                    .densify(&Haversine, 10_000.0)
                    .exterior()
                    .points()
                    .map(|point| {
                        let pos = projector.project(Position::new(point.x(), point.y()));
                        pos.to_pos2()
                    })
                    .collect();
                painter.line(points, PathStroke::new(4.0, Color32::BLACK));
            }
        }
        Geometry::MultiPolygon(polygons) => {
            for polygon in polygons {
                if polygon.intersects(&displayed_rect) {
                    let points: Vec<_> = polygon
                        .densify(&Haversine, 10_000.0)
                        .exterior()
                        .points()
                        .map(|point| {
                            let pos = projector.project(Position::new(point.x(), point.y()));
                            pos.to_pos2()
                        })
                        .collect();
                    painter.line(points, PathStroke::new(4.0, Color32::BLACK));
                }
            }
        }
        _ => todo!(),
    }
}

pub fn extract_displayed_rect(ui: &mut Ui, projector: &Projector) -> Rect {
    let x = ui.available_width();
    let y = ui.available_height();
    let top_left = projector.unproject(Vec2 { x: 0.0, y: 0.0 });
    let bottom_right = projector.unproject(Vec2 { x, y });
    Rect::new(
        Point::new(top_left.x(), top_left.y()),
        Point::new(bottom_right.x(), bottom_right.y()),
    )
}
