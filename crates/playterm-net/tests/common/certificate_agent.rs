//! A certificate-capable agent fixture backed by a real russh signing agent.
//! The ordinary identity is deliberately listed first, and duplicates are retained.
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{Certificate, PrivateKey};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{Fixture, start_test_agent};

type Stream = Box<dyn AgentStream + Send + Unpin>;

struct ConnectionCount(Arc<AtomicUsize>);
impl ConnectionCount {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}
impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn string(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    mut backend: Stream,
    bare: Vec<u8>,
    certificates: Vec<Certificate>,
    refuse: bool,
) {
    let blobs: Vec<_> = certificates.into_iter().map(|cert| cert.to_bytes().unwrap()).collect();
    while let Ok(len) = stream.read_u32().await {
        assert!(len <= 256 * 1024);
        let mut packet = vec![0; len as usize];
        if stream.read_exact(&mut packet).await.is_err() {
            break;
        }
        let reply = if packet[0] == 11 {
            let mut reply = vec![12];
            reply.extend_from_slice(&(1 + blobs.len() as u32).to_be_bytes());
            for blob in std::iter::once(&bare).chain(blobs.iter()) {
                string(&mut reply, blob);
                string(&mut reply, b"certificate agent fixture");
            }
            reply
        } else if packet[0] == 13 && refuse {
            vec![5]
        } else {
            if packet[0] == 13 {
                let blob_len = u32::from_be_bytes(packet[1..5].try_into().unwrap()) as usize;
                let blob = &packet[5..5 + blob_len];
                assert!(blob == bare || blobs.iter().any(|cert| cert == blob));
                let rest = packet[5 + blob_len..].to_vec();
                packet = vec![13];
                string(&mut packet, &bare);
                packet.extend(rest);
            }
            if backend.write_u32(packet.len() as u32).await.is_err() {
                break;
            }
            if backend.write_all(&packet).await.is_err() {
                break;
            }
            let Ok(len) = backend.read_u32().await else { break };
            assert!(len <= 256 * 1024);
            let mut reply = vec![0; len as usize];
            if backend.read_exact(&mut reply).await.is_err() {
                break;
            }
            reply
        };
        if stream.write_u32(reply.len() as u32).await.is_err() || stream.write_all(&reply).await.is_err() {
            break;
        }
    }
}

pub(super) fn start(fx: &Fixture, key: &PrivateKey, certs: Vec<Certificate>, label: &str, refuse: bool) -> String {
    start_counted(fx, key, certs, label, refuse).0
}

pub(super) fn start_counted(
    fx: &Fixture,
    key: &PrivateKey,
    certs: Vec<Certificate>,
    label: &str,
    refuse: bool,
) -> (String, Arc<AtomicUsize>) {
    let backend = start_test_agent(fx, &[key], &format!("cert-backend-{label}"));
    let bare = key.public_key().to_bytes().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let active = count.clone();
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ServerOptions;
        let pipe = format!(r"\\.\pipe\playterm-certificate-agent-{}-{label}", std::process::id());
        fx.server_rt.block_on(async {
            let mut server = ServerOptions::new().first_pipe_instance(true).create(&pipe).unwrap();
            let pipe = pipe.clone();
            tokio::spawn(async move {
                loop {
                    if server.connect().await.is_err() {
                        break;
                    }
                    let next = ServerOptions::new().create(&pipe).unwrap();
                    let client = AgentClient::connect_named_pipe(&backend).await.unwrap();
                    let guard = ConnectionCount::new(active.clone());
                    let relay = serve(server, client.into_inner(), bare.clone(), certs.clone(), refuse);
                    tokio::spawn(async move {
                        let _guard = guard;
                        relay.await
                    });
                    server = next;
                }
            });
        });
        (pipe, count)
    }
    #[cfg(unix)]
    {
        // Keep the full socket path below the macOS sockaddr_un limit even with a long TMPDIR.
        let socket_dir = tempfile::Builder::new().prefix("ptc-").tempdir_in("/tmp").unwrap();
        let path = socket_dir.path().join("agent");
        fx.server_rt.block_on(async {
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            tokio::spawn(async move {
                let _socket_dir = socket_dir;
                while let Ok((stream, _)) = listener.accept().await {
                    let client = AgentClient::connect_uds(&backend).await.unwrap();
                    let guard = ConnectionCount::new(active.clone());
                    let relay = serve(stream, client.into_inner(), bare.clone(), certs.clone(), refuse);
                    tokio::spawn(async move {
                        let _guard = guard;
                        relay.await
                    });
                }
            });
        });
        (path.to_string_lossy().into_owned(), count)
    }
}
