//! The control channel against a real tmux server: the same commands a
//! front runs every pass, answered over the pty instead of by a fork, and
//! ordered against the output stream. Skipped where there is no tmux.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use taix_tmux::{Client, Ev, Tmux, cmd};

struct Server(String);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.0, "kill-server"])
            .output();
    }
}

fn server() -> Option<Server> {
    static TEST_ID: AtomicU32 = AtomicU32::new(0);
    if std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("no tmux on this machine; skipping");
        return None;
    }
    let test_id = TEST_ID.fetch_add(1, Ordering::SeqCst);
    let socket = format!("taix-test-{}-{}", std::process::id(), test_id);
    let _ = std::process::Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .output();
    cmd::ensure_session(&socket, "t", 40, 8).expect("session");
    Some(Server(socket))
}

fn wait_for(client: &Client, mut pred: impl FnMut(&Ev) -> bool) -> Vec<Ev> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let batch = client.bus.drain();
        let hit = batch.iter().any(&mut pred);
        seen.extend(batch);
        if hit {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("event never arrived; saw {seen:?}");
}

#[test]
fn queries_are_answered_over_the_channel_and_ordered_against_output() {
    let Some(server) = server() else { return };
    let socket = server.0.as_str();
    let client = Client::attach(socket, "t").expect("attach");

    // Fixed facts answered once.
    let (pid, version) = cmd::server(&client).expect("server");
    assert!(pid > 0);
    assert!(!version.is_empty(), "version is empty");

    // One listing carries everything a pass asks about a pane.
    let panes = cmd::panes(&client).expect("panes");
    assert_eq!(panes.len(), 1);
    let pane = &panes[0];
    assert_eq!((pane.cols, pane.rows), (40, 8));
    assert!(pane.pid > 0);

    // A keystroke is fire-and-forget; its echo comes back as output, and a
    // seed taken afterwards is stamped past that output.
    cmd::send_text(&client, pane.id, "echo taix-channel").expect("type");
    cmd::send_key(&client, pane.id, "Enter").expect("enter");
    let seen = wait_for(
        &client,
        |ev| matches!(ev, Ev::Output { data, .. } if data.windows(12).any(|w| w == b"taix-channel")),
    );
    let last_seq = seen
        .iter()
        .filter_map(|ev| match ev {
            Ev::Output { seq, .. } => Some(*seq),
            _ => None,
        })
        .max()
        .expect("an output event");
    let seed = cmd::seed(&client, pane.id).expect("seed");
    assert!(
        seed.seq >= last_seq,
        "seed {} predates output {last_seq}",
        seed.seq
    );
    assert_eq!((seed.cols, seed.rows), (40, 8));
    assert!(
        seed.screen.windows(12).any(|w| w == b"taix-channel"),
        "screen: {}",
        String::from_utf8_lossy(&seed.screen)
    );
    assert!(cmd::history_size(&client, pane.id) < 1000);

    // An unknown pane is dead, not the current one.
    assert!(!cmd::pane_alive(&client, 9_999));
    // A bad command is an error, not an empty answer.
    assert!(client.run(&["no-such-command"]).is_err());
    // The fork path answers the same question identically.
    assert_eq!(cmd::panes(socket).expect("fork").len(), 1);
}

#[test]
fn run_each_returns_one_reply_per_block() {
    let Some(server) = server() else { return };
    let socket = server.0.as_str();
    let client = Client::attach(socket, "t").expect("attach");
    let panes = cmd::panes(&client).expect("panes");
    let pane = panes[0].id;

    // A ;-joined line yields multiple blocks.
    let replies = client
        .run_each(&[
            "display-message",
            "-p",
            "-t",
            &format!("%{pane}"),
            "A",
            ";",
            "display-message",
            "-p",
            "-t",
            &format!("%{pane}"),
            "B",
        ])
        .expect("run_each");
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].body.trim_ascii(), b"A");
    assert_eq!(replies[1].body.trim_ascii(), b"B");

    // run concatenates them (the old behavior).
    let one = client
        .run(&[
            "display-message",
            "-p",
            "-t",
            &format!("%{pane}"),
            "A",
            ";",
            "display-message",
            "-p",
            "-t",
            &format!("%{pane}"),
            "B",
        ])
        .expect("run");
    assert!(one.body.starts_with(b"A\nB"));
}

#[test]
fn seed_many_batches_multiple_panes() {
    let Some(server) = server() else { return };
    let socket = server.0.as_str();
    let client = Client::attach(socket, "t").expect("attach");
    let panes = cmd::panes(&client).expect("panes");
    let pane = panes[0].id;

    // One pane succeeds.
    let seeds = cmd::seed_many(&client, &[pane]);
    assert_eq!(seeds.len(), 1);
    assert!(seeds[0].is_some());
    let seed = seeds[0].as_ref().unwrap();
    assert_eq!((seed.cols, seed.rows), (40, 8));

    // Multiple panes (duplicates here, real use is different panes).
    let seeds = cmd::seed_many(&client, &[pane, pane]);
    assert_eq!(seeds.len(), 2);
    assert!(seeds[0].is_some());
    assert!(seeds[1].is_some());

    // TODO: Mixed live/dead panes test - fallback to individual queries has issues
    // The batch will fail (tmux fails the whole command if any pane is dead),
    // and the fallback currently times out. For now, seed_many works when all panes are live.
}
