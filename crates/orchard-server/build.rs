use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../ui/dist");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let dist = manifest.join("../../ui/dist");
    let mut files = Vec::new();
    collect_files(&dist, &dist, &mut files).unwrap_or_else(|error| {
        panic!(
            "could not read {}: {error}; run `npm --prefix ui run build` first",
            dist.display()
        )
    });
    files.sort();
    assert!(
        files.iter().any(|path| path == "index.html"),
        "ui/dist/index.html is missing; run `npm --prefix ui run build` first"
    );
    for relative in &files {
        println!("cargo:rerun-if-changed={}", dist.join(relative).display());
    }

    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("build output")).join("embedded_assets.rs");
    let mut generated = fs::File::create(output).expect("create embedded asset source");
    writeln!(generated, "pub const ASSETS: &[(&str, &[u8])] = &[").unwrap();
    for relative in files {
        let include_path = format!("/../../ui/dist/{relative}");
        writeln!(
            generated,
            "    ({relative:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {include_path:?}))),"
        )
        .unwrap();
    }
    writeln!(generated, "];").unwrap();
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, output)?;
        } else if entry.file_type()?.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("asset below root")
                .to_str()
                .ok_or_else(|| io::Error::other("UI asset path is not UTF-8"))?;
            output.push(relative.replace('\\', "/"));
        }
    }
    Ok(())
}
