use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::*;

fn forward(mut source: TcpStream, mut destination: TcpStream, counter: Arc<AtomicU64>) {
    let mut buffer = [0_u8; 16_384];
    loop {
        match source.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                counter.fetch_add(size as u64, Ordering::Relaxed);
                destination.write_all(&buffer[..size]).unwrap();
            }
        }
    }
    let _ = destination.shutdown(Shutdown::Write);
}

/// Count actual RESP traffic, including the store's repeated reads, CAS checks,
/// checkpoint encodings and archive writes, without adding production counters.
fn redis_traffic(url: &str, cycle_count: u64) -> (u64, u64) {
    let client = redis::Client::open(url).unwrap();
    let redis::ConnectionAddr::Tcp(host, port) = &client.get_connection_info().addr else {
        panic!("the traffic fixture requires a TCP Redis server");
    };
    let upstream = (host.clone(), *port);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let sent = Arc::new(AtomicU64::new(0));
    let received = Arc::new(AtomicU64::new(0));
    let sent_counter = Arc::clone(&sent);
    let received_counter = Arc::clone(&received);
    let proxy = std::thread::spawn(move || {
        let (incoming, _) = listener.accept().unwrap();
        let outgoing = TcpStream::connect(upstream).unwrap();
        incoming.set_nodelay(true).unwrap();
        outgoing.set_nodelay(true).unwrap();
        let incoming_copy = incoming.try_clone().unwrap();
        let outgoing_copy = outgoing.try_clone().unwrap();
        let request =
            std::thread::spawn(move || forward(incoming_copy, outgoing_copy, sent_counter));
        forward(outgoing, incoming, received_counter);
        request.join().unwrap();
    });
    let endpoint = url.strip_prefix("redis://").unwrap();
    let (authority, suffix) = endpoint.split_once('/').unwrap_or((endpoint, ""));
    let credentials = authority
        .rsplit_once('@')
        .map(|(credentials, _)| format!("{credentials}@"))
        .unwrap_or_default();
    let proxy_url = format!("redis://{credentials}{address}/{suffix}");
    let store = RedisCheckpointStore::new(proxy_url).unwrap();
    let key = format!("wire-history-{}", uuid::Uuid::new_v4());
    assert!(store.create_checkpoint(minimal_checkpoint(&key)).unwrap());
    let before = (
        sent.load(Ordering::Relaxed),
        received.load(Ordering::Relaxed),
    );
    for index in 1..=cycle_count {
        let mut candidate = claim(&store, &key, index);
        for _ in 0..3 {
            let revision = candidate.revision;
            assert!(store
                .progress_checkpoint(candidate, "owner", revision)
                .unwrap());
            candidate = store.load_checkpoint(&key).unwrap().unwrap();
        }
        completed_cycle(&mut candidate, index as u32);
        let revision = candidate.revision;
        assert!(store
            .commit_checkpoint(candidate, "owner", revision)
            .unwrap());
    }
    let measured = (
        sent.load(Ordering::Relaxed) - before.0,
        received.load(Ordering::Relaxed) - before.1,
    );
    store.delete_checkpoint(&key).unwrap();
    drop(store);
    proxy.join().unwrap();
    measured
}

#[test]
fn actual_redis_read_and_write_bytes_grow_linearly() {
    let Ok(url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let twenty = redis_traffic(&url, 20);
    let forty = redis_traffic(&url, 40);
    eprintln!(
        "Redis bytes for 20/40 cycles: sent {}/{}, received {}/{}",
        twenty.0, forty.0, twenty.1, forty.1
    );
    assert!(
        forty.0 * 10 < twenty.0 * 22,
        "checkpoint+archive write traffic must be linear"
    );
    assert!(
        forty.1 * 10 < twenty.1 * 22,
        "checkpoint read traffic must be linear"
    );
}
