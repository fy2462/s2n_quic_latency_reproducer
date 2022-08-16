#![cfg(feature = "rustls")]

use crate::{anyhow::anyhow, bytes_codec::BytesCodec, ResultType};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::{
    sink::SinkExt,
    stream::{SplitSink, SplitStream, StreamExt},
};
use protobuf::Message;
use rustls;
use s2n_quic::{
    client::Connect,
    provider::{io::tokio::Builder as IoBuilder, limits::Limits},
    stream::BidirectionalStream,
    Client, Server,
};
use std::{
    fs::File,
    io::{BufReader, Error},
    net::SocketAddr,
    ops::{Deref, DerefMut},
    path::Path,
    str,
    sync::Arc,
    time::Duration,
};
use tokio::{
    self,
    sync::{mpsc, RwLock},
};
use tokio_util::codec::Framed;

pub const SERVER_NAME: &str = "localhost";
const ACK_LATENCY: u64 = 0;
const MAX_HANDSHAKE_DURATION: u64 = 3;
const MAX_ACK_RANGES: u8 = 100;
const CONN_TIMEOUT: u64 = 5;
const KEEP_ALIVE_PERIOD: u64 = 2;

struct Cert<'a> {
    cert_pom: &'a str,
    key_pom: &'a str,
}

lazy_static::lazy_static! {
    static ref CERT: Cert<'static> = Cert {
        cert_pom: "libs/hbb_common/cert/cert.pem",
        key_pom: "libs/hbb_common/cert/key.pem"
    };
}

const MAX_BUFFER_SIZE: usize = 128;
type Sender = mpsc::Sender<Bytes>;
pub type QuicFramedSink = SplitSink<Framed<BidirectionalStream, BytesCodec>, Bytes>;
pub type QuicFramedStream = SplitStream<Framed<BidirectionalStream, BytesCodec>>;

pub struct FramedQuic(Framed<BidirectionalStream, BytesCodec>);

impl Deref for FramedQuic {
    type Target = Framed<BidirectionalStream, BytesCodec>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for FramedQuic {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct Connection {
    client: Option<Client>,
    mpsc_sender: Option<Sender>,
    inner_sender: Arc<RwLock<QuicFramedSink>>,
    inner_stream: QuicFramedStream,
}

#[async_trait]
pub trait SendAPI {
    #[inline]
    async fn send(&mut self, msg: &impl Message) -> ResultType<()> {
        self.send_raw(msg.write_to_bytes()?).await
    }

    #[inline]
    async fn send_raw(&mut self, msg: Vec<u8>) -> ResultType<()> {
        let data = Bytes::from(msg);
        self.send_bytes(data).await
    }

    async fn send_bytes(&mut self, bytes: Bytes) -> ResultType<()>;
}

#[async_trait]
pub trait ReceiveAPI {
    async fn next(&mut self) -> Option<Result<BytesMut, Error>>;

    #[inline]
    async fn next_timeout(&mut self, ms: u64) -> Option<Result<BytesMut, Error>> {
        if let Ok(res) =
            tokio::time::timeout(std::time::Duration::from_millis(ms), self.next()).await
        {
            return res;
        }
        None
    }
}

#[derive(Clone)]
pub struct ConnSender {
    sender: Sender,
}

#[async_trait]
impl SendAPI for ConnSender {
    #[inline]
    async fn send_bytes(&mut self, bytes: Bytes) -> ResultType<()> {
        self.sender
            .send(bytes)
            .await
            .map_err(|e| anyhow!("failed to send data: {}", e))?;
        Ok(())
    }
}

impl ConnSender {
    pub fn new(sender: Sender) -> Self {
        ConnSender { sender }
    }
}

#[async_trait]
impl ReceiveAPI for Connection {
    #[inline]
    async fn next(&mut self) -> Option<Result<BytesMut, Error>> {
        let res = self.inner_stream.next().await;
        res
    }
}

#[async_trait]
impl SendAPI for Connection {
    #[inline]
    async fn send_bytes(&mut self, bytes: Bytes) -> ResultType<()> {
        let mut lock = self.inner_sender.write().await;
        lock.send(bytes)
            .await
            .map_err(|e| anyhow!("failed to send data: {}", e))?;
        // lock.flush().await?;
        Ok(())
    }
}

impl Connection {
    pub async fn new_conn_wrapper(stream: BidirectionalStream) -> ResultType<Self> {
        let frame_wrapper = Framed::new(stream, BytesCodec::new());
        let (sink_sender, inner_stream) = frame_wrapper.split();
        let frame_sender = Arc::new(RwLock::new(sink_sender));
        let (conn_sender, mut conn_receiver) = mpsc::channel::<Bytes>(MAX_BUFFER_SIZE);
        let sender_ref = frame_sender.clone();
        tokio::spawn(async move {
            loop {
                match conn_receiver.recv().await {
                    Some(data) => {
                        sender_ref
                            .write()
                            .await
                            .send(data)
                            .await
                            .map_err(|e| anyhow!("failed to send data by sender channel: {}", e))
                            .ok();
                    }
                    None => break,
                }
            }
            log::error!("Conn sender exit loop!!!");
        });

        Ok(Connection {
            client: None,
            mpsc_sender: Some(conn_sender),
            inner_sender: frame_sender,
            inner_stream,
        })
    }

    pub async fn new_for_client_conn(
        server_addr: SocketAddr,
        local_addr: SocketAddr,
    ) -> ResultType<Self> {
        let io = IoBuilder::default()
            .with_receive_address(local_addr)?
            .build()?;

        let limits = Limits::new()
            .with_max_ack_delay(Duration::from_millis(ACK_LATENCY))
            .expect("set ack delay failed.");
        limits
            .with_max_ack_ranges(MAX_ACK_RANGES)
            .expect("set ack max rangees failed.");
        limits
            .with_max_handshake_duration(Duration::from_secs(MAX_HANDSHAKE_DURATION))
            .expect("set max handshake duration failed.");
        limits
            .with_max_idle_timeout(Duration::from_secs(CONN_TIMEOUT))
            .expect("set max idle timeout failed.");
        limits
            .with_max_keep_alive_period(Duration::from_secs(KEEP_ALIVE_PERIOD))
            .expect("set max keep alive period failed.");

        let client = Client::builder()
            .with_tls(Path::new(CERT.cert_pom))?
            .with_limits(limits)?
            .with_io(io)?
            .start()
            .unwrap();

        let connect = Connect::new(server_addr).with_server_name("localhost");
        let mut connection = client.connect(connect).await?;
        connection.keep_alive(true)?;

        let stream = connection.open_bidirectional_stream().await?;
        let mut conn = Connection::new_conn_wrapper(stream).await?;
        conn.set_client(client);
        Ok(conn)
    }

    fn set_client(&mut self, client: Client) {
        self.client = Some(client);
    }

    pub async fn get_conn_sender(&self) -> ResultType<ConnSender> {
        if self.mpsc_sender.is_some() {
            let wrapper = ConnSender {
                sender: self.mpsc_sender.as_ref().unwrap().clone(),
            };
            return Ok(wrapper);
        }
        return Err(anyhow!("Not found the conn sender!!"));
    }

    #[inline]
    pub fn local_address(&self) -> ResultType<SocketAddr> {
        if self.client.is_none() {
            return Err(anyhow!("Not init the client."));
        }

        match self.client.as_ref().unwrap().local_addr() {
            Ok(addr) => {
                return Ok(addr);
            }
            Err(e) => {
                return Err(anyhow!("Get local addr failed {}", e));
            }
        };
    }

    #[inline]
    pub async fn shutdown(&mut self) -> ResultType<()> {
        self.inner_sender.write().await.close().await?;
        Ok(())
    }
}

pub mod server {
    use super::*;

    pub fn new_server(bind_addr: SocketAddr) -> ResultType<Server> {
        let io = IoBuilder::default()
            .with_receive_address(bind_addr)?
            .build()?;
        let limits = Limits::new()
            .with_max_ack_delay(Duration::from_millis(ACK_LATENCY))
            .expect("set ack delay failed.");
        limits
            .with_max_ack_ranges(MAX_ACK_RANGES)
            .expect("set ack max rangees failed.");
        limits
            .with_max_handshake_duration(Duration::from_secs(MAX_HANDSHAKE_DURATION))
            .expect("set mac handshake duration failed.");
        limits
            .with_max_idle_timeout(Duration::from_secs(CONN_TIMEOUT))
            .expect("set max idle timeout failed.");
        limits
            .with_max_keep_alive_period(Duration::from_secs(KEEP_ALIVE_PERIOD))
            .expect("set max keep alive period failed.");

        let server = Server::builder()
            .with_tls((Path::new(CERT.cert_pom), Path::new(CERT.key_pom)))?
            .with_limits(limits)?
            .with_io(io)?
            .start()
            .unwrap();
        Ok(server)
    }
}

pub fn load_certs(filename: &str) -> Vec<rustls::Certificate> {
    let certfile =
        File::open(filename).expect(&format!("cannot open certificate file: {:?}", filename));
    let mut reader = BufReader::new(certfile);
    rustls_pemfile::certs(&mut reader)
        .unwrap()
        .iter()
        .map(|v| rustls::Certificate(v.clone()))
        .collect()
}

pub fn load_private_key(filename: &str) -> rustls::PrivateKey {
    let keyfile = File::open(filename).expect("cannot open private key file");
    let mut reader = BufReader::new(keyfile);

    loop {
        match rustls_pemfile::read_one(&mut reader).expect("cannot parse private key .pem file") {
            Some(rustls_pemfile::Item::RSAKey(key)) => return rustls::PrivateKey(key),
            Some(rustls_pemfile::Item::PKCS8Key(key)) => return rustls::PrivateKey(key),
            None => break,
            _ => {}
        }
    }

    panic!(
        "no keys found in {:?} (encrypted keys not supported)",
        filename
    );
}
