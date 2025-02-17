use std::convert::TryFrom;
use std::path::Path;

use clap::{App, Arg, SubCommand};

use near::{get_default_home, get_store_path, load_config, NightshadeRuntime};
use near_chain::{ChainStore, ChainStoreAccess};
use near_network::peer_store::PeerStore;
use near_primitives::account::{AccessKey, Account};
use near_primitives::crypto::signature::PublicKey;
use near_primitives::hash::hash;
use near_primitives::serialize::Decode;
use near_primitives::test_utils::init_integration_logger;
use near_primitives::transaction::Callback;
use near_primitives::utils::col;
use near_store::{create_store, DBValue, TrieIterator};
use node_runtime::ext::ACCOUNT_DATA_SEPARATOR;

fn to_printable(blob: &[u8]) -> String {
    if blob.len() > 60 {
        format!("{} bytes, hash: {}", blob.len(), hash(blob))
    } else {
        let ugly = blob.iter().find(|&&x| x < b' ').is_some();
        if ugly {
            return format!("0x{}", hex::encode(blob));
        }
        match String::from_utf8(blob.to_vec()) {
            Ok(v) => format!(" {}", v),
            Err(_e) => format!("0x{}", hex::encode(blob)),
        }
    }
}

fn print_state_entry(key: Vec<u8>, value: DBValue) {
    let column = &key[0..1];
    match column {
        col::ACCOUNT => {
            let separator = (1..key.len()).find(|&x| key[x] == ACCOUNT_DATA_SEPARATOR[0]);
            if let Some(separator) = separator {
                let account_name = to_printable(&key[1..separator]);
                let contract_key = to_printable(&key[(separator + 1)..]);
                println!(
                    "Storage {:?},{:?}: {:?}",
                    account_name,
                    contract_key,
                    to_printable(&value)
                );
            } else {
                let account: Account = Decode::decode(&value).unwrap();
                let account_name = to_printable(&key[1..]);
                println!("Account {:?}: {:?}", account_name, account);
            }
        }
        col::CALLBACK => {
            let _callback: Callback = Decode::decode(&value).unwrap();
            let callback_id = to_printable(&key[1..]);
            println!("Callback {}: {}", callback_id, to_printable(&value));
        }
        col::CODE => {
            let account_name = to_printable(&key[1..]);
            println!("Code for {:?}: {}", account_name, to_printable(&value));
        }
        col::ACCESS_KEY => {
            let separator = (1..key.len()).find(|&x| key[x] == col::ACCESS_KEY[0]).unwrap();
            let access_key: AccessKey = Decode::decode(&value).unwrap();
            let account_name = to_printable(&key[1..separator]);
            let public_key = PublicKey::try_from(&key[(separator + 1)..]).unwrap();
            println!("Access key {:?},{:?}: {:?}", account_name, public_key, access_key);
        }
        _ => {
            println!(
                "Unknown column {}, {:?}: {:?}",
                column[0],
                to_printable(&key[1..]),
                to_printable(&value)
            );
        }
    }
}

fn main() {
    init_integration_logger();

    let default_home = get_default_home();
    let matches = App::new("state-viewer")
        .arg(
            Arg::with_name("home")
                .long("home")
                .default_value(&default_home)
                .help("Directory for config and data (default \"~/.near\")")
                .takes_value(true),
        )
        .subcommand(SubCommand::with_name("peers"))
        .subcommand(SubCommand::with_name("state"))
        .get_matches();

    let home_dir = matches.value_of("home").map(|dir| Path::new(dir)).unwrap();
    let near_config = load_config(home_dir);

    let store = create_store(&get_store_path(&home_dir));

    match matches.subcommand() {
        ("peers", Some(_args)) => {
            let peer_store = PeerStore::new(store.clone(), &vec![]).unwrap();
            for (peer_id, peer_info) in peer_store.iter() {
                println!("{} {:?}", peer_id, peer_info);
            }
        }
        ("state", Some(_args)) => {
            let mut chain_store = ChainStore::new(store.clone());

            let runtime = NightshadeRuntime::new(&home_dir, store, near_config.genesis_config);
            let head = chain_store.head().unwrap();
            let last_header = chain_store.get_block_header(&head.last_block_hash).unwrap().clone();
            let state_root = chain_store.get_post_state_root(&head.last_block_hash).unwrap();
            let trie = TrieIterator::new(&runtime.trie, state_root).unwrap();

            println!("Storage root is {}, block height is {}", state_root, last_header.height);
            for item in trie {
                let (key, value) = item.unwrap();
                print_state_entry(key, value);
            }
        }
        (_, _) => unreachable!(),
    }
}
