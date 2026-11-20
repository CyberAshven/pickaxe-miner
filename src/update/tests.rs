//! The updater against a local test server: no request leaves this
//! computer, nothing outside a temporary folder is written, and no program
//! is started.
use super::install::{self, sha256, Installed};
use super::release::{self, Release, Source, Version, BINARY};
use super::trust::{self, Trust, Unsigned};
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

/// A local HTTP server: `answer` gets each request's head and gives the
/// whole response; the heads are kept.
struct Server {
    base: String,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(answer: impl Fn(&str) -> Vec<u8> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&heads);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    return;
                };
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => head.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let response = answer(&head);
                kept.lock().unwrap().push(head);
                let _ = stream.write_all(&response);
            }
        });
        Self { base, heads }
    }

    fn source(&self) -> Source {
        Source {
            latest_url: format!("{}/latest", self.base),
            hosts: vec!["127.0.0.1".into()],
            https_only: false,
        }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }

    fn asked_for(&self, path: &str) -> usize {
        self.heads()
            .iter()
            .filter(|head| head.starts_with(&format!("GET {path} ")))
            .count()
    }
}

fn ok(body: &[u8], headers: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn not_found() -> Vec<u8> {
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
}

const TAG: &str = "pickaxe-miner-v9.9.9";

/// A release newer than any Pickaxe, with `archive` (served as `served`,
/// so a test can swap it) and its checksum file.
fn release_server(archive: &[u8], served: Vec<u8>, signed: bool) -> Server {
    let asset = release::asset_name(TAG).unwrap();
    let sums = format!("{}  {asset}\n", hex::encode(sha256(archive)));
    let size = archive.len() as u64;
    Server::start(move |head| {
        let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
        let base = head
            .lines()
            .find_map(|line| line.strip_prefix("Host: "))
            .map(|host| format!("http://{}", host.trim()))
            .unwrap_or_default();
        match path.as_str() {
            "/latest" if head.contains("If-None-Match: \"one\"") => {
                b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n".to_vec()
            }
            "/latest" => ok(
                release::tests::latest_json(&base, TAG, size, signed)
                    .to_string()
                    .as_bytes(),
                "ETag: \"one\"\r\n",
            ),
            "/download/SHA256SUMS.txt" => ok(sums.as_bytes(), ""),
            "/download/SHA256SUMS.txt.minisig" => ok(trust::tests::SIGNATURE.as_bytes(), ""),
            path if path == format!("/download/{asset}") => ok(&served, ""),
            _ => not_found(),
        }
    })
}

fn package() -> Vec<u8> {
    install::tests::archive(&[(BINARY, b"pickaxe nine"), ("README.md", b"notes")])
}

#[test]
fn an_unchanged_release_list_is_asked_with_its_etag_and_read_from_the_cache() {
    let home = test_dir("etag");
    let archive = package();
    let server = release_server(&archive, archive.clone(), false);
    let source = server.source();
    let first = release::check(&source, &home, 100).unwrap();
    assert_eq!(first.version, Version(9, 9, 9));
    let second = release::check(&source, &home, 200).unwrap();
    assert_eq!(first, second);
    let heads = server.heads();
    assert_eq!(server.asked_for("/latest"), 2);
    assert!(!heads[0].contains("If-None-Match"));
    assert!(heads[1].contains("If-None-Match: \"one\""), "{}", heads[1]);
    let cache = release::Cache::load(&home);
    assert_eq!(cache.checked_at, 200);
    assert_eq!(cache.etag.as_deref(), Some("\"one\""));
    // The checksum file is downloaded once per release, then read here.
    for _ in 0..2 {
        let offer = offer(&source, &home, second.clone(), &[]).unwrap().unwrap();
        assert_eq!(offer.expected, sha256(&archive));
        assert_eq!(offer.trust, Trust::Unsigned(Unsigned::NoKey));
    }
    assert_eq!(server.asked_for("/download/SHA256SUMS.txt"), 1);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_release_that_is_not_newer_is_up_to_date_and_says_nothing() {
    let home = test_dir("up-to-date");
    let running = Version::running();
    let release = Release {
        tag: running.tag(),
        version: running,
        notes_url: String::new(),
        asset: "a.zip".into(),
        asset_url: "http://127.0.0.1:9/a.zip".into(),
        asset_size: 1,
        digest: None,
        sums_url: "http://127.0.0.1:9/SHA256SUMS.txt".into(),
        signature_url: None,
    };
    // Nothing is fetched: the version alone says it.
    let source = Source {
        latest_url: String::new(),
        hosts: vec![],
        https_only: true,
    };
    assert_eq!(offer(&source, &home, release, &[]).unwrap(), None);
    let updater = Updater::for_test(&home, source);
    updater.set(State::UpToDate);
    assert_eq!(updater.notice(), None);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn signatures_are_checked_against_the_compiled_keys_and_githubs_digest() {
    let home = test_dir("signature");
    let archive = package();
    let server = release_server(&archive, archive.clone(), true);
    let source = server.source();
    let release = release::check(&source, &home, 1).unwrap();
    assert!(release.signature_url.is_some());
    // The release's signature is a valid minisign signature, but not over
    // this checksum file: with the key trusted, it does not verify.
    let offered = offer(&source, &home, release.clone(), &[trust::tests::KEY])
        .unwrap()
        .unwrap();
    assert_eq!(offered.trust, Trust::Unsigned(Unsigned::BadSignature));
    // Without a key compiled in, the signature is not even consulted.
    let offered = offer(&source, &home, release.clone(), &[])
        .unwrap()
        .unwrap();
    assert_eq!(offered.trust, Trust::Unsigned(Unsigned::NoKey));
    // GitHub's own record of the archive must agree with the checksum file.
    let mut disagrees = release;
    disagrees.digest = Some("00".repeat(32));
    assert!(offer(&source, &home, disagrees, &[])
        .unwrap_err()
        .contains("disagree"));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn the_trust_rule_installs_only_signed_releases_by_themselves() {
    let signed = Trust::Signed { key: "RW".into() };
    let unsigned = Trust::Unsigned(Unsigned::NoKey);
    assert_eq!(
        decide(&signed, Ask::Auto { enabled: true }),
        Decision::Install
    );
    assert_eq!(
        decide(&signed, Ask::Auto { enabled: false }),
        Decision::Skip
    );
    assert_eq!(
        decide(&unsigned, Ask::Auto { enabled: true }),
        Decision::Skip
    );
    assert_eq!(
        decide(&unsigned, Ask::Person { confirmed: false }),
        Decision::Confirm
    );
    assert_eq!(
        decide(&unsigned, Ask::Person { confirmed: true }),
        Decision::Install
    );
    assert_eq!(
        decide(&signed, Ask::Person { confirmed: false }),
        Decision::Install
    );
    // Auto-update follows the key unless set; checks off turns it off.
    let mut settings = Settings::default();
    assert_eq!(settings.auto_update(), trust::has_key());
    assert_eq!(settings.mode(), Mode::Notify);
    settings.set_mode(Mode::On);
    assert!(settings.auto_update());
    settings.set_mode(Mode::Off);
    assert!(!settings.check && !settings.auto_update());
    assert_eq!(settings.mode(), Mode::Off);
    assert_eq!(Mode::On.next(true), Mode::Notify);
    assert_eq!(Mode::On.next(false), Mode::Off);
    assert_eq!(Mode::parse(" Notify ").unwrap(), Mode::Notify);
    assert!(Mode::parse("sometimes").is_err());
}

#[test]
fn an_unsigned_release_never_installs_by_itself_and_installs_once_confirmed() {
    let home = test_dir("unsigned");
    let archive = package();
    let server = release_server(&archive, archive.clone(), false);
    let source = server.source();
    Settings {
        auto_update: Some(true),
        check: true,
    }
    .save(&home)
    .unwrap();
    let updater = Updater::for_test(&home, source.clone());
    updater.consider(release::check(&source, &home, now()));
    let State::Available(offer) = updater.state() else {
        panic!("{:?}", updater.state());
    };
    assert!(!offer.trust.is_signed());
    // Auto-update is on, yet nothing was downloaded or installed.
    assert_eq!(
        server.asked_for(&format!("/download/{}", offer.release.asset)),
        0
    );
    assert_eq!(Installed::load(&home).unwrap().pending, None);
    assert_eq!(
        updater.notice().unwrap(),
        "Pickaxe v9.9.9 available · Unsigned release: update manually (U)"
    );
    // U says it is unsigned and why, and asks again.
    let Press::Said(said) = updater.press(false) else {
        panic!("no restart while one is offered");
    };
    assert!(said.contains("Unsigned release v9.9.9"), "{said}");
    assert!(said.contains(Unsigned::NoKey.why()), "{said}");
    assert!(said.contains("U again"), "{said}");
    assert!(matches!(updater.state(), State::Confirm(_)));
    // X keeps this version: back to the offer, its notice hidden.
    updater.dismiss();
    assert!(matches!(updater.state(), State::Available(_)));
    assert_eq!(updater.notice(), None);
    // U, then U again (the background thread's part, done here).
    updater.press(false);
    updater.install(Ask::Auto { enabled: true });
    assert!(matches!(updater.state(), State::Confirm(_)));
    updater.install(Ask::Person { confirmed: true });
    let State::Ready {
        version, signed, ..
    } = updater.state()
    else {
        panic!("{:?}", updater.state());
    };
    assert_eq!((version, signed), (Version(9, 9, 9), false));
    assert_eq!(
        updater.notice().unwrap(),
        "Updated to v9.9.9 · restart to apply"
    );
    let pending = Installed::load(&home).unwrap().pending.unwrap();
    assert_eq!(pending.tag, TAG);
    assert!(!pending.signed);
    // Restart only between sessions; while mining it says how.
    assert!(matches!(updater.press(false), Press::Said(text) if text.contains("never restarts")));
    assert_eq!(updater.press(true), Press::Restart);
    std::fs::remove_dir_all(home).unwrap();
}

/// An offer of the test release as if its checksum file were signed.
fn signed_offer(server: &Server, home: &Path) -> Offer {
    let release = release::check(&server.source(), home, now()).unwrap();
    let mut offer = offer(&server.source(), home, release, &[])
        .unwrap()
        .unwrap();
    offer.trust = Trust::Signed {
        key: "RWtest".into(),
    };
    offer
}

#[test]
fn a_signed_release_installs_by_itself_only_with_auto_update_on() {
    let home = test_dir("signed");
    let archive = package();
    let server = release_server(&archive, archive.clone(), true);
    let updater = Updater::for_test(&home, server.source());
    let offer = signed_offer(&server, &home);
    updater.set(State::Available(offer.clone()));
    assert_eq!(
        updater.notice().unwrap(),
        "Pickaxe v9.9.9 available · U to update"
    );
    updater.install(Ask::Auto { enabled: false });
    assert!(matches!(updater.state(), State::Available(_)));
    updater.install(Ask::Auto { enabled: true });
    assert!(matches!(updater.state(), State::Ready { signed: true, .. }));
    let pending = Installed::load(&home).unwrap().pending.unwrap();
    assert!(pending.signed);
    assert_eq!(pending.sha256, hex::encode(offer.expected));
    assert_eq!(
        std::fs::read(pending.folder.join(BINARY)).unwrap(),
        b"pickaxe nine"
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_tampered_archive_installs_nothing_even_when_signed() {
    let home = test_dir("tampered");
    let archive = package();
    let mut tampered = archive.clone();
    let middle = tampered.len() / 2;
    tampered[middle] ^= 0x55;
    let server = release_server(&archive, tampered, true);
    let updater = Updater::for_test(&home, server.source());
    updater.set(State::Available(signed_offer(&server, &home)));
    updater.install(Ask::Auto { enabled: true });
    let State::Failed { error, offer } = updater.state() else {
        panic!("{:?}", updater.state());
    };
    assert!(error.contains("nothing was installed"), "{error}");
    assert!(offer.is_some());
    assert_eq!(Installed::load(&home).unwrap(), Installed::default());
    assert_eq!(
        updater.notice().unwrap(),
        "Update to v9.9.9 failed · U to try again"
    );
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_download_larger_than_published_or_the_limit_is_refused() {
    let home = test_dir("too-large");
    let archive = package();
    let mut longer = archive.clone();
    longer.extend_from_slice(&[0; 4096]);
    let server = release_server(&archive, longer, true);
    let offer = signed_offer(&server, &home);
    let error = install::download(&server.source(), &offer.release, &|_| {}).unwrap_err();
    assert!(error.contains("larger than"), "{error}");
    // A published size over the limit is refused before any download.
    let mut huge = offer.release.clone();
    huge.asset_size = install::ARCHIVE_LIMIT + 1;
    let asked = server.heads().len();
    assert!(install::download(&server.source(), &huge, &|_| {}).is_err());
    assert_eq!(server.heads().len(), asked);
    // A redirect off the allowed hosts is refused too.
    let mut elsewhere = offer.release;
    elsewhere.asset_url = "http://localhost:9/x.zip".into();
    assert!(install::download(&server.source(), &elsewhere, &|_| {})
        .unwrap_err()
        .contains("not one of GitHub's"));
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn the_notice_is_one_line_for_each_state() {
    let offer = Offer {
        release: Release {
            tag: TAG.into(),
            version: Version(0, 0, 5),
            notes_url: "https://github.com/x/releases/tag/y".into(),
            asset: "a.zip".into(),
            asset_url: String::new(),
            asset_size: 1,
            digest: None,
            sums_url: String::new(),
            signature_url: None,
        },
        trust: Trust::Signed { key: "RW".into() },
        expected: [0; 32],
    };
    let unsigned = Offer {
        trust: Trust::Unsigned(Unsigned::NoSignature),
        ..offer.clone()
    };
    let cases = [
        (State::Idle, None),
        (State::Checking, None),
        (State::UpToDate, None),
        (
            State::Failed {
                error: "offline".into(),
                offer: None,
            },
            None,
        ),
        (
            State::Available(offer.clone()),
            Some("Pickaxe v0.0.5 available · U to update".to_owned()),
        ),
        (
            State::Available(unsigned.clone()),
            Some("Pickaxe v0.0.5 available · Unsigned release: update manually (U)".to_owned()),
        ),
        (
            State::Downloading {
                offer: offer.clone(),
                percent: 42,
            },
            Some("Updating… 42%".to_owned()),
        ),
        (
            State::Ready {
                version: Version(0, 0, 5),
                signed: true,
                waiting: None,
            },
            Some("Updated to v0.0.5 · restart to apply".to_owned()),
        ),
    ];
    for (state, notice) in cases {
        let shown = state.notice();
        assert_eq!(shown, notice, "{state:?}");
        assert!(shown.is_none_or(|line| !line.contains('\n')));
    }
    // A dismissed notice stays hidden while only the progress moves.
    let home = test_dir("dismiss");
    let updater = Updater::for_test(&home, Source::github());
    updater.set(State::Downloading {
        offer: offer.clone(),
        percent: 10,
    });
    updater.dismiss();
    updater.set(State::Downloading {
        offer: offer.clone(),
        percent: 60,
    });
    assert_eq!(updater.notice(), None);
    updater.set(State::Ready {
        version: Version(0, 0, 5),
        signed: true,
        waiting: None,
    });
    assert!(updater.notice().is_some());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn a_pending_claim_or_rig_winner_holds_the_swap() {
    let dir = test_dir("claims");
    assert_eq!(pending_claim(std::slice::from_ref(&dir)), None);
    std::fs::write(
        dir.join("electric-mainnet-ab.pending.json"),
        br#"{"version":1,"binding":"x","pending":null}"#,
    )
    .unwrap();
    std::fs::write(dir.join("config.rig-winners.json"), b"[]").unwrap();
    assert_eq!(pending_claim(std::slice::from_ref(&dir)), None);
    std::fs::write(
        dir.join("config.rig-winners.json"),
        br#"[{"job":{},"winner":{}}]"#,
    )
    .unwrap();
    assert_eq!(
        pending_claim(std::slice::from_ref(&dir)).as_deref(),
        Some("config.rig-winners.json")
    );
    std::fs::remove_file(dir.join("config.rig-winners.json")).unwrap();
    std::fs::write(
        dir.join("electric-mainnet-ab.pending.json"),
        br#"{"version":1,"binding":"x","pending":{"baton":{}}}"#,
    )
    .unwrap();
    assert!(pending_claim(std::slice::from_ref(&dir)).is_some());
    // A journal that does not read counts as holding a claim.
    std::fs::write(dir.join("electric-mainnet-ab.pending.json"), b"{").unwrap();
    assert!(pending_claim(std::slice::from_ref(&dir)).is_some());
    std::fs::remove_dir_all(dir).unwrap();
}
