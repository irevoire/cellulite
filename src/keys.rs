use std::borrow::Cow;

use h3o::CellIndex;
use heed::{
    byteorder::{BE, BigEndian, ByteOrder},
    types::U64,
};

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
                ret.push(KeyVariant::Cell as u8);
                let output: u64 = (*cell).into();
                ret.extend_from_slice(&output.to_be_bytes());
                ret.extend(std::iter::repeat_n(0, missing_to_align));
            }
            Key::Belly(cell) => {
                let capacity = size_of::<KeyVariant>() + size_of_val(cell);
                let missing_to_align = ALIGNMENT - (capacity % ALIGNMENT);
                ret = Vec::with_capacity(capacity + missing_to_align);
                ret.push(KeyVariant::Belly as u8);
                let output: u64 = (*cell).into();
                ret.extend_from_slice(&output.to_be_bytes());
                ret.extend(std::iter::repeat_n(0, missing_to_align));
            }
        }
        Ok(ret.into())
    }
}

impl heed::BytesDecode<'_> for CellKeyCodec {
    type DItem = Key;

    fn bytes_decode(bytes: &'_ [u8]) -> Result<Self::DItem, heed::BoxedError> {
        let variant = bytes[0];
        let bytes = &bytes[std::mem::size_of_val(&variant)..];
        let key = match variant {
            v if v == KeyVariant::Cell as u8 => {
                let cell = BigEndian::read_u64(bytes);
                Key::Cell(cell.try_into()?)
            }
            v if v == KeyVariant::Belly as u8 => {
                let cell = BigEndian::read_u64(bytes);
                Key::Belly(cell.try_into()?)
            }
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

pub struct KeyPrefixVariantCodec;

impl<'a> heed::BytesEncode<'a> for KeyPrefixVariantCodec {
    type EItem = KeyVariant;

    fn bytes_encode(
        variant: &'a Self::EItem,
    ) -> Result<std::borrow::Cow<'a, [u8]>, heed::BoxedError> {
        Ok(vec![*variant as u8].into())
    }
}

pub struct CellIndexCodec;

impl<'a> heed::BytesEncode<'a> for CellIndexCodec {
    type EItem = CellIndex;

    fn bytes_encode(cell: &'a Self::EItem) -> Result<std::borrow::Cow<'a, [u8]>, heed::BoxedError> {
        let output: u64 = (*cell).into();
        Ok(output.to_be_bytes().to_vec().into())
    }
}

impl heed::BytesDecode<'_> for CellIndexCodec {
    type DItem = CellIndex;

    fn bytes_decode(bytes: &'_ [u8]) -> Result<Self::DItem, heed::BoxedError> {
        let cell = BigEndian::read_u64(bytes);
        Ok(cell.try_into()?)
    }
}
