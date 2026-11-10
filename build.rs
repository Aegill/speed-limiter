// Встраиваем манифест Windows: программа сама запросит права администратора (нужны WinDivert).
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::ExecutionLevel;
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(
            new_manifest("SpeedLimiter").requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("не удалось встроить манифест");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
