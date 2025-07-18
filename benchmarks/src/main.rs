use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use cellulite::{Cellulite, roaring::RoaringBitmapCodec};
use clap::{Parser, ValueEnum};
use france_query_zones::{gard, le_vigan, nimes, occitanie};
use geojson::GeoJson;
use heed::{
    EnvOpenOptions,
    types::{Bytes, Str},
};
use steppe::default::DefaultProgress;
use roaring::RoaringBitmap;
use tempfile::TempDir;

mod france_arrondissements;
mod france_cadastre_addresses;
mod france_cantons;
mod france_communes;
mod france_departements;
mod france_query_zones;
mod france_regions;
mod france_shops;
mod france_zones;
mod france_cadastre_parcelles;

#[derive(Parser, Debug)]
struct Args {
    /// Name of the dataset to use
    #[arg(short, long, value_enum, default_value_t = Dataset::Shop)]
    dataset: Dataset,

    /// Selector to use for the dataset, will do something different for each dataset
    #[arg(long, value_delimiter = ',')]
    selector: Vec<String>,

    /// Skip indexing if set. You must provide the path to a database
    #[arg(long, default_value_t = false)]
    skip_indexing: bool,

    /// Index metadata if set. Only valid if skip_indexing is false.
    /// This will create a new database for the metadata which will
    /// significantly slow down the indexing process. It should not
    /// be set when doing actual benchmarks.
    /// It also consume a lot of memory as we must stores all the strings
    /// of the whole dataset in memory.
    #[arg(long, default_value_t = false, conflicts_with = "skip_indexing")]
    index_metadata: bool,

    /// Skip query if set
    #[arg(long, default_value_t = false)]
    skip_queries: bool,

    /// Set the number of items to index, will be capped at the number of items in the dataset
    #[arg(long)]
    limit: Option<usize>,

    /// Db path, if not provided, a temporary directory will be used and freed at the end of the benchmark
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Parser, Debug, ValueEnum)]
enum Dataset {
    /// 100_000 points representing shops in France
    Shop,
    /// 22_000_000 points representing houses and buildings in France
    CadastreAddr,
    /// With the selector you can chose a department in france with its number.
    CadastreParcelle,
    Canton,
    Arrondissement,
    Commune,
    Departement,
    /// 13 regions in France
    Region,
    /// Mix of all the canton, arrondissement, commune, departement and region
    Zone,
}

fn main() {
    let args = Args::parse();

    println!("Starting...");
    let time = std::time::Instant::now();
    let input = match args.dataset {
        Dataset::Shop => &mut france_shops::parse() as &mut dyn Iterator<Item = (String, GeoJson)>,
        Dataset::CadastreAddr => {
            &mut france_cadastre_addresses::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::CadastreParcelle => {
            &mut france_cadastre_parcelles::parse(args.selector) as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Canton => {
            &mut france_cantons::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Arrondissement => {
            &mut france_arrondissements::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Commune => {
            &mut france_communes::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Departement => {
            &mut france_departements::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Region => {
            &mut france_regions::parse() as &mut dyn Iterator<Item = (String, GeoJson)>
        }
        Dataset::Zone => &mut france_zones::parse() as &mut dyn Iterator<Item = (String, GeoJson)>,
    };
    let input = input.take(args.limit.unwrap_or(usize::MAX));

    println!("Deserialized the points in {:?}", time.elapsed());

    println!("Database setup");
    let (_temp_dir, path) = match args.db {
        None => {
            let temp_dir = TempDir::new().unwrap();
            let path = temp_dir.path().to_path_buf();
            (Some(temp_dir), path)
        }
        Some(path) => {
            std::fs::create_dir_all(&path).unwrap();
            (None, path)
        }
    };
    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(200 * 1024 * 1024 * 1024)
            .max_dbs(Cellulite::nb_dbs() + 1)
            .open(path)
    }
    .unwrap();
    let mut wtxn = env.write_txn().unwrap();
    let cellulite = Cellulite::create_from_env(&env, &mut wtxn).unwrap();
    let metadata: heed::Database<Str, Bytes> =
        env.create_database(&mut wtxn, Some("metadata")).unwrap();
    wtxn.commit().unwrap();

    if !args.skip_indexing {
        let mut metadata_builder: BTreeMap<String, RoaringBitmap> = BTreeMap::new();

        println!("Inserting points");
        let time = std::time::Instant::now();
        let mut cpt = 0;
        let mut prev_cpt = 0;
        let mut wtxn = env.write_txn().unwrap();

        let mut print_timer = time;
        for (name, geometry) in input {
            cpt += 1;
            let elapsed_since_last_print = print_timer.elapsed();
            if elapsed_since_last_print > Duration::from_secs(10) {
                let elapsed = time.elapsed();
                println!(
                    "Inserted {prev_cpt} additional points in {elapsed_since_last_print:.2?}, throughput: {} points / seconds || In total: {cpt} points, started {:.2?} ago, throughput: {} points / seconds",
                    prev_cpt as f32 / elapsed_since_last_print.as_secs_f32(),
                    time.elapsed(),
                    cpt as f32 / elapsed.as_secs_f32()
                );
                print_timer = std::time::Instant::now();
                prev_cpt = cpt;
            }
            cellulite.add(&mut wtxn, cpt, &geometry).unwrap();
            if args.index_metadata {
                metadata_builder.entry(name).or_default().insert(cpt);
            }
        }
        println!("Inserted {cpt} points in {:.2?}", time.elapsed());
        println!("Building the index...");
        let progress = DefaultProgress::default();
        progress.follow_progression_on_tty();
        cellulite.build(&mut wtxn, &progress).unwrap();
        progress.finish();

        println!("Index built in {:?}", time.elapsed());
        println!("Progress: {:#?}", progress.accumulated_durations());

        // If the metadata should be indexed, we must build an fst containing
        // all the names.
        if args.index_metadata {
            let mut fst_builder = fst::MapBuilder::memory();
            for (idx, (name, bitmap)) in metadata_builder.iter().enumerate() {
                metadata
                    .remap_data_type::<RoaringBitmapCodec>()
                    .put(&mut wtxn, &format!("bitmap_{idx:010}"), &bitmap)
                    .unwrap();
                fst_builder.insert(name, idx as u64).unwrap();
            }
            let fst = fst_builder.into_inner().unwrap();
            metadata.put(&mut wtxn, &"fst", &fst).unwrap();
        }
        wtxn.commit().unwrap();

        let elapsed = time.elapsed();
        println!("Inserted {cpt} points in {elapsed:?}.");
        println!("One point every {:?}", elapsed / cpt);
    }

    if !args.skip_queries {
        let repeat = 1000;

        let rtxn = env.read_txn().unwrap();
        let le_vigan = le_vigan();
        let time = std::time::Instant::now();
        let result = cellulite.in_shape(&rtxn, &le_vigan, &mut |_| ()).unwrap();
        for _ in 0..repeat {
            let sub_res = cellulite.in_shape(&rtxn, &le_vigan, &mut |_| ()).unwrap();
            assert_eq!(result.len(), sub_res.len());
        }
        println!(
            "Found {} stores in Le Vigan in {:?}",
            result.len(),
            time.elapsed() / repeat
        );

        let time = std::time::Instant::now();

        let nimes = nimes();
        let result = cellulite.in_shape(&rtxn, &nimes, &mut |_| ()).unwrap();
        for _ in 0..repeat {
            let sub_res = cellulite.in_shape(&rtxn, &nimes, &mut |_| ()).unwrap();
            assert_eq!(result.len(), sub_res.len());
        }
        println!(
            "Found {} stores in Nîmes in {:?}",
            result.len(),
            time.elapsed() / repeat
        );

        let repeat = 100;
        let gard = gard();
        let result = cellulite.in_shape(&rtxn, &gard, &mut |_| ()).unwrap();
        for _ in 0..repeat {
            let sub_res = cellulite.in_shape(&rtxn, &gard, &mut |_| ()).unwrap();
            assert_eq!(result.len(), sub_res.len());
        }
        println!(
            "Found {} stores in Gard in {:?}",
            result.len(),
            time.elapsed() / repeat
        );

        let repeat = 100;
        let occitanie = occitanie();
        let result = cellulite.in_shape(&rtxn, &occitanie, &mut |_| ()).unwrap();
        for _ in 0..repeat {
            let sub_res = cellulite.in_shape(&rtxn, &occitanie, &mut |_| ()).unwrap();
            assert_eq!(result.len(), sub_res.len());
        }
        println!(
            "Found {} stores in Occitanie in {:?}",
            result.len(),
            time.elapsed() / repeat
        );
    }
}
