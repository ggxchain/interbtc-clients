use super::Error;
use crate::delay::RandomDelay;
use async_trait::async_trait;
use bitcoin::{sha256, Hash};
use runtime::{BtcRelayPallet, H256Le, InterBtcParachain, RawBlockHeader};
use std::sync::Arc;

#[async_trait]
pub trait UtxoMonitor {
    /// Submit a block header and wait for inclusion
    ///
    /// # Arguments
    ///
    /// * `header` - Raw block header
    async fn submit_utxo_in_block(
      &self,
      header: Vec<u8>,
      random_delay: Arc<Box<dyn RandomDelay + Send + Sync>>,
  ) -> Result<(), Error>;

  /// Returns true if the block described by the hash
  /// has been stored in the light client
  ///
  /// # Arguments
  ///
  /// * `hash_le` - Hash (little-endian) of the block
  async fn is_utxo_need_stored(&self, hash_le: Vec<u8>, index: u32,) -> Result<bool, Error>;


}

#[async_trait]
impl UtxoMonitor for InterBtcParachain {

  #[tracing::instrument(name = "submit_utxo_in_block", skip(self, header))]
  async fn submit_utxo_in_block(
      &self,
      txid: H256Le,
      index: u32,
      number: Option<T::BlockNumber>,
      random_delay: Arc<Box<dyn RandomDelay + Send + Sync>>,
  ) -> Result<(), Error> {
      let raw_block_header = RawBlockHeader(header.clone());

      // wait a random amount of blocks, to avoid all vaults flooding the parachain with
      // this transaction
      (*random_delay)
          .delay(sha256::Hash::hash(header.as_slice()).as_byte_array())
          .await?;

      if self
          .is_utxo_stored(txid, index)
          .await? != 0
      {
          return Ok(());
      }

      BtcRelayPallet::store_utxo(self, txid, index, number)
          .await
          .map_err(Into::into)
  }

  async fn is_utxo_need_stored(&self, hash_le: Vec<u8>, index: u32,) -> Result<bool, Error> {
    let v = BtcRelayPallet::get_utxo(self, H256Le::from_bytes_le(&hash_le), index).await?;
    Ok(v > 0)
}
}