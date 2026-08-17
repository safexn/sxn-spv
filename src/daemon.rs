use base64::{engine::general_purpose, Engine as _};
use std::io::Cursor;
use std::{net::SocketAddr, str::FromStr, sync::Mutex};

use bitcoin::block::Header as BlockHeader;
use bitcoin::hashes::Hash;
use bitcoin::merkle_tree::PartialMerkleTree;
use bitcoin::MerkleBlock;
use bitcoin::{
    consensus::{deserialize, Decodable},
    BlockHash,
};
use bitcoin::{Transaction, VarInt};

#[cfg(test)]
use bitcoin::Block;
#[cfg(test)]
use bitcoinnakamoto::hashes::hex::ToHex;

use itertools::Itertools;
use serde_json::{json, Value};

use crate::types::MerkleProofParamChain;
use crate::{header::HeaderList, types::Network};

impl Counter {
    fn new() -> Self {
        Counter {
            value: Mutex::new(0),
        }
    }

    fn next(&self) -> u64 {
        let mut value = self.value.lock().unwrap();
        *value += 1;
        *value
    }
}

/// Parse JSONRPC error code, if exists.
fn parse_error_code(err: &Value) -> Option<i64> {
    err.as_object()?.get("code")?.as_i64()
}

fn parse_jsonrpc_reply(mut reply: Value, method: &str, expected_id: u64) -> Result<Value, String> {
    if let Some(reply_obj) = reply.as_object_mut() {
        if let Some(err) = reply_obj.get("error") {
            if !err.is_null() {
                if let Some(code) = parse_error_code(err) {
                    match code {
                        // RPC_IN_WARMUP -> retry by later reconnection
                        -28 => return Err(err.to_string()),
                        _ => return Err(format!("{} RPC error: {}", method, err)),
                    }
                }
            }
        }
        let id = match reply_obj.get("id") {
            Some(i) => i,
            None => return Err(format!("no id in reply: {:?}", reply_obj)),
        }
        .clone();

        if id != expected_id {
            return Err(format!(
                "wrong {} response id {}, expected {}",
                method, id, expected_id
            ));
        }
        if let Some(result) = reply_obj.get_mut("result") {
            return Ok(result.take());
        }
        return Err(format!("no result in reply: {:?}", reply_obj));
    }
    Err(format!("non-object reply: {:?}", reply))
}

pub struct Daemon {
    daemon_rpc_addr: SocketAddr,
    network: Network,
    message_id: Counter,
    cookie: String,
}

struct Counter {
    value: Mutex<u64>,
}

#[allow(dead_code)]
impl Daemon {
    pub fn new(
        daemon_rpc_addr: SocketAddr,
        cookie: String,
        network: Network,
        electrs_only: bool,
    ) -> Result<Daemon, String> {
        let daemon = Daemon {
            daemon_rpc_addr,
            network,
            message_id: Counter::new(),
            cookie,
        };

        if !electrs_only {
            let tip = daemon.getbestblockhash().unwrap();
            tracing::info!(target: "spv", "{network} tip is {tip}");
            let header = daemon.getblockheader(&tip).unwrap();
            tracing::info!(target: "spv", "{network} tip header is {header:?}");
        }

        Ok(daemon)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut values = self.handle_request_batch(method, &[params], 0.0)?;
        assert_eq!(values.len(), 1);
        Ok(values.remove(0))
    }

    fn requests(&self, method: &str, params_list: &[Value]) -> Result<Vec<Value>, String> {
        self.handle_request_batch(method, params_list, 0.0)
    }

    fn send_req(&self, req: &Value) -> Result<Value, String> {
        let url = format!("http://{}", self.daemon_rpc_addr);
        let auth = format!("Basic {}", general_purpose::STANDARD.encode(&self.cookie));
        crate::utils::request(url, auth, req)
    }

    fn handle_request_batch(
        &self,
        method: &str,
        params_list: &[Value],
        failure_threshold: f64,
    ) -> Result<Vec<Value>, String> {
        let id = self.message_id.next();
        let chunks = params_list
            .iter()
            .map(|params| json!({"jsonrpc":"2.0", "method": method, "params": params, "id": id}))
            .chunks(10000);
        let mut results = vec![];
        let total_requests = params_list.len();
        let mut failed_requests: u64 = 0;
        let threshold = (failure_threshold * total_requests as f64).round() as u64;

        for chunk in &chunks {
            let reqs = chunk.collect();
            //let mut replies = self.call_jsonrpc(method, &reqs)?;
            let mut replies = self.send_req(&reqs).map_err(|e| format!("{e:?}"))?;
            if let Some(replies_vec) = replies.as_array_mut() {
                for reply in replies_vec {
                    match parse_jsonrpc_reply(reply.take(), method, id) {
                        Ok(parsed_reply) => results.push(parsed_reply),
                        Err(e) => {
                            failed_requests += 1;
                            // abort and return the last error once a threshold number of requests have failed
                            if failed_requests > threshold {
                                return Err(e);
                            }
                        }
                    }
                }
            } else {
                return Err(format!("non-array replies: {replies:?}"));
            }
        }

        Ok(results)
    }

    pub fn gettxoutproof(
        &self,
        param: &crate::types::MerkleProofParam,
    ) -> Result<MerkleBlock, String> {
        let chain = &param.chain;
        let param = param.to_string();
        let value = self.request("gettxoutproof", param)?;
        match chain {
            MerkleProofParamChain::BitCoin => merkleblock_from_value(value),
            MerkleProofParamChain::DogeCoin => doge_merkleblock_from_value(value),
        }
    }

    pub fn gettxoutproofs(
        &self,
        param: Vec<crate::types::MerkleProofParam>,
    ) -> Result<Vec<MerkleBlock>, String> {
        let chain = &param[0].chain; // TODO::
        let param_string: Vec<Value> = param.iter().map(|param| param.to_string()).collect();
        let mut result = vec![];
        for mb in self.requests("gettxoutproof", &param_string)? {
            match chain {
                MerkleProofParamChain::BitCoin => result.push(merkleblock_from_value(mb)?),
                MerkleProofParamChain::DogeCoin => result.push(doge_merkleblock_from_value(mb)?),
            }
        }
        Ok(result)
    }

    pub fn getbestblockhash(&self) -> Result<BlockHash, String> {
        parse_hash(&self.request("getbestblockhash", json!([]))?)
    }

    #[cfg(test)]
    pub fn getblockcount(&self) -> Result<u64, String> {
        let value = self.request("getblockcount", json!([]))?;
        value
            .as_u64()
            .ok_or_else(|| format!("invalid block count: {}", value))
    }

    pub fn getblockheader(&self, blockhash: &BlockHash) -> Result<BlockHeader, String> {
        match self.network {
            Network::DogecoinMainnet
            | Network::DogecoinTestnet
            | Network::DogecoinRegtest
            | Network::Fractal => header_from_value_fractal_or_doge(
                self.request("getblockheader", json!([blockhash.to_string(), false]))?,
            ),
            _ => header_from_value(
                self.request("getblockheader", json!([blockhash.to_string(), false]))?,
            ),
        }
    }

    #[cfg(test)]
    pub fn getblockbyheight(&self, block_height: u64) -> Result<Block, String> {
        let hash_value = self.request("getblockhash", json!([block_height]))?;
        let block_hash = parse_hash::<BlockHash>(&hash_value)?;
        let block_value = self.request("getblock", json!([block_hash.to_hex(), false]))?;
        doge_block_from_value(block_value)
    }

    pub fn getblockheaders(&self, heights: &[usize]) -> Result<Vec<BlockHeader>, String> {
        let heights: Vec<Value> = heights.iter().map(|height| json!([height])).collect();
        let params_list: Vec<Value> = self
            .requests("getblockhash", &heights)?
            .into_iter()
            .map(|hash| json!([hash, /*verbose=*/ false]))
            .collect();
        let mut result = vec![];
        for h in self.requests("getblockheader", &params_list)? {
            match self.network {
                Network::Fractal
                | Network::DogecoinMainnet
                | Network::DogecoinTestnet
                | Network::DogecoinRegtest => {
                    result.push(header_from_value_fractal_or_doge(h)?);
                }
                _ => {
                    result.push(header_from_value(h)?);
                }
            }
        }
        Ok(result)
    }

    fn get_all_headers(&self, tip: &BlockHash) -> Result<Vec<BlockHeader>, String> {
        let info: Value = self.request("getblockheader", json!([tip.to_string()]))?;
        let tip_height = info
            .get("height")
            .expect("missing height")
            .as_u64()
            .expect("non-numeric height") as usize;
        let all_heights: Vec<usize> = (0..=tip_height).collect();
        let chunk_size = 100_000;
        let mut result = vec![];
        for heights in all_heights.chunks(chunk_size) {
            tracing::info!(target: "spv", "downloading {} block headers", heights.len());
            let mut headers = self.getblockheaders(heights)?;
            assert!(headers.len() == heights.len());
            result.append(&mut headers);
        }

        let mut blockhash = BlockHash::all_zeros();
        for header in &result {
            assert_eq!(header.prev_blockhash, blockhash);
            blockhash = header.block_hash();
        }
        assert_eq!(blockhash, *tip);
        Ok(result)
    }

    pub fn get_new_headers(
        &self,
        indexed_headers: &HeaderList,
        bestblockhash: &BlockHash,
    ) -> Result<Vec<BlockHeader>, String> {
        // Iterate back over headers until known blockash is found:
        if indexed_headers.is_empty() {
            tracing::info!(target: "spv", "downloading all block headers up to {}", bestblockhash);
            return self.get_all_headers(bestblockhash);
        }
        tracing::info!(target: "spv",
            "downloading new block headers ({} already indexed) from {}",
            indexed_headers.len(),
            bestblockhash,
        );
        let mut new_headers = vec![];
        let null_hash = BlockHash::all_zeros();
        let mut blockhash = *bestblockhash;
        while blockhash != null_hash {
            if indexed_headers.header_by_blockhash(&blockhash).is_some() {
                break;
            }
            let header = self
                .getblockheader(&blockhash)
                .map_err(|_| format!("failed to get {} header", blockhash))?;
            blockhash = header.prev_blockhash;
            new_headers.push(header);
        }
        tracing::info!(target: "spv", "downloaded {} block headers", new_headers.len());
        new_headers.reverse(); // so the tip is the last vector entry
        Ok(new_headers)
    }
}

fn parse_hash<T>(value: &Value) -> Result<T, String>
where
    T: FromStr,
{
    T::from_str(match value.as_str() {
        Some(i) => i,
        None => return Err(format!("non-string value: {}", value)),
    })
    .map_err(|_| format!("non-hex value: {}", value))
}

fn header_from_value(value: Value) -> Result<BlockHeader, String> {
    let header_hex = match value.as_str() {
        Some(i) => i,
        None => return Err(format!("non-string header: {}", value)),
    };
    let header_bytes = hex::decode(header_hex).map_err(|_| "non-hex header")?;
    deserialize(&header_bytes).map_err(|_| format!("failed to parse header {}", header_hex))
}

#[cfg(test)]
fn doge_block_from_value(value: Value) -> Result<Block, String> {
    let block_hex = value.as_str().ok_or_else(|| "non-string block")?;
    let block_bytes = hex::decode(block_hex).map_err(|_| "non-hex block")?;
    let mut cursor = Cursor::new(block_bytes);
    let header = BlockHeader::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
    const VERSION_FLAG_AUXPOW: i32 = 1 << 8;
    if header.version.to_consensus() & VERSION_FLAG_AUXPOW != 0 {
        let _parent_coinbase_tx =
            Transaction::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _parent_blockhash =
            BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _coinbase_merkle_branch_len =
            VarInt::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        for _ in 0.._coinbase_merkle_branch_len.0 {
            let _coinbase_merkle_branch_hash =
                BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        }
        let _coinbase_merkle_branch_size_mask =
            i32::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _blockchain_merkle_branch_len =
            VarInt::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        for _ in 0.._blockchain_merkle_branch_len.0 {
            let _blockchain_merkle_branch_hash =
                BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        }
        let _blockchain_merkle_branch_size_mask =
            i32::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _parent_block_header =
            BlockHeader::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
    }

    let txdata =
        Vec::<Transaction>::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

    Ok(Block { header, txdata })
}

fn header_from_value_fractal_or_doge(value: Value) -> Result<BlockHeader, String> {
    let header_hex = match value.as_str() {
        Some(i) => i,
        None => return Err(format!("non-string header: {}", value)),
    };
    let header_bytes = hex::decode(header_hex).map_err(|_| "non-hex header")?;
    if header_bytes.len() < 80 {
        return Err("header is too short".to_string());
    }
    let header_bytes = &header_bytes[0..80];
    deserialize(header_bytes).map_err(|_| format!("failed to parse header {}", header_hex))
}

fn merkleblock_from_value(value: Value) -> Result<MerkleBlock, String> {
    let mb_hex = match value.as_str() {
        Some(i) => i,
        None => return Err(format!("non-string MerkleBlock: {}", value)),
    };
    let mb_bytes = hex::decode(mb_hex).map_err(|_| "non-hex MerkleBlock")?;
    deserialize(&mb_bytes).map_err(|_| format!("failed to parse MerkleBlock {}", mb_hex))
}

fn doge_merkleblock_from_value(value: Value) -> Result<MerkleBlock, String> {
    let mb_hex = match value.as_str() {
        Some(i) => i,
        None => return Err(format!("non-string MerkleBlock: {}", value)),
    };
    let mb_bytes = hex::decode(mb_hex).map_err(|_| "non-hex MerkleBlock")?;
    let mut cursor = Cursor::new(mb_bytes);
    let header = BlockHeader::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
    const VERSION_FLAG_AUXPOW: i32 = 1 << 8;
    if header.version.to_consensus() & VERSION_FLAG_AUXPOW != 0 {
        let _parent_coinbase_tx =
            Transaction::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _parent_blockhash =
            BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _coinbase_merkle_branch_len =
            VarInt::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        for _ in 0.._coinbase_merkle_branch_len.0 {
            let _coinbase_merkle_branch_hash =
                BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        }
        let _coinbase_merkle_branch_size_mask =
            i32::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _blockchain_merkle_branch_len =
            VarInt::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        for _ in 0.._blockchain_merkle_branch_len.0 {
            let _blockchain_merkle_branch_hash =
                BlockHash::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
        }
        let _blockchain_merkle_branch_size_mask =
            i32::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

        let _parent_block_header =
            BlockHeader::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;
    }
    let txn = PartialMerkleTree::consensus_decode(&mut cursor).map_err(|err| err.to_string())?;

    Ok(MerkleBlock { header, txn })
}

#[cfg(test)]
mod test_daemon {
    use crate::daemon::{doge_merkleblock_from_value, Daemon};
    use crate::types::{MerkleProofParam, MerkleProofParamChain, Network};
    use bitcoin::block::Header;
    use bitcoin::consensus::encode;
    use bitcoin::constants::genesis_block;
    use bitcoin::hashes::Hash;
    use bitcoin::{BlockHash, CompactTarget, MerkleBlock, Txid};
    use deeps_rsv::SGXResponseV2;
    use rand::Rng;
    use reqwest::blocking::Client;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::thread::sleep;
    use std::time::Duration;

    fn reg_daemon() -> Daemon {
        Daemon::new(
            SocketAddr::from_str("127.0.0.1:18332").unwrap(),
            "asahi:asahi".to_string(),
            Network::DogecoinRegtest,
            false,
        )
        .unwrap()
    }

    #[test]
    fn test_doge_daemon() {
        let daemon = reg_daemon();
        let block_hash = daemon.getbestblockhash().unwrap();
        println!("block_hash: {}", block_hash);
        let expected_hash =
            BlockHash::from_str("ad10d2dae3f5b5ac70f6a927e2d29c899bc128bc2167267246d07f3e0bf8840a")
                .unwrap();
        assert_eq!(block_hash, expected_hash);

        let block_header = daemon.getblockheader(&block_hash).unwrap();
        let block_header_hex = hex::encode(&encode::serialize(&block_header));
        println!("block_header_hex {}", block_header_hex);
        let expected_header_hex = "0400620007c32cd627272260d41bd4e09b6b8014dcae1b139f1c74fcb2a80cd4f9daf155c23129604ed4ac2651eaa2c7e0d808711ab19563d94583076f8880c666e72b5323323c67ffff7f2001000000";
        assert_eq!(block_header_hex, expected_header_hex);
        let expected_header =
            encode::deserialize::<Header>(&hex::decode(expected_header_hex).unwrap()).unwrap();
        assert_eq!(block_header, expected_header);

        let tx = Txid::from_str("04ea10e81e2a552da1d43903ff7485002b0c03728ea37495f1b357dfed73ef2f")
            .unwrap();
        let param = MerkleProofParam::new(vec![tx], None, MerkleProofParamChain::DogeCoin);
        let proof = daemon.gettxoutproof(&param).unwrap();
        let proof_hex = hex::encode(encode::serialize(&proof));
        println!("proof_hex: {}", proof_hex);
        let expected_proof_hex = "04006200713abad7ed9e9b66290f11286e4d544a50de11545983cce2afb07a2b18ddcd372fef73eddf57b3f19574a38e72030c2b008574ff0339d4a12d552a1ee810ea0493173c67ffff7f200000000001000000012fef73eddf57b3f19574a38e72030c2b008574ff0339d4a12d552a1ee810ea040101";
        assert_eq!(proof_hex, expected_proof_hex);
        let expected_proof =
            encode::deserialize::<MerkleBlock>(&hex::decode(expected_proof_hex).unwrap()).unwrap();
        assert_eq!(proof, expected_proof)
    }

    #[test]
    fn test_doge_genesis() {
        // let a: &[u8; 32] = &[
        //     0x6f, 0xe2, 0x8c, 0x0a, 0xb6, 0xf1, 0xb3, 0x72,
        //     0xc1, 0xa6, 0xa2, 0x46, 0xae, 0x63, 0xf7, 0x4f,
        //     0x93, 0x1e, 0x83, 0x65, 0xe1, 0x5a, 0x08, 0x9c,
        //     0x68, 0xd6, 0x19, 0x00, 0x00, 0x00, 0x00, 0x00,
        // ];
        // let hash = bitcoin_hashes::sha256d::Hash::from_slice(a).unwrap();
        // println!("{}", hash);
        let doge_main_hash_s = "1a91e3dace36e2be3bf030a65679fe821aa1d6ef92e7c9902eb318182c355691";
        let doge_main_hash = BlockHash::from_str(doge_main_hash_s).unwrap();
        println!("doge_main_hash");
        for byte in doge_main_hash.as_byte_array().to_vec().iter() {
            print!(" 0x{:02x},", byte);
        }
        println!();
        let hash = BlockHash::from_slice(&doge_main_hash.as_byte_array().to_vec()).unwrap();
        println!("{}", hash);
        assert_eq!(doge_main_hash, hash);

        let doge_test_hash_s = "bb0a78264637406b6360aad926284d544d7049f45189db5664f3c4d07350559e";
        let doge_test_hash = BlockHash::from_str(doge_test_hash_s).unwrap();
        println!("doge_test_hash");
        for byte in doge_test_hash.as_byte_array().to_vec().iter() {
            print!(" 0x{:02x},", byte);
        }
        println!();
        let hash = BlockHash::from_slice(&doge_test_hash.as_byte_array().to_vec()).unwrap();
        println!("{}", hash);
        assert_eq!(doge_test_hash, hash);

        let doge_regtest_hash_s =
            "3d2160a3b5dc4a9d62e7e66a295f70313ac808440ef7400d6c0772171ce973a5";
        let doge_regtest_hash = BlockHash::from_str(doge_regtest_hash_s).unwrap();
        println!("doge_regtest_hash");
        for byte in doge_regtest_hash.as_byte_array().to_vec().iter() {
            print!(" 0x{:02x},", byte);
        }
        println!();
        let hash = BlockHash::from_slice(&doge_regtest_hash.as_byte_array().to_vec()).unwrap();
        println!("{}", hash);
        assert_eq!(doge_regtest_hash, hash);
    }

    #[test]
    fn test_dogecoin_genesis_block() {
        let block = genesis_block(bitcoin::Network::Regtest);
        println!("{:?}", block);
    }

    #[test]
    fn test_get_dogecoin_header() {
        let daemon = Daemon::new(
            SocketAddr::from_str("192.168.36.15:44555").unwrap(),
            "asahi:asahi".to_string(),
            Network::DogecoinTestnet,
            false,
        )
        .unwrap();
        let block_hash =
            BlockHash::from_str("86d591e00f605394dfdf51bd6469f3a41f90b319d763d5be145aa2068e403ae7")
                .unwrap();

        let header = daemon.getblockheader(&block_hash).unwrap();
        let _block_header_hex = hex::encode(&encode::serialize(&header));
        // assert_eq!(block_header_hex, "0201620029f1b0ead5503338292b295a0425f48c587469a4db9a9e9c5b8a210a408e750af3d3922ead23ea6ddde3b15d6484b74a9c310b289270fab21f286cb0f7bb131f784fba547ef7331d00000000");
        println!("{:?}", header);
        println!("header.bits {:?}", header.bits);
        let result = CompactTarget::from_hex("0x1d33f77e").unwrap();
        println!("CompactTarget {:?}", result)
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Resp {
        result: String,
        error: bool,
    }

    #[test]
    fn test_doge_merkleblock_from_value() {
        let aux_proof = json!("040162007c7b6c653fb76957703c28735ad23b595c58107f10dfa67964b682716ecc181e0047270fc9d1c7bbdaa70d2501a4343f5c56c8f311f822d105ca86575be6cb4302374767eb1f1f1c0000000001000000010000000000000000000000000000000000000000000000000000000000000000ffffffff5b031f833529303043796265724c65617020496e633030000000000f40d8fbbe9584940000000201000000000000002cfabe6d6d64cd75456a0a4ecc3748c43ad310f8ae493fac31909aa2fee2b3fda4e4200e4c0400000013b274edffffffff02205fa012000000001600145755e14e56b05fedd745a51c2de544d3457f18510000000000000000266a24aa21a9ed76b8d85e542cf1626b90d6fba072b0051411782ef5200ff63dbed6d3d0934fe900000000a4cd90355b1d85ec4bbc852ef94e20bb366dee1c4ce72d757e28f33f21e6318e015c322c5cd71c5c913420c399866a187994f06a575cf34c314b18f5d107b54ea500000000020000000000000000000000000000000000000000000000000000000000000000cc380b8ad52f4493140853150fe933c9261da7a2ace9674c23ec7da2eb1866720300000000000020068f8a001ed38caa352086272fe2fcef5fd5d198a5bd514fb1fc32858bad874c207681d6582f5747aa9cd5ebe42a8d5e182e6fde85d941692c785e440c8ed6ee09374767f0ff0f1d38487a2001000000010047270fc9d1c7bbdaa70d2501a4343f5c56c8f311f822d105ca86575be6cb430101");
        assert!(doge_merkleblock_from_value(aux_proof).is_ok());
        let no_aux_proof = json!("02000000447b38ce3d5dd6e470d752ba6c4be0f71eb6f96171465372713d9c8a2612846a1907c23f5e2e116884c710fcf66cf85d552b5eec73dc98e4895914ecf5fd27eca404fb52f0ff0f1e00069ddf01000000011907c23f5e2e116884c710fcf66cf85d552b5eec73dc98e4895914ecf5fd27ec0101");
        assert!(doge_merkleblock_from_value(no_aux_proof).is_ok());
    }

    #[test]
    fn test_doge_verify_tx_testnet() {
        let check_round = 1000;
        let daemon = Daemon::new(
            SocketAddr::from_str("192.168.36.15:44555").unwrap(),
            "asahi:asahi".to_string(),
            Network::DogecoinTestnet,
            false,
        )
        .unwrap();
        let spv_url = "http://192.168.36.15:3023";
        let http_client = Client::new();

        let block_count = daemon.getblockcount().unwrap();

        let mut rng = rand::thread_rng();
        for _round in 0..check_round {
            let block_random_number = rng.gen_range(0..=block_count);
            let block = daemon.getblockbyheight(block_random_number).unwrap();
            let tx_count = block.txdata.len();
            let tx_random_number = rng.gen_range(0..tx_count);
            let tx_id = block.txdata[tx_random_number].compute_txid();
            let spv_sgx_response = http_client
                .get(&format!("{spv_url}/verify_tx_doge/{tx_id}"))
                .send()
                .unwrap()
                .text()
                .unwrap();
            let sgx_resp: SGXResponseV2 = serde_json::from_str(&spv_sgx_response).unwrap();
            let resp_str: Resp = serde_json::from_str(sgx_resp.resp.as_str().unwrap()).unwrap();
            assert!(!resp_str.error)
        }
    }

    #[test]
    fn test_doge_verify_tx_mainnet() {
        let check_round = 1000;
        let daemon = Daemon::new(
            SocketAddr::from_str("192.168.36.15:22555").unwrap(),
            "asahi:asahi".to_string(),
            Network::DogecoinTestnet,
            false,
        )
        .unwrap();
        let spv_url = "http://192.168.36.15:3022";
        let http_client = Client::new();

        let block_count = daemon.getblockcount().unwrap();

        let mut rng = rand::thread_rng();
        for _round in 0..check_round {
            let block_random_number = rng.gen_range(0..=block_count);
            let block = daemon.getblockbyheight(block_random_number).unwrap();
            let tx_count = block.txdata.len();
            let tx_random_number = rng.gen_range(0..tx_count);
            let tx_id = block.txdata[tx_random_number].compute_txid();
            let spv_sgx_response = http_client
                .get(&format!("{spv_url}/verify_tx_doge/{tx_id}"))
                .send()
                .unwrap()
                .text()
                .unwrap();
            let sgx_resp: SGXResponseV2 = serde_json::from_str(&spv_sgx_response).unwrap();
            let resp_str: Resp = serde_json::from_str(sgx_resp.resp.as_str().unwrap()).unwrap();
            assert!(!resp_str.error);
            sleep(Duration::from_millis(50))
        }
    }
}
