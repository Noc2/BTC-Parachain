use crate::parser;
use crate::types::{BlockHeader, Error, H256Le};

use bitcoin_spv::btcspv::hash256_merkle_step;

/// Values taken from https://github.com/bitcoin/bitcoin/blob/78dae8caccd82cfbfd76557f1fb7d7557c7b5edb/src/consensus/consensus.h
const MAX_BLOCK_WEIGHT: u32 = 4000000;
const WITNESS_SCALE_FACTOR: u32 = 4;
const MIN_TRANSACTION_WEIGHT: u32 = WITNESS_SCALE_FACTOR * 60;
const MAX_TRANSACTIONS_IN_PROOF: u32 = MAX_BLOCK_WEIGHT / MIN_TRANSACTION_WEIGHT;

/// Struct to store the content of a merkle proof
pub struct MerkleProof {
    pub block_header: BlockHeader,
    pub transactions_count: u32,
    pub hashes: Vec<H256Le>,
    pub flag_bits: Vec<bool>,
}

struct MerkleProofTraversal {
    bits_used: usize,
    hashes_used: usize,
    merkle_position: Option<u32>,
    hash_position: Option<usize>,
}

pub struct ProofResult {
    pub extracted_root: H256Le,
    pub transaction_hash: H256Le,
    pub transaction_position: u32,
}

impl MerkleProof {
    fn compute_tree_width(&self, height: u32) -> u32 {
        (self.transactions_count + (1 << height) - 1) >> height
    }

    /// Returns the height of the partial merkle tree
    pub fn compute_tree_height(&self) -> u32 {
        let mut height = 0;
        while self.compute_tree_width(height) > 1 {
            height += 1;
        }
        height
    }

    /// Performs a depth-first traversal of the partial merkle tree
    /// and returns the computed merkle root
    /// the code is ported from the official Bitcoin client
    /// https://github.com/bitcoin/bitcoin/blob/99813a9745fe10a58bedd7a4cb721faf14f907a4/src/merkleblock.cpp
    fn traverse_and_extract(
        &self,
        height: u32,
        pos: u32,
        traversal: &mut MerkleProofTraversal,
    ) -> Result<H256Le, Error> {
        let parent_of_hash = self.flag_bits[traversal.bits_used];
        traversal.bits_used += 1;

        if height == 0 || !parent_of_hash {
            if traversal.hashes_used >= self.hashes.len() {
                return Err(Error::MalformedProof);
            }
            let hash = self.hashes[traversal.hashes_used];
            if height == 0 && parent_of_hash {
                traversal.merkle_position = Some(pos);
                traversal.hash_position = Some(traversal.hashes_used);
            }
            traversal.hashes_used += 1;
            return Ok(hash);
        }

        let left = self.traverse_and_extract(height - 1, pos * 2, traversal)?;
        let right = if pos * 2 + 1 < self.compute_tree_width(height - 1) {
            self.traverse_and_extract(height - 1, pos * 2 + 1, traversal)?
        } else {
            left
        };

        let hashed_bytes = hash256_merkle_step(&left.to_bytes_le(), &right.to_bytes_le());
        Ok(H256Le::from_bytes_le(&hashed_bytes))
    }

    /// Computes the merkle root of the proof partial merkle tree
    pub fn verify_proof(&self) -> Result<ProofResult, Error> {
        let mut traversal = MerkleProofTraversal {
            bits_used: 0,
            hashes_used: 0,
            merkle_position: None,
            hash_position: None,
        };

        // fail if no transactions
        if self.transactions_count == 0 {
            return Err(Error::MalformedProof);
        }

        // fail if too many transactions
        if self.transactions_count > MAX_TRANSACTIONS_IN_PROOF {
            return Err(Error::MalformedProof);
        }

        // fail if not at least one bit per hash
        if self.flag_bits.len() < self.hashes.len() {
            return Err(Error::MalformedProof);
        }

        let root = self.traverse_and_extract(self.compute_tree_height(), 0, &mut traversal)?;
        let merkle_position = traversal.merkle_position.ok_or(Error::InvalidProof)?;
        let hash_position = traversal.hash_position.ok_or(Error::InvalidProof)?;

        // fail if all hashes are not used
        if traversal.hashes_used != self.hashes.len() {
            return Err(Error::MalformedProof);
        }

        // fail if all bits are not used
        if (traversal.bits_used + 7) / 8 != (self.flag_bits.len() + 7) / 8 {
            return Err(Error::MalformedProof);
        }

        Ok(ProofResult {
            extracted_root: root,
            transaction_hash: self.hashes[hash_position],
            transaction_position: merkle_position,
        })
    }

    /// Parses a merkle proof as produced by the bitcoin client gettxoutproof
    ///
    /// Block header (80 bytes)
    /// Number of transactions in the block (unsigned int, 4 bytes, little endian)
    /// Number of hashes (varint, 1 - 3 bytes)
    /// Hashes (N * 32 bytes, little endian)
    /// Number of bytes of flag bits (varint, 1 - 3 bytes)
    /// Flag bits (little endian)
    ///
    /// See: https://bitqa.app/questions/how-to-decode-merkle-transaction-proof-that-bitcoin-sv-software-provides
    ///
    /// # Arguments
    ///
    /// * `merkle_proof` - Raw bytes of the merkle proof
    pub fn parse(merkle_proof: &[u8]) -> MerkleProof {
        let header = parser::parse_block_header(parser::header_from_bytes(&merkle_proof[0..80]));
        let mut transactions_count: [u8; 4] = Default::default();
        transactions_count.copy_from_slice(&merkle_proof[80..84]);
        let (bytes_consumed, hashes_count) = parser::parse_varint(&merkle_proof[84..87]);
        let mut current_index = bytes_consumed + 84;

        let mut hashes = Vec::new();
        for _ in 0..hashes_count {
            let raw_hash = &merkle_proof[current_index..current_index + 32];
            hashes.push(H256Le::from_bytes_le(raw_hash));
            current_index += 32;
        }

        let last_byte = std::cmp::min(current_index + 3, merkle_proof.len());
        let (bytes_consumed, flag_bits_count) =
            parser::parse_varint(&merkle_proof[current_index..last_byte]);
        current_index += bytes_consumed;

        let mut flag_bits = Vec::new();

        for i in 0..flag_bits_count {
            let byte = merkle_proof[current_index + i as usize];
            for i in 0..8 {
                let mask = 1 << i;
                let bit = (byte & mask) != 0;
                flag_bits.push(bit);
            }
        }

        MerkleProof {
            block_header: header,
            transactions_count: u32::from_le_bytes(transactions_count),
            hashes: hashes,
            flag_bits: flag_bits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bitcoin_spv::utils::deserialize_hex;
    use primitive_types::H256;
    use std::str::FromStr;

    // curl -s -H 'content-type: application/json' http://satoshi.doc.ic.ac.uk:8332 -d '{
    //   "jsonrpc": "1.0",
    //   "id": "test",
    //   "method": "gettxoutproof",
    //   "params": [["61a05151711e4716f31f7a3bb956d1b030c4d92093b843fa2e771b95564f0704"],
    //              "0000000000000000007962066dcd6675830883516bcf40047d42740a85eb2919"]
    // }'
    // block: https://www.blockchain.com/btc/block/0000000000000000007962066dcd6675830883516bcf40047d42740a85eb2919

    const PROOF_HEX: &str = "00000020ecf348128755dbeea5deb8eddf64566d9d4e59bc65d485000000000000000000901f0d92a66ee7dcefd02fa282ca63ce85288bab628253da31ef259b24abe8a0470a385a45960018e8d672f8a90a00000d0bdabada1fb6e3cef7f5c6e234621e3230a2f54efc1cba0b16375d9980ecbc023cbef3ba8d8632ea220927ec8f95190b30769eb35d87618f210382c9445f192504074f56951b772efa43b89320d9c430b0d156b93b7a1ff316471e715151a0619a39392657f25289eb713168818bd5b37476f1bc59b166deaa736d8a58756f9d7ce2aef46d8004c5fe3293d883838f87b5f1da03839878895b71530e9ff89338bb6d4578b3c3135ff3e8671f9a64d43b22e14c2893e8271cecd420f11d2359307403bb1f3128885b3912336045269ef909d64576b93e816fa522c8c027fe408700dd4bdee0254c069ccb728d3516fe1e27578b31d70695e3e35483da448f3a951273e018de7f2a8f657064b013c6ede75c74bbd7f98fdae1c2ac6789ee7b21a791aa29d60e89fff2d1d2b1ada50aa9f59f403823c8c58bb092dc58dc09b28158ca15447da9c3bedb0b160f3fe1668d5a27716e27661bcb75ddbf3468f5c76b7bed1004c6b4df4da2ce80b831a7c260b515e6355e1c306373d2233e8de6fda3674ed95d17a01a1f64b27ba88c3676024fbf8d5dd962ffc4d5e9f3b1700763ab88047f7d0000";

    #[test]
    fn test_parse_proof() {
        let raw_proof = deserialize_hex(&PROOF_HEX[..]).unwrap();
        let proof = MerkleProof::parse(&raw_proof);
        let expected_merkle_root =
            H256::from_str("a0e8ab249b25ef31da538262ab8b2885ce63ca82a22fd0efdce76ea6920d1f90")
                .unwrap();
        assert_eq!(proof.block_header.merkle_root, expected_merkle_root);
        assert_eq!(proof.transactions_count, 2729);
        assert_eq!(proof.hashes.len(), 13);
        // NOTE: following hash is in big endian
        let expected_hash =
            H256Le::from_hex_be("02bcec80995d37160bba1cfc4ef5a230321e6234e2c6f5f7cee3b61fdabada0b");
        assert_eq!(proof.hashes[0], expected_hash);
        assert_eq!(proof.flag_bits.len(), 4 * 8);
    }

    #[test]
    fn test_compute_tree_width() {
        let proof = MerkleProof::parse(&deserialize_hex(&PROOF_HEX[..]).unwrap());
        assert_eq!(proof.compute_tree_width(0), proof.transactions_count);
        assert_eq!(
            proof.compute_tree_width(1),
            proof.transactions_count / 2 + 1
        );
        assert_eq!(proof.compute_tree_width(12), 1);
    }

    #[test]
    fn test_compute_tree_height() {
        let proof = MerkleProof::parse(&deserialize_hex(&PROOF_HEX[..]).unwrap());
        assert_eq!(proof.compute_tree_height(), 12);
    }

    #[test]
    fn test_extract_hash() {
        let proof = MerkleProof::parse(&deserialize_hex(&PROOF_HEX[..]).unwrap());
        let merkle_root = H256Le::from_bytes_be(proof.block_header.merkle_root.as_bytes());
        let result = proof.verify_proof().unwrap();
        assert_eq!(result.extracted_root, merkle_root);
        assert_eq!(result.transaction_position, 48);
        let expected_tx_hash =
            H256Le::from_hex_be("61a05151711e4716f31f7a3bb956d1b030c4d92093b843fa2e771b95564f0704");
        assert_eq!(result.transaction_hash, expected_tx_hash);
    }
}
