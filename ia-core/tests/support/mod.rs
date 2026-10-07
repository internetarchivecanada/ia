//! Shared fixtures for the integration tests.

#![allow(dead_code)]

use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpStream};

/// A loopback proxy in front of a mock server. Its first `drop_first`
/// connections are accepted and closed at once, so the request on each of
/// them ends in a transport error; every later connection is forwarded byte
/// for byte to `target`. Returns the proxy's address.
pub async fn dropping_proxy(target: SocketAddr, drop_first: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut accepted = 0usize;
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            accepted += 1;
            if accepted <= drop_first {
                drop(inbound);
                continue;
            }
            tokio::spawn(async move {
                let Ok(mut outbound) = TcpStream::connect(target).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            });
        }
    });
    addr
}

/// The socket address a wiremock server listens on.
pub fn mock_addr(server: &wiremock::MockServer) -> SocketAddr {
    server
        .uri()
        .strip_prefix("http://")
        .unwrap()
        .parse()
        .unwrap()
}
