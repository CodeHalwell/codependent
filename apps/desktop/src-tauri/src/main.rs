// The packaged desktop application. Behind the `app` feature (see
// Cargo.toml): `tauri::generate_context!` embeds the built frontend, so this
// binary only compiles once `npm run build` has produced `apps/desktop/dist`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    codypendent_desktop::bridge::register(tauri::Builder::default())
        .run(tauri::generate_context!())
        .expect("running the Codypendent desktop shell");
}
