use h3o::error::{InvalidGeometry, InvalidLatLng};

use crate::ItemId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // User errors
    #[error("Document with id `{0}` contains a {1} but only `Geometry` type is supported")]
    InvalidGeoJsonTypeFormat(ItemId, &'static str),
    #[error(
        "Document with id `{0}` contains a {1} but only `Point`, `Polygon`, `MultiPoint` and `MultiPolygon` types are supported"
    )]
    InvalidGeometryTypeFormat(ItemId, &'static str),

    // External errors, sometimes it's a user error and sometimes it's not
    #[error(transparent)]
    Heed(#[from] heed::Error),
    #[error(transparent)]
    InvalidLatLng(#[from] InvalidLatLng),
    #[error(transparent)]
    InvalidGeometry(#[from] InvalidGeometry),
    #[error(transparent)]
    InvalidGeoJson(#[from] Box<geojson::Error>),

    // Internal errors
    #[error("Internal error: unexpected document id `{0}` missing at `{1}`")]
    InternalDocIdMissing(ItemId, String),
}

#[macro_export]
macro_rules! pos {
    () => {
        format!("{}:{}:{}", file!(), line!(), column!())
    };
}
