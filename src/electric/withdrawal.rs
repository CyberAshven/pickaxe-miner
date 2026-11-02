//! #### PR #65: owner-pot Max/send. Quotes reserve the pot in the mining
//! supervisor; approved exact transactions survive shutdown and lost replies.
use super::{build_refund, recovery::RecoveryKey, Coin, Pot};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Editing,
    Preparing,
    Preview,
    Sending,
    Complete,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct View {
    pub enabled: bool,
    pub phase: Phase,
    pub destination: String,
    pub amount: u64,
    pub fee: u64,
    pub count: usize,
    pub message: String,
}

#[derive(Default)]
pub(crate) struct Control {
    pub enabled: bool,
    view: View,
    request: Option<Request>,
    preview_presented: bool,
}

enum Request {
    Prepare(String),
    Confirm,
    Cancel,
}

impl Control {
    pub fn view(&self) -> View {
        View {
            enabled: self.enabled,
            ..self.view.clone()
        }
    }
    pub fn holding(&self) -> bool {
        matches!(
            self.view.phase,
            Phase::Editing | Phase::Preparing | Phase::Preview | Phase::Sending
        )
    }
    pub fn edit(&mut self) {
        if self.enabled && !self.holding() {
            self.preview_presented = false;
            self.view = View {
                phase: Phase::Editing,
                ..View::default()
            };
        }
    }
    pub fn character(&mut self, c: char) {
        if self.view.phase == Phase::Editing
            && (c.is_ascii_graphic() || c == ' ')
            && self.view.destination.len() < 150
        {
            self.view.destination.push(c);
        }
    }
    pub fn paste(&mut self, text: &str) {
        if self.view.phase != Phase::Editing {
            return;
        }
        let text = text.trim();
        if text.len() + self.view.destination.len() > 150
            || !text.bytes().all(|b| b.is_ascii_graphic())
        {
            self.failure(
                "Paste only a BCH address, without a payment URI or extra text.".into(),
                false,
            );
        } else {
            self.view.destination.push_str(text);
        }
    }
    pub fn backspace(&mut self) {
        if self.view.phase == Phase::Editing {
            self.view.destination.pop();
        }
    }
    pub fn enter(&mut self) {
        if self.request.is_some() {
            return;
        }
        self.request = match self.view.phase {
            Phase::Editing => {
                self.view.phase = Phase::Preparing;
                Some(Request::Prepare(self.view.destination.clone()))
            }
            _ => None,
        };
    }
    /// The terminal acknowledges the exact quote only after a successful draw.
    pub fn presented(&mut self, view: &View) {
        if self.view.phase == Phase::Preview && self.view() == *view {
            self.preview_presented = true;
        }
    }
    pub fn confirm(&mut self) {
        if self.view.phase == Phase::Preview && self.preview_presented && self.request.is_none() {
            self.view.phase = Phase::Sending;
            self.request = Some(Request::Confirm);
        }
    }
    pub fn cancel(&mut self) {
        if self.view.phase != Phase::Sending {
            self.preview_presented = false;
            self.view.phase = Phase::Idle;
            self.request = Some(Request::Cancel);
        }
    }
    pub fn failure(&mut self, message: String, approved: bool) {
        self.view.phase = if approved {
            Phase::Sending
        } else {
            Phase::Error
        };
        self.view.message = message;
        self.request = None;
        self.preview_presented = false;
    }
}

/// The same per-pot OS lock covers claims and refunds, including CLI refunds.
/// Keep the file: unlinking a locked inode permits another process to bypass it.
pub(super) fn lock(path: &Path) -> Result<fs::File, String> {
    let path =
        crate::mining_lock::pot_journal_path(path.file_name().ok_or("pot lock has no name")?)?
            .with_extension("lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "cannot create pot journal directory")?;
    }
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.is_file() {
            return Err("invalid pot lock file".into());
        }
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|_| "cannot open pot journal lock")?;
    crate::config::restrict_private_config(&path)?;
    file.try_lock()
        .map_err(|_| "this ELECTRIC pot is already in use; stop its miner before refunding")?;
    Ok(file)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    destination: String,
    relay_fee: u64,
    coins: Vec<Coin>,
    raw: Vec<String>,
    amount: u64,
    fee: u64,
}

fn plan(
    pot: &Pot,
    key: &RecoveryKey,
    destination: &str,
    coins: Vec<Coin>,
    relay_fee: u64,
) -> Result<Plan, String> {
    if !pot.owner_destination {
        return Err(
            "this funded pot has a fixed refund address; use a new owner pot for Max/send".into(),
        );
    }
    let destination = crate::config::validate_coinbase_address(pot.network, destination)?;
    let locking = crate::tx::cashaddr_to_coinbase_locking(&destination)?;
    if locking == pot.locking_bytecode() {
        return Err("choose a destination other than this pot".into());
    }
    if coins.is_empty() {
        return Err("no plain BCH in this pot".into());
    }
    if coins.len() > 256 {
        return Err("Max/send supports at most 256 pot coins at once; no funds were moved".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut result = Plan {
        destination,
        relay_fee,
        coins,
        raw: Vec::new(),
        amount: 0,
        fee: 0,
    };
    for coin in &result.coins {
        if !seen.insert((coin.txid.to_ascii_lowercase(), coin.vout)) {
            return Err("duplicate pot coin".into());
        }
        let built = build_refund(pot, coin, &locking, relay_fee, Some(key.secret()))?;
        if coin.value - built.fee < 3 * (148 + 9 + locking.len() as u64) {
            return Err("a pot coin would produce dust; no funds were moved".into());
        }
        result.amount = result
            .amount
            .checked_add(coin.value - built.fee)
            .ok_or("pot amount overflow")?;
        result.fee = result
            .fee
            .checked_add(built.fee)
            .ok_or("pot fee overflow")?;
        result.raw.push(hex::encode(built.raw));
    }
    // Max never silently drops an uneconomic coin or burns it as extra fee.
    Ok(result)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    binding: String,
    plan: Plan,
    next: usize,
    /// #### PR #65: an attempt to broadcast transfer `next` ended without a
    /// definite refusal (a lost reply, a dropped connection), so the network
    /// may hold it; a later fee refusal elsewhere must not abandon it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    uncertain: bool,
}

pub(super) use super::source::Spender;

pub(super) trait Link {
    /// Every plain-BCH coin the pot can spend now, and whether its
    /// transaction is confirmed.
    fn coins(&mut self) -> Result<Vec<(Coin, bool)>, String>;
    fn relay_fee(&mut self) -> Result<u64, String>;
    fn tip(&mut self) -> Result<u32, String>;
    fn published(&mut self, txid: &str, raw: &str) -> Result<bool, String>;
    fn broadcast(&mut self, raw: &str) -> Result<String, String>;
    /// What spent a pot coin that is no longer spendable.
    fn spender(&mut self, coin: &Coin) -> Result<Spender, String>;
}

/// #### PR #65: a poll that did not finish. Quote, journal and pending-send
/// failures keep the source connection; `source` marks a failed read or
/// broadcast, after which the supervisor may reconnect.
#[derive(Debug)]
pub(super) struct PollError {
    pub message: String,
    pub source: bool,
}

impl From<String> for PollError {
    fn from(message: String) -> Self {
        Self {
            message,
            source: false,
        }
    }
}

impl From<&str> for PollError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn source(message: String) -> PollError {
    PollError {
        message,
        source: true,
    }
}

/// #### PR #65: a broadcast refused for paying too little. The transaction's
/// fee is fixed, so retrying the same bytes cannot succeed.
/// A server's own refusal of a transaction, as opposed to a broadcast whose
/// outcome is unknown (a lost reply or a dropped connection).
fn definite_refusal(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    fee_rejected(error)
        || ["rejected", "bad-txns", "txn-mempool-conflict", "(code "]
            .iter()
            .any(|known| lower.contains(known))
}

fn fee_rejected(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "min relay fee not met",
        "mempool min fee not met",
        "insufficient fee",
        "insufficient priority",
    ]
    .iter()
    .any(|reason| error.contains(reason))
}

/// One step of an approved send.
enum Step {
    /// Waiting for the network; the pot stays reserved.
    Pending,
    /// One more transfer is known to the network.
    Advanced,
    /// The send can no longer complete; its journal is archived and the pot released.
    Abandoned(String),
}

pub(super) struct Transfers {
    path: PathBuf,
    binding: String,
    approved: Option<Record>,
    quote: Option<(Plan, Instant)>,
    /// The last spend search for an unavailable input, per tip and transfer.
    searched: Option<(u32, usize, Spender)>,
}

impl Transfers {
    pub fn open(path: &Path, pot: &Pot, key: Option<&RecoveryKey>) -> Result<Self, String> {
        let binding = format!(
            "{}:{}",
            pot.network.as_str(),
            hex::encode(crate::proof::hash256(&pot.redeem))
        );
        let approved = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("cannot inspect pot send journal".into()),
            Ok(meta) => {
                if !meta.is_file() || meta.len() > 2_000_000 {
                    return Err("invalid pot send journal; preserved".into());
                }
                crate::config::restrict_private_config(path)?;
                let record: Record = serde_json::from_slice(
                    &fs::read(path).map_err(|_| "cannot read pot send journal")?,
                )
                .map_err(|_| "corrupt pot send journal; preserved")?;
                if record.version != 1
                    || record.binding != binding
                    || record.next > record.plan.raw.len()
                {
                    return Err("pot send journal does not match this pot; preserved".into());
                }
                let expected = plan(
                    pot,
                    key.ok_or("pot send journal requires its owner key")?,
                    &record.plan.destination,
                    record.plan.coins.clone(),
                    record.plan.relay_fee,
                )?;
                if serde_json::to_vec(&expected).unwrap()
                    != serde_json::to_vec(&record.plan).unwrap()
                {
                    return Err("saved pot send changed; journal preserved".into());
                }
                Some(record)
            }
        };
        Ok(Self {
            path: path.into(),
            binding,
            approved,
            quote: None,
            searched: None,
        })
    }

    fn save(&self, record: &Record) -> Result<(), String> {
        crate::config::write_private_atomic(
            &self.path,
            &serde_json::to_vec(record).map_err(|_| "cannot encode pot send")?,
        )?;
        #[cfg(unix)]
        fs::File::open(self.path.parent().unwrap_or(Path::new(".")))
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync pot send directory")?;
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.approved
            .as_ref()
            .is_some_and(|r| r.next < r.plan.raw.len())
    }

    /// #### PR #65: keeps the exact approved send beside its journal, named
    /// by its hash like a superseded claim, and releases the pot.
    fn archive(&mut self, label: &str) -> Result<(), String> {
        let record = self.approved.as_ref().ok_or("no approved Max/send")?;
        let bytes = serde_json::to_vec(record).map_err(|_| "cannot encode pot send")?;
        let archive = self.path.with_extension(format!(
            "{}.{label}.json",
            hex::encode(crate::proof::hash256(&bytes))
        ));
        fs::rename(&self.path, &archive)
            .map_err(|_| "cannot archive the pot send journal; it is retained")?;
        #[cfg(unix)]
        fs::File::open(self.path.parent().unwrap_or(Path::new(".")))
            .and_then(|f| f.sync_all())
            .map_err(|_| "cannot sync pot send directory")?;
        self.approved = None;
        self.quote = None;
        self.searched = None;
        Ok(())
    }

    /// What spent transfer `index`'s input, searched once per tip.
    fn spender(
        &mut self,
        link: &mut dyn Link,
        index: usize,
        coin: &Coin,
    ) -> Result<Spender, PollError> {
        let tip = link.tip().map_err(source)?;
        if let Some((at, searched, spender)) = &self.searched {
            if *at == tip && *searched == index {
                return Ok(spender.clone());
            }
        }
        let spender = link.spender(coin).map_err(source)?;
        if spender != Spender::Unspent {
            self.searched = Some((tip, index, spender.clone()));
        }
        Ok(spender)
    }

    /// #### PR #65: checks the next approved transfer, broadcasting it when
    /// `broadcast` allows. The send is abandoned only when it cannot confirm:
    /// its input was spent by another confirmed transaction, or the network
    /// refused its fee while no transfer left to send is in the mempool or chain.
    fn step(&mut self, link: &mut dyn Link, broadcast: bool) -> Result<Step, PollError> {
        let record = self.approved.as_ref().ok_or("no approved Max/send")?;
        let index = record.next;
        let count = record.plan.raw.len();
        let raw = record.plan.raw[index].clone();
        let coin = record.plan.coins[index].clone();
        let txid = crate::reward::transaction_id(
            &hex::decode(&raw).map_err(|_| "invalid saved pot send")?,
        );
        if !link.published(&txid, &raw).map_err(source)? {
            // Each transfer spends exactly its own pot coin.
            if link
                .coins()
                .map_err(source)?
                .iter()
                .any(|(spendable, _)| *spendable == coin)
            {
                if !broadcast {
                    return Ok(Step::Pending);
                }
                // Recorded before sending: the reply can be lost after the
                // network took the transfer.
                let uncertain = record.uncertain;
                if !uncertain {
                    self.mark_uncertain(true)?;
                }
                return match link.broadcast(&raw) {
                    Ok(id) if id.eq_ignore_ascii_case(&txid) => Ok(Step::Pending),
                    Ok(_) => Err(source(
                        "server returned another send transaction id; journal retained".into(),
                    )),
                    Err(error) if definite_refusal(&error) => {
                        if !uncertain {
                            self.mark_uncertain(false)?;
                        }
                        if !fee_rejected(&error) {
                            Err(source(error))
                        } else if uncertain {
                            Err("the network refused an approved transfer's fee after an earlier                                  attempt may have sent it; journal retained until it confirms                                  or its coin is spent elsewhere"
                                .into())
                        } else {
                            self.abandon_unsent(link, index)
                        }
                    }
                    Err(error) => Err(source(error)),
                };
            }
            match self.spender(link, index, &coin)? {
                // Located in a block: the identical transaction confirmed.
                Spender::Confirmed(id) if id.eq_ignore_ascii_case(&txid) => {}
                Spender::Confirmed(_) => {
                    self.archive("superseded")?;
                    return Ok(Step::Abandoned(format!(
                        "A confirmed transaction outside this Max/send spent its pot coin, so \
                         transfer {} of {count} can never confirm. The rest of the send was \
                         abandoned (its journal is kept) and the pot is released; prepare Max \
                         again for a fresh quote.",
                        index + 1
                    )));
                }
                Spender::Mempool(_) => {
                    return Err(
                        "approved send input is spent by an unconfirmed transaction; \
                         waiting until it confirms or leaves the mempool"
                            .into(),
                    )
                }
                Spender::Unspent => return Ok(Step::Pending),
                Spender::Unknown => return Err(
                    "approved send input is unavailable; retaining its transaction for recovery"
                        .into(),
                ),
            }
        }
        // Persist advancement only after fetching or locating the identical transaction.
        let record = self.approved.as_ref().ok_or("no approved Max/send")?;
        let next = Record {
            version: 1,
            binding: record.binding.clone(),
            plan: record.plan.clone(),
            next: index + 1,
            uncertain: false,
        };
        self.save(&next)?;
        self.approved = Some(next);
        self.searched = None;
        Ok(Step::Advanced)
    }

    /// Saves whether transfer `next` may already be on the network.
    fn mark_uncertain(&mut self, uncertain: bool) -> Result<(), PollError> {
        let record = self.approved.as_mut().ok_or("no approved Max/send")?;
        record.uncertain = uncertain;
        let record = self.approved.as_ref().ok_or("no approved Max/send")?;
        self.save(record)?;
        Ok(())
    }

    /// The network refused transfer `index`'s fee. Its input is still
    /// spendable, so it is in neither the mempool nor the chain; abandon the
    /// send only if no later transfer reached them either.
    fn abandon_unsent(&mut self, link: &mut dyn Link, index: usize) -> Result<Step, PollError> {
        let plan = self
            .approved
            .as_ref()
            .ok_or("no approved Max/send")?
            .plan
            .clone();
        for raw in &plan.raw[index + 1..] {
            let txid = crate::reward::transaction_id(
                &hex::decode(raw).map_err(|_| "invalid saved pot send")?,
            );
            if link.published(&txid, raw).map_err(source)? {
                return Err(
                    "the network refused an approved transfer's fee while a later one is known; \
                     journal retained"
                        .into(),
                );
            }
        }
        self.archive("abandoned")?;
        Ok(Step::Abandoned(format!(
            "The network refused this Max/send's fee. None of its {} unsent transfer(s) is in \
             the mempool or chain, so the send was abandoned (its journal is kept) and the pot is \
             released; prepare Max again for a fresh quote.",
            plan.raw.len() - index
        )))
    }

    /// #### PR #65: settles an approved send without broadcasting, for a
    /// refund: records propagated transfers and abandons one that can no
    /// longer confirm. Returns what happened when the send ended.
    pub fn settle(&mut self, link: &mut dyn Link) -> Result<Option<String>, PollError> {
        while self.active() {
            match self.step(link, false)? {
                Step::Pending => return Ok(None),
                Step::Advanced => {}
                Step::Abandoned(message) => return Ok(Some(message)),
            }
        }
        Ok(Some("The approved Max/send has propagated.".into()))
    }

    /// Called only after the claim journal is drained, on the same supervisor
    /// thread. A broadcast reply alone is never counted as a received transfer.
    pub fn poll(
        &mut self,
        pot: &Pot,
        key: Option<&RecoveryKey>,
        link: &mut dyn Link,
        control: &std::sync::Mutex<Control>,
    ) -> Result<bool, PollError> {
        let request = control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request
            .take();
        if !self.active() {
            match request {
                Some(Request::Prepare(destination)) => {
                    self.quote = None;
                    let key = key.ok_or("owner key unavailable")?;
                    let coins = link.coins().map_err(source)?;
                    let relay_fee = link.relay_fee().map_err(source)?;
                    // #### PR #65: an unconfirmed coin can vanish with its
                    // parent, so quotes spend confirmed coins only.
                    let waiting: u64 = coins
                        .iter()
                        .filter(|(_, confirmed)| !confirmed)
                        .map(|(coin, _)| coin.value)
                        .sum();
                    let confirmed = confirmed_coins(coins);
                    let prepared = if confirmed.is_empty() && waiting > 0 {
                        Err(format!(
                            "no confirmed plain BCH in this pot yet; {waiting} sats await a \
                             confirmation"
                        ))
                    } else {
                        plan(pot, key, &destination, confirmed, relay_fee)
                    };
                    let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                    if c.view.phase == Phase::Idle {
                        return Ok(false);
                    }
                    match prepared {
                        Ok(plan) => {
                            c.preview_presented = false;
                            c.view = View {
                                phase: Phase::Preview,
                                destination: plan.destination.clone(),
                                amount: plan.amount,
                                fee: plan.fee,
                                count: plan.raw.len(),
                                message: if waiting > 0 {
                                    format!(
                                        "Y sends Max; Esc cancels. Quote expires in 60 seconds. \
                                         {waiting} unconfirmed sats are not included."
                                    )
                                } else {
                                    "Y sends Max; Esc cancels. Quote expires in 60 seconds.".into()
                                },
                                ..View::default()
                            };
                            self.quote = Some((plan, Instant::now()));
                        }
                        Err(error) => {
                            c.view.phase = Phase::Error;
                            c.view.message = error;
                        }
                    }
                }
                Some(Request::Confirm) => {
                    let (plan, time) = self.quote.take().ok_or("no current Max/send quote")?;
                    if time.elapsed() > Duration::from_secs(60) {
                        return Err("Max/send quote expired; prepare it again".into());
                    }
                    let current = confirmed_coins(link.coins().map_err(source)?);
                    if link.relay_fee().map_err(source)? > plan.relay_fee
                        || current.len() != plan.coins.len()
                        || !plan.coins.iter().all(|coin| current.contains(coin))
                    {
                        return Err("pot balance or relay fee changed; prepare Max again".into());
                    }
                    let record = Record {
                        version: 1,
                        binding: self.binding.clone(),
                        plan,
                        next: 0,
                        uncertain: false,
                    };
                    self.save(&record)?; // Durable approval before the first network side effect.
                    self.approved = Some(record);
                    self.searched = None;
                }
                Some(Request::Cancel) => self.quote = None,
                None => {}
            }
        }
        if self.active() {
            let record = self.approved.as_ref().unwrap();
            {
                let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                c.view = View {
                    phase: Phase::Sending,
                    destination: record.plan.destination.clone(),
                    amount: record.plan.amount,
                    fee: record.plan.fee,
                    count: record.plan.raw.len(),
                    message: format!(
                        "Checking transfer {} of {}; approved sends survive restart.",
                        record.next + 1,
                        record.plan.raw.len()
                    ),
                    ..View::default()
                };
            }
            match self.step(link, true)? {
                Step::Pending => {}
                Step::Advanced => {
                    if !self.active() {
                        let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                        c.view.phase = Phase::Complete;
                        c.view.message =
                            "Max/send propagated. Transfers may still be unconfirmed.".into();
                    }
                }
                Step::Abandoned(message) => {
                    let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                    c.view.phase = Phase::Error;
                    c.view.message = message;
                    c.request = None;
                    c.preview_presented = false;
                }
            }
        }
        Ok(self.active() || control.lock().unwrap_or_else(|e| e.into_inner()).holding())
    }
}

fn confirmed_coins(coins: Vec<(Coin, bool)>) -> Vec<Coin> {
    coins
        .into_iter()
        .filter_map(|(coin, confirmed)| confirmed.then_some(coin))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiningNetwork;
    use std::sync::Mutex;

    struct Fixture {
        dir: PathBuf,
        key: RecoveryKey,
        pot: Pot,
        destination: String,
        coins: Vec<Coin>,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("pickaxe-owner-send-{:016x}", rand::random::<u64>()));
            fs::create_dir_all(&dir).unwrap();
            let key = RecoveryKey::load(&dir.join("owner.pot-key"), true).unwrap();
            let destination =
                crate::tx::cashaddr_with_version(0, &[0x33; 20], MiningNetwork::Mainnet);
            let locks = [0x11, 0x22, 0x33].map(|b| {
                crate::tx::cashaddr_to_p2pkh_locking(&crate::tx::cashaddr_with_version(
                    0,
                    &[b; 20],
                    MiningNetwork::Mainnet,
                ))
                .unwrap()
            });
            let pot = Pot::with_owner_destination(
                &super::super::Deployment::mainnet(),
                &locks,
                key.public(),
            )
            .unwrap();
            Self {
                dir,
                key,
                pot,
                destination,
                coins: vec![
                    Coin {
                        txid: "12".repeat(32),
                        vout: 0,
                        value: 20_000,
                    },
                    Coin {
                        txid: "34".repeat(32),
                        vout: 1,
                        value: 15_000,
                    },
                ],
            }
        }
        fn path(&self) -> PathBuf {
            self.dir.join("send.json")
        }
        fn transfers(&self) -> Transfers {
            Transfers::open(&self.path(), &self.pot, Some(&self.key)).unwrap()
        }
        fn control(&self) -> Mutex<Control> {
            let mut c = Control {
                enabled: true,
                ..Default::default()
            };
            c.edit();
            for ch in self.destination.chars() {
                c.character(ch);
            }
            c.enter();
            Mutex::new(c)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
    struct Fake {
        /// Confirmed, spendable pot coins.
        coins: Vec<Coin>,
        rate: u64,
        published: Vec<String>,
        attempts: Vec<String>,
        lost_reply: bool,
        // #### PR #65
        unconfirmed: Vec<Coin>,
        tip: u32,
        spenders: Vec<(Coin, Spender)>,
        searches: usize,
        reject: Option<String>,
        offline: bool,
    }
    impl Fake {
        fn new(f: &Fixture) -> Self {
            Self {
                coins: f.coins.clone(),
                rate: 1000,
                published: Vec::new(),
                attempts: Vec::new(),
                lost_reply: false,
                unconfirmed: Vec::new(),
                tip: 100,
                spenders: Vec::new(),
                searches: 0,
                reject: None,
                offline: false,
            }
        }
        /// Another transaction spends `coin`: it leaves the spendable set.
        fn spent(&mut self, coin: &Coin, by: Spender) {
            self.coins.retain(|c| c != coin);
            self.spenders.retain(|(c, _)| c != coin);
            self.spenders.push((coin.clone(), by));
        }
    }
    fn approve(c: &Mutex<Control>) {
        let mut c = c.lock().unwrap();
        let view = c.view();
        c.presented(&view);
        c.confirm();
    }
    impl Link for Fake {
        fn coins(&mut self) -> Result<Vec<(Coin, bool)>, String> {
            if self.offline {
                return Err("connection reset".into());
            }
            Ok(self
                .coins
                .iter()
                .map(|coin| (coin.clone(), true))
                .chain(self.unconfirmed.iter().map(|coin| (coin.clone(), false)))
                .collect())
        }
        fn relay_fee(&mut self) -> Result<u64, String> {
            Ok(self.rate)
        }
        fn tip(&mut self) -> Result<u32, String> {
            Ok(self.tip)
        }
        fn published(&mut self, _: &str, raw: &str) -> Result<bool, String> {
            Ok(self.published.iter().any(|s| s == raw))
        }
        fn broadcast(&mut self, raw: &str) -> Result<String, String> {
            self.attempts.push(raw.into());
            if let Some(reason) = &self.reject {
                return Err(reason.clone());
            }
            self.published.push(raw.into());
            if self.lost_reply {
                return Err("lost reply".into());
            }
            Ok(crate::reward::transaction_id(&hex::decode(raw).unwrap()))
        }
        fn spender(&mut self, coin: &Coin) -> Result<Spender, String> {
            self.searches += 1;
            Ok(self
                .spenders
                .iter()
                .find(|(c, _)| c == coin)
                .map(|(_, by)| by.clone())
                .unwrap_or(Spender::Unknown))
        }
    }

    #[test]
    fn max_send_is_confirmed_durable_and_recovers_a_lost_reply_and_partial_progress() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        assert!(txs.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap());
        assert_eq!(c.lock().unwrap().view.phase, Phase::Preview);
        let view = c.lock().unwrap().view();
        assert_eq!(view.amount + view.fee, 35_000);
        assert_eq!(view.count, 2);
        assert!(link.attempts.is_empty());
        assert!(!f.path().exists());
        // Holding/repeating Enter cannot accept the quote, nor can an unseen
        // quote be accepted by the distinct confirmation key.
        c.lock().unwrap().enter();
        c.lock().unwrap().enter();
        c.lock().unwrap().confirm();
        assert_eq!(c.lock().unwrap().view.phase, Phase::Preview);
        assert!(!f.path().exists());
        approve(&c);
        c.lock().unwrap().cancel(); // Confirmation already submitted: cannot discard it.
        link.lost_reply = true;
        assert!(txs.poll(&f.pot, Some(&f.key), &mut link, &c).is_err());
        assert!(f.path().exists());
        assert!(f.transfers().active());
        drop(txs);
        link.lost_reply = false;
        let mut resumed = f.transfers();
        // Recovery fetches the exact accepted bytes instead of sending again.
        assert!(resumed.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap());
        assert_eq!(link.attempts.len(), 1);
        drop(resumed);
        let mut resumed = f.transfers();
        assert!(resumed.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap());
        assert!(!resumed.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap());
        assert_eq!(link.attempts.len(), 2);
        assert_eq!(c.lock().unwrap().view.phase, Phase::Complete);
        assert!(!f.transfers().active());
    }

    #[test]
    fn changed_or_expired_quotes_never_send_and_cancellation_releases_the_pot() {
        let f = Fixture::new();
        for mode in [0, 1, 2, 3] {
            let c = f.control();
            let mut txs = f.transfers();
            let mut link = Fake::new(&f);
            txs.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap();
            match mode {
                0 => link.rate = 1100,
                1 => link.coins[0].value -= 1,
                2 => txs.quote.as_mut().unwrap().1 -= Duration::from_secs(61),
                _ => {
                    c.lock().unwrap().cancel();
                    assert!(!txs.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap());
                    continue;
                }
            }
            approve(&c);
            assert!(txs.poll(&f.pot, Some(&f.key), &mut link, &c).is_err());
            assert!(link.attempts.is_empty());
            assert!(!f.path().exists());
        }
    }

    #[test]
    fn network_key_dust_duplicates_and_modified_journals_fail_closed() {
        let f = Fixture::new();
        let wrong_network =
            crate::tx::cashaddr_with_version(0, &[0x33; 20], MiningNetwork::Chipnet);
        assert!(plan(&f.pot, &f.key, &wrong_network, f.coins.clone(), 1000).is_err());
        assert!(plan(&f.pot, &f.key, &f.pot.address(), f.coins.clone(), 1000).is_err());
        let duplicate = vec![f.coins[0].clone(), f.coins[0].clone()];
        assert!(plan(&f.pot, &f.key, &f.destination, duplicate, 1000).is_err());
        let dust = vec![Coin {
            value: 500,
            ..f.coins[0].clone()
        }];
        assert!(plan(&f.pot, &f.key, &f.destination, dust, 1000).is_err());
        assert!(plan(&f.pot, &f.key, &f.destination, f.coins.clone(), 100_000).is_err());
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        txs.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap();
        approve(&c);
        txs.poll(&f.pot, Some(&f.key), &mut link, &c).unwrap();
        let original = fs::read(f.path()).unwrap();
        let mut bad: serde_json::Value = serde_json::from_slice(&original).unwrap();
        bad["plan"]["amount"] = 1.into();
        let corrupt = serde_json::to_vec(&bad).unwrap();
        fs::write(f.path(), &corrupt).unwrap();
        assert!(Transfers::open(&f.path(), &f.pot, Some(&f.key)).is_err());
        assert_eq!(fs::read(f.path()).unwrap(), corrupt);
        fs::write(f.path(), &original).unwrap();
        let other = RecoveryKey::load(&f.dir.join("other.pot-key"), true).unwrap();
        assert!(Transfers::open(&f.path(), &f.pot, Some(&other)).is_err());
        assert!(Transfers::open(&f.path(), &f.pot, None).is_err());
        let mut elsewhere = f.pot.clone();
        elsewhere.network = MiningNetwork::Chipnet;
        assert!(Transfers::open(&f.path(), &elsewhere, Some(&f.key)).is_err());
        assert_eq!(fs::read(f.path()).unwrap(), original);
        let lock_path = f.dir.join("claim.json");
        let first = lock(&lock_path).unwrap();
        assert!(lock(&lock_path).is_err());
        assert!(lock(&f.dir.join("another-config/claim.json")).is_err());
        drop(first);
        assert!(lock(&lock_path).is_ok());
    }

    // #### PR #65
    fn poll(
        txs: &mut Transfers,
        f: &Fixture,
        link: &mut Fake,
        c: &Mutex<Control>,
    ) -> Result<bool, PollError> {
        txs.poll(&f.pot, Some(&f.key), link, c)
    }

    fn archived(f: &Fixture, label: &str) -> Vec<PathBuf> {
        fs::read_dir(&f.dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with(&format!(".{label}.json")))
            .collect()
    }

    #[test]
    fn quotes_spend_only_confirmed_coins() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        link.unconfirmed.push(Coin {
            txid: "56".repeat(32),
            vout: 2,
            value: 40_000,
        });
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        let view = c.lock().unwrap().view();
        assert_eq!(view.phase, Phase::Preview);
        assert_eq!(view.amount + view.fee, 35_000);
        assert_eq!(view.count, 2);
        assert!(view
            .message
            .contains("40000 unconfirmed sats are not included"));
        // The coin confirming after the quote changes the balance: quote again.
        approve(&c);
        let confirmed = link.unconfirmed.remove(0);
        link.coins.push(confirmed);
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        assert!(link.attempts.is_empty() && !f.path().exists());
        // Unconfirmed funding alone is never quoted.
        link.unconfirmed = std::mem::take(&mut link.coins);
        let c = f.control();
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap());
        let view = c.lock().unwrap().view();
        assert_eq!(view.phase, Phase::Error);
        assert!(
            view.message.contains("no confirmed plain BCH"),
            "{}",
            view.message
        );
        assert!(txs.quote.is_none() && link.attempts.is_empty() && !f.path().exists());
    }

    #[test]
    fn only_a_confirmed_conflicting_spend_abandons_the_send_and_releases_the_pot() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        approve(&c);
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        assert_eq!(txs.approved.as_ref().unwrap().next, 1);
        let first = f.coins[0].clone();
        link.coins.retain(|coin| *coin != first); // Spent by our own first transfer.
        let second = f.coins[1].clone();
        let other = "ab".repeat(32);
        // An unconfirmed conflict can still be evicted, and an unexplained
        // missing input may still return: both keep the pot reserved.
        link.spent(&second, Spender::Mempool(other.clone()));
        for _ in 0..2 {
            let error = poll(&mut txs, &f, &mut link, &c).unwrap_err();
            assert!(!error.source, "{}", error.message);
            assert!(txs.active() && f.path().exists());
        }
        assert_eq!(link.searches, 1, "one search per tip");
        link.tip += 1;
        link.spent(&second, Spender::Unknown);
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        assert_eq!(link.searches, 2);
        assert!(f.transfers().active());
        let journal = fs::read(f.path()).unwrap();
        link.tip += 1;
        link.spent(&second, Spender::Confirmed(other));
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap());
        assert!(!txs.active() && !f.path().exists());
        assert!(!f.transfers().active());
        let view = c.lock().unwrap().view();
        assert_eq!(view.phase, Phase::Error);
        assert!(
            view.message.contains("can never confirm"),
            "{}",
            view.message
        );
        assert!(!c.lock().unwrap().holding());
        let kept = archived(&f, "superseded");
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read(&kept[0]).unwrap(), journal);
        assert_eq!(link.attempts.len(), 1);
        // A stale approval cannot send: only a fresh quote and Y can.
        c.lock().unwrap().request = Some(Request::Confirm);
        assert!(poll(&mut txs, &f, &mut link, &c).is_err());
        let fresh = Coin {
            txid: "78".repeat(32),
            vout: 0,
            value: 30_000,
        };
        link.coins.push(fresh.clone());
        let c = f.control();
        poll(&mut txs, &f, &mut link, &c).unwrap();
        assert_eq!(c.lock().unwrap().view.phase, Phase::Preview);
        assert_eq!(c.lock().unwrap().view.count, 1);
        approve(&c);
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        assert_eq!(txs.approved.as_ref().unwrap().plan.coins, vec![fresh]);
        assert_eq!(link.attempts.len(), 2);
    }

    #[test]
    fn our_transfer_located_in_a_block_counts_as_propagated() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        approve(&c);
        link.lost_reply = true;
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        link.lost_reply = false;
        // A source without a transaction index no longer finds it by id.
        let raw = link.published.remove(0);
        let ours = crate::reward::transaction_id(&hex::decode(&raw).unwrap());
        link.spent(&f.coins[0].clone(), Spender::Confirmed(ours.to_uppercase()));
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        assert_eq!(txs.approved.as_ref().unwrap().next, 1);
        assert!(archived(&f, "superseded").is_empty());
        assert_eq!(link.attempts.len(), 1);
    }

    #[test]
    fn a_fee_refusal_never_abandons_a_transfer_an_earlier_attempt_may_have_sent() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        approve(&c);
        // The first server takes the transfer but the reply is lost, and
        // the connection fails over to a server with a higher fee floor.
        link.lost_reply = true;
        assert!(poll(&mut txs, &f, &mut link, &c).is_err());
        assert!(txs.approved.as_ref().unwrap().uncertain);
        link.lost_reply = false;
        link.published.clear();
        link.reject = Some("min relay fee not met, 200 < 226 (code 66)".into());
        for _ in 0..3 {
            let error = poll(&mut txs, &f, &mut link, &c).unwrap_err();
            assert!(!error.source);
            assert!(txs.active() && f.path().exists());
        }
        assert!(archived(&f, "abandoned").is_empty());
        // The journal remembers the attempt across a restart.
        let reopened = f.transfers();
        assert!(reopened.approved.as_ref().unwrap().uncertain);
    }

    #[test]
    fn a_fee_refusal_abandons_only_a_send_with_nothing_in_the_mempool_or_chain() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        approve(&c);
        // Any other refusal keeps the approved send for retry.
        link.reject = Some("txn-mempool-conflict (code 18)".into());
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        assert!(txs.active() && f.path().exists());
        let refusal = "the transaction was rejected by network rules.\n\n\
                       min relay fee not met, 200 < 226 (code 66)";
        link.reject = Some(refusal.into());
        // Part of the send is known: never abandoned.
        let later = txs.approved.as_ref().unwrap().plan.raw[1].clone();
        link.published.push(later.clone());
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        assert!(txs.active() && f.path().exists());
        link.published.clear();
        let journal = fs::read(f.path()).unwrap();
        assert!(!poll(&mut txs, &f, &mut link, &c).unwrap());
        assert!(!txs.active() && !f.path().exists() && !f.transfers().active());
        let view = c.lock().unwrap().view();
        assert_eq!(view.phase, Phase::Error);
        assert!(
            view.message.contains("refused this Max/send's fee"),
            "{}",
            view.message
        );
        let kept = archived(&f, "abandoned");
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read(&kept[0]).unwrap(), journal);
        assert!(link.published.is_empty());
        // The approval does not carry over; a new quote and Y send again.
        c.lock().unwrap().request = Some(Request::Confirm);
        assert!(poll(&mut txs, &f, &mut link, &c).is_err());
        link.reject = None;
        link.rate = 2000;
        let c = f.control();
        poll(&mut txs, &f, &mut link, &c).unwrap();
        assert_eq!(c.lock().unwrap().view.phase, Phase::Preview);
        approve(&c);
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap());
        assert_eq!(txs.approved.as_ref().unwrap().plan.relay_fee, 2000);
        assert_eq!(link.published.len(), 1);
        for reason in [
            "66: mempool min fee not met",
            "insufficient priority",
            "Insufficient fee",
        ] {
            assert!(fee_rejected(reason));
        }
        for reason in ["txn-mempool-conflict", "bad-txns-inputs-missingorspent", ""] {
            assert!(!fee_rejected(reason));
        }
    }

    #[test]
    fn quote_errors_keep_the_source_connection_and_read_failures_do_not() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        txs.quote.as_mut().unwrap().1 -= Duration::from_secs(61);
        approve(&c);
        let error = poll(&mut txs, &f, &mut link, &c).unwrap_err();
        assert!(!error.source && error.message.contains("expired"));
        c.lock().unwrap().request = Some(Request::Confirm);
        let error = poll(&mut txs, &f, &mut link, &c).unwrap_err();
        assert!(!error.source && error.message.contains("no current"));
        let c = f.control();
        link.offline = true;
        assert!(poll(&mut txs, &f, &mut link, &c).unwrap_err().source);
        assert!(link.attempts.is_empty());
    }

    #[test]
    fn refund_settles_a_send_without_broadcasting() {
        let f = Fixture::new();
        let c = f.control();
        let mut txs = f.transfers();
        let mut link = Fake::new(&f);
        poll(&mut txs, &f, &mut link, &c).unwrap();
        approve(&c);
        link.lost_reply = true;
        assert!(poll(&mut txs, &f, &mut link, &c).is_err());
        link.lost_reply = false;
        drop(txs);
        // The first transfer propagated; the second is still spendable, so
        // the refund waits for the TUI to send it.
        let mut refund = f.transfers();
        assert_eq!(refund.settle(&mut link).unwrap(), None);
        assert!(refund.active());
        assert_eq!(refund.approved.as_ref().unwrap().next, 1);
        assert_eq!(link.attempts.len(), 1);
        link.spent(&f.coins[1].clone(), Spender::Confirmed("cd".repeat(32)));
        let mut refund = f.transfers();
        let ended = refund.settle(&mut link).unwrap().unwrap();
        assert!(ended.contains("can never confirm"), "{ended}");
        assert!(!refund.active() && !f.path().exists());
        assert_eq!(archived(&f, "superseded").len(), 1);
        assert_eq!(link.attempts.len(), 1);
    }
}
