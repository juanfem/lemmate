//! Host entry point. The real one is [`lemmate_mobile::run`], which Android's `MainActivity`
//! and iOS's `main` call into through `tauri::mobile_entry_point`; this binary exists so the
//! crate can be built, checked and run on a development machine like any other.
fn main() {
    lemmate_mobile::run();
}
