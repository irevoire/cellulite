use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use cellulite::{reader::FilteringStep, roaring::RoaringBitmapCodec, Cellulite, Stats};
use egui::mutex::Mutex;
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use geo_types::{Coord, LineString, Polygon};
use geojson::GeoJson;
use h3o::CellIndex;
use heed::{
    types::{Bytes, Str},
    Database, Env,
};
use roaring::RoaringBitmap;
use steppe::default::DefaultProgress;

#[derive(Clone)]
pub struct Runner {
    pub env: Env,
    pub db: Cellulite,
    pub metadata: Database<Str, Bytes>,
    pub wake_up: Arc<synchronoise::SignalEvent>,

    // Communication input
    pub to_insert: Arc<Mutex<Vec<(String, GeoJson)>>>,
    pub polygon_filter: Arc<Mutex<Vec<Coord<f64>>>>,

    // Communication output
    pub stats: Arc<Mutex<Stats>>,
    pub filter_stats: Arc<Mutex<Option<FilterStats>>>,
    pub points_matched: Arc<Mutex<Vec<GeoJson>>>,

    // Current state of the DB
    last_id: Arc<AtomicU32>,
    pub all_items: Arc<Mutex<HashMap<u32, GeoJson>>>,
    pub all_db_cells: Arc<Mutex<Vec<(CellIndex, RoaringBitmap)>>>,
    pub inner_shape_cell_db: Arc<Mutex<Vec<(CellIndex, RoaringBitmap)>>>,
    pub fst: Arc<Mutex<fst::Map<Vec<u8>>>>,
}

pub struct FilterStats {
    pub nb_points_matched: usize,
    pub processed_in_cold: Duration,
    pub processed_in_hot: Duration,
    pub cell_explored: Vec<(FilteringStep, CellIndex)>,
}

impl Runner {
    pub fn new(env: Env, db: Cellulite, metadata: Database<Str, Bytes>) -> Self {
        let this = Self {
            env,
            db,
            metadata,
            wake_up: Arc::new(synchronoise::SignalEvent::auto(true)),
            to_insert: Arc::default(),
            last_id: Arc::default(),
            all_items: Arc::default(),
            all_db_cells: Arc::default(),
            inner_shape_cell_db: Arc::default(),
            polygon_filter: Arc::default(),
            stats: Arc::default(),
            filter_stats: Arc::default(),
            points_matched: Arc::default(),
            fst: Arc::new(Mutex::new(Map::default())),
        };
        this.clone().run();
        this
    }

    pub fn add_shape(&self, name: String, value: GeoJson) {
        // We still need to update all_items for visualization purposes
        self.to_insert.lock().push((name, value));
        self.wake_up.signal();
    }

    fn merge_fst_and_bitmaps(
        &self,
        wtxn: &mut heed::RwTxn,
        current_fst: &Map<Vec<u8>>,
        fst_builder: BTreeMap<&str, RoaringBitmap>,
    ) {
        // Retrieve the last bitmap id
        let mut last_bitmap_id = self
            .metadata
            .rev_prefix_iter(wtxn, "bitmap_")
            .unwrap()
            .next()
            .map_or(0, |ret| {
                let (key, _) = ret.unwrap();
                key["bitmap_".len()..].parse::<usize>().unwrap()
            });

        // Create a new FST builder
        let mut builder = MapBuilder::new(Vec::new()).unwrap();

        // Create iterators for both sources
        let mut fst_stream = current_fst.into_stream();
        let mut fst_builder_iter = fst_builder.into_iter();

        // Get the first entries from both sources
        let mut fst_next = fst_stream.next();
        let mut builder_next = fst_builder_iter.next();

        // Merge the entries in lexicographic order
        while let (Some(fst_entry), Some(builder_entry)) = (fst_next, builder_next.as_ref()) {
            let (fst_key, fst_value) = fst_entry;
            let (builder_key, builder_bitmap) = builder_entry;

            // Compare the keys
            match fst_key.cmp(builder_key.as_bytes()) {
                std::cmp::Ordering::Less => {
                    // FST key comes first
                    builder.insert(fst_key, fst_value).unwrap();
                    fst_next = fst_stream.next();
                }
                std::cmp::Ordering::Greater => {
                    // Builder key comes first
                    last_bitmap_id += 1;
                    self.metadata
                        .remap_data_type::<RoaringBitmapCodec>()
                        .put(
                            wtxn,
                            &format!("bitmap_{last_bitmap_id:010}"),
                            builder_bitmap,
                        )
                        .unwrap();
                    builder.insert(builder_key, last_bitmap_id as u64).unwrap();
                    builder_next = fst_builder_iter.next();
                }
                std::cmp::Ordering::Equal => {
                    todo!("should never happen");
                }
            }
        }

        // Add remaining entries from FST
        while let Some((key, value)) = fst_next {
            builder.insert(key, value).unwrap();
            fst_next = fst_stream.next();
        }

        // Add remaining entries from builder
        while let Some((name, bitmap)) = builder_next.as_ref() {
            last_bitmap_id += 1;
            self.metadata
                .remap_data_type::<RoaringBitmapCodec>()
                .put(wtxn, &format!("bitmap_{last_bitmap_id:010}"), bitmap)
                .unwrap();
            builder.insert(name, last_bitmap_id as u64).unwrap();
            builder_next = fst_builder_iter.next();
        }

        // Build the new FST
        let fst = builder.into_inner().unwrap();
        // Store the new FST in the database
        self.metadata.put(wtxn, "fst", &fst).unwrap();
        *self.fst.lock() = Map::new(fst).unwrap();
    }

    fn run(self) {
        std::thread::spawn(move || {
            // Before entering the main loop we have to:
            // 1. Retrieve the last_id in the DB
            // 2. Retrieve all the items
            // 3. Retrieve all the DB cells
            let rtxn = self.env.read_txn().unwrap();
            let mut last_id = 0;
            let mut all_points = HashMap::new();
            for entry in self.db.items(&rtxn).unwrap() {
                let (id, geometry) = entry.unwrap();
                last_id = last_id.max(id);
                all_points.insert(id, GeoJson::from(&geometry.to_geo()));
            }
            *self.all_items.lock() = all_points;
            self.last_id.store(last_id + 1, Ordering::Relaxed);
            let mut all_db_cells = Vec::new();
            for entry in self.db.inner_db_cells(&rtxn).unwrap() {
                let (cell, bitmap) = entry.unwrap();
                all_db_cells.push((cell, bitmap));
            }
            *self.all_db_cells.lock() = all_db_cells;

            let mut inner_shape_db_cells = Vec::new();
            let mut inner_shape_db_cells_count = 0;
            for entry in self.db.inner_belly_cells(&rtxn).unwrap() {
                inner_shape_db_cells_count += 1;
                let (cell, bitmap) = entry.unwrap();
                inner_shape_db_cells.push((cell, bitmap));
            }
            println!("inner_shape_db_cells_count: {inner_shape_db_cells_count:?}");
            *self.inner_shape_cell_db.lock() = inner_shape_db_cells;

            if let Some(fst) = self.metadata.get(&rtxn, "fst").unwrap() {
                if !fst.is_empty() {
                    *self.fst.lock() = Map::new(fst.to_vec()).unwrap();
                }
            }
            drop(rtxn);

            loop {
                self.wake_up.wait();
                println!("wake_up");
                let to_insert = std::mem::take(&mut *self.to_insert.lock());
                let mut wtxn = self.env.write_txn().unwrap();
                let current_fst = self.fst.lock().clone();
                let mut fst_builder: BTreeMap<&str, RoaringBitmap> = BTreeMap::new();

                for (name, shape) in to_insert.iter() {
                    let id = self.last_id.fetch_add(1, Ordering::Relaxed);
                    match current_fst.get(name.as_bytes()) {
                        Some(bitmap_id) => {
                            let mut bitmap = self
                                .metadata
                                .remap_data_type::<RoaringBitmapCodec>()
                                .get(&wtxn, &format!("bitmap_{bitmap_id:010}"))
                                .unwrap()
                                .unwrap();
                            bitmap.insert(id);
                            self.metadata
                                .remap_data_type::<RoaringBitmapCodec>()
                                .put(&mut wtxn, &format!("bitmap_{bitmap_id:010}"), &bitmap)
                                .unwrap();
                        }
                        None => {
                            fst_builder.entry(name).or_default().insert(id);
                        }
                    }
                    self.all_items.lock().insert(id, shape.clone());
                    self.db.add(&mut wtxn, id, shape).unwrap();
                }
                self.db
                    .build(&mut wtxn, &DefaultProgress::default())
                    .unwrap();

                // We must recompute the fst, stats and db cells
                if !to_insert.is_empty() {
                    // Merge the FSTs and update the bitmaps
                    self.merge_fst_and_bitmaps(&mut wtxn, &current_fst, fst_builder);

                    let mut all_db_cells = Vec::new();
                    for entry in self.db.inner_db_cells(&wtxn).unwrap() {
                        let (cell, bitmap) = entry.unwrap();
                        all_db_cells.push((cell, bitmap));
                    }
                    *self.all_db_cells.lock() = all_db_cells;

                    let mut inner_shape_db_cells = Vec::new();
                    for entry in self.db.inner_belly_cells(&wtxn).unwrap() {
                        let (cell, bitmap) = entry.unwrap();
                        inner_shape_db_cells.push((cell, bitmap));
                    }
                    *self.inner_shape_cell_db.lock() = inner_shape_db_cells;

                    *self.stats.lock() = self.db.stats(&wtxn).unwrap();
                }

                // if a point has been added OR the shape has been modified we must recompute the filter
                let polygon = self.polygon_filter.lock().to_vec();
                if polygon.len() >= 3 {
                    let polygon = Polygon::new(LineString(polygon), Vec::new());
                    let mut steps = Vec::new();
                    let now = std::time::Instant::now();
                    let matched = self.db.in_shape(&wtxn, &polygon, &mut |_| ()).unwrap();
                    let cold = now.elapsed();
                    self.db.in_shape(&wtxn, &polygon, &mut |_| ()).unwrap();
                    self.db
                        .in_shape(&wtxn, &polygon, &mut |step| steps.push(step))
                        .unwrap();
                    let now = std::time::Instant::now();
                    self.db.in_shape(&wtxn, &polygon, &mut |_| ()).unwrap();
                    let hot = now.elapsed();

                    *self.filter_stats.lock() = Some(FilterStats {
                        nb_points_matched: matched.len() as usize,
                        processed_in_cold: cold,
                        processed_in_hot: hot,
                        cell_explored: steps,
                    });
                    let mut points_matched = Vec::new();
                    for point in matched {
                        let point = self.db.item(&wtxn, point).unwrap().unwrap();
                        points_matched.push(GeoJson::from(&point.to_geo()));
                    }
                    *self.points_matched.lock() = points_matched;
                }

                wtxn.commit().unwrap();
            }
        });
    }
}
