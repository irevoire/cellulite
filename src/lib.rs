use core::f64;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::atomic::Ordering,
};

use ::roaring::RoaringBitmap;
use ::zerometry::{InputRelation, RelationBetweenShapes, Zerometry};
use geo::{Densify, Destination, Haversine, MultiPolygon, Point};
use geo_types::Polygon;
use geojson::GeoJson;
use h3o::{
    CellIndex, LatLng, Resolution,
    geom::{ContainmentMode, PlotterBuilder, TilerBuilder},
};
use heed::{
    DatabaseStat, Env, RoTxn, RwTxn, Unspecified,
    byteorder::BE,
    types::{Bytes, U32},
};
use intmap::IntMap;
use keys::{CellIndexCodec, CellKeyCodec, ItemKeyCodec, Key, KeyPrefixVariantCodec, KeyVariant};
use metadata::{Version, VersionCodec};
use rayon::iter::{ParallelBridge, ParallelIterator};
use steppe::Progress;
use thread_local::ThreadLocal;

mod error;
mod keys;
mod metadata;
pub mod roaring;
pub mod zerometry;

#[cfg(test)]
mod test;

pub use crate::error::Error;
use crate::{roaring::RoaringBitmapCodec, zerometry::ZerometryCodec};

pub type ItemDb = heed::Database<ItemKeyCodec, ZerometryCodec>;
pub type CellDb = heed::Database<CellKeyCodec, CellIndexCodec>;
pub type UpdateDb = heed::Database<U32<BE>, UpdateType>;
pub type MetadataDb = heed::Database<MetadataKey, Unspecified>;
pub type ItemId = u32;

steppe::make_enum_progress! {
    pub enum BuildSteps {
        RetrieveUpdatedItems,
        ClearUpdatedItems,
        RetrieveAndClearDeletedItems,
        RemoveDeletedItemsFromDatabase,
        InsertItemsAtLevelZero,
        InsertItemsRecursively,
        UpdateTheMetadata,
    }
}
steppe::make_atomic_progress!(Item alias AtomicItemStep => "item");
steppe::make_atomic_progress!(Cell alias AtomicCellStep => "cell");

type Result<O, E = Error> = std::result::Result<O, E>;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateType {
    Insert = 0,
    Delete = 1,
}

impl<'a> heed::BytesEncode<'a> for UpdateType {
    type EItem = Self;

    fn bytes_encode(item: &Self::EItem) -> Result<Cow<'a, [u8]>, heed::BoxedError> {
        Ok(Cow::Owned(vec![*item as u8]))
    }
}

impl<'a> heed::BytesDecode<'a> for UpdateType {
    type DItem = Self;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, heed::BoxedError> {
        match bytes {
            [b] if *b == UpdateType::Insert as u8 => Ok(UpdateType::Insert),
            [b] if *b == UpdateType::Delete as u8 => Ok(UpdateType::Delete),
            _ => panic!("Invalid update type {bytes:?}"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MetadataKey {
    Version = 0,
}

impl<'a> heed::BytesEncode<'a> for MetadataKey {
    type EItem = Self;

    fn bytes_encode(item: &Self::EItem) -> Result<Cow<'a, [u8]>, heed::BoxedError> {
        Ok(Cow::Owned(vec![*item as u8]))
    }
}

impl<'a> heed::BytesDecode<'a> for MetadataKey {
    type DItem = Self;

    fn bytes_decode(bytes: &'a [u8]) -> Result<Self::DItem, heed::BoxedError> {
        match bytes {
            [b] if *b == MetadataKey::Version as u8 => Ok(MetadataKey::Version),
            _ => panic!("Invalid metadata key {bytes:?}"),
        }
    }
}

#[derive(Clone)]
pub struct Cellulite {
    pub(crate) item: ItemDb,
    pub(crate) cell: CellDb,
    pub(crate) update: UpdateDb,
    pub(crate) metadata: MetadataDb,
    /// After how many elements should we break a cell into sub-cells
    pub threshold: u64,
}

impl Cellulite {
    pub const fn nb_dbs() -> u32 {
        4
    }

    pub fn item_db_stats(&self, rtxn: &RoTxn) -> heed::Result<DatabaseStat> {
        self.item.stat(rtxn)
    }

    pub fn cell_db_stats(&self, rtxn: &RoTxn) -> heed::Result<DatabaseStat> {
        self.cell.stat(rtxn)
    }

    pub fn update_db_stats(&self, rtxn: &RoTxn) -> heed::Result<DatabaseStat> {
        self.update.stat(rtxn)
    }

    pub fn metadata_db_stats(&self, rtxn: &RoTxn) -> heed::Result<DatabaseStat> {
        self.metadata.stat(rtxn)
    }

    pub const fn default_threshold() -> u64 {
        200
    }

    pub fn create_from_env<Tls>(env: &Env<Tls>, wtxn: &mut RwTxn) -> Result<Self> {
        let item = env.create_database(wtxn, Some("cellulite-item"))?;
        let cell = env.create_database(wtxn, Some("cellulite-cell"))?;
        let update = env.create_database(wtxn, Some("cellulite-update"))?;
        let metadata = env.create_database(wtxn, Some("cellulite-metadata"))?;
        Ok(Self {
            item,
            cell,
            update,
            metadata,
            threshold: Self::default_threshold(),
        })
    }

    pub fn from_dbs(item: ItemDb, cell: CellDb, update: UpdateDb, metadata: MetadataDb) -> Self {
        Self {
            item,
            cell,
            update,
            metadata,
            threshold: Self::default_threshold(),
        }
    }

    pub fn clear(&self, wtxn: &mut RwTxn) -> Result<()> {
        self.item.clear(wtxn)?;
        self.cell.clear(wtxn)?;
        self.update.clear(wtxn)?;
        self.metadata.clear(wtxn)?;
        Ok(())
    }

    fn item_db(&self) -> heed::Database<ItemKeyCodec, ZerometryCodec> {
        self.item
    }

    fn cell_db(&self) -> heed::Database<CellKeyCodec, RoaringBitmapCodec> {
        self.cell.remap_data_type()
    }

    fn belly_cell_db(&self) -> heed::Database<CellKeyCodec, RoaringBitmapCodec> {
        self.cell.remap_data_type()
    }

    pub fn get_version(&self, rtxn: &RoTxn) -> heed::Result<Version> {
        self.metadata
            .remap_data_type::<VersionCodec>()
            .get(rtxn, &MetadataKey::Version)
            // If there is no version in the database it means we never wrote anything to the database
            // and we're at the last/current version
            .map(|opt| opt.unwrap_or_default())
    }

    fn set_version(&self, wtxn: &mut RwTxn, version: &Version) -> heed::Result<()> {
        self.metadata
            .remap_data_type::<VersionCodec>()
            .put(wtxn, &MetadataKey::Version, version)
    }

    /// Return all the cells used internally in the database
    pub fn inner_db_cells<'a>(
        &self,
        rtxn: &'a RoTxn,
    ) -> Result<impl Iterator<Item = Result<(CellIndex, RoaringBitmap), heed::Error>> + 'a> {
        Ok(self
            .cell
            .remap_key_type::<KeyPrefixVariantCodec>()
            .prefix_iter(rtxn, &KeyVariant::Cell)?
            .remap_types::<CellKeyCodec, RoaringBitmapCodec>()
            .map(|res| {
                res.map(|(key, bitmap)| {
                    let Key::Cell(cell) = key else { unreachable!() };
                    (cell, bitmap)
                })
            }))
    }

    /// Return all the cells used internally in the database
    pub fn inner_belly_cells<'a>(
        &self,
        rtxn: &'a RoTxn,
    ) -> Result<impl Iterator<Item = Result<(CellIndex, RoaringBitmap), heed::Error>> + 'a> {
        Ok(self
            .cell
            .remap_key_type::<KeyPrefixVariantCodec>()
            .prefix_iter(rtxn, &KeyVariant::Belly)?
            .remap_types::<CellKeyCodec, RoaringBitmapCodec>()
            .map(|res| {
                res.map(|(key, bitmap)| {
                    let Key::Belly(cell) = key else {
                        unreachable!()
                    };
                    (cell, bitmap)
                })
            }))
    }

    /// Return the coordinates of the items rounded down to 50cm if this id exists in the DB. Returns `None` otherwise.
    pub fn item<'a>(&self, rtxn: &'a RoTxn, item: ItemId) -> Result<Option<Zerometry<'a>>> {
        self.item_db().get(rtxn, &item).map_err(Error::from)
    }

    /// Iterate over all the items in the database
    pub fn items<'a>(
        &self,
        rtxn: &'a RoTxn,
    ) -> Result<impl Iterator<Item = Result<(ItemId, Zerometry<'a>), heed::Error>> + 'a> {
        Ok(self.item.iter(rtxn)?)
    }

    fn retrieve_frozen_items<'a>(&self, rtxn: &'a RoTxn) -> Result<FrozenItems<'a>> {
        let mut items = IntMap::with_capacity(self.item.len(rtxn)? as usize);
        for ret in self.item.iter(rtxn)? {
            let (k, v) = ret?;
            items.insert(k, v);
        }
        Ok(FrozenItems { items })
    }

    pub fn add(&self, wtxn: &mut RwTxn, item: ItemId, geo: &GeoJson) -> Result<()> {
        let geom = geo_types::Geometry::<f64>::try_from(geo.clone()).unwrap();
        self.item_db().put(wtxn, &item, &geom)?;
        self.update.put(wtxn, &item, &UpdateType::Insert)?;
        Ok(())
    }

    /// The `geo` must be a valid zerometry otherwise the database will be corrupted.
    pub fn add_raw_zerometry(&self, wtxn: &mut RwTxn, item: ItemId, geo: &[u8]) -> Result<()> {
        self.item_db()
            .remap_data_type::<Bytes>()
            .put(wtxn, &item, geo)?;
        self.update.put(wtxn, &item, &UpdateType::Insert)?;
        Ok(())
    }

    pub fn delete(&self, wtxn: &mut RwTxn, item: ItemId) -> Result<()> {
        self.update.put(wtxn, &item, &UpdateType::Delete)?;
        Ok(())
    }

    fn retrieve_and_clear_updated_items(
        &self,
        wtxn: &mut RwTxn,
        progress: &impl Progress,
    ) -> Result<(RoaringBitmap, RoaringBitmap)> {
        progress.update(BuildSteps::RetrieveUpdatedItems);
        let (atomic, step) = AtomicItemStep::new(self.update.len(wtxn)?);
        progress.update(step);

        let mut inserted = RoaringBitmap::new();
        let mut deleted = RoaringBitmap::new();

        for ret in self.update.iter(wtxn)? {
            match ret? {
                (item, UpdateType::Insert) => inserted.try_push(item).unwrap(),
                (item, UpdateType::Delete) => deleted.try_push(item).unwrap(),
            }
            atomic.fetch_add(1, Ordering::Relaxed);
        }
        progress.update(BuildSteps::ClearUpdatedItems);
        self.update.clear(wtxn)?;

        Ok((inserted, deleted))
    }

    /// Indexing is in 4 steps:
    /// 1. We retrieve all the items that have been updated since the last indexing
    /// 2. We remove the deleted items from the database and remove the empty cells at the same time
    ///    TODO: If a cell becomes too small we cannot delete it so the database won't shrink properly but won't be corrupted either
    /// 3. We insert the new items in the database **only at the level 0**
    /// 4. We take each level-zero cell one by one and if it contains new items we insert them in the database in batch at the next level
    ///    TODO: Could be parallelized fairly easily I think
    pub fn build(&self, wtxn: &mut RwTxn, progress: &impl Progress) -> Result<()> {
        let db_version = self.get_version(wtxn)?;
        if db_version != Version::default() {
            return Err(Error::VersionMismatchOnBuild(db_version));
        }

        // 1.
        let (inserted_items, removed_items) =
            self.retrieve_and_clear_updated_items(wtxn, progress)?;

        // 2.
        self.remove_deleted_items(wtxn, progress, removed_items)?;

        // 3.0
        let frozen_items = self.retrieve_frozen_items(wtxn)?;
        // currently heed doesn't know that writing in a database doesn't invalidate the pointers in another
        let frozen_items: FrozenItems<'static> = unsafe { std::mem::transmute(frozen_items) };

        // 3.
        self.insert_items_at_level_zero(wtxn, progress, &inserted_items, &frozen_items)?;

        // 4. We have to iterate over all the level-zero cells and insert the new items that are in them in the database at the next level if we need to
        //    TODO: Could be parallelized
        progress.update(BuildSteps::InsertItemsRecursively); // we cannot detail more here
        for cell in CellIndex::base_cells() {
            let bitmap = self
                .cell_db()
                .get(wtxn, &Key::Cell(cell))?
                .unwrap_or_default();
            // Awesome, we don't care about what's in the cell, wether it have multiple levels or not
            if bitmap.len() < self.threshold || bitmap.intersection_len(&inserted_items) == 0 {
                continue;
            }
            self.insert_chunk_of_items_recursively(wtxn, bitmap, cell, &frozen_items)?;
        }

        progress.update(BuildSteps::UpdateTheMetadata);
        self.set_version(wtxn, &Version::default())?;

        Ok(())
    }

    /// 1. We remove all the items by id of the items database
    /// 2. We do a scan of the whole cell database and remove the items from the bitmaps
    /// 3. We do a scan of the whole belly_cell_db and remove the items from the bitmaps
    ///
    /// TODO: We could optimize 2 and 3 by diving into the cells and stopping early when one is empty
    fn remove_deleted_items(
        &self,
        wtxn: &mut RwTxn,
        progress: &impl Progress,
        items: RoaringBitmap,
    ) -> Result<()> {
        progress.update(BuildSteps::RemoveDeletedItemsFromDatabase);
        steppe::make_enum_progress! {
            pub enum RemoveDeletedItemsSteps {
                RemoveDeletedItemsFromItemsDatabase,
                RemoveDeletedItemsFromCellsDatabase,
                RemoveDeletedItemsFromBellyCellsDatabase,
            }
        }

        progress.update(RemoveDeletedItemsSteps::RemoveDeletedItemsFromItemsDatabase);
        let (atomic, step) = AtomicItemStep::new(items.len());
        progress.update(step.clone());
        for item in items.iter() {
            self.item_db().delete(wtxn, &item)?;
            atomic.fetch_add(1, Ordering::Relaxed);
        }

        progress.update(RemoveDeletedItemsSteps::RemoveDeletedItemsFromCellsDatabase);
        atomic.store(0, Ordering::Relaxed);
        progress.update(step.clone());
        let mut iter = self
            .cell_db()
            .remap_key_type::<KeyPrefixVariantCodec>()
            .prefix_iter_mut(wtxn, &KeyVariant::Cell)?
            .remap_key_type::<CellKeyCodec>();
        while let Some(ret) = iter.next() {
            let (key, mut bitmap) = ret?;
            let len = bitmap.len();
            bitmap -= &items;
            let removed = len - bitmap.len();
            atomic.fetch_add(removed, Ordering::Relaxed);

            // safe because everything is owned
            unsafe {
                if bitmap.is_empty() {
                    iter.del_current()?;
                } else {
                    iter.put_current(&key, &bitmap)?;
                }
            }
        }
        drop(iter);

        progress.update(RemoveDeletedItemsSteps::RemoveDeletedItemsFromBellyCellsDatabase);
        atomic.store(0, Ordering::Relaxed);
        progress.update(step);
        let mut iter = self
            .belly_cell_db()
            .remap_key_type::<KeyPrefixVariantCodec>()
            .prefix_iter_mut(wtxn, &KeyVariant::Belly)?
            .remap_key_type::<CellKeyCodec>();
        while let Some(ret) = iter.next() {
            let (key, mut bitmap) = ret?;
            let len = bitmap.len();
            bitmap -= &items;
            let removed = len - bitmap.len();
            atomic.fetch_add(removed, Ordering::Relaxed);

            // safe because everything is owned
            unsafe {
                if bitmap.is_empty() {
                    iter.del_current()?;
                } else {
                    iter.put_current(&key, &bitmap)?;
                }
            }
        }
        Ok(())
    }

    fn insert_items_at_level_zero(
        &self,
        wtxn: &mut RwTxn,
        progress: &impl Progress,
        items: &RoaringBitmap,
        frozen_items: &FrozenItems<'static>,
    ) -> Result<()> {
        progress.update(BuildSteps::InsertItemsAtLevelZero);
        steppe::make_enum_progress! {
            pub enum InsertItemsAtLevelZeroSteps {
                SplitItemsToCells,
                MergeCellsMap,
                WriteCellsToDatabase,
            }
        }
        progress.update(InsertItemsAtLevelZeroSteps::SplitItemsToCells);
        let (atomic, step) = AtomicItemStep::new(items.len());
        progress.update(step);

        let tls: ThreadLocal<RefCell<(HashMap<_, _>, HashMap<_, _>)>> = ThreadLocal::new();

        items
            .iter()
            .par_bridge()
            .try_for_each(|item| -> Result<_> {
                let (to_insert, belly_cells) = &mut *tls.get_or_default().borrow_mut();

                let shape = frozen_items
                    .get(item)
                    .ok_or_else(|| Error::InternalDocIdMissing(item, pos!()))?;
                let (cells, belly) = Self::explode_level_zero_geo(shape)?;
                for cell in cells {
                    to_insert
                        .entry(cell)
                        .or_insert_with(RoaringBitmap::new)
                        .insert(item);
                }
                for cell in belly {
                    belly_cells
                        .entry(cell)
                        .or_insert_with(RoaringBitmap::new)
                        .insert(item);
                }
                atomic.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })?;
        progress.update(InsertItemsAtLevelZeroSteps::MergeCellsMap);
        let (to_insert, belly) = tls
            .into_iter()
            .par_bridge()
            .map(|refcell| refcell.into_inner())
            .reduce(
                Default::default,
                |(mut l_insert, mut l_belly), (r_insert, r_belly)| {
                    for (k, v) in r_insert {
                        *l_insert.entry(k).or_default() |= v;
                    }
                    for (k, v) in r_belly {
                        *l_belly.entry(k).or_default() |= v;
                    }
                    (l_insert, l_belly)
                },
            );
        progress.update(InsertItemsAtLevelZeroSteps::WriteCellsToDatabase);
        let (atomic, step) = AtomicCellStep::new(to_insert.len() as u64 + belly.len() as u64);
        progress.update(step);
        for (cell, items) in to_insert {
            let mut bitmap = self
                .cell_db()
                .get(wtxn, &Key::Cell(cell))?
                .unwrap_or_default();
            bitmap |= items;
            self.cell_db().put(wtxn, &Key::Cell(cell), &bitmap)?;
            atomic.fetch_add(1, Ordering::Relaxed);
        }
        for (cell, items) in belly {
            let mut bitmap = self
                .cell_db()
                .get(wtxn, &Key::Belly(cell))?
                .unwrap_or_default();
            bitmap |= items;
            self.cell_db().put(wtxn, &Key::Belly(cell), &bitmap)?;
            atomic.fetch_add(1, Ordering::Relaxed);
        }

        Ok(())
    }

    fn explode_level_zero_geo(shape: Zerometry) -> Result<(Vec<CellIndex>, Vec<CellIndex>), Error> {
        match shape {
            Zerometry::Point(point) => {
                let cell = LatLng::new(point.lat(), point.lng())
                    .unwrap()
                    .to_cell(Resolution::Zero);
                Ok((vec![cell], vec![]))
            }
            Zerometry::MultiPoints(multi_point) => {
                let to_insert = multi_point
                    .coords()
                    .iter()
                    .map(|point| {
                        LatLng::new(point.lat(), point.lng())
                            .unwrap()
                            .to_cell(Resolution::Zero)
                    })
                    .collect();
                Ok((to_insert, vec![]))
            }
            Zerometry::Polygon(polygon) => {
                let mut tiler = TilerBuilder::new(Resolution::Zero)
                    .containment_mode(ContainmentMode::Covers)
                    .build();
                tiler.add(polygon.to_geo())?;

                let mut to_insert = Vec::new();
                let mut belly_cells = Vec::new();
                for cell in tiler.into_coverage() {
                    // If the cell is entirely contained in the polygon, insert directly to belly_cell_db
                    let cell_polygon = MultiPolygon::from(cell);
                    if polygon.strict_contains(&cell_polygon) {
                        belly_cells.push(cell);
                    } else {
                        // Otherwise use insert_shape_in_cell for partial overlaps
                        to_insert.push(cell);
                    }
                }
                Ok((to_insert, belly_cells))
            }
            Zerometry::MultiPolygon(multi_polygon) => {
                let mut to_insert = Vec::new();
                let mut belly_cells = Vec::new();
                for polygon in multi_polygon.polygons() {
                    let (cells, belly) = Self::explode_level_zero_geo(polygon.into())?;
                    to_insert.extend(cells);
                    belly_cells.extend(belly);
                }
                Ok((to_insert, belly_cells))
            }
            Zerometry::Line(line) => {
                let mut plotter = PlotterBuilder::new(Resolution::Zero).build();
                plotter.add_batch(line.to_geo().lines()).unwrap();

                let to_insert = plotter.plot().collect::<Result<Vec<_>, _>>().unwrap();

                Ok((to_insert, vec![]))
            }
            Zerometry::MultiLines(multi_lines) => {
                let mut plotter = PlotterBuilder::new(Resolution::Zero).build();
                for line in multi_lines.lines() {
                    plotter.add_batch(line.to_geo().lines()).unwrap();
                }

                let to_insert = plotter.plot().collect::<Result<Vec<_>, _>>().unwrap();

                Ok((to_insert, vec![]))
            }
            Zerometry::Collection(collection) => {
                let (mut insert, mut belly) =
                    Self::explode_level_zero_geo(Zerometry::MultiPoints(collection.points()))?;
                let (mut i, mut b) =
                    Self::explode_level_zero_geo(Zerometry::MultiLines(collection.lines()))?;
                insert.append(&mut i);
                belly.append(&mut b);
                let (mut i, mut b) =
                    Self::explode_level_zero_geo(Zerometry::MultiPolygon(collection.polygons()))?;
                insert.append(&mut i);
                belly.append(&mut b);

                insert.sort_unstable();
                belly.sort_unstable();

                insert.dedup();
                belly.dedup();

                Ok((insert, belly))
            }
        }
    }

    /// To insert a bunch of items in a cell we have to:
    /// 1. Get all the possible children cells
    /// 2. See which items fits in which cells by doing an intersection between the shape and the child cell
    /// 3. Insert the cells in the database
    /// 4. For all the cells that are too large:
    ///  - If it was already too large, repeat the process with the next resolution
    ///  - If it **just became** too large. Retrieve all the items it contains and add them to the list of items to handle
    ///    Call ourselves recursively on the next resolution
    fn insert_chunk_of_items_recursively(
        &self,
        wtxn: &mut RwTxn,
        items: RoaringBitmap,
        cell: CellIndex,
        frozen_items: &FrozenItems<'static>,
    ) -> Result<()> {
        // 1. If we cannot increase the resolution, we are done
        let Some(children_cells) = get_children_cells(cell)? else {
            return Ok(());
        };
        // 2.
        let mut to_insert = HashMap::with_capacity(children_cells.len());
        let mut to_insert_in_belly = HashMap::new();

        for &cell in children_cells.iter() {
            let cell_shape = get_cell_shape(cell);
            for item in items.iter() {
                let shape = frozen_items
                    .get(item)
                    .ok_or_else(|| Error::InternalDocIdMissing(item, pos!()))?;
                let relation = shape.relation(
                    &cell_shape,
                    InputRelation {
                        // we don't need to know if we're being strictly contained or not
                        strict_contained: false,
                        ..InputRelation::all()
                    },
                );
                if relation.strict_contains.unwrap_or_default() {
                    let entry = to_insert_in_belly
                        .entry(cell)
                        .or_insert_with(RoaringBitmap::new);
                    entry.insert(item);
                } else if relation.any_relation() {
                    let entry = to_insert.entry(cell).or_insert_with(RoaringBitmap::new);
                    entry.insert(item);
                }
            }
        }

        // 3.
        for (cell, items) in to_insert_in_belly {
            let mut bitmap = self
                .belly_cell_db()
                .get(wtxn, &Key::Belly(cell))?
                .unwrap_or_default();
            bitmap |= items;
            self.belly_cell_db().put(wtxn, &Key::Belly(cell), &bitmap)?;
        }

        for (cell, mut items_to_insert) in to_insert {
            let original_bitmap = self
                .cell_db()
                .get(wtxn, &Key::Cell(cell))?
                .unwrap_or_default();
            let new_bitmap = &original_bitmap | &items_to_insert;
            self.cell_db().put(wtxn, &Key::Cell(cell), &new_bitmap)?;
            if original_bitmap.len() >= self.threshold {
                // if we were already too large we can immediately jump to the next resolution
                self.insert_chunk_of_items_recursively(wtxn, items_to_insert, cell, frozen_items)?;
            } else if new_bitmap.len() >= self.threshold {
                let cell_shape = get_cell_shape(cell);
                let mut belly_items = RoaringBitmap::new();

                // If we just became too large, we have to retrieve the items that were already in the database insert them at the next resolution
                for item_id in original_bitmap.iter() {
                    let shape = frozen_items
                        .get(item_id)
                        .ok_or_else(|| Error::InternalDocIdMissing(item_id, pos!()))?;

                    let relation = shape.relation(
                        &cell_shape,
                        InputRelation {
                            // we don't need to know if we're being strictly contained or not
                            strict_contained: false,
                            ..InputRelation::all()
                        },
                    );
                    if relation.strict_contains.unwrap_or_default() {
                        belly_items.insert(item_id);
                    } else if relation.any_relation() {
                        items_to_insert.insert(item_id);
                    }
                }

                let mut belly_cells = self
                    .belly_cell_db()
                    .get(wtxn, &Key::Belly(cell))?
                    .unwrap_or_default();
                belly_cells |= belly_items;
                self.belly_cell_db()
                    .put(wtxn, &Key::Belly(cell), &belly_cells)?;

                self.insert_chunk_of_items_recursively(wtxn, items_to_insert, cell, frozen_items)?;
            }
            // If we are not too large, we have nothing else to do yaay
        }
        Ok(())
    }

    pub fn stats(&self, rtxn: &RoTxn) -> Result<Stats> {
        let total_items = self.items(rtxn)?.count();
        let mut total_cells = 0;
        let mut cells_by_resolution = BTreeMap::new();

        for entry in self.inner_db_cells(rtxn)? {
            let (cell, _) = entry?;
            total_cells += 1;
            *cells_by_resolution.entry(cell.resolution()).or_default() += 1;
        }

        Ok(Stats {
            total_cells,
            total_items,
            cells_by_resolution,
        })
    }

    // The strategy to retrieve the points in a shape is to:
    // 1. Retrieve all the cell@res0 that contains the shape
    // 2. Iterate over these cells
    //  2.1.If a cell fit entirely *inside* the shape, add all its items to the result
    //  2.2 Otherwise:
    //   - If the cell is a leaf => iterate over all of its point and add the one that fits in the shape to the result
    //   - Otherwise, increase the precision and iterate on the range of cells => repeat step 2
    pub fn in_shape(
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
        self.in_circle_with_params(rtxn, center, radius, resolution, &Haversine, &mut |_| ())
    }

    /// Retrieve all items intersecting a circle with a given center and radius.
    /// This is approximate. It may miss items that are in the circle, but it will never return items that are not in the circle.
    /// The resolution parameter controls the number of points used to approximate the circle.
    pub fn in_circle_with_params<Measure: Destination<f64>>(
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

        self.in_shape(rtxn, &polygon, inspector)
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

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub total_cells: usize,
    pub total_items: usize,
    pub cells_by_resolution: BTreeMap<Resolution, usize>,
}

fn get_cell_shape(cell: CellIndex) -> MultiPolygon {
    cell.into()
}

/// Return None if we cannot increase the resolution
/// Otherwise, return the children cells in a very non-efficient way
/// Note: We cannot use the `get_children_cells` function because it doesn't return the full coverage of our cells and leaves holes
fn get_children_cells(cell: CellIndex) -> Result<Option<Vec<CellIndex>>, Error> {
    let Some(next_res) = cell.resolution().succ() else {
        return Ok(None);
    };
    // safe to unwrap because we just increased the resolution
    let center_child = cell.center_child(next_res).unwrap();
    Ok(Some(center_child.grid_disk(2)))
}

struct FrozenItems<'a> {
    items: IntMap<ItemId, Zerometry<'a>>,
}

impl<'a> FrozenItems<'a> {
    pub fn get(&self, item: u32) -> Option<Zerometry<'a>> {
        self.items.get(item).copied()
    }
}
