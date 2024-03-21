use std::path::Path;

fn panic_if_not_exists(path: &str) {
    if !Path::new(path).exists() {
        panic!("File {} does not exist", path);
    }
}

fn main() {
    panic_if_not_exists("metadata_ggx_brooklyn.scale");
    panic_if_not_exists("metadata_ggx_sydney.scale");

    println!("cargo:rerun-if-changed=metadata_ggx_brooklyn.scale");
    println!("cargo:rerun-if-changed=metadata_ggx_sydney.scale");
}
