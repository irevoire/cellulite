// Importer for https://www.data.gouv.fr/fr/datasets/adresses-extraites-du-cadastre/#/resources

use std::{fs, io::BufReader};

use flate2::bufread::GzDecoder;
use geojson::{GeoJson, de::deserialize_feature_collection_to_vec};

pub fn parse(selector: &[String]) -> impl Iterator<Item = (String, GeoJson)> {
    fs::read_dir("assets/cadastre")
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(move |dir| {
            selector.is_empty()
                || selector
                    .iter()
                    .any(|s| dir.path().to_str().unwrap().contains(s))
        })
        .flat_map(|dir| {
            println!("Importing {}", dir.path().display());
            let file = std::fs::File::open(dir.path()).unwrap();
            let file = BufReader::new(file);
            let file = GzDecoder::new(file);
            let input: Vec<serde_json::Value> =
                deserialize_feature_collection_to_vec(file).unwrap();
            input
                .into_iter()
                .filter(|value| {
                    selector.is_empty()
                        || selector
                            .iter()
                            .any(|s| value["codeCommune"].as_str().unwrap().starts_with(s))
                })
                .map(|value| {
                    (
                        format!(
                            "{} {}, {}",
                            value["numero"].as_str().unwrap(),
                            value["nomVoie"].as_str().unwrap().to_lowercase(),
                            value["codeCommune"].as_str().unwrap()
                        ),
                        GeoJson::from_json_value(value["geometry"].clone()).unwrap(),
                    )
                })
        })
}
