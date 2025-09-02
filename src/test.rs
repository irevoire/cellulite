use std::{fmt, ops::Deref};

use geo::{GeometryCollection, point, polygon};
use geojson::{FeatureCollection, GeoJson};
use h3o::LatLng;
use heed::{Env, EnvOpenOptions, RoTxn, WithTls};
use steppe::NoProgress;
use tempfile::TempDir;

use crate::{
    CellKeyCodec, Cellulite, ItemId, Key, KeyPrefixVariantCodec, KeyVariant,
    roaring::RoaringBitmapCodec,
};

pub struct DatabaseHandle {
    pub env: Env<WithTls>,
    pub database: Cellulite,
    #[allow(unused)]
    pub tempdir: TempDir,
}

impl Deref for DatabaseHandle {
    type Target = Cellulite;

    fn deref(&self) -> &Self::Target {
        &self.database
    }
}

impl DatabaseHandle {
    fn snap(&self, rtxn: &RoTxn) -> String {
        let mut s = String::new();

        s.push_str(&format!(
            "# Version: {}\n",
            self.database.get_version(rtxn).unwrap()
        ));
        s.push_str("# Items\n");
        let iter = self.database.item.iter(rtxn).unwrap();
        for ret in iter {
            let (key, value) = ret.unwrap();
            s.push_str(&format!("{key}: {value:?}\n"));
        }

        s.push_str("# Cells\n");
        let iter = self
            .database
            .cell
            .remap_types::<KeyPrefixVariantCodec, RoaringBitmapCodec>()
            .prefix_iter(rtxn, &KeyVariant::Cell)
            .unwrap()
            .remap_key_type::<CellKeyCodec>();
        for ret in iter {
            let (key, value) = ret.unwrap();
            let Key::Cell(cell) = key else { unreachable!() };
            let lat_lng = LatLng::from(cell);
            let (lat, lng) = (lat_lng.lat(), lat_lng.lng());
            let res = cell.resolution();
            s.push_str(&format!(
                "Cell {{ res: {res}, center: ({lat:.4}, {lng:.4}) }}: {value:?}\n"
            ));
        }

        s.push_str("# Belly Cells\n");
        let iter = self
            .database
            .cell
            .remap_types::<KeyPrefixVariantCodec, RoaringBitmapCodec>()
            .prefix_iter(rtxn, &KeyVariant::Belly)
            .unwrap()
            .remap_key_type::<CellKeyCodec>();
        for ret in iter {
            let (key, value) = ret.unwrap();
            let Key::Belly(cell) = key else {
                unreachable!()
            };
            let lat_lng = LatLng::from(cell);
            let (lat, lng) = (lat_lng.lat(), lat_lng.lng());
            let res = cell.resolution();
            s.push_str(&format!(
                "Cell {{ res: {res}, center: ({lat:.4}, {lng:.4}) }}: {value:?}\n"
            ));
        }

        s
    }
}

fn create_database() -> DatabaseHandle {
    let dir = tempfile::tempdir().unwrap();
    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(200 * 1024 * 1024)
            .max_dbs(Cellulite::nb_dbs())
            .open(dir.path())
    }
    .unwrap();
    let mut wtxn = env.write_txn().unwrap();
    let cellulite = Cellulite::create_from_env(&env, &mut wtxn).unwrap();
    wtxn.commit().unwrap();
    DatabaseHandle {
        env,
        database: cellulite,
        tempdir: dir,
    }
}

pub struct NnRes(pub Option<Vec<(ItemId, (f64, f64))>>);

impl fmt::Display for NnRes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(ref vec) => {
                for (id, (lat, lng)) in vec {
                    writeln!(f, "id({id}): coord({lat:.5}, {lng:.5})")?;
                }
                Ok(())
            }
            None => f.write_str("No results found"),
        }
    }
}

#[test]
fn basic_write() {
    let mut db = create_database();
    let mut wtxn = db.env.write_txn().unwrap();
    db.database.threshold = 3;
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        0.0, 0.0,
    ])));
    db.add(&mut wtxn, 0, &point).unwrap();

    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    # Cells
    ");

    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    # Cells
    Cell { res: 0, center: (2.3009, -5.2454) }: RoaringBitmap<[0]>
    ");

    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        0.0, 1.0,
    ])));
    db.add(&mut wtxn, 1, &point).unwrap();
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        0.0, 2.0,
    ])));
    db.add(&mut wtxn, 2, &point).unwrap();

    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    1: Point(Zoint { lng: 0.0, lat: 1.0 })
    2: Point(Zoint { lng: 0.0, lat: 2.0 })
    # Cells
    Cell { res: 0, center: (2.3009, -5.2454) }: RoaringBitmap<[0]>
    ");

    db.build(&mut wtxn, &NoProgress).unwrap();

    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    1: Point(Zoint { lng: 0.0, lat: 1.0 })
    2: Point(Zoint { lng: 0.0, lat: 2.0 })
    # Cells
    Cell { res: 0, center: (2.3009, -5.2454) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 1, center: (2.0979, 0.4995) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 2, center: (2.0979, 0.4995) }: RoaringBitmap<[1, 2]>
    Cell { res: 2, center: (-0.4597, 0.5342) }: RoaringBitmap<[0]>
    ");

    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        0.0, 3.0,
    ])));
    db.add(&mut wtxn, 3, &point).unwrap();
    db.build(&mut wtxn, &NoProgress).unwrap();

    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    1: Point(Zoint { lng: 0.0, lat: 1.0 })
    2: Point(Zoint { lng: 0.0, lat: 2.0 })
    3: Point(Zoint { lng: 0.0, lat: 3.0 })
    # Cells
    Cell { res: 0, center: (2.3009, -5.2454) }: RoaringBitmap<[0, 1, 2, 3]>
    Cell { res: 1, center: (2.0979, 0.4995) }: RoaringBitmap<[0, 1, 2, 3]>
    Cell { res: 2, center: (2.0979, 0.4995) }: RoaringBitmap<[1, 2, 3]>
    Cell { res: 2, center: (-0.4597, 0.5342) }: RoaringBitmap<[0]>
    Cell { res: 3, center: (2.1299, -0.3656) }: RoaringBitmap<[2]>
    Cell { res: 3, center: (1.2792, -0.0699) }: RoaringBitmap<[1]>
    Cell { res: 3, center: (2.9436, 0.1993) }: RoaringBitmap<[3]>
    ");
}

#[test]
fn bug_write_points_create_cells_too_deep() {
    // This simple test was creating 5 cells instead of 3 with two cells too deep for on reason.
    let mut db = create_database();
    let mut wtxn = db.env.write_txn().unwrap();
    db.database.threshold = 2;
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        -11.460678226504395,
        48.213563161838714,
    ])));
    db.add(&mut wtxn, 0, &point).unwrap();
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        -1.520397001416467,
        54.586501531522245,
    ])));
    db.add(&mut wtxn, 1, &point).unwrap();
    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: -11.460678226504395, lat: 48.213563161838714 })
    1: Point(Zoint { lng: -1.520397001416467, lat: 54.586501531522245 })
    # Cells
    Cell { res: 0, center: (52.6758, -11.6016) }: RoaringBitmap<[0, 1]>
    Cell { res: 1, center: (45.2992, -14.2485) }: RoaringBitmap<[0]>
    Cell { res: 1, center: (53.6528, 0.2143) }: RoaringBitmap<[1]>
    ");
}

#[test]
fn bug_write_points_create_unrelated_cells() {
    // This simple test was creating 4 cells instead of 3 with two completely unrelated cells.
    let mut db = create_database();
    let mut wtxn = db.env.write_txn().unwrap();
    db.database.threshold = 2;
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        6.0197316417968105,
        49.63676497357687,
    ])));
    db.add(&mut wtxn, 0, &point).unwrap();

    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        7.435508967561083,
        43.76438119061842,
    ])));
    db.add(&mut wtxn, 1, &point).unwrap();
    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: 6.0197316417968105, lat: 49.63676497357687 })
    1: Point(Zoint { lng: 7.435508967561083, lat: 43.76438119061842 })
    # Cells
    Cell { res: 0, center: (48.7583, 18.3030) }: RoaringBitmap<[0, 1]>
    Cell { res: 1, center: (47.9847, 6.9179) }: RoaringBitmap<[0]>
    Cell { res: 1, center: (40.9713, 2.8207) }: RoaringBitmap<[1]>
    ");
}

#[test]
fn query_points_on_transmeridian_cell() {
    // In this test we want to make sure that we can insert point that fall on a transmeridian cell.
    // We want to first make sure it works with a res0 cell and then a res1 as they're both handled
    // in two different function.

    // The lake is a transmeridian cell at res0 but not at res1
    let lake = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        -172.36201, 64.42921,
    ])));
    // The airport is a transmeridian cell at res0 and res1
    let airport = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![
        -173.23841, 64.37949,
    ])));
    let contains_lake =
        polygon![(x: -172.37, y: 64.420), (x: -172.17, y: 64.420), (x: -172.17, y: 64.69)];
    let contains_airport =
        polygon![(x: -173.3, y: 64.3), (x: -173.3, y: 64.4), (x: -173.0, y: 64.4)];
    let contains_both = polygon![(x: -174.0, y: 64.0), (x: -172.0, y: 64.0), (x: -172.0, y: 65.0)];

    let mut db = create_database();
    let mut wtxn = db.env.write_txn().unwrap();
    db.database.threshold = 2;

    db.add(&mut wtxn, 0, &lake).unwrap();
    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: -172.36201, lat: 64.42921 })
    # Cells
    Cell { res: 0, center: (64.4181, -158.9175) }: RoaringBitmap<[0]>
    ");

    let ret = db.in_shape(&wtxn, &contains_lake, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[0]>");
    let ret = db.in_shape(&wtxn, &contains_airport, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[]>");
    let ret = db.in_shape(&wtxn, &contains_both, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[0]>");

    db.add(&mut wtxn, 1, &airport).unwrap();
    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Point(Zoint { lng: -172.36201, lat: 64.42921 })
    1: Point(Zoint { lng: -173.23841, lat: 64.37949 })
    # Cells
    Cell { res: 0, center: (64.4181, -158.9175) }: RoaringBitmap<[0, 1]>
    Cell { res: 1, center: (67.6370, -175.8874) }: RoaringBitmap<[0, 1]>
    Cell { res: 2, center: (62.9574, -171.6851) }: RoaringBitmap<[0]>
    Cell { res: 2, center: (64.6946, -176.8313) }: RoaringBitmap<[1]>
    ");

    let ret = db.in_shape(&wtxn, &contains_lake, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[0]>");
    let ret = db
        .in_shape(&wtxn, &contains_airport, &mut |s| println!("{s:?}"))
        .unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[1]>");
    let ret = db.in_shape(&wtxn, &contains_both, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[0, 1]>");
}

#[test]
fn store_all_kind_of_collection() {
    // Purpose of the test is just to make sure w e can store all kinds of collection
    let mut db = create_database();
    let mut wtxn = db.env.write_txn().unwrap();
    db.database.threshold = 2;
    let geometry_collection = geojson::Value::GeometryCollection(vec![geojson::Geometry::new(
        geojson::Value::Point(vec![6.0197316417968105, 49.63676497357687]),
    )]);
    let geometry = geojson::Geometry::new(geometry_collection.clone());
    let feature = geojson::Feature {
        geometry: Some(geometry.clone()),
        ..Default::default()
    };
    let feature_collection = geojson::FeatureCollection {
        features: vec![feature.clone()],
        ..Default::default()
    };
    db.add(&mut wtxn, 0, &geometry.into()).unwrap();
    db.add(&mut wtxn, 1, &feature.into()).unwrap();
    db.add(&mut wtxn, 2, &feature_collection.into()).unwrap();

    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Collection(Zollection { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: ZultiPoints { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: [Zoint { lng: 6.0197316417968105, lat: 49.63676497357687 }] }, lines: ZultiLines { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zines: [] }, polygons: ZultiPolygons { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zolygons: [] } })
    1: Collection(Zollection { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: ZultiPoints { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: [Zoint { lng: 6.0197316417968105, lat: 49.63676497357687 }] }, lines: ZultiLines { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zines: [] }, polygons: ZultiPolygons { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zolygons: [] } })
    2: Collection(Zollection { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: ZultiPoints { bounding_box: BoundingBox { bottom_left: Coord { x: 6.0197316417968105, y: 49.63676497357687 }, top_right: Coord { x: 6.0197316417968105, y: 49.63676497357687 } }, points: [Zoint { lng: 6.0197316417968105, lat: 49.63676497357687 }] }, lines: ZultiLines { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zines: [] }, polygons: ZultiPolygons { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zolygons: [] } })
    # Cells
    Cell { res: 0, center: (48.7583, 18.3030) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 1, center: (47.9847, 6.9179) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 2, center: (50.4683, 5.6987) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 3, center: (49.7185, 6.6446) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 4, center: (49.7700, 6.0547) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 5, center: (49.6335, 5.9622) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 6, center: (49.6264, 6.0462) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 7, center: (49.6418, 6.0270) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 8, center: (49.6356, 6.0186) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 9, center: (49.6356, 6.0186) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 10, center: (49.6365, 6.0198) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 11, center: (49.6368, 6.0194) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 12, center: (49.6368, 6.0197) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 13, center: (49.6368, 6.0197) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 14, center: (49.6368, 6.0197) }: RoaringBitmap<[0, 1, 2]>
    Cell { res: 15, center: (49.6368, 6.0197) }: RoaringBitmap<[0, 1, 2]>
    ");
    let contains =
        polygon![(x: 6.0, y: 49.0), (x: 7.0, y: 49.0), (x: 7.0, y: 50.0), (x: 6.0, y: 50.0)];
    let ret = db.in_shape(&wtxn, &contains, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[0, 1, 2]>");
}

#[test]
fn write_polygon_with_belly_cells_at_res0() {
    // We'll put one point inside of a large polygon.
    // With a thresholds of 2, if the belly cells are broken this will generates
    // cell at res 1.
    // If everything goes well, the polygon will have a bunch of belly cells +
    // normal cells for its edges.
    let mut cellulite = create_database();
    let mut wtxn = cellulite.env.write_txn().unwrap();
    cellulite.database.threshold = 2;
    let point = GeometryCollection::from(point! { x:-10.38791, y: 51.68380 });
    cellulite
        .add(&mut wtxn, 0, &FeatureCollection::from(&point).into())
        .unwrap();
    let shape = GeoJson::from(geojson::Geometry::new(geojson::Value::from(&polygon![
         (x: -36.80442428588867, y: 59.85004425048828),
         (x: -8.567954063415527, y: 65.76936340332031),
         (x: 12.589740753173828, y: 56.09892654418945),
         (x: 6.169264793395996, y: 41.49180603027344),
         (x: -11.232604026794434, y: 37.05668258666992),
         (x: -32.81175231933594, y: 44.35645294189453),
         (x: -36.80442428588867, y: 59.85004425048828)
    ])));
    cellulite.add(&mut wtxn, 1, &shape).unwrap();

    cellulite.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(cellulite.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Collection(Zollection { bounding_box: BoundingBox { bottom_left: Coord { x: -10.38791, y: 51.6838 }, top_right: Coord { x: -10.38791, y: 51.6838 } }, points: ZultiPoints { bounding_box: BoundingBox { bottom_left: Coord { x: -10.38791, y: 51.6838 }, top_right: Coord { x: -10.38791, y: 51.6838 } }, points: [Zoint { lng: -10.38791, lat: 51.6838 }] }, lines: ZultiLines { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zines: [] }, polygons: ZultiPolygons { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zolygons: [] } })
    1: Polygon(Zolygon { bounding_box: BoundingBox { bottom_left: Coord { x: -36.80442428588867, y: 37.05668258666992 }, top_right: Coord { x: 12.589740753173828, y: 65.76936340332031 } }, coords: [Coord { x: -36.80442428588867, y: 59.85004425048828 }, Coord { x: -8.567954063415527, y: 65.76936340332031 }, Coord { x: 12.589740753173828, y: 56.09892654418945 }, Coord { x: 6.169264793395996, y: 41.49180603027344 }, Coord { x: -11.232604026794434, y: 37.05668258666992 }, Coord { x: -32.81175231933594, y: 44.35645294189453 }, Coord { x: -36.80442428588867, y: 59.85004425048828 }] })
    # Cells
    Cell { res: 0, center: (69.6635, -30.9680) }: RoaringBitmap<[1]>
    Cell { res: 0, center: (64.7000, 10.5362) }: RoaringBitmap<[1]>
    Cell { res: 0, center: (52.6758, -11.6016) }: RoaringBitmap<[0]>
    Cell { res: 0, center: (50.1598, -44.6097) }: RoaringBitmap<[1]>
    Cell { res: 0, center: (48.7583, 18.3030) }: RoaringBitmap<[1]>
    Cell { res: 0, center: (34.3884, -25.8177) }: RoaringBitmap<[1]>
    Cell { res: 0, center: (33.7110, -0.5345) }: RoaringBitmap<[1]>
    # Belly Cells
    Cell { res: 0, center: (52.6758, -11.6016) }: RoaringBitmap<[1]>
    ");

    let filter = polygon![
         (x: -13.35211181640625, y: 51.78105163574219),
         (x: -9.380537986755371, y: 53.26967239379883),
         (x: -6.9279303550720215, y: 49.60155487060547),
         (x: -13.3521118164062, y: 51.781051635742195)
    ];
    let res = cellulite.in_shape(&wtxn, &filter, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(res, @"RoaringBitmap<[0]>");
}

#[test]
fn write_polygon_with_belly_cells_at_res1() {
    // same test as above except we're doing everything at the res1 to be sure both the code at resolution 0 and 1 works
    let mut cellulite = create_database();
    let mut wtxn = cellulite.env.write_txn().unwrap();
    cellulite.database.threshold = 2;
    let point = GeometryCollection::from(point! { x:-10.89288, y: 52.91525 });
    cellulite
        .add(&mut wtxn, 0, &FeatureCollection::from(&point).into())
        .unwrap();
    let shape = GeoJson::from(geojson::Geometry::new(geojson::Value::from(&polygon![
        (x: -22.350751876831055, y: 54.04570388793945),
        (x: -14.230262756347656, y: 57.86238098144531),
        (x: -3.6089367866516113, y: 56.31303405761719),
        (x: -1.9412200450897217, y: 50.917137145996094),
        (x: -7.79402494430542, y: 46.764404296875),
        (x: -18.57700538635254, y: 48.349578857421875),
        (x: -22.350751876831055, y: 54.04570388793945)
    ])));
    cellulite.add(&mut wtxn, 1, &shape).unwrap();

    cellulite.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(cellulite.snap(&wtxn), @r"
    # Version: 0.2.0
    # Items
    0: Collection(Zollection { bounding_box: BoundingBox { bottom_left: Coord { x: -10.89288, y: 52.91525 }, top_right: Coord { x: -10.89288, y: 52.91525 } }, points: ZultiPoints { bounding_box: BoundingBox { bottom_left: Coord { x: -10.89288, y: 52.91525 }, top_right: Coord { x: -10.89288, y: 52.91525 } }, points: [Zoint { lng: -10.89288, lat: 52.91525 }] }, lines: ZultiLines { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zines: [] }, polygons: ZultiPolygons { bounding_box: BoundingBox { bottom_left: Coord { x: 0.0, y: 0.0 }, top_right: Coord { x: 0.0, y: 0.0 } }, zolygons: [] } })
    1: Polygon(Zolygon { bounding_box: BoundingBox { bottom_left: Coord { x: -22.350751876831055, y: 46.764404296875 }, top_right: Coord { x: -1.9412200450897217, y: 57.86238098144531 } }, coords: [Coord { x: -22.350751876831055, y: 54.04570388793945 }, Coord { x: -14.230262756347656, y: 57.86238098144531 }, Coord { x: -3.6089367866516113, y: 56.31303405761719 }, Coord { x: -1.9412200450897217, y: 50.917137145996094 }, Coord { x: -7.79402494430542, y: 46.764404296875 }, Coord { x: -18.57700538635254, y: 48.349578857421875 }, Coord { x: -22.350751876831055, y: 54.04570388793945 }] })
    # Cells
    Cell { res: 0, center: (52.6758, -11.6016) }: RoaringBitmap<[0, 1]>
    Cell { res: 1, center: (52.6758, -11.6016) }: RoaringBitmap<[0]>
    Cell { res: 1, center: (46.9280, -3.5647) }: RoaringBitmap<[1]>
    Cell { res: 1, center: (50.3971, -23.4023) }: RoaringBitmap<[1]>
    Cell { res: 1, center: (45.2992, -14.2485) }: RoaringBitmap<[1]>
    Cell { res: 1, center: (59.3461, -8.2620) }: RoaringBitmap<[1]>
    Cell { res: 1, center: (53.6528, 0.2143) }: RoaringBitmap<[1]>
    Cell { res: 1, center: (57.6409, -21.6939) }: RoaringBitmap<[1]>
    # Belly Cells
    Cell { res: 1, center: (52.6758, -11.6016) }: RoaringBitmap<[1]>
    ");

    let filter = polygon![
         (x: -14.853970527648926, y: 52.716609954833984),
         (x: -10.159256935119629, y: 55.055213928222656),
         (x: -7.785157203674316, y: 51.10857391357422),
         (x: -14.853970527648926, y: 52.716609954833984)
    ];
    let res = cellulite.in_shape(&wtxn, &filter, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(res, @"RoaringBitmap<[0]>");
}

/*
#[test]
fn basic_nearest() {
    let handle = create_database();
    let mut wtxn = handle.env.write_txn().unwrap();
    let mut writer = Writer::new(handle.database);
    writer.threshold = 10;
    // We'll draw a simple line over the y as seen below
    // (0,0) # # # # # # ...
    for i in 0..100 {
        let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![i as f64, 0.0])));
        writer.add_item(&mut wtxn, i, &point).unwrap();
    }
    // insta::assert_snapshot!(handle.snap(&wtxn), @r###"
    // "###);
    wtxn.commit().unwrap();

    let rtxn = handle.env.read_txn().unwrap();
    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![0.0, 0.0])));
    let ret = writer.nearest_point(&rtxn, &point, 5).unwrap();

    insta::assert_snapshot!(NnRes(Some(ret)), @r###"
        id(0): coord(0.00000, 0.00000)
        id(1): coord(1.00000, -0.00000)
        id(2): coord(2.00000, 0.00000)
        id(3): coord(3.00000, -0.00000)
        id(4): coord(4.00000, 0.00000)
        "###);

    let point = GeoJson::from(geojson::Geometry::new(geojson::Value::Point(vec![50.0, 0.0])));
    let ret = writer.nearest_point(&rtxn, &point, 5).unwrap();

    insta::assert_snapshot!(NnRes(Some(ret)), @r###"
        id(50): coord(50.00000, 0.00001)
        id(49): coord(49.00000, 0.00000)
        id(51): coord(51.00000, -0.00000)
        id(48): coord(48.00000, 0.00000)
        id(52): coord(52.00000, 0.00000)
        "###);
}
*/
