use lib::*;
use quinn::crypto::rustls::QuicServerConfig;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinSet;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::{rustls, server::TlsStream, TlsAcceptor};

#[tokio::main]
async fn main() {
    let args = ConnectionArgs::parse();

    if args.encrypted {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::ring::default_provider(),
        );
    }

    let players = Arc::new(RwLock::new(HashMap::new()));

    match (args.protocol, args.encrypted) {
        (NetworkProtocol::Tcp, true) => start_encrypted_tcp_server(players).await,
        (NetworkProtocol::Tcp, false) => start_tcp_server(players).await,
        (NetworkProtocol::Udp, _) => start_udp_server(players).await,
        (NetworkProtocol::Quic, _) => start_quic_server(players).await,
    }
}

async fn start_tcp_server(players: Arc<RwLock<HashMap<u16, Player>>>) {
    let (tx, mut rx) = mpsc::channel::<Player>(100);
    let streams = Arc::new(Mutex::new(HashMap::<u16, OwnedWriteHalf>::new()));
    {
        let players = players.clone();
        let streams = streams.clone();
        tokio::spawn(async move {
            while let Some(mut player) = rx.recv().await {
                let mut players_map = players.write().await;
                if players_map
                    .values()
                    .any(|p| p.id != player.id && p.position == player.position)
                {
                    if player.health <= 10 {
                        // respawn
                        player = Player::new(player.id);
                    } else {
                        // take damage
                        player.health -= 10;
                    }
                }
                players_map.insert(player.id, player);
                let player_bytes = player.to_bytes();
                let mut streams_val = streams.lock().await;
                let new_streams_val = std::mem::take(&mut *streams_val);
                let mut join_set = JoinSet::new();
                for (id, mut stream) in new_streams_val {
                    let player_bytes = player_bytes.clone();
                    join_set.spawn(async move {
                        let result = stream.write_all(&player_bytes).await;
                        (id, stream, result)
                    });
                }
                *streams_val = join_set
                    .join_all()
                    .await
                    .into_iter()
                    .filter_map(|result| {
                        if result.2.is_ok() {
                            Some((result.0, result.1))
                        } else {
                            None
                        }
                    })
                    .collect();
            }
        });
    }

    let listener = TcpListener::bind(TCP_SERVER_ADDRESS)
        .await
        .expect("unable to bind server socket");
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .expect("failed to accept incoming stream");
        let (mut reader, mut writer) = stream.into_split();
        let mut player_id = [0; 2];
        reader
            .read_exact(&mut player_id)
            .await
            .expect("failed to read player id bytes");
        let player_id = u16::from_le_bytes(player_id);
        let all_player_bytes: Vec<u8> = players
            .read()
            .await
            .values()
            .flat_map(|p| p.to_bytes())
            .collect();
        if !all_player_bytes.is_empty() {
            if writer.write_all(&all_player_bytes).await.is_err() {
                continue;
            }
        }
        streams.lock().await.insert(player_id, writer);
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                let mut player_bytes = [0; 9];
                reader.read_exact(&mut player_bytes).await?;
                let player = Player::from_bytes(player_bytes);
                if player.id != player_id {
                    // players can only modify their own state
                    eprintln!("server discarding illegal player modification");
                    continue;
                }
                if tx.send(player).await.is_err() {
                    break;
                }
            }
            std::io::Result::Ok(())
        });
    }
}

async fn start_encrypted_tcp_server(players: Arc<RwLock<HashMap<u16, Player>>>) {
    let (tx, mut rx) = mpsc::channel::<Player>(100);
    let streams = Arc::new(Mutex::new(
        HashMap::<u16, WriteHalf<TlsStream<TcpStream>>>::new(),
    ));
    {
        let players = players.clone();
        let streams = streams.clone();
        tokio::spawn(async move {
            while let Some(mut player) = rx.recv().await {
                let mut players_map = players.write().await;
                if players_map
                    .values()
                    .any(|p| p.id != player.id && p.position == player.position)
                {
                    if player.health <= 10 {
                        // respawn
                        player = Player::new(player.id);
                    } else {
                        // take damage
                        player.health -= 10;
                    }
                }
                players_map.insert(player.id, player);
                let player_bytes = player.to_bytes();
                let mut streams_val = streams.lock().await;
                let new_streams_val = std::mem::take(&mut *streams_val);
                let mut join_set = JoinSet::new();
                for (id, mut stream) in new_streams_val {
                    let player_bytes = player_bytes.clone();
                    join_set.spawn(async move {
                        let result = stream.write_all(&player_bytes).await;
                        (id, stream, result)
                    });
                }
                *streams_val = join_set
                    .join_all()
                    .await
                    .into_iter()
                    .filter_map(|result| {
                        if result.2.is_ok() {
                            Some((result.0, result.1))
                        } else {
                            None
                        }
                    })
                    .collect();
            }
        });
    }

    let listener = TcpListener::bind(TCP_SERVER_ADDRESS)
        .await
        .expect("unable to bind server socket");
    let tls_acceptor = {
        let server_config = create_server_tls_config().await;
        TlsAcceptor::from(Arc::new(server_config))
    };
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .expect("failed to accept incoming stream");
        let stream = tls_acceptor
            .accept(stream)
            .await
            .expect("failed to accept incoming encrypted stream");
        let (mut reader, mut writer) = split(stream);
        let mut player_id = [0; 2];
        reader
            .read_exact(&mut player_id)
            .await
            .expect("failed to read player id bytes");
        let player_id = u16::from_le_bytes(player_id);
        let all_player_bytes: Vec<u8> = players
            .read()
            .await
            .values()
            .flat_map(|p| p.to_bytes())
            .collect();
        if !all_player_bytes.is_empty() {
            if writer.write_all(&all_player_bytes).await.is_err() {
                continue;
            }
        }
        streams.lock().await.insert(player_id, writer);
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                let mut player_bytes = [0; 9];
                reader.read_exact(&mut player_bytes).await?;
                let player = Player::from_bytes(player_bytes);
                if player.id != player_id {
                    // players can only modify their own state
                    eprintln!("server discarding illegal player modification");
                    continue;
                }
                if tx.send(player).await.is_err() {
                    break;
                }
            }
            std::io::Result::Ok(())
        });
    }
}

async fn start_udp_server(players: Arc<RwLock<HashMap<u16, Player>>>) {
    let (tx, mut rx) = mpsc::channel::<Player>(100);
    let socket = UdpSocket::bind(UDP_SERVER_ADDRESS)
        .await
        .expect("unable to bind server socket");
    let socket = Arc::new(socket);
    let clients = Arc::new(Mutex::new(HashMap::<SocketAddr, u16>::new()));
    {
        let players = players.clone();
        let socket = socket.clone();
        let clients = clients.clone();
        tokio::spawn(async move {
            while let Some(mut player) = rx.recv().await {
                let mut players_map = players.write().await;
                // check for collision; if so, take damage
                if players_map
                    .values()
                    .any(|p| p.id != player.id && p.position == player.position)
                {
                    if player.health <= 10 {
                        // respawn
                        player = Player::new(player.id);
                    } else {
                        player.health -= 10;
                    }
                }
                players_map.insert(player.id, player);
                let player_bytes = player.to_bytes();
                let mut clients_map = clients.lock().await;
                let mut join_set = JoinSet::new();
                for (client, id) in clients_map.iter() {
                    let listener = socket.clone();
                    let player_bytes = player_bytes.clone();
                    let client = client.clone();
                    let id = *id;
                    join_set.spawn(async move {
                        let result = listener.send_to(&player_bytes, client).await;
                        (id, result)
                    });
                }
                let error_results: HashSet<u16> = join_set
                    .join_all()
                    .await
                    .into_iter()
                    .filter_map(|(id, result)| if result.is_err() { Some(id) } else { None })
                    .collect();
                clients_map.retain(|_client, id| !error_results.contains(id));
            }
        });
    }

    loop {
        let mut player_bytes = [0; 9];
        match socket.recv_from(&mut player_bytes).await {
            Ok((read_len, addr)) => {
                if read_len == 2 {
                    let player_id = u16::from_le_bytes(player_bytes[0..2].try_into().unwrap());
                    for player in players.read().await.values() {
                        if socket
                            .send_to(&player.to_bytes(), addr.clone())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    clients.lock().await.insert(addr, player_id);
                } else if read_len == 9 {
                    let player = Player::from_bytes(player_bytes);
                    if let Some(player_id) = clients.lock().await.get(&addr) {
                        if &player.id != player_id {
                            // players can only modify their own state
                            eprintln!("server discarding illegal player modification");
                            continue;
                        }
                        if tx.send(player).await.is_err() {
                            break;
                        }
                    }
                } else {
                    continue;
                }
            }
            Err(_) => continue,
        }
    }
}

async fn start_quic_server(players: Arc<RwLock<HashMap<u16, Player>>>) {
    let (tx, mut rx) = mpsc::channel::<Player>(100);
    let mut crypto = create_server_tls_config().await;
    crypto.alpn_protocols = vec![b"h3".to_vec()];

    let server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto).unwrap()));

    let endpoint =
        quinn::Endpoint::server(server_config, QUIC_SERVER_ADDRESS.parse().unwrap()).unwrap();

    let clients = Arc::new(Mutex::new(HashMap::<u16, Arc<quinn::Connection>>::new()));
    {
        let players = players.clone();
        let clients = clients.clone();
        tokio::spawn(async move {
            while let Some(mut player) = rx.recv().await {
                let mut players_map = players.write().await;
                // check for collision; if so, take damage
                if players_map
                    .values()
                    .any(|p| p.id != player.id && p.position == player.position)
                {
                    if player.health <= 10 {
                        // respawn
                        player = Player::new(player.id);
                    } else {
                        player.health -= 10;
                    }
                }
                players_map.insert(player.id, player);
                let player_bytes = player.to_bytes();
                let mut clients_map = clients.lock().await;
                let mut join_set = JoinSet::new();
                for (id, connection) in clients_map.iter() {
                    let player_bytes = player_bytes.clone();
                    let id = *id;
                    let connection = connection.clone();
                    join_set.spawn(async move {
                        let result = match connection.open_uni().await {
                            Ok(mut stream) => {
                                if stream.write_all(&player_bytes).await.is_ok() {
                                    let _ = stream.finish();
                                    false
                                } else {
                                    true
                                }
                            }
                            Err(_) => true,
                        };
                        (id, result)
                    });
                }
                let error_results: HashSet<u16> = join_set
                    .join_all()
                    .await
                    .into_iter()
                    .filter_map(|(id, result)| if result { Some(id) } else { None })
                    .collect();
                clients_map.retain(|id, _connection| !error_results.contains(id));
            }
        });
    }

    loop {
        let conn = endpoint
            .accept()
            .await
            .expect("failed to accept incoming connection")
            .await
            .expect("failed to accept incoming connection");
        let player_id = {
            let mut player_id = [0; 2];
            let mut stream = conn.accept_uni().await.unwrap();
            stream
                .read_exact(&mut player_id)
                .await
                .expect("failed to read player id bytes");
            u16::from_le_bytes(player_id)
        };
        let all_player_bytes: Vec<u8> = players
            .read()
            .await
            .values()
            .flat_map(|p| p.to_bytes())
            .collect();
        if !all_player_bytes.is_empty() {
            let mut send = conn.open_uni().await.unwrap();
            if send.write_all(&all_player_bytes).await.is_err() {
                continue;
            }
        }
        let conn = Arc::new(conn);
        clients.lock().await.insert(player_id, conn.clone());
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                let mut stream = conn.accept_uni().await?;
                let mut player_bytes = [0; 9];
                if stream.read_exact(&mut player_bytes).await.is_err() {
                    break;
                }
                let player = Player::from_bytes(player_bytes);
                if player.id != player_id {
                    // players can only modify their own state
                    eprintln!("server discarding illegal player modification");
                    continue;
                }
                if tx.send(player).await.is_err() {
                    break;
                }
            }
            std::io::Result::Ok(())
        });
    }
}

async fn create_server_tls_config() -> rustls::ServerConfig {
    let cert_path = "cert.pem";
    let key_path = "key.pem";
    if !tokio::fs::try_exists(cert_path).await.unwrap_or_default()
        || !tokio::fs::try_exists(key_path).await.unwrap_or_default()
    {
        let cert = rcgen::generate_simple_self_signed(vec![String::from("localhost")])
            .expect("unable to generate self-signed cert");
        tokio::fs::write(cert_path, cert.cert.pem())
            .await
            .expect("unable to write to cert file");
        tokio::fs::write(key_path, cert.key_pair.serialize_pem())
            .await
            .expect("unable to write to key file");
    }
    let (certs, key) = (
        CertificateDer::pem_file_iter(cert_path)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        PrivateKeyDer::from_pem_file(key_path).unwrap(),
    );
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("unable to construct server tls config")
}
