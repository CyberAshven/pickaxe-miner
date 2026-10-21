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

#[derive(Clone, Debug, Default)]
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
            Phase::Preview => {
                self.view.phase = Phase::Sending;
                Some(Request::Confirm)
            }
            _ => None,
        };
    }
    pub fn cancel(&mut self) {
        if self.view.phase != Phase::Sending {
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
    }
}

/// The same per-pot OS lock covers claims and refunds, including CLI refunds.
/// Keep the file: unlinking a locked inode permits another process to bypass it.
pub(super) fn lock(path: &Path) -> Result<fs::File, String> {
    let path = path.with_extension("lock");
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
}

pub(super) trait Link {
    fn coins(&mut self) -> Result<Vec<Coin>, String>;
    fn relay_fee(&mut self) -> Result<u64, String>;
    fn published(&mut self, txid: &str, raw: &str) -> Result<bool, String>;
    fn broadcast(&mut self, raw: &str) -> Result<String, String>;
}

pub(super) struct Transfers {
    path: PathBuf,
    binding: String,
    approved: Option<Record>,
    quote: Option<(Plan, Instant)>,
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

    /// Called only after the claim journal is drained, on the same supervisor
    /// thread. A broadcast reply alone is never counted as a received transfer.
    pub fn poll(
        &mut self,
        pot: &Pot,
        key: Option<&RecoveryKey>,
        link: &mut dyn Link,
        control: &std::sync::Mutex<Control>,
    ) -> Result<bool, String> {
        let request = control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request
            .take();
        if !self.active() {
            match request {
                Some(Request::Prepare(destination)) => {
                    self.quote = None;
                    let prepared = plan(
                        pot,
                        key.ok_or("owner key unavailable")?,
                        &destination,
                        link.coins()?,
                        link.relay_fee()?,
                    );
                    let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                    if c.view.phase == Phase::Idle {
                        return Ok(false);
                    }
                    match prepared {
                        Ok(plan) => {
                            c.view = View {
                                phase: Phase::Preview,
                                destination: plan.destination.clone(),
                                amount: plan.amount,
                                fee: plan.fee,
                                count: plan.raw.len(),
                                message:
                                    "Enter sends Max; Esc cancels. Quote expires in 60 seconds."
                                        .into(),
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
                    let current = link.coins()?;
                    if link.relay_fee()? > plan.relay_fee
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
                    };
                    self.save(&record)?; // Durable approval before the first network side effect.
                    self.approved = Some(record);
                }
                Some(Request::Cancel) => self.quote = None,
                None => {}
            }
        }
        if self.active() {
            let record = self.approved.as_ref().unwrap();
            let i = record.next;
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
                        i + 1,
                        record.plan.raw.len()
                    ),
                    ..View::default()
                };
            }
            let raw = &record.plan.raw[i];
            let txid = crate::reward::transaction_id(
                &hex::decode(raw).map_err(|_| "invalid saved pot send")?,
            );
            if link.published(&txid, raw)? {
                // Persist advancement only after fetching the identical transaction.
                let mut next = Record {
                    version: 1,
                    binding: record.binding.clone(),
                    plan: record.plan.clone(),
                    next: i + 1,
                };
                self.save(&next)?;
                std::mem::swap(self.approved.as_mut().unwrap(), &mut next);
                if !self.active() {
                    let mut c = control.lock().unwrap_or_else(|e| e.into_inner());
                    c.view.phase = Phase::Complete;
                    c.view.message =
                        "Max/send propagated. Transfers may still be unconfirmed.".into();
                }
            } else {
                if !link.coins()?.contains(&record.plan.coins[i]) {
                    return Err("approved send input is unavailable; retaining its transaction for recovery".into());
                }
                if !link.broadcast(raw)?.eq_ignore_ascii_case(&txid) {
                    return Err(
                        "server returned another send transaction id; journal retained".into(),
                    );
                }
            }
        }
        Ok(self.active() || control.lock().unwrap_or_else(|e| e.into_inner()).holding())
    }
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
        coins: Vec<Coin>,
        rate: u64,
        published: Vec<String>,
        attempts: Vec<String>,
        lost_reply: bool,
    }
    impl Fake {
        fn new(f: &Fixture) -> Self {
            Self {
                coins: f.coins.clone(),
                rate: 1000,
                published: Vec::new(),
                attempts: Vec::new(),
                lost_reply: false,
            }
        }
    }
    impl Link for Fake {
        fn coins(&mut self) -> Result<Vec<Coin>, String> {
            Ok(self.coins.clone())
        }
        fn relay_fee(&mut self) -> Result<u64, String> {
            Ok(self.rate)
        }
        fn published(&mut self, _: &str, raw: &str) -> Result<bool, String> {
            Ok(self.published.iter().any(|s| s == raw))
        }
        fn broadcast(&mut self, raw: &str) -> Result<String, String> {
            self.attempts.push(raw.into());
            self.published.push(raw.into());
            if self.lost_reply {
                return Err("lost reply".into());
            }
            Ok(crate::reward::transaction_id(&hex::decode(raw).unwrap()))
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
        c.lock().unwrap().enter();
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
            c.lock().unwrap().enter();
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
        c.lock().unwrap().enter();
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
        drop(first);
        assert!(lock(&lock_path).is_ok());
    }
}
