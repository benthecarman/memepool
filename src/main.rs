use crate::mempool::Mempool;
use crate::networking::peer::Peer;
use bitcoin::Network;
use std::sync::Arc;
use std::time::Duration;

mod mempool;
mod networking;

fn main() {
    let mempool: Arc<Mempool> = Arc::new(Mempool::new());

    let _peer = Peer::connect("127.0.0.1:8333", mempool.clone(), Network::Bitcoin)
        .expect("Failed to connect to peer");

    loop {
        std::thread::sleep(Duration::from_millis(1000));
        println!("mempool size: {}", mempool.clone().iter_txs().len())
    }
}
