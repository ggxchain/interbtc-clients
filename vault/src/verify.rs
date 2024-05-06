use bitcoinlib::{
    blockdata::script::{Script, ScriptBuf},
    hashes::Hash,
    key::Secp256k1,
    secp256k1::schnorr::Signature,
    sighash::{Prevouts, ScriptPath},
    taproot::{TapTweakHash, TaprootBuilder},
    Address, Network, ScriptHash, TapSighashType, Transaction, Txid, XOnlyPublicKey,
};

use bitcoincore_rpc::{
    bitcoin,
    bitcoin::{key::UntweakedPublicKey, taproot::LeafVersion, TapNodeHash},
    Auth, Client, RpcApi,
};

use secp256k1::Message;
use std::{ops::Index, process::Command, str::FromStr};

use crate::Error;

fn trim_newline(s: &mut String) {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
}

fn get_path1_locktime_script(lock_time: u32, pubkey: &str) -> ScriptBuf {
    let script = format!("{lock_time} OP_CHECKLOCKTIMEVERIFY OP_DROP {pubkey} OP_CHECKSIG");
    let parts = script.split(" ");

    let output = Command::new("btcc")
        .args(parts.collect::<Vec<_>>())
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());

    let mut hex_script1 = String::from_utf8_lossy(&output.stdout).into_owned();

    trim_newline(&mut hex_script1);

    let script1 = hex::decode(&hex_script1).expect("Decoding failed");

    let s1 = Script::from_bytes(&script1);

    s1.into()
}

fn get_path1_locktime_script_hash(lock_time: u32, pubkey: &str) -> ScriptHash {
    let script = format!("{lock_time} OP_CHECKLOCKTIMEVERIFY OP_DROP {pubkey} OP_CHECKSIG");
    let parts = script.split(" ");

    let output = Command::new("btcc")
        .args(parts.collect::<Vec<_>>())
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());

    let mut hex_script1 = String::from_utf8_lossy(&output.stdout).into_owned();

    trim_newline(&mut hex_script1);

    let script1 = hex::decode(&hex_script1).expect("Decoding failed");

    let s1 = Script::from_bytes(&script1);

    s1.script_hash()
}

fn get_path2_ggx_script(pubkey: &str) -> ScriptBuf {
    let script = format!("{pubkey} OP_CHECKSIG OP_FALSE OP_IF OP_3 6f7264 OP_1 1 0x1e 6170706c69636174696f6e2f6a736f6e3b636861727365743d7574662d38 OP_1 5 0x4b   7b73656e6465723a20223465646663663964666536633062356338336431616233663738643162333961343665626163363739386530386531393736316635656438396563383363313022 OP_ENDIF");
    let parts = script.split(" ");

    let output = Command::new("btcc")
        .args(parts.collect::<Vec<_>>())
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());

    let mut hex_script1 = String::from_utf8_lossy(&output.stdout).into_owned();

    trim_newline(&mut hex_script1);

    let script1 = hex::decode(&hex_script1).expect("Decoding failed");

    let s1 = Script::from_bytes(&script1);

    s1.into()
}

fn get_path2_ggx_script_hash(pubkey: &str) -> ScriptHash {
    let script = format!("{pubkey} OP_CHECKSIG OP_FALSE OP_IF OP_3 6f7264 OP_1 1 0x1e 6170706c69636174696f6e2f6a736f6e3b636861727365743d7574662d38 OP_1 5 0x4b   7b73656e6465723a20223465646663663964666536633062356338336431616233663738643162333961343665626163363739386530386531393736316635656438396563383363313022 OP_ENDIF");
    let parts = script.split(" ");

    let output = Command::new("btcc")
        .args(parts.collect::<Vec<_>>())
        .output()
        .expect("failed to execute process");

    assert!(output.status.success());

    let mut hex_script1 = String::from_utf8_lossy(&output.stdout).into_owned();

    trim_newline(&mut hex_script1);

    let script1 = hex::decode(&hex_script1).expect("Decoding failed");

    let s1 = Script::from_bytes(&script1);

    s1.script_hash()
}

fn get_rawtx(rpc: &Client, txid: &str) -> bitcoinlib::Transaction {
    let mut decoded = [0u8; 32];
    hex::decode_to_slice(txid, &mut decoded).expect("Decoding failed");
    decoded.reverse();

    let txid = Txid::from_byte_array(decoded);
    let raw_tx = rpc.get_raw_transaction(&txid, None).unwrap();

    raw_tx
}

pub fn check_script_in_utxo(
    rpc: &Client,
    hex_tx_id: &str,
    _utxo_index: usize,
    locktime: u32,
    pubkey_locktime: &str,
    pubkey_ggx: &str,
) -> Result<bool, Error> {
    let s1 = get_path1_locktime_script(locktime, pubkey_locktime);
    let s2 = get_path2_ggx_script(pubkey_ggx);

    let root = TaprootBuilder::new();
    let root = root.add_leaf(1, s1.clone()).unwrap();
    let root = root.add_leaf(1, s2.clone()).unwrap();

    let _tree = root.try_into_taptree().unwrap();

    let hash1 = TapNodeHash::from_script(&s1, LeafVersion::TapScript);
    let hash2 = TapNodeHash::from_script(&s2, LeafVersion::TapScript);
    let root_hash = TapNodeHash::from_node_hashes(hash1, hash2);

    let internal_pubkey = "f30544d6009c8d8d94f5d030b2e844b1a3ca036255161c479db1cca5b374dd1c";
    let un_tweak_key = UntweakedPublicKey::from_str(internal_pubkey).unwrap();
    let _un_tweak_hash = TapTweakHash::from_key_and_tweak(un_tweak_key, Some(root_hash));

    let secp = Secp256k1::new();

    let addr = Address::p2tr(&secp, un_tweak_key, Some(root_hash), Network::Regtest);
    //println!("@@@ product addr from two script hash {:?}", addr); //check addr equ utxo scriptPubKey.address

    let mut decoded = [0u8; 32];
    hex::decode_to_slice(hex_tx_id, &mut decoded).expect("Decoding failed");
    decoded.reverse();

    let _txid = Txid::from_byte_array(decoded);

    let utxo_raw_tx = get_rawtx(&rpc, hex_tx_id);
    let new_address = Address::from_script(utxo_raw_tx.output[0].script_pubkey.as_script(), Network::Regtest).unwrap();

    println!("@@@ address in utxo script_pubkey {:?}", new_address);

    if addr == new_address {
        println!("@@@ script in utxo");
        Ok(true)
    } else {
        println!("@@@ script not in utxo");
        Ok(false)
    }
}

pub fn check_spend_uxto_is_locktime(
    rpc: &Client,
    lock_time: u32,
    lock_time_pubkey: &str,
    spend_tx_id: &str,
) -> Result<bool, Error> {
    //let utxo_raw_tx = get_rawtx(rpc, utxo_tx_id);
    let spend_raw_tx = get_rawtx(rpc, spend_tx_id);

    //script_pubkey
    let t = &spend_raw_tx.input[0].witness.index(1);

    let witness_script = Script::from_bytes(t);

    let script1 = get_path1_locktime_script_hash(lock_time, lock_time_pubkey);

    if witness_script.script_hash() == script1 {
        println!("@@@ script1 is same, this spend utxo is use locktime script");
        Ok(true)
    } else {
        println!("@@@ script1 is not same, this spend utxo is not use locktime script ");
        Ok(false)
    }
}
