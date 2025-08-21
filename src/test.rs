use std::{fmt, ops::Deref};

use geo::polygon;
use geojson::GeoJson;
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
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    # Cells
    ");

    db.build(&mut wtxn, &NoProgress).unwrap();
    insta::assert_snapshot!(db.snap(&wtxn), @r"
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
    # Items
    0: Point(Zoint { lng: 0.0, lat: 0.0 })
    1: Point(Zoint { lng: 0.0, lat: 1.0 })
    2: Point(Zoint { lng: 0.0, lat: 2.0 })
    # Cells
    Cell { res: 0, center: (2.3009, -5.2454) }: RoaringBitmap<[0]>
    ");

    db.build(&mut wtxn, &NoProgress).unwrap();

    insta::assert_snapshot!(db.snap(&wtxn), @r"
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
