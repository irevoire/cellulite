use std::collections::{HashSet, VecDeque};

use geo::{Densify, Destination, Haversine, MultiPolygon, Point, Polygon};
use h3o::{
    CellIndex, Resolution,
    geom::{ContainmentMode, TilerBuilder},
};
use heed::RoTxn;
use roaring::RoaringBitmap;
use zerometry::RelationBetweenShapes;

use crate::{Cellulite, Result, keys::Key};

impl Cellulite {
    pub fn in_shape(&self, rtxn: &RoTxn, polygon: &Polygon) -> Result<RoaringBitmap> {
        self.in_shape_with_inspector(rtxn, polygon, &mut |_| ())
    }

    /// Return all the items that intersects or are contained in the specified polygon.
    /// The `inspector` lets you see how the search was made internally.
    // The strategy to retrieve the points in a shape is to:
    // 1. Retrieve all the cell@res0 that contains the shape
    // 2. Iterate over these cells
    //  2.1.If a cell fit entirely *inside* the shape, add all its items to the result
    //  2.2 Otherwise:
    //   - If the cell is a leaf => iterate over all of its point and add the one that fits in the shape to the result
    //   - Otherwise, increase the precision and iterate on the range of cells => repeat step 2
    pub fn in_shape_with_inspector(
        &self,
        rtxn: &RoTxn,
        polygon: &Polygon,
        inspector: &mut dyn FnMut((FilteringStep, CellIndex)),
    ) -> Result<RoaringBitmap> {
        let polygon = Haversine.densify(polygon, 1_000.0);
        let mut tiler = TilerBuilder::new(Resolution::Zero)
            .containment_mode(ContainmentMode::Covers)
            .build();
        tiler.add(polygon.clone())?;

        let mut ret = RoaringBitmap::new();
        let mut double_check = RoaringBitmap::new();
        let mut to_explore: VecDeque<_> = tiler.into_coverage().collect();
        let mut already_explored: HashSet<CellIndex> = HashSet::with_capacity(to_explore.len());
        let mut too_large = false;

        while let Some(cell) = to_explore.pop_front() {
            if !already_explored.insert(cell) {
                continue;
            }

            let Some(items) = self.cell_db().get(rtxn, &Key::Cell(cell))? else {
                (inspector)((FilteringStep::NotPresentInDB, cell));
                continue;
            };

            let cell_polygon = MultiPolygon::from(cell);

            if geo::Contains::contains(&polygon, &cell_polygon) {
                (inspector)((FilteringStep::Returned, cell));
                ret |= items;
            } else if geo::Intersects::intersects(&polygon, &cell_polygon) {
                let resolution = cell.resolution();
                if items.len() < self.threshold || resolution == Resolution::Fifteen {
                    (inspector)((FilteringStep::RequireDoubleCheck, cell));
                    double_check |= items;
                } else {
                    (inspector)((FilteringStep::DeepDive, cell));
                    let mut tiler = TilerBuilder::new(resolution.succ().unwrap())
                        .containment_mode(ContainmentMode::Covers)
                        .build();
                    if too_large {
                        tiler.add_batch(cell_polygon.into_iter())?;
                    } else {
                        tiler.add(polygon.clone())?;
                    }

                    let mut cell_number = 0;

                    for cell in tiler.into_coverage() {
                        if !already_explored.contains(&cell) {
                            to_explore.push_back(cell);
                        }
                        cell_number += 1;
                    }

                    if cell_number > 3 {
                        too_large = true;
                    }
                }
            } else {
                // else: we can ignore the cell, it's not part of our shape
                (inspector)((FilteringStep::OutsideOfShape, cell));
            }
        }

        // Since we have overlap some items may have been definitely validated somewhere but were also included as something to double check
        double_check -= &ret;

        for item in double_check {
            let shape = self.item_db().get(rtxn, &item)?.unwrap();
            if shape.any_relation(&polygon).any_relation() {
                ret.insert(item);
            }
        }

        Ok(ret)
    }

    /// Retrieve all items intersecting a circle with a given center and radius, according to the Haversine model.
    /// This is approximate. It may miss items that are in the circle, but it will never return items that are not in the circle.
    /// The resolution parameter controls the number of points used to approximate the circle.
    ///
    /// See also [`in_circle_with_params`] for a more advanced version of this function.
    pub fn in_circle(
        &self,
        rtxn: &RoTxn,
        center: Point,
        radius: f64,
        resolution: usize,
    ) -> Result<RoaringBitmap> {
        self.in_circle_with_inspector(rtxn, center, radius, resolution, &Haversine, &mut |_| ())
    }

    /// Retrieve all items intersecting a circle with a given center and radius.
    /// This is approximate. It may miss items that are in the circle, but it will never return items that are not in the circle.
    /// The resolution parameter controls the number of points used to approximate the circle.
    pub fn in_circle_with_inspector<Measure: Destination<f64>>(
        &self,
        rtxn: &RoTxn,
        center: Point,
        radius: f64,
        resolution: usize,
        measure: &Measure,
        inspector: &mut dyn FnMut((FilteringStep, CellIndex)),
    ) -> Result<RoaringBitmap> {
        let n = resolution as f64;

        // Build a circle-approximating polygon that tries to cover the real circle
        let mut points = Vec::new();
        for i in 0..resolution {
            let bearing = 360.0 * i as f64 / n;
            points.push(measure.destination(center, bearing, radius));
        }

        let polygon = Polygon::new(points.into(), Vec::new());

        self.in_shape_with_inspector(rtxn, &polygon, inspector)
    }
}

#[derive(Debug, Copy, Clone)]
pub enum FilteringStep {
    NotPresentInDB,
    OutsideOfShape,
    Returned,
    RequireDoubleCheck,
    DeepDive,
}
