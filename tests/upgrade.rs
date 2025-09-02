use cellulite::Cellulite;
use geo::polygon;
use steppe::NoProgress;

#[test]
fn from_0_2_0() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/assets/v0_2_0.mdb/data.mdb",
        dir.path().join("data.mdb"),
    )
    .unwrap();
    let env = unsafe {
        heed::EnvOpenOptions::new()
            .map_size(1024 * 1024 * 1024)
            .max_dbs(Cellulite::nb_dbs())
            .open(dir.path())
            .unwrap()
    };
    let mut wtxn = env.write_txn().unwrap();
    let cellulite = Cellulite::create_from_env(&env, &mut wtxn).unwrap();
    insta::assert_snapshot!(cellulite.get_version(&wtxn).unwrap(), @"0.2.0");

    // This matches only a subset of the multi-point containing all the trees
    let trees = polygon![
       (x: 3.6056618690490723, y: 43.990875244140625),
       (x: 3.6060049533843994, y: 43.99085998535156),
       (x: 3.6059892177581787, y: 43.990657806396484),
       (x: 3.6056606769561768, y: 43.99066162109375),
       (x: 3.6056618690490723, y: 43.990875244140625)
    ];

    let ret = cellulite.in_shape(&wtxn, &trees, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[3]>");
    let shape = cellulite.item(&wtxn, 3).unwrap().unwrap();
    assert!(shape.to_multi_points().is_some());

    // This matches my desk (a point) which is contained in the movie theater
    let desk = polygon![
         (x: 3.607173442840576, y: 43.991546630859375),
         (x: 3.607184648513794, y: 43.99155807495117),
         (x: 3.6071949005126953, y: 43.99154281616211),
         (x: 3.607174873352051, y: 43.99154281616211),
         (x: 3.607173442840576, y: 43.991546630859375)
    ];

    let ret = cellulite.in_shape(&wtxn, &desk, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[1, 2]>");
    let shape = cellulite.item(&wtxn, 1).unwrap().unwrap();
    assert!(shape.to_point().is_some());
    let shape = cellulite.item(&wtxn, 2).unwrap().unwrap();
    assert!(shape.to_polygon().is_some());

    // The next lines will break the day we do a DB-breaking, we should call the update function here
    cellulite.delete(&mut wtxn, 2).unwrap();
    cellulite
        .add(
            &mut wtxn,
            1000,
            &geojson::GeoJson::Geometry(geojson::Value::Point(vec![0.0, 0.0]).into()),
        )
        .unwrap();
    cellulite.build(&mut wtxn, &NoProgress).unwrap();

    // We do the two same query except we removed the movie theater
    let ret = cellulite.in_shape(&wtxn, &trees, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[3]>");
    let shape = cellulite.item(&wtxn, 3).unwrap().unwrap();
    assert!(shape.to_multi_points().is_some());

    let ret = cellulite.in_shape(&wtxn, &desk, &mut |_| ()).unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[1]>");
    let shape = cellulite.item(&wtxn, 1).unwrap().unwrap();
    assert!(shape.to_point().is_some());
    let shape = cellulite.item(&wtxn, 2).unwrap();
    assert!(shape.is_none());

    // We should also be able to query the new point we inserted in 0,0
    let ret = cellulite
        .in_circle(&wtxn, geo::Point::default(), 0.2, 20)
        .unwrap();
    insta::assert_debug_snapshot!(ret, @"RoaringBitmap<[1000]>");
    let shape = cellulite.item(&wtxn, 1000).unwrap().unwrap();
    assert!(shape.to_point().is_some());
}
