//! The admin socket outlives a failed `accept` (readiness review 2026-09-27,
//! S7). Alone in its test binary on purpose: it lowers the process's file
//! descriptor limit, which would starve any test running beside it.

mod common;

use std::{
    io::{BufRead, BufReader, Write},
    time::Duration,
};

use common::{TestServer, SOON};
use icu_server::{admin, AdminRequest};

/// Out of file descriptors, `accept` on the admin socket fails with `EMFILE`
/// for as long as a connection waits. The accept loop used to end right
/// there and leave the socket dead until a restart — CLI and Checkmk locked
/// out while voice went on. Now it waits, tries again, and serves the
/// connection once descriptors are free.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_admin_socket_survives_running_out_of_file_descriptors() {
    use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};

    let server = TestServer::start().await;
    let socket = server.socket();

    let limit = getrlimit(Resource::Nofile);
    let open = std::fs::read_dir("/proc/self/fd").unwrap().count() as u64;
    let low = limit
        .current
        .map_or(open + 32, |current| current.min(open + 32));
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(low),
            maximum: limit.maximum,
        },
    )
    .unwrap();
    let mut filler = Vec::new();
    loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => filler.push(file),
            Err(e) if rustix::io::Errno::from_io_error(&e) == Some(rustix::io::Errno::MFILE) => {
                break
            }
            Err(e) => panic!("{e}"),
        }
    }
    // One descriptor for this end of the connection, none for the server's.
    filler.pop();
    let mut stream = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(filler);
    setrlimit(Resource::Nofile, limit).unwrap();

    stream.set_read_timeout(Some(SOON)).unwrap();
    stream.write_all(b"{\"cmd\":\"status\"}\n").unwrap();
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .expect("the waiting connection is served");
    let value: serde_json::Value = serde_json::from_str(&line).expect("an answer");
    assert_eq!(value["ok"], serde_json::json!(true));

    let value = admin::request(&socket, &AdminRequest::Status)
        .await
        .expect("and the socket takes new ones");
    assert_eq!(value["ok"], serde_json::json!(true));
    server.shutdown().await;
}
