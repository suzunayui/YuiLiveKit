//! Reads the chat replays of a real channel's past streams (or of one video) for a while and
//! prints counts only (no names or text). The database is in memory unless a path is given.
//! Usage: archive_probe <youtube channel or video> <seconds> [database path]

use yui_live_kit::{
    core::{Core, Notice},
    db::Db,
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).cloned().unwrap_or_default();
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let db = match args.get(3) {
        Some(path) => Db::open(std::path::Path::new(path)),
        None => Db::open_in_memory(),
    };

    let core = Core::new(db.expect("database"));
    let mut notices = core.subscribe();
    if let Err(error) = core.start_archive(&input) {
        eprintln!("start failed: {error}");
        std::process::exit(1);
    }

    let started = std::time::Instant::now();
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut reading = false;
    let mut other = 0;
    loop {
        tokio::select! {
            _ = &mut deadline => {
                core.stop_archive();
                println!("time is up: stopping");
                deadline.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
            }
            notice = notices.recv() => match notice {
                Ok(Notice::Archive(state)) => {
                    if let Some(wait) = state.waiting_seconds {
                        println!("asked to slow down: waiting {wait}s");
                    }
                    // One line per stream, when it is done.
                    if reading && state.current.is_none() {
                        println!(
                            "{:>6.1}s  {}/{} streams looked at: {} read, {} without replay, {} new messages",
                            started.elapsed().as_secs_f64(), state.checked, state.total, state.streams, state.unavailable, state.messages
                        );
                    }
                    reading = state.current.is_some();
                    if !state.running {
                        println!("finished{}", state.error.map(|e| format!(" with an error: {e}")).unwrap_or_default());
                        break;
                    }
                }
                Ok(_) => other += 1,
                Err(_) => break,
            }
        }
    }
    println!("other notices {other} (expected 0), shown messages {} (expected 0)", core.snapshot(30).len());
}
