//! Reads a real YouTube and/or Twitch chat for a while with an in-memory database and prints
//! counts only (no names or text). Usage: chat_probe <youtube input or -> <twitch channel or -> <seconds>

use std::collections::BTreeMap;

use yui_live_kit::{
    core::{Core, Notice},
    db::Db,
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pick = |i: usize| args.get(i).filter(|a| a.as_str() != "-").cloned().unwrap_or_default();
    let (youtube, twitch) = (pick(1), pick(2));
    let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);

    let core = Core::new(Db::open_in_memory().expect("database"));
    let mut notices = core.subscribe();
    if let Err(error) = core.start(&youtube, &twitch) {
        eprintln!("start failed: {error}");
        std::process::exit(1);
    }

    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut with_author_id = 0;
    let mut first_time = 0;
    let mut deleted = 0;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            notice = notices.recv() => match notice {
                Ok(Notice::Message(m)) => {
                    *kinds.entry(format!("{}:{}", m.message.platform.as_str(), m.message.kind)).or_default() += 1;
                    with_author_id += usize::from(m.message.author_id.is_some());
                    first_time += usize::from(m.first_time);
                }
                Ok(Notice::Deleted { keys }) => deleted += keys.len(),
                Ok(Notice::Status { platform, status, detail }) => {
                    println!("status {} {status}{}", platform.as_str(), detail.map(|d| format!(" ({d})")).unwrap_or_default());
                }
                Ok(Notice::Reset) => {}
                Err(_) => break,
            }
        }
    }
    core.stop();
    let total: usize = kinds.values().sum();
    println!("messages {total}, with author id {with_author_id}, first time {first_time}, deleted {deleted}");
    for (kind, count) in kinds {
        println!("  {kind} {count}");
    }
    println!("snapshot {}", core.snapshot(30).len());
}
