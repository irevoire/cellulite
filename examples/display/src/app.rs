use cellulite::Cellulite;
use egui::{CentralPanel, RichText, Ui};
use heed::{
    types::{Bytes, Str},
    Env, EnvOpenOptions,
};
use tempfile::TempDir;
use walkers::{lon_lat, sources::OpenStreetMap, HttpTiles, Map, MapMemory};

use crate::{plugins, runner::Runner};

pub struct App {
    // Map
    tiles: HttpTiles,
    map_memory: MapMemory,

    // Database
    #[allow(dead_code)]
    env: Env,
    #[allow(dead_code)]
    db: Cellulite,
    #[allow(dead_code)]
    temp_dir: Option<tempfile::TempDir>,
    #[allow(dead_code)]
    runner: Runner,

    // Plugins
    extract_lat_lng: plugins::ExtractMousePos,
    insert_into_database: plugins::InsertIntoDatabase,
    display_db_content: plugins::DisplayDbContent,
    polygon_filtering: plugins::PolygonFiltering,
    items_inspector: plugins::ItemsInspector,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (temp_dir, path) = match std::env::args().nth(1) {
            None => {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().to_str().unwrap().to_string();
                (Some(temp_dir), path)
            }
            Some(path) => (None, path),
        };

        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(200 * 1024 * 1024 * 1024)
                .max_dbs(Cellulite::nb_dbs() + 1)
                .open(path)
        }
        .unwrap();
        let mut wtxn = env.write_txn().unwrap();
        let cellulite = Cellulite::create_from_env(&env, &mut wtxn, "cellulite").unwrap();
        let metadata: heed::Database<Str, Bytes> =
            env.create_database(&mut wtxn, Some("metadata")).unwrap();
        wtxn.commit().unwrap();
        let db = cellulite;

        let runner = Runner::new(env.clone(), db.clone(), metadata);
        let insert_into_database = plugins::InsertIntoDatabase::new(runner.clone());
        let polygon_filtering =
            plugins::PolygonFiltering::new(runner.clone(), insert_into_database.clone());

        Self {
            tiles: HttpTiles::new(OpenStreetMap, cc.egui_ctx.clone()),
            map_memory: MapMemory::default(),
            extract_lat_lng: plugins::ExtractMousePos::default(),
            insert_into_database,
            display_db_content: plugins::DisplayDbContent::new(runner.clone()),
            items_inspector: plugins::ItemsInspector::new(runner.clone()),
            polygon_filtering,
            env,
            db,
            temp_dir,
            runner,
        }
    }

    pub fn side_panel(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.debug(ui);
            self.insert_into_database.ui(ui);
            self.polygon_filtering.ui(ui);
            self.items_inspector.ui(ui);
        });
    }

    fn debug(&mut self, ui: &mut Ui) {
        ui.collapsing(RichText::new("Debug").heading(), |ui| {
            self.extract_lat_lng.ui(ui);
            if ui.button("Insert 1000 random points").clicked() {
                self.insert_into_database.insert_random_items(1000);
            }
            self.display_db_content.ui(ui);
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.add_space(16.0);

                egui::widgets::global_theme_preference_buttons(ui);
            });
        });
        egui::SidePanel::right("side_panel").show(ctx, |ui| {
            self.side_panel(ui);
        });

        CentralPanel::default().show(ctx, |ui| {
            ui.add(
                Map::new(
                    Some(&mut self.tiles),
                    &mut self.map_memory,
                    lon_lat(3.60698, 43.99155), // best city ever
                )
                .with_plugin(self.extract_lat_lng.clone())
                .with_plugin(self.insert_into_database.clone())
                .with_plugin(self.display_db_content.clone())
                .with_plugin(self.polygon_filtering.clone())
                .with_plugin(self.items_inspector.clone())
                .zoom_speed(0.5),
            );
        });
    }
}
