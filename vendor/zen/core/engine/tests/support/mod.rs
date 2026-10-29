use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, GraphContent};

#[allow(dead_code)]
pub fn test_data_root() -> String {
    let cargo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    cargo_root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-data")
        .to_string_lossy()
        .to_string()
}

pub fn load_raw_test_data(key: &str) -> BufReader<File> {
    let file = File::open(Path::new(&test_data_root()).join(key)).unwrap();
    BufReader::new(file)
}

#[allow(dead_code)]
pub fn load_test_data(key: &str) -> GraphContent {
    let content: DecisionContent = serde_json::from_reader(load_raw_test_data(key)).unwrap();
    match content {
        DecisionContent::Graph(g) => (*g).clone(),
    }
}

/// Norn (#3404): the filesystem loader was removed; every top-level `test-data/*.json`
/// graph is served from memory under its file name instead.
#[allow(dead_code)]
pub fn create_test_loader() -> MemoryLoader {
    let loader = MemoryLoader::default();
    for entry in std::fs::read_dir(test_data_root()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let key = path.file_name().unwrap().to_string_lossy().to_string();
        let content: DecisionContent =
            serde_json::from_reader(BufReader::new(File::open(&path).unwrap())).unwrap();
        loader.add(key, content);
    }
    loader
}
