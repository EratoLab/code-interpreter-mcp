use std::{fs, path::PathBuf, process};

use code_interpreter_mcp::config_reference::generate_config_reference;

fn main() {
    let check = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => {
            eprintln!("usage: gen-config-reference [--check]");
            process::exit(2);
        }
    };
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("generated/config-reference.json");
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&generate_config_reference())
            .expect("config reference is serializable")
    );

    if check {
        match fs::read_to_string(&output) {
            Ok(committed) if committed == generated => println!("Config reference is up to date."),
            _ => {
                eprintln!(
                    "Config reference is out of date. Run cargo run --bin gen-config-reference to update it."
                );
                process::exit(1);
            }
        }
    } else {
        fs::create_dir_all(output.parent().unwrap()).expect("create generated directory");
        fs::write(&output, generated).expect("write config reference");
        println!("Wrote {}", output.display());
    }
}
