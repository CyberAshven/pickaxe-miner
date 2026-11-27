//! One Template Distribution client: SetupConnection, its coinbase output
//! constraints, then this server's templates as the node worker publishes
//! them, transaction data on request, and its solutions relayed to the node.
//! A client that negotiates 0x4C54 is pushed each
//! template's transactions (see `large`).

use super::super::{
    extensions::{
        negotiation_answer, ExtensionMessage, RequestExtensions, LARGE_TEMPLATES, NEGOTIATION,
        REQUEST_EXTENSIONS,
    },
    server::{update_relay_stats, Relay, Shared},
    template::{BchTemplate, Hash},
    transport::{Limits, Receiver, Sender, Session},
    wire::encoded,
};
use super::{convert, large::Pusher, COINBASE_FIXED, MAX_RETAINED, STALE_GRACE};
use std::{
    collections::VecDeque,
    net::TcpStream,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{
        self as common, Protocol, SetupConnection, SetupConnectionError, SetupConnectionSuccess,
    },
    template_distribution_sv2::{
        CoinbaseOutputConstraints, RequestTransactionData, SubmitSolution,
        MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA,
        MESSAGE_TYPE_SUBMIT_SOLUTION,
    },
};

/// How long a client has for SetupConnection, and then for its first
/// CoinbaseOutputConstraints.
const SETUP_DEADLINE: Duration = if cfg!(test) {
    Duration::from_secs(3)
} else {
    Duration::from_secs(10)
};
/// Transaction-data requests a client may make per second.
const REQUESTS_PER_SECOND: usize = 4;
/// A constraints change sends the template again at most this often.
const CONSTRAINTS_INTERVAL: Duration = Duration::from_secs(1);
/// How often a solution Pickaxe's checks refused may still reach the node.
const UNCHECKED_INTERVAL: Duration = Duration::from_secs(10);

struct Retained {
    id: u64,
    template: Arc<BchTemplate>,
    /// When the client was told of a newer parent.
    stale_since: Option<Instant>,
}

/// A client that has sent its constraints.
struct Active {
    reserve: u32,
    /// The node worker's template generation offered last, sent or withheld.
    generation: Option<u64>,
    /// The parent of the client's latest SetNewPrevHash (and
    /// its bits).
    parent: Option<(Hash, u32)>,
    retained: VecDeque<Retained>,
    /// When a constraints change sends the template again.
    resend: Option<Instant>,
    last_constraints: Instant,
    requests: VecDeque<Instant>,
    /// The pushes, once the client negotiated
    /// 0x4C54.
    large: Option<Pusher>,
}

/// Serves one client until it leaves, breaks the protocol or the server
/// stops; the session is closed either way.
pub(in crate::stratum_v2) fn serve(
    stream: TcpStream,
    public: [u8; 32],
    secret: &[u8; 32],
    shared: &Shared,
) -> Result<(), String> {
    let relay = shared
        .relay
        .as_ref()
        .ok_or("template server not configured")?;
    let session = Session::accept_with(stream, &public, secret, Limits::TDP_SERVER)?;
    let (mut sender, mut receiver) = session.split();
    let result = session_loop(&mut sender, &mut receiver, shared, relay);
    sender.close();
    receiver.close();
    result
}

fn session_loop(
    sender: &mut Sender,
    receiver: &mut Receiver,
    shared: &Shared,
    relay: &Relay,
) -> Result<(), String> {
    let Some(mut frame) = next_frame(receiver, shared, Instant::now() + SETUP_DEADLINE)? else {
        return Ok(());
    };
    setup(&mut frame, sender)?;
    // A negotiating client sends
    // RequestExtensions before its constraints. Extension frames are
    // answered or skipped, and the constraints keep one deadline.
    let deadline = Instant::now() + SETUP_DEADLINE;
    let mut large = false;
    let reserve = loop {
        let Some(mut frame) = next_frame(receiver, shared, deadline)? else {
            return Ok(());
        };
        if !extension(&mut frame, sender, &mut large, true)? {
            break constraints(&mut frame)?
                .ok_or("template client sent no coinbase constraints")?;
        }
    };
    let mut active = Active {
        reserve,
        generation: None,
        parent: None,
        retained: VecDeque::new(),
        resend: None,
        last_constraints: Instant::now(),
        requests: VecDeque::new(),
        large: large.then(Pusher::default),
    };
    while !shared.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        active.retained.retain(|old| {
            old.stale_since
                .is_none_or(|since| now < since + STALE_GRACE)
        });
        // What: with 0x4C54 negotiated, transaction data goes out one frame
        // at a time as the socket takes it, never a whole template at once.
        // An answer the client asked for, with any push ahead of it, goes
        // out before anything else is read or offered; otherwise a waiting
        // frame from the client, or a new template, goes before the next
        // frame of a push, and a new template abandons a push not yet
        // finished. Without the extension nothing here runs.
        // Why: Knuth's listener does the same (template_provider.cpp
        // :784-793): a slow client holds up only its own session, and its
        // solutions never wait behind a push.
        // Look here if: a negotiating client's pushes stall, or its
        // solutions or new templates wait behind one.
        if active.answering() {
            active.send_data(sender)?;
            continue;
        }
        let job = shared
            .job
            .read()
            .map(|job| job.clone())
            .map_err(|_| "template state unavailable")?;
        // A new generation is a new template; the same generation again (a
        // lease renewal, or a token set's change) sends nothing. Without a
        // job (the node is down) nothing is sent either: TDP has no revoke,
        // and the client keeps its last template.
        if let Some(job) = job {
            if active.generation != Some(job.generation)
                || active.resend.is_some_and(|at| now >= at)
            {
                active.generation = Some(job.generation);
                active.resend = None;
                active.offer(&job.template, sender, shared, relay)?;
            }
        }
        if active.sending() {
            match receiver.receive(Duration::from_millis(1))? {
                Some(mut frame) => active.handle(&mut frame, sender, shared, relay)?,
                None => active.send_data(sender)?,
            }
            continue;
        }
        if let Some(mut frame) = receiver.receive(Duration::from_millis(100))? {
            active.handle(&mut frame, sender, shared, relay)?;
        }
    }
    Ok(())
}

/// The client's next frame before `deadline`; none once the server stops.
fn next_frame(
    receiver: &mut Receiver,
    shared: &Shared,
    deadline: Instant,
) -> Result<Option<SerializedFrame>, String> {
    while !shared.stop.load(Ordering::Relaxed) {
        if Instant::now() >= deadline {
            return Err("template client too slow to set up".into());
        }
        if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
            return Ok(Some(frame));
        }
    }
    Ok(None)
}

/// SetupConnection for Template Distribution version 2, which defines no
/// flags: anything else is answered with the error, echoing the flags it
/// cannot honor, and the session ends.
fn setup(frame: &mut SerializedFrame, sender: &mut Sender) -> Result<(), String> {
    let header = frame.header();
    if header.msg_type() != common::MESSAGE_TYPE_SETUP_CONNECTION
        || header.channel_msg()
        || header.ext_type_without_channel_msg() != 0
    {
        return Err("template client did not start with SetupConnection".into());
    }
    let setup: SetupConnection =
        binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed setup message")?;
    let error = if setup.protocol != Protocol::TemplateDistributionProtocol {
        Some((0, "unsupported-protocol"))
    } else if setup.min_version > 2 || setup.max_version < 2 {
        Some((0, "protocol-version-mismatch"))
    } else if setup.flags != 0 {
        Some((setup.flags, "unsupported-feature-flags"))
    } else {
        None
    };
    if let Some((flags, code)) = error {
        sender.send(encoded(
            SetupConnectionError {
                flags,
                error_code: code.try_into().map_err(|_| "error code too long")?,
            },
            common::MESSAGE_TYPE_SETUP_CONNECTION_ERROR,
            false,
        )?)?;
        return Err(format!("template client setup refused: {code}"));
    }
    sender.send(encoded(
        SetupConnectionSuccess {
            used_version: 2,
            flags: 0,
        },
        common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
        false,
    )?)
}

/// The coinbase bytes a CoinbaseOutputConstraints frame reserves; none for
/// another message. Sigops are not counted on BCH, so they are ignored.
fn constraints(frame: &mut SerializedFrame) -> Result<Option<u32>, String> {
    if frame.header().msg_type() != MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS {
        return Ok(None);
    }
    let message: CoinbaseOutputConstraints = binary_sv2::from_bytes(frame.payload())
        .map_err(|_| "malformed coinbase output constraints")?;
    Ok(Some(message.coinbase_output_max_additional_size))
}

// What: a frame of a non-zero extension type (the channel bit aside) is an
// extension's, not Template Distribution's. Template Distribution has no
// channel messages, so a frame of extension 0 with the channel bit is an
// error. #### BCH large templates: RequestExtensions (extension 0x0001,
// message 0x00, channel bit clear) before the constraints grants 0x4C54
// when it is asked for, answering RequestExtensions.Success listing only
// it; one without it is answered with RequestExtensions.Error, every
// requested extension unsupported and none required. The latest request
// decides. A malformed request, or one after the constraints, closes the
// session. Once 0x4C54 is granted its messages come only from the server,
// so the client's close the session; any other extension's frame is
// discarded.
// Why: Knuth's provider answers so (template_provider.cpp:383-419), and SV2
// requires frames of an unknown extension meant for this end to be
// discarded and ignored. A negotiating client sends RequestExtensions right
// after SetupConnection.Success and waits for the answer.
// Look here if: a client that asks for extensions gets no templates, is
// pushed data it did not negotiate, or an extension's frame closes a
// template session.
/// Answers or discards an extension's frame: true when the frame was one.
/// Before the constraints (`negotiating`) RequestExtensions is answered and
/// decides `large`, the 0x4C54 grant.
fn extension(
    frame: &mut SerializedFrame,
    sender: &mut Sender,
    large: &mut bool,
    negotiating: bool,
) -> Result<bool, String> {
    let header = frame.header();
    let extension = header.ext_type_without_channel_msg();
    if extension == 0 {
        if header.channel_msg() {
            return Err("unexpected template distribution frame".into());
        }
        return Ok(false);
    }
    if extension == NEGOTIATION && header.msg_type() == REQUEST_EXTENSIONS && !header.channel_msg()
    {
        let request =
            RequestExtensions::decode(frame.payload()).ok_or("malformed extensions request")?;
        if !negotiating {
            return Err("template client asked for extensions after its constraints".into());
        }
        *large = request.requested.contains(&LARGE_TEMPLATES);
        sender.send(negotiation_answer(&request, |extension| {
            extension == LARGE_TEMPLATES
        })?)?;
        return Ok(true);
    }
    if *large && extension == LARGE_TEMPLATES {
        return Err("template client sent a large templates message".into());
    }
    Ok(true)
}

impl Active {
    /// Sends `template` when the client's reserve fits it: on a new parent
    /// (or new bits) as a future template followed by its SetNewPrevHash,
    /// else as a template for the current parent.
    fn offer(
        &mut self,
        template: &Arc<BchTemplate>,
        sender: &mut Sender,
        shared: &Shared,
        relay: &Relay,
    ) -> Result<(), String> {
        // The size budget
        // What: a template goes out only when the largest coinbase the
        // client may build (153 bytes and its reserved output bytes) fits
        // the block beside the template's transactions; otherwise it is
        // withheld and counted, and the next one is tried.
        // Why: the spec forbids sending a template that would make an
        // invalid block once the client's outputs are added.
        // Look here if: a client gets no templates while the dashboard
        // counts withheld ones.
        if COINBASE_FIXED + u64::from(self.reserve) > template.coinbase_budget() {
            shared.template_stats(|stats| stats.withheld = stats.withheld.saturating_add(1));
            return Ok(());
        }
        let id = relay.next_id();
        // New bits on the same parent
        // What: a template whose bits differ from the client's latest
        // SetNewPrevHash on the same parent goes out as a new parent does: a
        // future template and a SetNewPrevHash with the new bits, after which
        // the earlier templates are stale (answered for 10 seconds more).
        // Why: on Chipnet the node switches to the limit's bits 20 minutes
        // after the parent; SetNewPrevHash is the only Template Distribution
        // message that carries bits, so a current template alone left
        // clients mining blocks the node would refuse (bad-diffbits).
        // Look here if: a template client keeps normal bits past Chipnet's
        // 20-minute point, or gets a second SetNewPrevHash on one parent.
        let tip = (template.previous_hash, template.bits);
        let future = self.parent != Some(tip);
        sender.send(convert::new_template(id, future, template)?)?;
        if future {
            sender.send(convert::set_new_prev_hash(id, template)?)?;
            let now = Instant::now();
            for old in &mut self.retained {
                old.stale_since.get_or_insert(now);
            }
            self.parent = Some(tip);
        }
        self.retained.push_back(Retained {
            id,
            template: template.clone(),
            stale_since: None,
        });
        while self.retained.len() > MAX_RETAINED {
            self.retained.pop_front();
        }
        // Its transactions follow, unasked.
        if let Some(pusher) = self.large.as_mut() {
            pusher.push(id, template);
        }
        shared.template_stats(|stats| stats.sent = stats.sent.saturating_add(1));
        Ok(())
    }

    /// An answer the client asked for is still
    /// to be sent.
    fn answering(&self) -> bool {
        self.large.as_ref().is_some_and(Pusher::answering)
    }

    /// Transaction data is still to be sent.
    fn sending(&self) -> bool {
        self.large.as_ref().is_some_and(Pusher::sending)
    }

    /// Sends the next frame of the transaction
    /// data under way, built now from a template the client can still
    /// name.
    fn send_data(&mut self, sender: &mut Sender) -> Result<(), String> {
        let Some(pusher) = self.large.as_mut() else {
            return Ok(());
        };
        let retained = &self.retained;
        let frame = pusher.next_frame(|id| {
            retained
                .iter()
                .find(|old| old.id == id)
                .map(|old| (&*old.template, old.stale_since.is_some()))
        })?;
        match frame {
            Some(frame) => sender.send(frame),
            None => Ok(()),
        }
    }

    fn handle(
        &mut self,
        frame: &mut SerializedFrame,
        sender: &mut Sender,
        shared: &Shared,
        relay: &Relay,
    ) -> Result<(), String> {
        let header = frame.header();
        let mut granted = self.large.is_some();
        if extension(frame, sender, &mut granted, false)? {
            return Ok(());
        }
        match header.msg_type() {
            MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS => {
                let reserve = constraints(frame)?.unwrap_or(self.reserve);
                if reserve != self.reserve {
                    let now = Instant::now();
                    self.reserve = reserve;
                    self.resend = Some((self.last_constraints + CONSTRAINTS_INTERVAL).max(now));
                    self.last_constraints = now;
                }
            }
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA => {
                let request: RequestTransactionData = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "malformed transaction data request")?;
                let now = Instant::now();
                self.requests
                    .retain(|at| now.duration_since(*at) < Duration::from_secs(1));
                if self.requests.len() >= REQUESTS_PER_SECOND {
                    return Err("template client asked for transaction data too often".into());
                }
                self.requests.push_back(now);
                let id = request.template_id;
                // Answered with the whole list in
                // pages after any push under way, never template-too-large.
                if let Some(pusher) = self.large.as_mut() {
                    return pusher.request(id);
                }
                let reply = match self.retained.iter().find(|old| old.id == id) {
                    None => convert::transaction_data_error(id, "template-id-not-found")?,
                    Some(old) if old.stale_since.is_some() => {
                        convert::transaction_data_error(id, "stale-template-id")?
                    }
                    Some(old) => match convert::transaction_data(id, &old.template) {
                        Ok(frame) => frame,
                        Err(code) => convert::transaction_data_error(id, code)?,
                    },
                };
                sender.send(reply)?;
            }
            MESSAGE_TYPE_SUBMIT_SOLUTION => {
                let solution: SubmitSolution =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed solution")?;
                self.solution(&solution, shared, relay)?;
            }
            _ => return Err("unexpected template distribution message".into()),
        }
        Ok(())
    }

    // SubmitSolution → relay journal
    // What: a client's block on one of its templates is assembled, checked,
    // saved in the relay journal (fsync) and the node worker is woken to
    // submit it. One block per parent is kept, as for this server's own. A
    // block the journal cannot save goes to the node once; one that fails
    // Pickaxe's checks goes once per 10 s and is never saved.
    // Why: SRI's pool sends its blocks only through SubmitSolution, which
    // has no reply; the journal is their only durable way to the chain.
    // Look here if: a pool's block is missing on chain.
    fn solution(
        &self,
        solution: &SubmitSolution,
        shared: &Shared,
        relay: &Relay,
    ) -> Result<(), String> {
        shared.template_stats(|stats| stats.solutions = stats.solutions.saturating_add(1));
        let assembled = self
            .retained
            .iter()
            .find(|old| old.id == solution.template_id)
            .map(|old| (old, convert::assemble(&old.template, solution)));
        let Some((retained, Ok(assembled))) = assembled else {
            shared.template_stats(|stats| stats.invalid = stats.invalid.saturating_add(1));
            return Ok(());
        };
        if assembled.refused.is_some() {
            shared.template_stats(|stats| {
                stats.refused_locally = stats.refused_locally.saturating_add(1)
            });
            // The bounded unchecked relay (D24)
            let due = relay.unchecked_at.lock().is_ok_and(|mut at| {
                let due = at.is_none_or(|at| at.elapsed() >= UNCHECKED_INTERVAL);
                if due {
                    *at = Some(Instant::now());
                }
                due
            });
            if due {
                relay.submit_once(assembled.hash(), &assembled.block);
                let _ = shared.wake.try_send(());
            }
            return Ok(());
        }
        let first = relay
            .solved
            .lock()
            .map_err(|_| "relay state unavailable")?
            .first(&retained.template.previous_hash);
        if !first {
            return Ok(());
        }
        let saved = relay
            .journal
            .lock()
            .map_err(|_| "relay journal unavailable".to_owned())
            .and_then(|mut journal| journal.enqueue_relayed(&assembled.block));
        if saved.is_err() {
            shared.template_stats(|stats| stats.unsaved = stats.unsaved.saturating_add(1));
            relay.submit_once(assembled.hash(), &assembled.block);
        }
        update_relay_stats(shared)?;
        let _ = shared.wake.try_send(());
        Ok(())
    }
}
