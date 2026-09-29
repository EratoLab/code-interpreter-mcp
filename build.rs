use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=runtime/bootstrap.js");
    println!("cargo:rerun-if-changed=Cargo.lock");

    let metadata = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--format-version=1", "--locked"])
        .output()
        .expect("could not run cargo metadata to locate Deno extension sources");
    assert!(
        metadata.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).expect("cargo metadata returned invalid JSON");

    let mut sources = Vec::new();
    for package_name in ["deno_web", "deno_webidl"] {
        let package = metadata["packages"]
            .as_array()
            .and_then(|packages| {
                packages
                    .iter()
                    .find(|package| package["name"].as_str() == Some(package_name))
            })
            .unwrap_or_else(|| panic!("{package_name} package was not found in cargo metadata"));
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .expect("package manifest path is missing"),
        );
        let root = manifest
            .parent()
            .expect("package manifest has no parent directory");
        add_javascript_sources(package_name, root, root, &mut sources);
    }

    let project_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    sources.push((
        "ext:pyodide_host/bootstrap.js".to_owned(),
        project_root.join("runtime/bootstrap.js"),
    ));

    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("extension_sources.rs");
    let mut generated = String::from("pub(crate) static EXTENSION_SOURCES: &[(&str, &str)] = &[\n");
    for (specifier, path) in sources {
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!(
            "    ({specifier:?}, include_str!({:?})),\n",
            path.to_string_lossy()
        ));
    }
    generated.push_str("];\n");
    fs::write(output, generated).expect("could not write embedded extension source table");
}

fn add_javascript_sources(
    package: &str,
    root: &Path,
    directory: &Path,
    out: &mut Vec<(String, PathBuf)>,
) {
    for entry in fs::read_dir(directory).unwrap_or_else(|error| {
        panic!(
            "could not read Deno extension directory {}: {error}",
            directory.display()
        )
    }) {
        let entry = entry.expect("could not read Deno extension directory entry");
        let path = entry.path();
        if path.is_dir() {
            add_javascript_sources(package, root, &path, out);
        } else if path.extension().is_some_and(|extension| extension == "js") {
            let relative = path
                .strip_prefix(root)
                .expect("source is outside package root");
            let relative = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((format!("ext:{package}/{relative}"), path));
        }
    }
}
