use std::env;
use std::fs;
use std::io::Write;

fn main() {
    // Generate build timestamp
    let build_time = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let dest_path = std::path::Path::new(&out_dir).join("build_info.rs");

    let mut file = fs::File::create(&dest_path).expect("Could not create build_info.rs");
    writeln!(
        file,
        r#"/// Build timestamp (generated at compile time)
pub const BUILD_TIME: &str = "{}";"#,
        build_time
    )
    .expect("Could not write to build_info.rs");

    println!("cargo:rerun-if-changed=build.rs");
}
