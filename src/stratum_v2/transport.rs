//! #### PR #38
//! Authenticated SV2 transport shared by ASIC channels and rig connections.
//! Use reference Noise/framing, pin the authority, bound frame bytes and time,
//! and destroy both directions after any authentication or partial-I/O error.

use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    time::{Duration, Instant},
};
use stratum_core::{
    codec_sv2::{
        Decrypted, EncodableFrame, Handshake, NoiseDecoder, NoiseEncoder, SerializedFrame,
        Transport, TransportDecryptState, TransportEncryptState,
    },
    noise_sv2::{
        Initiator, Responder, ELLSWIFT_ENCODING_SIZE, INITIATOR_EXPECTED_HANDSHAKE_MESSAGE_SIZE,
    },
};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_WIRE_BYTES: usize = MAX_FRAME_BYTES + 1024;

pub struct Session {
    reader: Receiver,
    writer: Sender,
}

pub struct Sender {
    stream: TcpStream,
    encoder: NoiseEncoder,
    state: Option<TransportEncryptState>,
}

pub struct Receiver {
    stream: TcpStream,
    decoder: NoiseDecoder,
    state: Option<TransportDecryptState>,
}

impl Session {
    pub fn initiate(mut stream: TcpStream, authority: [u8; 32]) -> Result<Self, String> {
        prepare(&stream)?;
        let result = (|| {
            let initiator =
                Initiator::from_raw_k(authority).map_err(|_| "invalid SV2 authority key")?;
            let (message, handshake) = Handshake::initiator(initiator)
                .step_0()
                .map_err(|_| "SV2 handshake initialization failed")?;
            write_until(&mut stream, message.payload(), Instant::now() + IO_TIMEOUT)?;
            let mut reply = [0; INITIATOR_EXPECTED_HANDSHAKE_MESSAGE_SIZE];
            read_until(&mut stream, &mut reply, Instant::now() + IO_TIMEOUT)?;
            handshake
                .step_2(reply)
                .map_err(|_| "SV2 authority authentication failed".to_owned())
        })();
        match result {
            Ok(state) => Self::from_transport(stream, state),
            Err(error) => {
                let _ = stream.shutdown(Shutdown::Both);
                Err(error)
            }
        }
    }

    pub fn accept(
        mut stream: TcpStream,
        authority: &[u8; 32],
        secret: &[u8; 32],
    ) -> Result<Self, String> {
        prepare(&stream)?;
        let result = (|| {
            let responder =
                Responder::from_authority_kp(authority, secret, Duration::from_secs(3600))
                    .map_err(|_| "invalid SV2 authority key pair")?;
            let mut first = [0; ELLSWIFT_ENCODING_SIZE];
            read_until(&mut stream, &mut first, Instant::now() + IO_TIMEOUT)?;
            let (reply, state) = Handshake::responder(responder)
                .step_1(first)
                .map_err(|_| "SV2 handshake failed")?;
            write_until(&mut stream, reply.payload(), Instant::now() + IO_TIMEOUT)?;
            Ok(state)
        })();
        match result {
            Ok(state) => Self::from_transport(stream, state),
            Err(error) => {
                let _ = stream.shutdown(Shutdown::Both);
                Err(error)
            }
        }
    }

    fn from_transport(stream: TcpStream, state: Transport) -> Result<Self, String> {
        let write_stream = stream.try_clone().map_err(|_| "cannot split SV2 socket")?;
        let (encrypt, decrypt) = state.split();
        Ok(Self {
            reader: Receiver {
                stream,
                decoder: NoiseDecoder::new(),
                state: Some(decrypt),
            },
            writer: Sender {
                stream: write_stream,
                encoder: NoiseEncoder::new(),
                state: Some(encrypt),
            },
        })
    }

    pub fn split(self) -> (Sender, Receiver) {
        (self.writer, self.reader)
    }
}

impl Sender {
    pub fn send(&mut self, frame: impl EncodableFrame) -> Result<(), String> {
        let result = (|| {
            if frame.encoded_length() > MAX_FRAME_BYTES {
                return Err("SV2 frame exceeds size limit".into());
            }
            let state = self.state.as_mut().ok_or("SV2 connection is closed")?;
            let bytes = self
                .encoder
                .encode_transport(frame, state)
                .map_err(|_| "SV2 frame encoding failed")?;
            write_until(
                &mut self.stream,
                bytes.as_ref(),
                Instant::now() + IO_TIMEOUT,
            )
        })();
        if result.is_err() {
            self.close();
        }
        result
    }

    pub fn close(&mut self) {
        self.state = None;
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Receiver {
    /// Idle polling preserves cipher state. Once even one byte arrives, the
    /// complete frame must arrive by the deadline or the connection is closed.
    pub fn receive(&mut self, idle: Duration) -> Result<Option<SerializedFrame>, String> {
        let result = self.receive_inner(idle);
        if result.is_err() {
            self.close();
        }
        result
    }

    fn receive_inner(&mut self, idle: Duration) -> Result<Option<SerializedFrame>, String> {
        if self.state.is_none() {
            return Err("SV2 connection is closed".into());
        }
        self.stream
            .set_read_timeout(Some(idle))
            .map_err(|_| "invalid SV2 idle timeout")?;
        let mut first = [0];
        match self.stream.peek(&mut first) {
            Ok(0) => return Err("SV2 peer disconnected".into()),
            Ok(_) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None)
            }
            Err(_) => return Err("SV2 receive failed".into()),
        }
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut bytes_read = 0usize;
        loop {
            let len = self.decoder.writable_len();
            bytes_read = bytes_read
                .checked_add(len)
                .ok_or("SV2 frame size overflow")?;
            if bytes_read > MAX_WIRE_BYTES {
                return Err("SV2 frame exceeds size limit".into());
            }
            read_until(&mut self.stream, self.decoder.writable(), deadline)?;
            let state = self.state.take().ok_or("SV2 connection is closed")?;
            match self
                .decoder
                .next_transport_frame(state)
                .map_err(|_| "SV2 authentication or framing failed")?
            {
                Decrypted::Frame(frame, state) => {
                    if frame.encoded_length() > MAX_FRAME_BYTES {
                        return Err("SV2 frame exceeds size limit".into());
                    }
                    self.state = Some(state);
                    return Ok(Some(frame));
                }
                Decrypted::Incomplete(_, state) => self.state = Some(state),
            }
        }
    }

    pub fn close(&mut self) {
        self.state = None;
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn prepare(stream: &TcpStream) -> Result<(), String> {
    stream
        .set_nodelay(true)
        .map_err(|_| "cannot configure SV2 socket".to_owned())
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "SV2 frame deadline exceeded".into())
}

fn read_until(
    stream: &mut TcpStream,
    mut output: &mut [u8],
    deadline: Instant,
) -> Result<(), String> {
    while !output.is_empty() {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| "cannot set SV2 read timeout")?;
        match stream.read(output) {
            Ok(0) => return Err("SV2 peer disconnected during frame".into()),
            Ok(n) => output = &mut output[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => return Err("SV2 frame read failed or timed out".into()),
        }
    }
    Ok(())
}

fn write_until(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> Result<(), String> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline)?))
            .map_err(|_| "cannot set SV2 write timeout")?;
        match stream.write(bytes) {
            Ok(0) => return Err("SV2 peer disconnected during write".into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(_) => return Err("SV2 frame write failed or timed out".into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, thread};
    use stratum_core::{
        binary_sv2,
        bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey},
        codec_sv2::MessageFrame,
        common_messages_sv2::SetupConnectionSuccess,
    };

    fn authority(seed: u8) -> ([u8; 32], [u8; 32]) {
        let secret = [seed; 32];
        let key = SecretKey::from_slice(&secret).unwrap();
        let pair = Keypair::from_secret_key(&Secp256k1::new(), &key);
        (pair.x_only_public_key().0.serialize(), secret)
    }
    fn pair() -> (Session, Session) {
        let (public, secret) = authority(42);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            Session::accept(listener.accept().unwrap().0, &public, &secret).unwrap()
        });
        let client = Session::initiate(TcpStream::connect(address).unwrap(), public).unwrap();
        (client, server.join().unwrap())
    }
    fn message(value: u32) -> MessageFrame<SetupConnectionSuccess> {
        MessageFrame::from_message(
            SetupConnectionSuccess {
                used_version: 2,
                flags: value,
            },
            1,
            0,
            false,
        )
        .unwrap()
    }
    fn read(receiver: &mut Receiver) -> u32 {
        let mut frame = receiver
            .receive(Duration::from_millis(100))
            .unwrap()
            .unwrap();
        assert_eq!(frame.header().msg_type(), 1);
        let msg: SetupConnectionSuccess = binary_sv2::from_bytes(frame.payload()).unwrap();
        msg.flags
    }
    #[test]
    fn encrypted_sessions_round_trip_and_idle_does_not_consume_state() {
        let (client, server) = pair();
        let (mut a, mut ar) = client.split();
        let (mut b, mut br) = server.split();
        assert!(br.receive(Duration::from_millis(5)).unwrap().is_none());
        for value in 0..4 {
            a.send(message(value)).unwrap();
            assert_eq!(read(&mut br), value);
            b.send(message(value + 10)).unwrap();
            assert_eq!(read(&mut ar), value + 10);
        }
    }
    #[test]
    fn pinned_authority_rejects_wrong_server_key() {
        let (public, secret) = authority(42);
        let (wrong, _) = authority(43);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server =
            thread::spawn(move || Session::accept(listener.accept().unwrap().0, &public, &secret));
        assert!(Session::initiate(TcpStream::connect(address).unwrap(), wrong).is_err());
        drop(server.join().unwrap());
    }
    #[test]
    fn tampered_replayed_and_truncated_frames_close_connection() {
        for mode in 0..3 {
            let (client, server) = pair();
            let (mut sender, _) = client.split();
            let (_, mut receiver) = server.split();
            let bytes = sender
                .encoder
                .encode_transport(message(7), sender.state.as_mut().unwrap())
                .unwrap();
            let mut bytes = bytes.to_vec();
            if mode == 0 {
                *bytes.last_mut().unwrap() ^= 1;
            }
            if mode == 1 {
                sender.stream.write_all(&bytes).unwrap();
                assert_eq!(read(&mut receiver), 7);
            }
            if mode == 2 {
                bytes.truncate(bytes.len() - 1);
            }
            sender.stream.write_all(&bytes).unwrap();
            if mode == 2 {
                sender.stream.shutdown(Shutdown::Write).unwrap();
            }
            assert!(receiver.receive(Duration::from_millis(100)).is_err());
            assert!(receiver.state.is_none());
            assert!(receiver.receive(Duration::from_millis(100)).is_err());
        }
    }
    #[test]
    fn fragmented_wire_frame_is_reassembled() {
        let (client, server) = pair();
        let (mut sender, _) = client.split();
        let (_, mut receiver) = server.split();
        let bytes = sender
            .encoder
            .encode_transport(message(19), sender.state.as_mut().unwrap())
            .unwrap();
        let bytes = bytes.to_vec();
        let writer = thread::spawn(move || {
            for byte in bytes {
                sender.stream.write_all(&[byte]).unwrap();
            }
        });
        assert_eq!(read(&mut receiver), 19);
        writer.join().unwrap();
    }
}
