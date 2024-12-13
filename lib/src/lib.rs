pub use rand::random;

pub const TCP_SERVER_ADDRESS: &str = "127.0.0.1:5123";
pub const UDP_SERVER_ADDRESS: &str = "127.0.0.1:5124";
pub const QUIC_SERVER_ADDRESS: &str = "127.0.0.1:5125";

#[derive(Debug, Default, Clone, Copy)]
pub enum NetworkProtocol {
    #[default]
    Tcp,
    Udp,
    Quic,
}

#[derive(Debug, Default)]
pub struct ConnectionArgs {
    pub protocol: NetworkProtocol,
    pub encrypted: bool,
}

impl ConnectionArgs {
    pub fn parse() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let protocol = args
            .iter()
            .position(|arg| matches!(arg.as_str(), "-p" | "--p"))
            .and_then(|idx| args.get(idx + 1))
            .and_then(|arg| match arg.as_str() {
                "tcp" => Some(NetworkProtocol::Tcp),
                "udp" => Some(NetworkProtocol::Udp),
                "quic" => Some(NetworkProtocol::Quic),
                _ => None,
            })
            .unwrap_or_default();
        let encrypted_arg = String::from("--encrypted");
        let encrypted = args.contains(&encrypted_arg);
        ConnectionArgs {
            protocol,
            encrypted,
        }
    }
}

#[derive(Debug, Default)]
pub struct ClientArgs {
    pub num_players: u16,
}

impl ClientArgs {
    pub fn parse() -> Self {
        ClientArgs { num_players: 100 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Player {
    pub id: u16,
    pub position: [u16; 3],
    pub health: u8,
}

impl Player {
    pub fn new(id: u16) -> Self {
        Player {
            id,
            position: [random(), random(), random()],
            health: 100,
        }
    }

    pub fn to_bytes(self) -> Vec<u8> {
        let mut result = Vec::with_capacity(9);
        result.extend_from_slice(&self.id.to_le_bytes());
        for pos in self.position {
            result.extend_from_slice(&pos.to_le_bytes());
        }
        result.push(self.health);
        result
    }

    pub fn from_bytes(player_bytes: [u8; 9]) -> Self {
        let id = u16::from_le_bytes(player_bytes[0..2].try_into().unwrap());
        let pos_x = u16::from_le_bytes(player_bytes[2..4].try_into().unwrap());
        let pos_y = u16::from_le_bytes(player_bytes[4..6].try_into().unwrap());
        let pos_z = u16::from_le_bytes(player_bytes[6..8].try_into().unwrap());
        let position = [pos_x, pos_y, pos_z];
        let health = player_bytes[8];
        Player {
            id,
            position,
            health,
        }
    }
}
