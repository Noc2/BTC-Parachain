#![cfg_attr(not(feature = "std"), no_std)]
#[cfg(test)]
mod tests; 

/// For more guidance on FRAME pallets, see the example.
/// https://github.com/paritytech/substrate/blob/master/frame/example/src/lib.rs

/// # BTC-Relay implementation
/// This is the implementation of the BTC-Relay following the spec at:
/// https://interlay.gitlab.io/polkabtc-spec/btcrelay-spec/

// Substrate
use frame_support::{decl_module, decl_storage, decl_event, decl_error, dispatch::DispatchResult, ensure};
use {system::ensure_signed};
use node_primitives::{Moment};
use sp_core::{U256, H256, H160};
use sp_std::collections::btree_map::BTreeMap;

// Crates
use bitcoin::types::{RichBlockHeader, BlockChain};
use bitcoin::parser::{header_from_bytes, parse_block_header};
use security::{ErrorCodes};

/// ## Configuration and Constants
/// The pallet's configuration trait.
/// For further reference, see: 
/// https://interlay.gitlab.io/polkabtc-spec/btcrelay-spec/spec/data-model.html
pub trait Trait: system::Trait {
    /// The overarching event type.
    type Event: From<Event> + Into<<Self as system::Trait>::Event>;
    
}

/// Difficulty Adjustment Interval
pub const DIFFICULTY_ADJUSTMENT_INTERVAL: u16 = 2016;

/// Target Timespan
pub const TARGET_TIMESPAN: u64 = 1209600;

/// Unrounded Maximum Target
/// 0x00000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
pub const UNROUNDED_MAX_TARGET: U256 = U256([0x00000000ffffffffu64, <u64>::max_value(), <u64>::max_value(), <u64>::max_value()]);

/// Main chain id
pub const MAIN_CHAIN_ID: u32 = 0;

// This pallet's storage items.
decl_storage! {
	trait Store for Module<T: Trait> as BTCRelay {
    /// ## Storage
        /// Store Bitcoin block headers
        BlockHeaders get(fn blockheader): map H256 => RichBlockHeader<H256, U256, Moment>;
        
        /// Sorted mapping of BlockChain elements with reference to ChainsIndex
        Chains get(fn chain): linked_map u32 => u32;

        /// Store the index for each tracked blockchain
        ChainsIndex get(fn chainindex): map u32 => BlockChain<u32, BTreeMap<u32, H256>>;
        
        /// Store the current blockchain tip
        BestBlock get(fn bestblock): H256;

        /// Store the height of the best block
        BestBlockHeight get(fn bestblockheight): u32;

        /// Track existing BlockChain entries
        ChainCounter get(fn chaincounter): u32;
	}
}

// The pallet's dispatchable functions.
decl_module! {
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		// Initializing events
		fn deposit_event() = default;
        
        // Initialize errors
        type Error = Error<T>;

        fn initialize(
            origin,
            block_header_bytes: Vec<u8>,
            block_height: u32) 
            -> DispatchResult
        {
            let _ = ensure_signed(origin)?;

            // Check if BTC-Relay was already initialized
            ensure!(!<BestBlock>::exists(), Error::<T>::AlreadyInitialized);

            // Parse the block header bytes to extract the required info
            let raw_block_header = header_from_bytes(&block_header_bytes);
            let basic_block_header = parse_block_header(raw_block_header);
            let block_header_hash = basic_block_header.block_hash; 
            
            // construct the BlockChain struct
            let blockchain = Self::initialize_blockchain(&block_height, &block_header_hash)
                .map_err(|_e| <Error<T>>::AlreadyInitialized)?;
            // Create rich block header
            
            let block_header = RichBlockHeader {
                block_header: basic_block_header,
                block_height: block_height,
                chain_ref: blockchain.chain_id
            };
            
            // Store a new BlockHeader struct in BlockHeaders
            <BlockHeaders>::insert(&block_header_hash, &block_header);

            // Store a pointer to BlockChain in ChainsIndex
            <ChainsIndex>::insert(&MAIN_CHAIN_ID, &blockchain); 
  
            // Store the reference to the new BlockChain in Chains
            <Chains>::insert(&MAIN_CHAIN_ID, &MAIN_CHAIN_ID);

            // Set BestBlock and BestBlockHeight to the submitted block
            <BestBlock>::put(&block_header_hash);
            <BestBlockHeight>::put(&block_height);

            // Emit a Initialized Event
            Self::deposit_event(Event::Initialized(block_height, block_header_hash));
            
            Ok(())
        }
    
        fn store_block_header(origin, block_header_bytes: Vec<u8>)
        -> DispatchResult {
            let _ = ensure_signed(origin)?;
            // TODO: Check if BTC _Parachain is in shutdown state.

            // Parse the block header bytes to extract the required info
            let raw_block_header = header_from_bytes(&block_header_bytes);
            let basic_block_header = parse_block_header(raw_block_header);
            let block_header_hash = basic_block_header.block_hash; 
           
            // TODO: call verify_block_header
            

            // get the block header of the previous block
            ensure!(<BlockHeaders>::exists(basic_block_header.hash_prev_block), Error::<T>::PrevBlock);
            let prev_header = Self::blockheader(basic_block_header.hash_prev_block);

            // get the block chain of the previous header
            let prev_blockchain = Self::chainindex(prev_header.chain_ref);
              
            // Update the current block header
            // check if the prev block is the highest block in the chain
            // load the previous block header block height
            let prev_block_height = prev_header.block_height;
            
            // update the current block header structure with height and chain ref
            // Set the height of the block header
            let current_block_height = prev_block_height
                .checked_add(1)
                .ok_or(<Error<T>>::BlockHeightOverflow)?;
            
            // Update the blockchain
            // check if we create a new blockchain or extend the existing one
            let blockchain = match prev_blockchain.max_height {
                // extend the current chain
                prev_block_height => Self::extend_blockchain(
                    &current_block_height, &block_header_hash, prev_blockchain)
                    .map_err(|_e| <Error<T>>::DuplicateBlock)?,
                // create new blockchain element
                _ => Self::create_blockchain(
                    &current_block_height, &block_header_hash)
                    .map_err(|_e| <Error<T>>::DuplicateBlock)?,
            };
            
            // Create rich block header
            let block_header = RichBlockHeader {
                block_header: basic_block_header,
                block_height: current_block_height,
                chain_ref: blockchain.chain_id
            };
            

            // Store a new BlockHeader struct in BlockHeaders
            <BlockHeaders>::insert(&block_header_hash, &block_header);

            // Storing the blockchain depends if we extend or create a new chain
            match blockchain.chain_id {
                // extended the chain
                prev_chain_id => {
                    // Update the pointer to BlockChain in ChainsIndex
                    <ChainsIndex>::mutate(&blockchain.chain_id, |_b| &blockchain); 
                
                    // check if ordering of Chains needs updating
                    Self::check_and_do_reorg(&blockchain);
                }
                // create a new chain
                _ => {
                    // Store a pointer to BlockChain in ChainsIndex
                    <ChainsIndex>::insert(&blockchain.chain_id, &blockchain);
                    // Store the reference to the blockchain in Chains
                    Self::insert_sorted(&blockchain);
                }
            };
            
            // Determine if this block extends the main chain or a fork
            let current_best_block = <BestBlock>::get();
            match current_best_block {
                // extends the main chain
                block_header_hash => {
                    Self::deposit_event(
                    Event::StoreMainChainHeader(
                        current_block_height,
                        block_header_hash));
                }
                // created a new fork or updated an existing one
                _ => {
                    Self::deposit_event(
                    Event::StoreForkHeader(
                        blockchain.chain_id, 
                        current_block_height, 
                        block_header_hash));
                }
            };
                

            Ok(())
        }

        fn verify_transaction_inclusion(
            origin,
            tx_id: H256,
            tx_block_height: u32,
            tx_index: u64,
            merkle_proof: Vec<u8>,
            confirmations: u32)
        -> DispatchResult {
            let _ = ensure_signed(origin)?;

            // TODO: check if Parachain is in error status
            
            // TODO: check no data blocks

            Ok(())

        }
        
        fn flag_block_error(origin, block_hash: H256, error: ErrorCodes)
            -> DispatchResult {
           
            // TODO: ensure this is a staked relayer
            let _ = ensure_signed(origin)?;
            
            // Get the chain id of the block header
            ensure!(<BlockHeaders>::exists(block_hash), Error::<T>::BlockNotFound);
            let block_header = Self::blockheader(block_hash);
            let chain_id = block_header.chain_ref;

            // Get the blockchain element for the chain id
            let mut blockchain = Self::chainindex(&chain_id);

            // Flag errors in the blockchain entry
            // Check which error we are dealing with
            match error {
                ErrorCodes::NoDataBTCRelay => blockchain
                    .no_data
                    .push(block_header.block_height),
                ErrorCodes::InvalidBTCRelay => blockchain
                    .invalid
                    .push(block_header.block_height),
                _ => return Err(<Error<T>>::UnknownErrorcode.into()),
            };

            // Store the updated blockchain entry
            <ChainsIndex>::mutate(&chain_id, |_b| blockchain);

            Self::deposit_event(Event::FlagBlockError(block_hash, chain_id, error));
            Ok (())
        }
        
        fn clear_block_error(origin, block_hash: H256, error: ErrorCodes)
            -> DispatchResult {
           
            // TODO: ensure this is a staked relayer
            let _ = ensure_signed(origin)?;
            
            // Get the chain id of the block header
            ensure!(<BlockHeaders>::exists(block_hash), Error::<T>::BlockNotFound);
            let block_header = Self::blockheader(block_hash);
            let chain_id = block_header.chain_ref;

            // Get the blockchain element for the chain id
            let mut blockchain = Self::chainindex(&chain_id);

            // Clear errors in the blockchain entry
            // Check which error we are dealing with
            match error {
                ErrorCodes::NoDataBTCRelay => {
                    let index = blockchain.no_data
                        .iter()
                        .position(|x| *x == block_header.block_height)
                        .unwrap();
                    blockchain.no_data.remove(index);
                },
                ErrorCodes::InvalidBTCRelay => {
                    let index = blockchain.invalid
                        .iter()
                        .position(|x| *x == block_header.block_height)
                        .unwrap();
                    blockchain.invalid.remove(index);
                },
                _ => return Err(<Error<T>>::UnknownErrorcode.into()),
            };

            // Store the updated blockchain entry
            <ChainsIndex>::mutate(&chain_id, |_b| blockchain);

            Self::deposit_event(Event::ClearBlockError(block_hash, chain_id, error));
            Ok (())
        }

	}
}

/// Utility functions
impl<T: Trait> Module<T> {
    fn increment_chain_counter() -> Result<u32, Error<T>> {
        let new_counter = <ChainCounter>::get()
            .checked_add(1)
            .ok_or(<Error<T>>::ChainCounterOverflow)?;
        <ChainCounter>::put(new_counter);

        Ok(new_counter)
    }
    fn initialize_blockchain(
        block_height: &u32,
        block_hash: &H256)
        -> Result<BlockChain<u32, BTreeMap<u32, H256>>, Error<T>> 
    {
        let chain_id = MAIN_CHAIN_ID;

        // generate an empty blockchain
        let blockchain = Self::generate_blockchain(
            &chain_id, &block_height, &block_hash)?;
        
        Ok(blockchain)
    }
    fn create_blockchain(
        block_height: &u32,
        block_hash: &H256)
        -> Result<BlockChain<u32, BTreeMap<u32, H256>>, Error<T>> 
    {
        // get a new chain id
        let chain_id: u32 = Self::increment_chain_counter()?; 
        
        // generate an empty blockchain
        let blockchain = Self::generate_blockchain(
            &chain_id, &block_height, &block_hash)?;
        
        Ok(blockchain)
    }
    fn generate_blockchain(
        chain_id: &u32,
        block_height: &u32,
        block_hash: &H256)
        -> Result<BlockChain<u32, BTreeMap<u32, H256>>, Error<T>> 
    {
        // initialize an empty chain
        let mut chain = BTreeMap::new();

        if let Some(_) = chain.insert(*block_height, *block_hash) {
            return Err(<Error<T>>::DuplicateBlock.into())
        }
                
        let blockchain = BlockChain {
                    chain_id: *chain_id,
                    chain: chain,
                    start_height: *block_height,
                    max_height: *block_height,
                    no_data: vec![],
                    invalid: vec![],
        };
        Ok(blockchain)
    }
    fn extend_blockchain(
        block_height: &u32,
        block_hash: &H256,
        prev_blockchain: BlockChain<u32, BTreeMap<u32, H256>>) 
        -> Result<BlockChain<u32, BTreeMap<u32, H256>>, Error<T>> 
    {

        let mut blockchain = prev_blockchain;
        
        if let Some(_) = blockchain.chain.insert(*block_height, *block_hash) {
            return Err(<Error<T>>::DuplicateBlock.into())
        }
                
        blockchain.max_height = *block_height;

        Ok(blockchain)
    }
    fn swap_main_blockchain(fork: &BlockChain<u32, BTreeMap<u32, H256>>) -> Option<Error<T>> {
        // load the main chain
        let mut main_chain = <ChainsIndex>::get(MAIN_CHAIN_ID);
      
        // the start height of the fork
        let start_height = &fork.start_height;

        // create a new blockchain element to store the part of the main chain
        // that is being forked
        // generate a chain id
        let chain_id = match Self::increment_chain_counter() {
            Ok(id) => id,
            Err(err) => return Some(err),
        };

        // split off the chain
        let forked_chain = main_chain.chain.split_off(start_height); 
        
        // maybe split off the no data elements
        // check if there is a no_data block element 
        // that is greater than start_height
        let index_no_data = main_chain.no_data
            .iter()
            .position(|&h| &h >= start_height);
        let no_data = match index_no_data {
            Some(index) => main_chain.no_data.split_off(index),
            None => vec![],
        };

        // maybe split off the invalid elements
        let index_invalid = main_chain.invalid
            .iter()
            .position(|&h| &h >= start_height);
        let invalid = match index_invalid {
            Some(index) => main_chain.invalid.split_off(index),
            None => vec![],
        };

        // store the main chain part that is going to be replaced by the new fork
        // into the forked_main_chain element
        let forked_main_chain: BlockChain<u32, BTreeMap<u32, H256>> = BlockChain {
            chain_id: chain_id, 
            chain: forked_chain.clone(),
            start_height: *start_height,
            max_height: main_chain.max_height,
            no_data: no_data,
            invalid: invalid,
        };

        // append the fork to the main chain
        main_chain.chain.append(&mut fork.chain.clone());
        main_chain.max_height = fork.max_height;
        main_chain.no_data.append(&mut fork.no_data.clone());
        main_chain.invalid.append(&mut fork.invalid.clone());
        
        // get the best block hash
        let best_block = match main_chain.chain.get(&main_chain.max_height) {
            Some(block) => block,
            None => return Some(<Error<T>>::HeaderNotFound),
        };

        // get the position of the fork in Chains
        let position: u32 = match <Chains>::enumerate().position(|(_k,v)| v == fork.chain_id) {
            Some(pos) => pos as u32,
            None => return Some(<Error<T>>::ForkIdNotFound),
        };

        // Update the stored main chain
        <ChainsIndex>::insert(&MAIN_CHAIN_ID, &main_chain);

        // Set BestBlock and BestBlockHeight to the submitted block
        <BestBlock>::put(&best_block);
        <BestBlockHeight>::put(&main_chain.max_height);
       
        // remove the fork from storage
        <ChainsIndex>::remove(fork.chain_id);
        Self::remove_blockchain(&position);

        // store the forked main chain
        <ChainsIndex>::insert(&forked_main_chain.chain_id, &forked_main_chain); 

        // insert the reference to the forked main chain in Chains
        Self::insert_sorted(&main_chain);

        // get an iterator of all forked block headers
        // update all the forked block headers
        for (_height, block) in forked_chain.iter() {
            <BlockHeaders>::mutate(
                    &block, |header| header.chain_ref = forked_main_chain.chain_id);
        };

        // get an iterator of all new main chain block headers
        // update all new main chain block headers
        for (_height, block) in fork.chain.iter() {
            <BlockHeaders>::mutate(
                    &block, |header| header.chain_ref = MAIN_CHAIN_ID);
        };

        None
    }

    fn check_and_do_reorg(fork: &BlockChain<u32, BTreeMap<u32, H256>>) -> Option<Error<T>> {
        // Check if the ordering needs updating
        // if the fork is the main chain, we don't need to update the ordering
        if fork.chain_id == MAIN_CHAIN_ID {
            return None 
        }

        // get the position of the fork in Chains
        let fork_position: u32 = match <Chains>::enumerate().position(|(_k,v)| v == fork.chain_id) {
            Some(pos) => pos as u32,
            None => return Some(<Error<T>>::ForkIdNotFound),
        };
        
        // check if the previous element in Chains has a lower block_height
        let mut current_position = fork_position;
        let mut current_height = fork.max_height;

        // swap elements as long as previous block height is smaller
        while current_position > 0 {
            // get the previous position
            let prev_position = current_position - 1;
            // get the blockchain id
            let prev_blockchain_id = <Chains>::get(&prev_position);
            // get the previous blockchain height
            let prev_height = <ChainsIndex>
                ::get(&prev_blockchain_id)
                .max_height;
            // swap elements if block height is greater
            if prev_height < current_height {
                // Check if swap occurs on the main chain element
                match prev_position {
                    // if the previous position is the top element,
                    // we are swapping the main chain
                    MAIN_CHAIN_ID => {
                        match Self::swap_main_blockchain(&fork) {
                            Some(err) => return Some(err),
                            None => break,
                        };
                    },
                    // else, simply swap the chain_id ordering in Chains
                    _ => <Chains>::swap(prev_position, current_position),
                }
                
                // update the current chain to the previous one
                current_position = prev_position;
                current_height = prev_height;
            } else {
                break;
            }
        }

        None 

    }
    fn insert_sorted(
        blockchain: &BlockChain<u32, BTreeMap<u32, H256>>) {
        // get a sorted vector over the Chains elements
        // NOTE: LinkedStorageMap iterators are not sorted over the keys
        let mut chains = <Chains>::enumerate().collect::<Vec<(u32, u32)>>();
        chains.sort_by_key(|k| k.0);
     
        let max_chain_element = chains.len() as u32;
        // define the position of the new blockchain
        // by default, we insert it as the last element
        let mut position_blockchain = max_chain_element;

        // Starting from the second highest element, find where to insert the new fork
        // the previous element's block height should be higher or equal 
        // the next element's block height should be lower or equal
        // NOTE: we never want to insert a new main chain through this function
        for (curr_position, curr_chain_id) in chains.iter().skip(1) { 
            // get the height of the current chain_id
            let curr_height = <ChainsIndex>::get(curr_chain_id).max_height;
          
            // if the height of the current blockchain is lower than
            // the new blockchain, it should be inserted at that position
            if curr_height <= blockchain.max_height {
                let position_blockchain = curr_position;
                break;
            };
        };

        // insert the new fork into the chains element
        <Chains>::insert(&max_chain_element, &blockchain.chain_id);
        // starting from the last element swap the positions until 
        // the new blockchain is at the position_blockchain
        for curr_position in (position_blockchain..max_chain_element).rev() {
            // stop when the blockchain element is at it's 
            // designated position
            if curr_position < position_blockchain {
                break;
            };
            let prev_position = curr_position - 1;
            // swap the current element with the previous one
            <Chains>::swap(curr_position, prev_position);
        };
    }
    fn remove_blockchain(position: &u32) {
        // swap the element with the last element in the mapping
        let head_index = <Chains>::head().unwrap();
        <Chains>::swap(position, head_index);
        // remove the header (now the value at the initial position)
        <Chains>::remove(head_index);
    }
}

decl_event! {
	pub enum Event {
        Initialized(u32, H256),
        StoreMainChainHeader(u32, H256),
        StoreForkHeader(u32, u32, H256),
        ChainReorg(H256, u32, u32),
        VerifyTransaction(H256, u32, u32),
        ValidateTransaction(H256, u32, H160, H256),
        FlagBlockError(H256, u32, ErrorCodes),
        ClearBlockError(H256, u32, ErrorCodes),
	}
}

// TODO: how to include message in errors?
decl_error! {
    pub enum Error for Module<T: Trait> {
        AlreadyInitialized,
        NotMainChain,
        ForkPrevBlock,
        NotFork,
        InvalidForkId,
        MissingBlockHeight,
        InvalidHeaderSize,
        DuplicateBlock,
        PrevBlock,
        LowDiff,
        DiffTargetHeader,
        MalformedTxid,
        Confirmations,
        InvalidMerkleProof,
        ForkIdNotFound,
        HeaderNotFound,
        Partial,
        Invalid,
        Shutdown,
        InvalidTxid,
        InsufficientValue,
        TxFormat,
        WrongRecipient,
        InvalidOpreturn,
        InvalidTxVersion,
        NotOpReturn,
        UnknownErrorcode,
        BlockNotFound,
        AlreadyReported,
        ChainCounterOverflow,
        BlockHeightOverflow,
        ChainsUnderflow,
    }
}

