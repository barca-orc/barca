//! Project-wide token state, separate from the per-environment metadata ledger.
//! Each operation locks and reloads state, so revocation is visible across processes.
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use subtle::ConstantTimeEq;

const LIMIT: usize = 1000;
const MAX_STATE: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read or write project authentication state")]
    State,
    #[error("invalid or expired credential")]
    Invalid,
    #[error("token name must contain 1–128 UTF-8 bytes without control characters")]
    Name,
    #[error("token expiry must be in the future")]
    Expiry,
    #[error("the project has 1000 API tokens; revoke an unused token before creating another")]
    Full,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, rename = "TokenMetadata")
)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub created_at: DateTime<Utc>,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Secret is returned once by issuance; it is never persisted or included in Debug.
#[derive(Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "IssuedToken"))]
pub struct Issued {
    #[serde(flatten)]
    pub metadata: Metadata,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    metadata: Metadata,
    digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    tokens: Vec<Record>,
    connection: Option<Record>,
    bootstrap: Option<Record>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            tokens: Vec::new(),
            connection: None,
            bootstrap: None,
        }
    }
}

/// All callers use the deployed project's root, independently of the active environment.
#[derive(Clone)]
pub struct Store {
    directory: PathBuf,
    clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

fn private_file(path: &Path) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error::State)?;
    ensure_private(&file)?;
    Ok(file)
}
fn ensure_private(file: &File) -> Result<(), Error> {
    if file
        .metadata()
        .map_err(|_| Error::State)?
        .permissions()
        .mode()
        & 0o777
        != 0o600
    {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| Error::State)?;
    }
    Ok(())
}
fn random() -> Result<String, Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::State)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn digest(kind: &str, secret: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"barca-auth-v1\0");
    hash.update(kind);
    hash.update(b"\0");
    hash.update(secret);
    hash.finalize().into()
}
fn issue(
    kind: &str,
    name: &str,
    expiry: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(Issued, Record), Error> {
    let id = random()?;
    let token = format!("barca_{kind}_v1_{id}_{}", random()?);
    let metadata = Metadata {
        id,
        name: name.into(),
        created_at: now,
        expires_at: expiry,
    };
    let record = Record {
        metadata: metadata.clone(),
        digest: digest(kind, &token),
    };
    Ok((Issued { metadata, token }, record))
}
fn matches(record: &Record, kind: &str, token: &str, now: DateTime<Utc>) -> bool {
    bool::from(record.digest.ct_eq(&digest(kind, token)))
        && record.metadata.expires_at.is_none_or(|end| end > now)
}

fn token_id<'a>(token: &'a str, kind: &str) -> Option<&'a str> {
    let (id, secret) = token
        .strip_prefix(&format!("barca_{kind}_v1_"))?
        .split_once('_')?;
    let hex = |part: &str| {
        part.len() == 64
            && part
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    (hex(id) && hex(secret)).then_some(id)
}

impl Store {
    pub fn open(project: &Path) -> Result<Self, Error> {
        let parent = project.join(".barca");
        fs::create_dir_all(&parent).map_err(|_| Error::State)?;
        if fs::symlink_metadata(&parent)
            .map_err(|_| Error::State)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::State);
        }
        let directory = parent.join("auth");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(Error::State),
        }
        if !fs::symlink_metadata(&directory)
            .map_err(|_| Error::State)?
            .is_dir()
        {
            return Err(Error::State);
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::State)?;
        let store = Self {
            directory,
            clock: Arc::new(Utc::now),
        };
        store.transaction(false, |_| Ok(()))?;
        Ok(store)
    }

    fn transaction<T>(
        &self,
        write: bool,
        action: impl FnOnce(&mut State) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let lock = private_file(&self.directory.join("lock"))?;
        lock.lock().map_err(|_| Error::State)?;
        let path = self.directory.join("state.json");
        let mut state = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => {
                if !file.metadata().map_err(|_| Error::State)?.is_file() {
                    return Err(Error::State);
                }
                ensure_private(&file)?;
                let mut bytes = Vec::new();
                file.take(MAX_STATE + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| Error::State)?;
                if bytes.len() as u64 > MAX_STATE {
                    return Err(Error::State);
                }
                serde_json::from_slice::<State>(&bytes).map_err(|_| Error::State)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(_) => return Err(Error::State),
        };
        if state.version != 1 || state.tokens.len() > LIMIT {
            return Err(Error::State);
        }
        let answer = action(&mut state)?;
        if write {
            let mut temporary =
                tempfile::NamedTempFile::new_in(&self.directory).map_err(|_| Error::State)?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|_| Error::State)?;
            let bytes = serde_json::to_vec(&state).map_err(|_| Error::State)?;
            if bytes.len() as u64 > MAX_STATE {
                return Err(Error::State);
            }
            temporary.write_all(&bytes).map_err(|_| Error::State)?;
            temporary.as_file().sync_all().map_err(|_| Error::State)?;
            temporary.persist(path).map_err(|_| Error::State)?;
            File::open(&self.directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| Error::State)?;
        }
        Ok(answer)
    }

    pub fn bootstrap(&self) -> Result<String, Error> {
        self.transaction(true, |state| {
            let now = (self.clock)();
            let (issued, record) = issue(
                "bootstrap",
                "local bootstrap",
                Some(now + Duration::minutes(15)),
                now,
            )?;
            state.bootstrap = Some(record);
            Ok(issued.token)
        })
    }
    pub fn redeem(&self, code: &str) -> Result<Issued, Error> {
        self.transaction(true, |state| {
            let now = (self.clock)();
            if code.len() > 256
                || !state
                    .bootstrap
                    .as_ref()
                    .is_some_and(|r| matches(r, "bootstrap", code, now))
            {
                return Err(Error::Invalid);
            }
            let (issued, record) = issue(
                "api",
                "bootstrap connection",
                Some(now + Duration::hours(12)),
                now,
            )?;
            state.bootstrap = None;
            state.connection = Some(record);
            Ok(issued)
        })
    }
    pub fn validate(&self, token: &str) -> Result<Metadata, Error> {
        let id = token_id(token, "api").ok_or(Error::Invalid)?;
        self.transaction(false, |state| {
            let now = (self.clock)();
            state
                .tokens
                .iter()
                .chain(state.connection.iter())
                .find(|record| record.metadata.id == id && matches(record, "api", token, now))
                .map(|record| record.metadata.clone())
                .ok_or(Error::Invalid)
        })
    }
    pub fn create(&self, name: &str, expires_at: Option<DateTime<Utc>>) -> Result<Issued, Error> {
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(Error::Name);
        }
        self.transaction(true, |state| {
            let now = (self.clock)();
            if expires_at.is_some_and(|end| end <= now) {
                return Err(Error::Expiry);
            }
            state
                .tokens
                .retain(|r| r.metadata.expires_at.is_none_or(|end| end > now));
            if state.tokens.len() >= LIMIT {
                return Err(Error::Full);
            }
            let (issued, record) = issue("api", name, expires_at, now)?;
            state.tokens.push(record);
            Ok(issued)
        })
    }
    pub fn list(&self) -> Result<Vec<Metadata>, Error> {
        self.transaction(false, |state| {
            let now = (self.clock)();
            Ok(state
                .tokens
                .iter()
                .chain(state.connection.iter())
                .filter(|r| r.metadata.expires_at.is_none_or(|end| end > now))
                .map(|r| r.metadata.clone())
                .collect())
        })
    }
    pub fn revoke(&self, id: &str) -> Result<(), Error> {
        self.transaction(true, |state| {
            state.tokens.retain(|r| r.metadata.id != id);
            if state
                .connection
                .as_ref()
                .is_some_and(|r| r.metadata.id == id)
            {
                state.connection = None;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clocked() -> (
        tempfile::TempDir,
        Store,
        Arc<std::sync::Mutex<DateTime<Utc>>>,
    ) {
        let root = tempfile::tempdir().unwrap();
        let time = Arc::new(std::sync::Mutex::new(
            DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let clock = time.clone();
        let mut store = Store::open(root.path()).unwrap();
        store.clock = Arc::new(move || *clock.lock().unwrap());
        (root, store, time)
    }

    #[test]
    fn private_clock_checks_exact_expiry_without_sleeping_or_mutating_state() {
        let (root, store, clock) = clocked();
        let initial = *clock.lock().unwrap();
        let expires = initial + Duration::minutes(1);
        let token = store.create("short", Some(expires)).unwrap();
        let path = root.path().join(".barca/auth/state.json");
        let before = fs::read(&path).unwrap();
        *clock.lock().unwrap() = expires - Duration::nanoseconds(1);
        assert!(store.validate(&token.token).is_ok());
        *clock.lock().unwrap() = expires;
        assert!(matches!(store.validate(&token.token), Err(Error::Invalid)));
        assert!(store.list().unwrap().is_empty());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(matches!(
            store.create("already expired", Some(expires)),
            Err(Error::Expiry)
        ));
        // Revocation remains authoritative even if the host clock moves backwards.
        store.revoke(&token.metadata.id).unwrap();
        *clock.lock().unwrap() = initial - Duration::hours(1);
        assert!(matches!(store.validate(&token.token), Err(Error::Invalid)));
        assert!(
            Store::open(root.path())
                .unwrap()
                .validate(&token.token)
                .is_err()
        );
    }

    #[test]
    fn private_clock_checks_bootstrap_and_connection_expiry_and_rotation() {
        let (_root, store, clock) = clocked();
        let initial = *clock.lock().unwrap();
        let expired = store.bootstrap().unwrap();
        *clock.lock().unwrap() = initial + Duration::minutes(15);
        assert!(matches!(store.redeem(&expired), Err(Error::Invalid)));
        let code = store.bootstrap().unwrap();
        *clock.lock().unwrap() += Duration::minutes(15) - Duration::nanoseconds(1);
        let old = store.redeem(&code).unwrap();
        assert_eq!(
            old.metadata.expires_at,
            Some(*clock.lock().unwrap() + Duration::hours(12))
        );
        assert!(matches!(store.redeem(&code), Err(Error::Invalid)));
        let fresh = store.redeem(&store.bootstrap().unwrap()).unwrap();
        assert!(matches!(store.validate(&old.token), Err(Error::Invalid)));
        let expires = fresh.metadata.expires_at.unwrap();
        *clock.lock().unwrap() = expires - Duration::nanoseconds(1);
        assert!(store.validate(&fresh.token).is_ok());
        *clock.lock().unwrap() = expires;
        assert!(matches!(store.validate(&fresh.token), Err(Error::Invalid)));
        *clock.lock().unwrap() = initial;
        assert!(matches!(store.validate(&old.token), Err(Error::Invalid)));
    }

    #[test]
    fn issuance_persists_only_digests_and_public_metadata() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let issued = store.create("laptop", None).unwrap();
        assert_eq!(store.validate(&issued.token).unwrap(), issued.metadata);
        let disk = fs::read_to_string(root.path().join(".barca/auth/state.json")).unwrap();
        assert!(!disk.contains(&issued.token));
        assert!(
            !serde_json::to_string(&store.list().unwrap())
                .unwrap()
                .contains("digest")
        );
        assert_eq!(
            fs::metadata(root.path().join(".barca/auth"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for name in ["lock", "state.json"] {
            assert_eq!(
                fs::metadata(root.path().join(".barca/auth").join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn bootstrap_is_replaced_consumed_once_and_never_authenticates_as_an_api_token() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let old = store.bootstrap().unwrap();
        let fresh = store.bootstrap().unwrap();
        assert!(matches!(store.redeem(&old), Err(Error::Invalid)));
        assert!(matches!(store.validate(&fresh), Err(Error::Invalid)));
        let connection = store.redeem(&fresh).unwrap();
        assert!(matches!(store.redeem(&fresh), Err(Error::Invalid)));
        assert!(store.validate(&connection.token).is_ok());
        let replacement = store.redeem(&store.bootstrap().unwrap()).unwrap();
        assert!(matches!(
            store.validate(&connection.token),
            Err(Error::Invalid)
        ));
        assert!(store.validate(&replacement.token).is_ok());
    }

    #[test]
    fn independent_stores_observe_revocation_and_do_not_lose_concurrent_writes() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).unwrap();
        let second = Store::open(root.path()).unwrap();
        let issued = first.create("first", None).unwrap();
        assert!(second.validate(&issued.token).is_ok());
        second.revoke(&issued.metadata.id).unwrap();
        second.revoke(&issued.metadata.id).unwrap();
        assert!(matches!(first.validate(&issued.token), Err(Error::Invalid)));
        std::thread::scope(|scope| {
            for index in 0..12 {
                let store = first.clone();
                scope.spawn(move || {
                    store.create(&format!("client {index}"), None).unwrap();
                });
            }
        });
        assert_eq!(first.list().unwrap().len(), 12);
    }

    #[test]
    fn concurrent_bootstrap_redemption_has_exactly_one_winner() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let code = store.bootstrap().unwrap();
        let barrier = std::sync::Barrier::new(8);
        let winners = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (store, code, barrier) = (&store, &code, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        store.redeem(code).is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(winners, 1);
    }

    #[test]
    fn malformed_state_fails_closed_without_overwriting_it() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let path = root.path().join(".barca/auth/state.json");
        fs::write(&path, b"broken").unwrap();
        assert!(matches!(store.bootstrap(), Err(Error::State)));
        assert!(matches!(Store::open(root.path()), Err(Error::State)));
        assert_eq!(fs::read(path).unwrap(), b"broken");
    }

    #[test]
    fn expired_records_fail_validation_and_are_reclaimed_before_capacity_checks() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let issued = store.create("expires", None).unwrap();
        store
            .transaction(true, |state| {
                state.tokens[0].metadata.expires_at = Some(Utc::now() - Duration::seconds(1));
                Ok(())
            })
            .unwrap();
        assert!(matches!(store.validate(&issued.token), Err(Error::Invalid)));
        assert!(store.list().unwrap().is_empty());
        store.create("replacement", None).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        for name in ["", "line\nbreak", &"a".repeat(129)] {
            assert!(matches!(store.create(name, None), Err(Error::Name)));
        }
        assert!(matches!(
            store.create("past", Some(Utc::now() - Duration::seconds(1))),
            Err(Error::Expiry)
        ));
    }

    #[test]
    fn full_api_list_still_allows_local_bootstrap_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        store
            .transaction(true, |state| {
                let now = Utc::now();
                for _ in 0..LIMIT {
                    state.tokens.push(issue("api", "agent", None, now)?.1);
                }
                Ok(())
            })
            .unwrap();
        assert!(matches!(store.create("overflow", None), Err(Error::Full)));
        let connection = store.redeem(&store.bootstrap().unwrap()).unwrap();
        assert!(store.validate(&connection.token).is_ok());
        assert_eq!(store.list().unwrap().len(), LIMIT + 1);
    }

    #[test]
    fn symlink_state_and_directories_are_rejected() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();
        let other = root.path().join("other");
        fs::write(&other, "protected").unwrap();
        symlink(&other, root.path().join(".barca/auth/state.json")).unwrap();
        assert!(matches!(store.bootstrap(), Err(Error::State)));
        assert_eq!(fs::read_to_string(other).unwrap(), "protected");
        let second = tempfile::tempdir().unwrap();
        fs::create_dir(second.path().join(".barca")).unwrap();
        symlink(
            root.path().join(".barca/auth"),
            second.path().join(".barca/auth"),
        )
        .unwrap();
        assert!(matches!(Store::open(second.path()), Err(Error::State)));
    }
}
