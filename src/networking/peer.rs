use std::collections::HashMap;
use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use socks::{Socks5Stream, ToTargetAddr};

use rand::{thread_rng, Rng};

use crate::mempool::Mempool;
use bitcoin::consensus::{Decodable, Encodable};
use bitcoin::hash_types::BlockHash;
use bitcoin::network::constants::ServiceFlags;
use bitcoin::network::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::network::message_blockdata::*;
use bitcoin::network::message_network::VersionMessage;
use bitcoin::network::Address;
use bitcoin::{Block, Network, Transaction};

use super::error::NetworkingError;

type ResponsesMap = HashMap<&'static str, Arc<(Mutex<Vec<NetworkMessage>>, Condvar)>>;

pub(crate) const TIMEOUT_SECS: u64 = 30;

/// A Bitcoin peer
#[derive(Debug)]
#[allow(dead_code)]
pub struct Peer {
    writer: Arc<Mutex<TcpStream>>,
    responses: Arc<RwLock<ResponsesMap>>,

    reader_thread: thread::JoinHandle<()>,
    connected: Arc<RwLock<bool>>,

    mempool: Arc<Mempool>,

    version: VersionMessage,
    network: Network,
}

impl Peer {
    /// Connect to a peer over a plaintext TCP connection
    ///
    /// This function internally spawns a new thread that will monitor incoming messages from the
    /// peer, and optionally reply to some of them transparently, like [pings](NetworkMessage::Ping)
    pub fn connect<A: ToSocketAddrs>(
        address: A,
        mempool: Arc<Mempool>,
        network: Network,
    ) -> Result<Self, NetworkingError> {
        let stream = TcpStream::connect(address)?;

        Peer::from_stream(stream, mempool, network)
    }

    /// Connect to a peer through a SOCKS5 proxy, optionally by using some credentials, specified
    /// as a tuple of `(username, password)`
    ///
    /// This function internally spawns a new thread that will monitor incoming messages from the
    /// peer, and optionally reply to some of them transparently, like [pings](NetworkMessage::Ping)
    pub fn connect_proxy<T: ToTargetAddr, P: ToSocketAddrs>(
        target: T,
        proxy: P,
        credentials: Option<(&str, &str)>,
        mempool: Arc<Mempool>,
        network: Network,
    ) -> Result<Self, NetworkingError> {
        let socks_stream = if let Some((username, password)) = credentials {
            Socks5Stream::connect_with_password(proxy, target, username, password)?
        } else {
            Socks5Stream::connect(proxy, target)?
        };

        Peer::from_stream(socks_stream.into_inner(), mempool, network)
    }

    /// Create a [`Peer`] from an already connected TcpStream
    fn from_stream(
        stream: TcpStream,
        mempool: Arc<Mempool>,
        network: Network,
    ) -> Result<Self, NetworkingError> {
        let writer = Arc::new(Mutex::new(stream.try_clone()?));
        let responses: Arc<RwLock<ResponsesMap>> = Arc::new(RwLock::new(HashMap::new()));
        let connected = Arc::new(RwLock::new(true));

        let reader_thread_responses = Arc::clone(&responses);
        let reader_thread_writer = Arc::clone(&writer);
        let reader_thread_mempool = Arc::clone(&mempool);
        let reader_thread_connected = Arc::clone(&connected);
        let reader_thread = thread::spawn(move || {
            Self::reader_thread(
                network,
                stream,
                reader_thread_responses,
                reader_thread_writer,
                reader_thread_mempool,
                reader_thread_connected,
            )
        });

        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let nonce = thread_rng().gen();
        let services = ServiceFlags::WITNESS;
        let mut locked_writer = writer.lock().unwrap();
        let receiver = Address::new(&locked_writer.peer_addr()?, services);
        let sender = Address {
            services,
            address: [0u16; 8],
            port: 0,
        };

        Self::_send(
            &mut locked_writer,
            network.magic(),
            NetworkMessage::Version(VersionMessage {
                version: 70016,
                services,
                timestamp,
                receiver,
                sender,
                nonce,
                user_agent: "/Satoshi:23.0.0/".into(),
                start_height: 0,
                relay: true,
            }),
        )?;
        let version = if let NetworkMessage::Version(version) =
            Self::_recv(&responses, "version", None).unwrap()
        {
            version
        } else {
            return Err(NetworkingError::InvalidResponse);
        };

        if let NetworkMessage::Verack = Self::_recv(&responses, "verack", None).unwrap() {
            Self::_send(&mut locked_writer, network.magic(), NetworkMessage::Verack)?;
        } else {
            return Err(NetworkingError::InvalidResponse);
        }

        drop(locked_writer);

        println!("Connected to peer!");

        Ok(Peer {
            writer,
            responses,
            reader_thread,
            connected,
            mempool,
            version,
            network,
        })
    }

    /// Send a Bitcoin network message
    fn _send(
        writer: &mut TcpStream,
        magic: u32,
        payload: NetworkMessage,
    ) -> Result<(), NetworkingError> {
        let raw_message = RawNetworkMessage { magic, payload };

        raw_message
            .consensus_encode(writer)
            .map_err(|_| NetworkingError::DataCorruption)?;

        Ok(())
    }

    /// Wait for a specific incoming Bitcoin message, optionally with a timeout
    fn _recv(
        responses: &Arc<RwLock<ResponsesMap>>,
        wait_for: &'static str,
        timeout: Option<Duration>,
    ) -> Option<NetworkMessage> {
        let message_resp = {
            let mut lock = responses.write().unwrap();
            let message_resp = lock.entry(wait_for).or_default();
            Arc::clone(message_resp)
        };

        let (lock, cvar) = &*message_resp;

        let mut messages = lock.lock().unwrap();
        while messages.is_empty() {
            match timeout {
                None => messages = cvar.wait(messages).unwrap(),
                Some(t) => {
                    let result = cvar.wait_timeout(messages, t).unwrap();
                    if result.1.timed_out() {
                        return None;
                    }
                    messages = result.0;
                }
            }
        }

        messages.pop()
    }

    /// Return the [`VersionMessage`] sent by the peer
    pub fn get_version(&self) -> &VersionMessage {
        &self.version
    }

    /// Return the Bitcoin [`Network`] in use
    pub fn get_network(&self) -> Network {
        self.network
    }

    /// Return the mempool used by this peer
    pub fn get_mempool(&self) -> Arc<Mempool> {
        Arc::clone(&self.mempool)
    }

    /// Return whether or not the peer is still connected
    pub fn is_connected(&self) -> bool {
        *self.connected.read().unwrap()
    }

    /// Internal function called once the `reader_thread` is spawned
    fn reader_thread(
        network: Network,
        connection: TcpStream,
        reader_thread_responses: Arc<RwLock<ResponsesMap>>,
        reader_thread_writer: Arc<Mutex<TcpStream>>,
        reader_thread_mempool: Arc<Mempool>,
        reader_thread_connected: Arc<RwLock<bool>>,
    ) {
        macro_rules! check_disconnect {
            ($call:expr) => {
                match $call {
                    Ok(good) => good,
                    Err(e) => {
                        println!("Error {:?}", e);
                        *reader_thread_connected.write().unwrap() = false;

                        break;
                    }
                }
            };
        }

        let mut reader = BufReader::new(connection);
        loop {
            let raw_message: RawNetworkMessage =
                check_disconnect!(Decodable::consensus_decode(&mut reader));

            let in_message = if raw_message.magic != network.magic() {
                continue;
            } else {
                raw_message.payload
            };

            let result = match in_message {
                NetworkMessage::Ping(nonce) => {
                    check_disconnect!(Self::_send(
                        &mut reader_thread_writer.lock().unwrap(),
                        network.magic(),
                        NetworkMessage::Pong(nonce),
                    ));

                    continue;
                }
                NetworkMessage::Alert(_) => continue,
                NetworkMessage::Tx(ref tx) => reader_thread_mempool.add_tx(tx.clone()),
                NetworkMessage::Inv(ref invs) => {
                    let get_data: Vec<Inventory> = invs
                        .iter()
                        .filter_map(|inv| match inv {
                            Inventory::Error
                            | Inventory::Block(_)
                            | Inventory::WitnessBlock(_)
                            | Inventory::CompactBlock(_) => None,
                            Inventory::Transaction(_) => Some(*inv),
                            Inventory::WitnessTransaction(_) => Some(*inv),
                            Inventory::WTx(_) => Some(*inv),
                            Inventory::Unknown { inv_type, hash } => {
                                println!(
                                    "Unknown inventory request type `{}`, hash `{:?}`",
                                    inv_type, hash
                                );
                                None
                            }
                        })
                        .collect();

                    check_disconnect!(Self::_send(
                        &mut reader_thread_writer.lock().unwrap(),
                        network.magic(),
                        NetworkMessage::GetData(get_data)
                    ));

                    Ok(())
                }
                NetworkMessage::GetData(ref inv) => {
                    let (found, not_found): (Vec<_>, Vec<_>) = inv
                        .iter()
                        .map(|item| (*item, reader_thread_mempool.get_tx(item)))
                        .partition(|(_, d)| d.as_ref().unwrap_or(&None).is_some());
                    for (_, found_tx) in found {
                        check_disconnect!(Self::_send(
                            &mut reader_thread_writer.lock().unwrap(),
                            network.magic(),
                            NetworkMessage::Tx(found_tx.unwrap().unwrap()),
                        ));
                    }

                    if !not_found.is_empty() {
                        check_disconnect!(Self::_send(
                            &mut reader_thread_writer.lock().unwrap(),
                            network.magic(),
                            NetworkMessage::NotFound(
                                not_found.into_iter().map(|(i, _)| i).collect(),
                            ),
                        ));
                    }

                    Ok(())
                }
                _ => Ok(())
            };

            if let Err(e) = result {
                println!("Error {:?}", e);
            }

            let message_resp = {
                let mut lock = reader_thread_responses.write().unwrap();
                let message_resp = lock.entry(in_message.cmd()).or_default();
                Arc::clone(message_resp)
            };

            let (lock, cvar) = &*message_resp;
            let mut messages = lock.lock().unwrap();
            messages.push(in_message);
            cvar.notify_all();
        }
    }

    /// Send a raw Bitcoin message to the peer
    pub fn send(&self, payload: NetworkMessage) -> Result<(), NetworkingError> {
        let mut writer = self.writer.lock().unwrap();
        Self::_send(&mut writer, self.network.magic(), payload)
    }

    /// Waits for a specific incoming Bitcoin message, optionally with a timeout
    pub fn recv(
        &self,
        wait_for: &'static str,
        timeout: Option<Duration>,
    ) -> Result<Option<NetworkMessage>, NetworkingError> {
        Ok(Self::_recv(&self.responses, wait_for, timeout))
    }
}

pub trait InvPeer {
    fn get_block(&self, block_hash: BlockHash) -> Result<Option<Block>, NetworkingError>;
    fn ask_for_mempool(&self) -> Result<(), NetworkingError>;
    fn broadcast_tx(&self, tx: Transaction) -> Result<(), NetworkingError>;
}

impl InvPeer for Peer {
    fn get_block(&self, block_hash: BlockHash) -> Result<Option<Block>, NetworkingError> {
        self.send(NetworkMessage::GetData(vec![Inventory::WitnessBlock(
            block_hash,
        )]))?;

        match self.recv("block", Some(Duration::from_secs(TIMEOUT_SECS)))? {
            None => Ok(None),
            Some(NetworkMessage::Block(response)) => Ok(Some(response)),
            _ => Err(NetworkingError::InvalidResponse),
        }
    }

    fn ask_for_mempool(&self) -> Result<(), NetworkingError> {
        if !self.version.services.has(ServiceFlags::BLOOM) {
            return Err(NetworkingError::PeerBloomDisabled);
        }

        self.send(NetworkMessage::MemPool)?;
        let inv = match self.recv("inv", Some(Duration::from_secs(5)))? {
            None => return Ok(()), // empty mempool
            Some(NetworkMessage::Inv(inv)) => inv,
            _ => return Err(NetworkingError::InvalidResponse),
        };

        let getdata = inv
            .iter()
            .cloned()
            .filter(
                |item| matches!(item, Inventory::Transaction(txid) if !self.mempool.has_tx(txid).unwrap_or(false)),
            )
            .collect::<Vec<_>>();
        let num_txs = getdata.len();
        self.send(NetworkMessage::GetData(getdata))?;

        for _ in 0..num_txs {
            let tx = self
                .recv("tx", Some(Duration::from_secs(TIMEOUT_SECS)))?
                .ok_or(NetworkingError::Timeout)?;
            let tx = match tx {
                NetworkMessage::Tx(tx) => tx,
                _ => return Err(NetworkingError::InvalidResponse),
            };

            if let Err(e) = self.mempool.add_tx(tx) {
                println!("Error adding tx to mempool: {e:?}");
            }
        }

        Ok(())
    }

    fn broadcast_tx(&self, tx: Transaction) -> Result<(), NetworkingError> {
        if let Err(e) = self.mempool.add_tx(tx.clone()) {
            println!("Error adding tx to mempool: {e:?}");
        }
        self.send(NetworkMessage::Tx(tx))?;

        Ok(())
    }
}
