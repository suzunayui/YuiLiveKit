//! Imports an Electron-version comments.db into a throwaway database and prints counts only.
//! The source is opened read-only. Usage: import_probe <path to comments.db>

use yui_live_kit::{db::Db, model::Platform};

fn main() {
    let source = std::env::args().nth(1).expect("path to comments.db");
    let target = std::env::temp_dir().join(format!("yuilivekit-import-probe-{}.db", std::process::id()));
    let mut db = Db::open(&target).expect("database");
    let started = std::time::Instant::now();
    match db.import_legacy(std::path::Path::new(&source)) {
        Ok(summary) => println!(
            "imported {} messages, {} streams, {} viewers in {:.1}s",
            summary.messages,
            summary.streams,
            summary.viewers,
            started.elapsed().as_secs_f64()
        ),
        Err(error) => println!("import failed: {error}"),
    }
    let stream = db.ensure_stream(Platform::Youtube, "import-probe-new-stream", i64::MAX / 2).unwrap();
    println!("second import refused: {}", db.import_legacy(std::path::Path::new(&source)).is_err());
    drop(db);
    let _ = stream;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", target.display()));
    }
}
