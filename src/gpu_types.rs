//! Bounded candidate results shared by GPU backends.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonCudaWinner {
    pub nonce: u32,
    pub digest: [u8; 32],
    /// Explicit signing scalar for incremental search; never a reward-key nonce.
    pub schnorr_k: Option<u64>,
    /// T2 amount offset from the job's base reward, when amount grinding is active.
    pub tail_j: Option<u16>,
    /// BCH value of the miner payout output for the V search coordinate.
    pub tail_value_sats: Option<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonCudaBatchResult {
    pub candidates: u32,
    pub total_winners: u32,
    pub winners: Vec<PhotonCudaWinner>,
}

impl PhotonCudaBatchResult {
    /// Checks whether a reported GPU result exceeds the readback limit.
    pub fn truncated(&self) -> bool {
        self.total_winners as usize > self.winners.len()
    }
}
