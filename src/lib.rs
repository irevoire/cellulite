use core::f64;
use std::collections::BTreeMap;

use ::roaring::RoaringBitmap;
use ::zerometry::Zerometry;
use geojson::GeoJson;
use h3o::{CellIndex, Resolution};
use heed::{
    DatabaseStat, Env, RoTxn, RwTxn, Unspecified,
    byteorder::BE,
    types::{Bytes, U32},
};
use keys::{
    CellIndexCodec, CellKeyCodec, ItemKeyCodec, Key, KeyPrefixVariantCodec, KeyVariant,
    MetadataKey, UpdateType,
};
use metadata::{Version, VersionCodec};

mod builder;
mod error;
mod keys;
mod metadata;
pub mod reader;
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

/// The entry-point of the lib. It contains all the database required to write and read stuff.
#[derive(Clone)]
pub struct Cellulite {
    /// Links the item IDs with their Zerometry
    pub(crate) item: ItemDb,
    /// Links both the normal and belly `CellIndex` with a roaring bitmap of the items they contains.
    pub(crate) cell: CellDb,
    /// Temporary database holding the operation (addition or deletion) made on the items.
    /// It's in a temporary database so we can clear it quiclky.
    pub(crate) update: UpdateDb,
    /// Contains all the metadata related to the database.
    pub(crate) metadata: MetadataDb,

    /// After how many elements should we break a cell into sub-cells
    /// This is only available for the test and visualizing tools to use it.
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
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub total_cells: usize,
    pub total_items: usize,
    pub cells_by_resolution: BTreeMap<Resolution, usize>,
}
