use std::borrow::Cow;

use h3o::CellIndex;
use heed::{
    RoTxn,
    byteorder::{BE, BigEndian, ByteOrder},
    types::U64,
};
use roaring::RoaringBitmap;

use crate::CellDb;

/// Codec used to encode and decode the item id in the item database.
///
/// The reason why we're using this codec instead of a `U32<BE>` is because the
/// keys must be aligned of 64 bits.
pub struct ItemKeyCodec;

impl heed::BytesEncode<'_> for ItemKeyCodec {
    type EItem = u32;

    fn bytes_encode(item: &'_ Self::EItem) -> Result<std::borrow::Cow<'_, [u8]>, heed::BoxedError> {
        let aligned_item = *item as u64;
        Ok(Cow::from(aligned_item.to_be_bytes().to_vec()))
    }
}

impl heed::BytesDecode<'_> for ItemKeyCodec {
    type DItem = u32;

    fn bytes_decode(bytes: &'_ [u8]) -> Result<Self::DItem, heed::BoxedError> {
        U64::<BE>::bytes_decode(bytes).map(|n| n as u32)
    }
}

/// Codec used to encode and decode the cell id.
///
/// - The cell is encoded as a u64
/// - The next byte is used to indicate if it's a belly cell or a normal cell.
/// - And finally there is some padding to align the roaring bitmap on 64 bits
pub struct CellKeyCodec;

impl<'a> heed::BytesEncode<'a> for CellKeyCodec {
    type EItem = Key;

    fn bytes_encode(key: &'a Self::EItem) -> Result<std::borrow::Cow<'a, [u8]>, heed::BoxedError> {
        const ALIGNMENT: usize = std::mem::size_of::<f64>();
        let mut ret: Vec<u8>;
        match key {
            Key::Cell(cell) => {
                let capacity = size_of::<KeyVariant>() + size_of_val(cell);
                let missing_to_align = ALIGNMENT - (capacity % ALIGNMENT);
                ret = Vec::with_capacity(capacity + missing_to_align);
                let output: u64 = (*cell).into();
                ret.extend_from_slice(&output.to_be_bytes());
                ret.push(KeyVariant::Cell as u8);
                ret.extend(std::iter::repeat_n(0, missing_to_align));
            }
            Key::Belly(cell) => {
                let capacity = size_of::<KeyVariant>() + size_of_val(cell);
                let missing_to_align = ALIGNMENT - (capacity % ALIGNMENT);
                ret = Vec::with_capacity(capacity + missing_to_align);
                let output: u64 = (*cell).into();
                ret.extend_from_slice(&output.to_be_bytes());
                ret.push(KeyVariant::Belly as u8);
                ret.extend(std::iter::repeat_n(0, missing_to_align));
            }
        }
        Ok(ret.into())
    }
}

impl heed::BytesDecode<'_> for CellKeyCodec {
    type DItem = Key;

    fn bytes_decode(bytes: &'_ [u8]) -> Result<Self::DItem, heed::BoxedError> {
        let cell = BigEndian::read_u64(bytes);
        let bytes = &bytes[std::mem::size_of_val(&cell)..];
        let variant = bytes[0];
        let key = match variant {
            v if v == KeyVariant::Cell as u8 => Key::Cell(cell.try_into()?),
            v if v == KeyVariant::Belly as u8 => Key::Belly(cell.try_into()?),
            _ => unreachable!(),
        };
        // In any case we can skip the padding

        Ok(key)
    }
}

pub enum Key {
    Cell(CellIndex),
    Belly(CellIndex),
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyVariant {
    Cell = 1,
    Belly = 2,
}

pub(crate) fn retrieve_cell_and_belly(
    rtxn: &RoTxn,
    db: &CellDb,
    cell_index: CellIndex,
) -> Result<(Option<RoaringBitmap>, Option<RoaringBitmap>), heed::Error> {
    let mut cell = None;
    let mut belly = None;
    let iter = db
        .remap_key_type::<U64<BE>>()
        .prefix_iter(rtxn, &(cell_index.into()))?
        .remap_key_type::<CellKeyCodec>();
    for ret in iter {
        let (key, value) = ret?;
        match key {
            Key::Cell(_) => cell = Some(value),
            Key::Belly(_) => belly = Some(value),
        }
    }

    Ok((cell, belly))
}

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
