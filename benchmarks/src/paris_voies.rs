// From: https://opendata.paris.fr/explore/dataset/troncon_voie/

use geojson::{GeoJson, de::deserialize_feature_collection_to_vec};
use std::io::BufReader;

use flate2::bufread::GzDecoder;

pub fn parse() -> impl Iterator<Item = (String, GeoJson)> {
    let file = std::fs::File::open("assets/troncon_voie.geojson.gz").unwrap();
    let file = BufReader::new(file);
    let file = GzDecoder::new(file);
    let input: Vec<serde_json::Value> = deserialize_feature_collection_to_vec(file).unwrap();

    input.into_iter().map(|value| {
        (
            value["objectid"].to_string(),
            GeoJson::from_json_value(value["geometry"].clone()).unwrap(),
        )
    })
}
