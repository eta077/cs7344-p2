use lib::*;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::{rustls, TlsConnector};

#[tokio::main]
async fn main() {
    let connection_args = ConnectionArgs::parse();
    let client_args = ClientArgs::parse();

    let mut tasks = JoinSet::new();
    let mut all_player_maps = HashMap::with_capacity(client_args.num_players as usize);
    for player_id in 0..client_args.num_players {
        let players = Arc::new(RwLock::new(HashMap::with_capacity(
            client_args.num_players as usize,
        )));
        all_player_maps.insert(player_id, players.clone());
        tasks.spawn(async move {
            let mut join_set = JoinSet::new();
            match (connection_args.protocol, connection_args.encrypted) {
                (NetworkProtocol::Tcp, true) => {
                    let (read_task, write_task) = game_logic(
                        EncryptedTcpGameProtocol::connect(player_id).await,
                        player_id,
                        players,
                    );
                    join_set.spawn(read_task);
                    join_set.spawn(write_task);
                }
                (NetworkProtocol::Tcp, false) => {
                    let (read_task, write_task) = game_logic(
                        TcpGameProtocol::connect(player_id).await,
                        player_id,
                        players,
                    );
                    join_set.spawn(read_task);
                    join_set.spawn(write_task);
                }
                (NetworkProtocol::Udp, _) => {
                    let (read_task, write_task) = game_logic(
                        UdpGameProtocol::connect(player_id).await,
                        player_id,
                        players,
                    );
                    join_set.spawn(read_task);
                    join_set.spawn(write_task);
                }
                (NetworkProtocol::Quic, _) => todo!(),
            }
            join_set.join_all().await;
        });
    }

    tasks.join_all().await;

    let player_map_value = all_player_maps.remove(&0).unwrap();
    let player_map = player_map_value.read().await;
    println!("final player map: {player_map:#?}");
    let mut all_match = true;
    for (id, map) in all_player_maps.into_iter() {
        if *map.read().await != *player_map {
            all_match = false;
            println!("{id} doesn't match player 0 state");
        }
    }
    if all_match {
        println!("all match",);
    }
}

trait GameProtocol {
    type R: GameProtocolReader;
    type W: GameProtocolWriter;

    async fn connect(player_id: u16) -> Self;
    fn reader(&mut self) -> Self::R;
    fn writer(&mut self) -> Self::W;
}

trait GameProtocolReader {
    async fn read(&mut self) -> std::io::Result<[u8; 9]>;
}

trait GameProtocolWriter {
    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;
}

fn game_logic<GP: GameProtocol>(
    mut game_protocol: GP,
    player_id: u16,
    players: Arc<RwLock<HashMap<u16, Player>>>,
) -> (impl Future<Output = ()>, impl Future<Output = ()>) {
    let read_task = {
        let players = players.clone();
        let mut reader = game_protocol.reader();
        async move {
            loop {
                let player_bytes = match timeout(Duration::from_millis(750), reader.read()).await {
                    Ok(Ok(x)) => x,
                    Ok(Err(_)) | Err(_) => break,
                };

                let player = Player::from_bytes(player_bytes);
                players.write().await.insert(player.id, player);
            }
        }
    };

    let write_task = async move {
        let mut writer = game_protocol.writer();
        writer
            .write(&player_id.to_le_bytes())
            .await
            .expect("failed to send player id bytes");

        {
            // spawn player
            let player = Player::new(player_id);
            writer
                .write(&player.to_bytes())
                .await
                .expect("failed to send player spawn bytes");
        }
        let mut read_spawn_counter = 0;
        while read_spawn_counter < 5 {
            if players.read().await.contains_key(&player_id) {
                break;
            }
            sleep(Duration::from_millis(150)).await;
            read_spawn_counter += 1;
        }
        if read_spawn_counter == 5 {
            eprintln!("timeout waiting for player {player_id} to spawn");
            return;
        }
        for _ in 0..99 {
            let Some(mut player) = players.read().await.get(&player_id).cloned() else {
                eprintln!("player {player_id} no longer exists");
                return;
            };
            for coord in 0..3 {
                if random() {
                    if let Some(next) = player.position[coord].checked_add(1) {
                        player.position[coord] = next;
                    }
                }
            }
            writer
                .write(&player.to_bytes())
                .await
                .expect("failed to send player move bytes");
        }
    };

    (read_task, write_task)
}

struct TcpGameProtocol {
    reader: Option<OwnedReadHalf>,
    writer: Option<OwnedWriteHalf>,
}

impl GameProtocol for TcpGameProtocol {
    type R = TcpProtocolReader<OwnedReadHalf>;
    type W = TcpProtocolWriter<OwnedWriteHalf>;

    async fn connect(_player_id: u16) -> Self {
        let stream = TcpStream::connect(TCP_SERVER_ADDRESS)
            .await
            .expect("unable to connect to tcp server");

        let (reader, writer) = stream.into_split();
        Self {
            reader: Some(reader),
            writer: Some(writer),
        }
    }

    fn reader(&mut self) -> Self::R {
        if let Some(reader) = self.reader.take() {
            TcpProtocolReader { reader }
        } else {
            panic!("attempted to take tcp protocol reader twice");
        }
    }

    fn writer(&mut self) -> Self::W {
        if let Some(writer) = self.writer.take() {
            TcpProtocolWriter { writer }
        } else {
            panic!("attempted to take tcp protocol writer twice");
        }
    }
}

struct EncryptedTcpGameProtocol {
    reader: Option<ReadHalf<TlsStream<TcpStream>>>,
    writer: Option<WriteHalf<TlsStream<TcpStream>>>,
}

impl GameProtocol for EncryptedTcpGameProtocol {
    type R = TcpProtocolReader<ReadHalf<TlsStream<TcpStream>>>;
    type W = TcpProtocolWriter<WriteHalf<TlsStream<TcpStream>>>;

    async fn connect(_player_id: u16) -> Self {
        let domain = ServerName::try_from("localhost").unwrap().to_owned();
        let mut root_cert_store = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter("cert.pem").expect("unable to read cert file") {
            root_cert_store
                .add(cert.expect("unable to parse cert file"))
                .expect("unable to add cert to store");
        }

        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));
        let stream = TcpStream::connect(TCP_SERVER_ADDRESS)
            .await
            .expect("unable to connect to tcp server");
        let stream = connector
            .connect(domain, stream)
            .await
            .expect("unable to connect tls to stream");

        let (reader, writer) = split(stream);
        Self {
            reader: Some(reader),
            writer: Some(writer),
        }
    }

    fn reader(&mut self) -> Self::R {
        if let Some(reader) = self.reader.take() {
            TcpProtocolReader { reader }
        } else {
            panic!("attempted to take tcp protocol reader twice");
        }
    }

    fn writer(&mut self) -> Self::W {
        if let Some(writer) = self.writer.take() {
            TcpProtocolWriter { writer }
        } else {
            panic!("attempted to take tcp protocol writer twice");
        }
    }
}

struct TcpProtocolReader<Reader: AsyncReadExt> {
    reader: Reader,
}

impl<Reader: AsyncReadExt + Unpin> GameProtocolReader for TcpProtocolReader<Reader> {
    async fn read(&mut self) -> std::io::Result<[u8; 9]> {
        let mut bytes = [0; 9];
        self.reader.read_exact(&mut bytes).await?;
        std::io::Result::Ok(bytes)
    }
}

struct TcpProtocolWriter<Writer: AsyncWriteExt> {
    writer: Writer,
}

impl<Writer: AsyncWriteExt + Unpin> GameProtocolWriter for TcpProtocolWriter<Writer> {
    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes).await
    }
}

struct UdpGameProtocol {
    socket: Arc<UdpSocket>,
}

impl GameProtocol for UdpGameProtocol {
    type R = UdpProtocolReader;
    type W = UdpProtocolWriter;

    async fn connect(player_id: u16) -> Self {
        let socket = UdpSocket::bind(&format!("0.0.0.0:{}", 5125 + player_id))
            .await
            .expect("unable to bind to local udp socket");
        socket
            .connect(UDP_SERVER_ADDRESS)
            .await
            .expect("unable to register server udp address");
        UdpGameProtocol {
            socket: Arc::new(socket),
        }
    }

    fn reader(&mut self) -> Self::R {
        UdpProtocolReader {
            socket: self.socket.clone(),
        }
    }

    fn writer(&mut self) -> Self::W {
        UdpProtocolWriter {
            socket: self.socket.clone(),
        }
    }
}

struct UdpProtocolReader {
    socket: Arc<UdpSocket>,
}

impl GameProtocolReader for UdpProtocolReader {
    async fn read(&mut self) -> std::io::Result<[u8; 9]> {
        let mut bytes = [0; 9];
        let read_len = self.socket.recv(&mut bytes).await?;
        if read_len < 9 {
            std::io::Result::Err(std::io::Error::other("could not fill entire player buffer"))
        } else {
            std::io::Result::Ok(bytes)
        }
    }
}

struct UdpProtocolWriter {
    socket: Arc<UdpSocket>,
}

impl GameProtocolWriter for UdpProtocolWriter {
    async fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let write_len = self.socket.send(bytes).await?;
        if write_len < bytes.len() {
            std::io::Result::Err(std::io::Error::other("could not send entire buffer"))
        } else {
            std::io::Result::Ok(())
        }
    }
}
