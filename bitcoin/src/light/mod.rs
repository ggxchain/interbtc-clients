mod error;
mod wallet;

pub use crate::{Error as BitcoinError, *};
pub use error::Error;

use async_trait::async_trait;
use backoff::future::retry;
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::sleep};

const RETRY_DURATION: Duration = Duration::from_millis(1000);

#[derive(Clone)]
pub struct BitcoinLight {
    network: Network,
    private_key: PrivateKey,
    secp_ctx: secp256k1::Secp256k1<secp256k1::All>,
    electrs: ElectrsClient,
    transaction_creation_lock: Arc<Mutex<()>>,
    wallet: wallet::Wallet,
}

impl BitcoinLight {
    // TODO: implement cli config
    pub fn new(electrs_url: Option<String>, network: Network, private_key: PrivateKey) -> Self {
        let electrs_client = ElectrsClient::new(electrs_url, network).unwrap();
        Self {
            network,
            private_key,
            secp_ctx: secp256k1::Secp256k1::new(),
            electrs: electrs_client.clone(),
            transaction_creation_lock: Arc::new(Mutex::new(())),
            wallet: wallet::Wallet::new(network, electrs_client),
        }
    }

    fn get_change_address(&self) -> Result<Address, Error> {
        self.wallet
            .key_store
            .read()?
            .first_key_value()
            .map(|(address, _)| address.clone())
            .ok_or(Error::NoChangeAddress)
    }

    async fn create_transaction(
        &self,
        recipient: Address,
        sat: u64,
        fee_rate: SatPerVbyte,
        request_id: Option<H256>,
    ) -> Result<LockedTransaction, BitcoinError> {
        let lock = self.transaction_creation_lock.clone().lock_owned().await;

        let unsigned_tx = self.wallet.create_transaction(recipient.clone(), sat, request_id);

        let change_address = self.get_change_address()?;

        let mut psbt = self
            .wallet
            .fund_transaction(unsigned_tx, change_address, fee_rate.0.saturating_mul(1000))
            .await?;
        self.wallet.sign_transaction(&mut psbt)?;
        let signed_tx = psbt.extract_tx();

        Ok(LockedTransaction::new(signed_tx, recipient.to_string(), Some(lock)))
    }

    // TODO: hold tx lock until inclusion
    // otherwise electrs may report stale utxos
    async fn send_transaction(&self, transaction: LockedTransaction) -> Result<Txid, BitcoinError> {
        let txid = self.electrs.send_transaction(transaction.transaction).await?;
        Ok(txid)
    }
}

#[async_trait]
impl BitcoinCoreApi for BitcoinLight {
    fn network(&self) -> Network {
        self.network
    }

    async fn wait_for_block(&self, height: u32, num_confirmations: u32) -> Result<Block, BitcoinError> {
        loop {
            match futures::future::try_join(
                self.electrs.get_block_hash(height),
                self.electrs.get_blocks_tip_height(),
            )
            .await
            {
                Ok((hash, best)) => {
                    if best.saturating_sub(height) >= num_confirmations {
                        return Ok(self.electrs.get_block(&hash).await?);
                    } else {
                        sleep(RETRY_DURATION).await;
                        continue;
                    }
                }
                _ => {
                    // TODO: handle error
                    sleep(RETRY_DURATION).await;
                    continue;
                }
            }
        }
    }

    async fn get_block_count(&self) -> Result<u64, BitcoinError> {
        Ok(self.electrs.get_blocks_tip_height().await?.into())
    }

    fn get_balance(&self, _min_confirmations: Option<u32>) -> Result<Amount, BitcoinError> {
        // TODO: implement
        Ok(Default::default())
    }

    fn list_transactions(&self, _max_count: Option<usize>) -> Result<Vec<json::ListTransactionResult>, BitcoinError> {
        // TODO: implement
        Ok(Default::default())
    }

    async fn get_raw_tx(&self, txid: &Txid, _block_hash: &BlockHash) -> Result<Vec<u8>, BitcoinError> {
        Ok(self.electrs.get_raw_tx(txid).await?)
    }

    async fn get_transaction(&self, txid: &Txid, _block_hash: Option<BlockHash>) -> Result<Transaction, BitcoinError> {
        let raw_tx = self.electrs.get_raw_tx(txid).await?;
        deserialize(&raw_tx).map_err(Into::into)
    }

    async fn get_proof(&self, txid: Txid, _block_hash: &BlockHash) -> Result<Vec<u8>, BitcoinError> {
        Ok(self.electrs.get_raw_merkle_proof(&txid).await?)
    }

    async fn get_block_hash(&self, height: u32) -> Result<BlockHash, BitcoinError> {
        match self.electrs.get_block_hash(height).await {
            Ok(block_hash) => Ok(block_hash),
            Err(_) => Err(BitcoinError::InvalidBitcoinHeight),
        }
    }

    async fn get_new_address(&self) -> Result<Address, BitcoinError> {
        Ok(self.get_change_address()?)
    }

    async fn get_new_public_key(&self) -> Result<PublicKey, BitcoinError> {
        Ok(self.private_key.public_key(&self.secp_ctx))
    }

    fn dump_derivation_key(&self, _public_key: &PublicKey) -> Result<PrivateKey, BitcoinError> {
        Ok(self.private_key)
    }

    fn import_derivation_key(&self, _private_key: &PrivateKey) -> Result<(), BitcoinError> {
        // nothing to do
        Ok(())
    }

    async fn add_new_deposit_key(&self, _public_key: PublicKey, secret_key: Vec<u8>) -> Result<(), BitcoinError> {
        fn mul_secret_key(vault_key: SecretKey, issue_key: SecretKey) -> Result<SecretKey, BitcoinError> {
            let mut deposit_key = vault_key;
            deposit_key.mul_assign(&issue_key[..])?;
            Ok(deposit_key)
        }

        self.wallet.put_p2wpkh_key(mul_secret_key(
            self.private_key.key,
            SecretKey::from_slice(&secret_key)?,
        )?)?;

        Ok(())
    }

    async fn get_best_block_hash(&self) -> Result<BlockHash, BitcoinError> {
        Ok(self.electrs.get_blocks_tip_hash().await?)
    }

    async fn get_pruned_height(&self) -> Result<u64, BitcoinError> {
        // nothing to do
        Ok(Default::default())
    }

    async fn get_block(&self, hash: &BlockHash) -> Result<Block, BitcoinError> {
        Ok(self.electrs.get_block(hash).await?)
    }

    async fn get_block_header(&self, hash: &BlockHash) -> Result<BlockHeader, BitcoinError> {
        Ok(self.electrs.get_block_header(hash).await?)
    }

    async fn get_mempool_transactions<'a>(
        &'a self,
    ) -> Result<Box<dyn Iterator<Item = Result<Transaction, BitcoinError>> + Send + 'a>, BitcoinError> {
        let txids = self.electrs.get_raw_mempool().await?;
        let txs = futures::future::join_all(txids.iter().map(|txid| self.get_transaction(txid, None))).await;
        Ok(Box::new(txs.into_iter()))
    }

    async fn wait_for_transaction_metadata(
        &self,
        txid: Txid,
        num_confirmations: u32,
    ) -> Result<TransactionMetadata, BitcoinError> {
        let (block_height, block_hash, fee) = retry(get_exponential_backoff(), || async {
            Ok(match self.electrs.get_tx_info(&txid).await {
                Ok(electrs::TxInfo {
                    confirmations,
                    height,
                    hash,
                    fee,
                }) if confirmations >= num_confirmations => Ok((height, hash, fee)),
                Ok(_) => Err(BitcoinError::ConfirmationError),
                Err(_e) => Err(BitcoinError::ConnectionRefused),
            }?)
        })
        .await?;

        let proof = self.get_proof(txid, &block_hash).await?;
        let raw_tx = self.get_raw_tx(&txid, &block_hash).await?;

        Ok(TransactionMetadata {
            txid,
            proof,
            raw_tx,
            block_height,
            block_hash,
            fee: Some(fee),
        })
    }

    async fn bump_fee(&self, _txid: &Txid, _address: Address, _fee_rate: SatPerVbyte) -> Result<Txid, BitcoinError> {
        unimplemented!()
    }

    async fn create_and_send_transaction(
        &self,
        address: Address,
        sat: u64,
        fee_rate: SatPerVbyte,
        request_id: Option<H256>,
    ) -> Result<Txid, BitcoinError> {
        let tx = self.create_transaction(address, sat, fee_rate, request_id).await?;
        let txid = self.send_transaction(tx).await?;
        Ok(txid)
    }

    async fn send_to_address(
        &self,
        address: Address,
        sat: u64,
        request_id: Option<H256>,
        fee_rate: SatPerVbyte,
        num_confirmations: u32,
    ) -> Result<TransactionMetadata, BitcoinError> {
        let txid = self
            .create_and_send_transaction(address, sat, fee_rate, request_id)
            .await?;

        Ok(self.wait_for_transaction_metadata(txid, num_confirmations).await?)
    }

    async fn create_or_load_wallet(&self) -> Result<(), BitcoinError> {
        // nothing to do
        Ok(())
    }

    async fn rescan_blockchain(&self, _start_height: usize, _end_height: usize) -> Result<(), BitcoinError> {
        // nothing to do
        Ok(())
    }

    async fn rescan_electrs_for_addresses(&self, _addresses: Vec<Address>) -> Result<(), BitcoinError> {
        // nothing to do
        Ok(())
    }

    fn get_utxo_count(&self) -> Result<usize, BitcoinError> {
        // TODO: implement
        Ok(Default::default())
    }

    fn is_in_mempool(&self, _txid: Txid) -> Result<bool, BitcoinError> {
        Err(BitcoinError::WalletNotFound)
    }

    fn fee_rate(&self, _txid: Txid) -> Result<SatPerVbyte, BitcoinError> {
        Err(BitcoinError::WalletNotFound)
    }
}
