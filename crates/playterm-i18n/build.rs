mod catalog;

fn main() {
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=catalog.rs");
    let mut paths: Vec<_> = std::fs::read_dir("locales")
        .expect("read locales directory")
        .map(|entry| entry.expect("read locale path").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "toml"))
        .collect();
    paths.sort();
    let catalogs: Vec<catalog::Catalog> = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        })
        .collect();
    let generated = catalog::generate(&catalogs).unwrap_or_else(|e| panic!("invalid translation catalog: {e}"));
    let path = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("catalog.rs");
    std::fs::write(path, generated).expect("write generated catalog");
}
