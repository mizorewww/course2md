//! Secret storage is deliberately separate from serializable preferences and task records.
//!
//! Tests inject `MemoryCredentialVault`; they never access the user's Keychain.

use anyhow::{Result, anyhow, bail};
#[cfg(any(test, not(target_os = "macos")))]
use std::collections::BTreeMap;
use std::fmt;
#[cfg(any(test, not(target_os = "macos")))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use zeroize::Zeroizing;

pub type CredentialRef = String;

/// A secret cannot accidentally be serialized or printed by a derived Debug implementation.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.expose().trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// Insert always creates a new immutable identity. Updating a service cannot change the
/// credential used by an already queued task. Resolving never consults mutable environment
/// variables; a user-selected environment credential must be captured at publication time.
pub trait CredentialVault: Send + Sync {
    fn insert(&self, secret: Secret) -> Result<CredentialRef>;
    fn resolve(&self, reference: &str) -> Result<Secret>;
    fn remove(&self, reference: &str) -> Result<()>;
}

fn new_reference() -> String {
    format!("credential-{}", uuid::Uuid::new_v4())
}

/// A store answered, and it does not hold this reference. Anything else (a locked
/// store, an unreadable file) leaves the question open.
#[cfg(any(test, not(target_os = "macos")))]
#[derive(Debug)]
struct NotStored(&'static str);

#[cfg(any(test, not(target_os = "macos")))]
impl fmt::Display for NotStored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(any(test, not(target_os = "macos")))]
impl std::error::Error for NotStored {}

#[cfg(any(test, not(target_os = "macos")))]
fn not_stored(error: &anyhow::Error) -> bool {
    error.downcast_ref::<NotStored>().is_some()
}

fn validate_reference(reference: &str) -> Result<()> {
    let id = reference.strip_prefix("credential-").unwrap_or("");
    if uuid::Uuid::parse_str(id).is_err() {
        bail!(course2md::tr!("凭据引用无效，请为此服务重新保存 API Key"));
    }
    Ok(())
}

/// In-memory isolated store for tests and caller-controlled temporary use.
/// It intentionally does not provide a plaintext file fallback.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryCredentialVault {
    values: Mutex<BTreeMap<CredentialRef, Zeroizing<String>>>,
}

#[cfg(test)]
impl MemoryCredentialVault {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl CredentialVault for MemoryCredentialVault {
    fn insert(&self, secret: Secret) -> Result<CredentialRef> {
        if secret.is_empty() {
            bail!("请输入 API Key");
        }
        let reference = new_reference();
        self.values
            .lock()
            .map_err(|_| anyhow!("暂时无法保存凭据"))?
            .insert(
                reference.clone(),
                Zeroizing::new(secret.expose().to_owned()),
            );
        Ok(reference)
    }

    fn resolve(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        self.values
            .lock()
            .map_err(|_| anyhow!("暂时无法读取凭据"))?
            .get(reference)
            .map(|value| Secret::new(value.as_str()))
            .ok_or_else(|| NotStored("找不到此服务的凭据，请重新保存 API Key").into())
    }

    fn remove(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        self.values
            .lock()
            .map_err(|_| anyhow!("暂时无法移除凭据"))?
            .remove(reference);
        Ok(())
    }
}

/// errSecItemNotFound: the Keychain has no item for this reference.
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
pub struct KeychainCredentialVault {
    service: String,
}

#[cfg(target_os = "macos")]
impl Default for KeychainCredentialVault {
    fn default() -> Self {
        Self {
            service: "com.course2md.desktop.service-credentials.v1".into(),
        }
    }
}

#[cfg(target_os = "macos")]
impl CredentialVault for KeychainCredentialVault {
    fn insert(&self, secret: Secret) -> Result<CredentialRef> {
        if secret.is_empty() {
            bail!(course2md::tr!("请输入 API Key"));
        }
        let reference = new_reference();
        security_framework::passwords::set_generic_password(
            &self.service,
            &reference,
            secret.expose().as_bytes(),
        )
        .map_err(|error| {
            anyhow!(
                "{}",
                course2md::tr!("无法将 API Key 保存到钥匙串（系统代码 {}）", error.code())
            )
        })?;
        Ok(reference)
    }

    fn resolve(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        let bytes = security_framework::passwords::get_generic_password(&self.service, reference)
            .map_err(|error| match error.code() {
            ERR_SEC_ITEM_NOT_FOUND => anyhow!(course2md::tr!(
                "钥匙串中找不到此服务的凭据，请重新保存 API Key"
            )),
            code => anyhow!(
                "{}",
                course2md::tr!("暂时无法读取此服务的钥匙串凭据（系统代码 {}）", code)
            ),
        })?;
        // Keep the intermediate byte buffer zeroizing as well as the returned UTF-8 value.
        let bytes = Zeroizing::new(bytes);
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!(course2md::tr!("此服务的凭据无法读取，请重新保存 API Key")))?;
        Ok(Secret::new(value))
    }

    fn remove(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        match security_framework::passwords::delete_generic_password(&self.service, reference) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => bail!(
                "{}",
                course2md::tr!("无法移除此服务的钥匙串凭据（系统代码 {}）", error.code())
            ),
        }
    }
}

/// System keyring-backed credential store for Linux (Secret Service) and
/// Windows (Credential Manager). Availability is checked once through the
/// keyring crate's store status.
#[cfg(not(target_os = "macos"))]
pub struct KeyringCredentialVault {
    service: String,
}

#[cfg(not(target_os = "macos"))]
impl KeyringCredentialVault {
    pub fn new() -> Self {
        Self {
            service: "com.course2md.desktop.service-credentials.v1".into(),
        }
    }

    /// Whether the platform credential store initialized successfully.
    pub fn available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    fn store(&self, reference: &str, secret: &Secret) -> Result<()> {
        keyring::Entry::new(&self.service, reference)
            .and_then(|entry| entry.set_password(secret.expose()))
            .map_err(|_| anyhow!(course2md::tr!("无法将 API Key 保存到系统凭据库")))
    }
}

#[cfg(not(target_os = "macos"))]
impl CredentialVault for KeyringCredentialVault {
    fn insert(&self, secret: Secret) -> Result<CredentialRef> {
        if secret.is_empty() {
            bail!(course2md::tr!("请输入 API Key"));
        }
        let reference = new_reference();
        self.store(&reference, &secret)?;
        Ok(reference)
    }

    fn resolve(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        let password = keyring::Entry::new(&self.service, reference)
            .and_then(|entry| entry.get_password())
            .map_err(|error| match error {
                keyring::Error::NoEntry => anyhow::Error::new(NotStored(course2md::tr!(
                    "系统凭据库中找不到此服务的凭据，请重新保存 API Key"
                ))),
                _ => anyhow!(course2md::tr!("暂时无法读取系统凭据库中此服务的凭据")),
            })?;
        Ok(Secret::new(password))
    }

    fn remove(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        // 与 Keychain 实现一致：条目不存在视为已删除
        match keyring::Entry::new(&self.service, reference)
            .and_then(|entry| entry.delete_credential())
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => bail!(course2md::tr!("无法移除系统凭据库中此服务的凭据")),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl SystemStore for KeyringCredentialVault {
    fn adopt(&self, reference: &str, secret: &Secret) -> Result<()> {
        self.store(reference, secret)
    }
}

/// A system store that can also take a credential under the reference it already has.
#[cfg(any(test, not(target_os = "macos")))]
trait SystemStore: CredentialVault {
    fn adopt(&self, reference: &str, secret: &Secret) -> Result<()>;
}

/// File-backed store for when no system keychain is reachable.
///
/// Credentials are written as JSON next to the desktop preferences. The file is
/// created through `atomic_write`, which uses a 0o600 temporary file on Unix, so
/// the persisted secrets are not world-readable. This mirrors the CLI, which
/// stores API keys in the user's `config.toml` under the same permissions.
///
/// The file is the only copy of its state: every operation reads it, and a change
/// holds a lock shared with other processes, so two running apps cannot overwrite
/// each other's keys. An unreadable file refuses writes, so an empty map never
/// replaces the saved keys.
#[cfg(any(test, not(target_os = "macos")))]
pub struct FileCredentialVault {
    path: PathBuf,
}

#[cfg(any(test, not(target_os = "macos")))]
type StoredValues = BTreeMap<CredentialRef, Zeroizing<String>>;

#[cfg(any(test, not(target_os = "macos")))]
impl FileCredentialVault {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn lock(&self) -> Result<course2md::runtime::FileLock> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        course2md::runtime::lock_file_within(
            &self.path.with_extension("lock"),
            std::time::Duration::from_secs(5),
        )
    }

    fn read(&self) -> Result<StoredValues> {
        Self::load(&self.path).map_err(|error| {
            anyhow!(course2md::tr!(
                "{}。请备份后删除该文件再重试：{}",
                error,
                self.path.display()
            ))
        })
    }

    fn load(path: &Path) -> std::result::Result<StoredValues, &'static str> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => Zeroizing::new(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(_) => {
                return Err(course2md::tr!(
                    "已保存的 API Key 文件无法读取，为避免丢失暂不写入"
                ));
            }
        };
        // Moving each parsed String into `Zeroizing` keeps the same allocation.
        let plain: BTreeMap<CredentialRef, String> = serde_json::from_slice(&bytes)
            .map_err(|_| course2md::tr!("已保存的 API Key 文件已损坏，为避免丢失暂不写入"))?;
        Ok(plain
            .into_iter()
            .map(|(reference, value)| (reference, Zeroizing::new(value)))
            .collect())
    }

    /// An emptied file is deleted rather than kept as an empty secret store.
    fn persist(&self, values: &StoredValues) -> Result<()> {
        if values.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            };
        }
        let plain: BTreeMap<&str, &str> = values
            .iter()
            .map(|(reference, value)| (reference.as_str(), value.as_str()))
            .collect();
        // Sized up front: a growing buffer would leave copies behind in freed memory.
        let capacity = values
            .iter()
            .map(|(reference, value)| reference.len() + value.len() + 16)
            .sum::<usize>()
            * 2;
        let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
        serde_json::to_writer_pretty(&mut *bytes, &plain)?;
        course2md::checkpoint::atomic_write(&self.path, &bytes)
    }

    /// Removes `reference` once it is safe elsewhere. Returns whether it was still here.
    fn forget(&self, reference: &str) -> Result<bool> {
        let _lock = self.lock()?;
        let mut values = self.read()?;
        if values.remove(reference).is_none() {
            return Ok(false);
        }
        self.persist(&values)?;
        Ok(true)
    }
}

#[cfg(any(test, not(target_os = "macos")))]
impl CredentialVault for FileCredentialVault {
    fn insert(&self, secret: Secret) -> Result<CredentialRef> {
        if secret.is_empty() {
            bail!(course2md::tr!("请输入 API Key"));
        }
        let _lock = self.lock()?;
        let mut values = self.read()?;
        let reference = new_reference();
        values.insert(
            reference.clone(),
            Zeroizing::new(secret.expose().to_owned()),
        );
        self.persist(&values)?;
        Ok(reference)
    }

    fn resolve(&self, reference: &str) -> Result<Secret> {
        validate_reference(reference)?;
        self.read()?
            .get(reference)
            .map(|value| Secret::new(value.as_str()))
            .ok_or_else(|| {
                NotStored(course2md::tr!("找不到此服务的凭据，请重新保存 API Key")).into()
            })
    }

    fn remove(&self, reference: &str) -> Result<()> {
        validate_reference(reference)?;
        self.forget(reference).map(|_| ())
    }
}

/// The vault for platforms where a system store may or may not be reachable on a
/// given launch (a Linux session without Secret Service, for one).
///
/// The choice is not remembered, so it cannot go stale: new credentials go to the
/// system store whenever it is reachable, and a reference is looked up in both
/// places, since either can hold it. Credentials the file holds move to the system
/// store as soon as it is reachable, and leave the file only after reading back equal.
/// Nothing moves the other way: secrets in an unreachable store cannot be read, so
/// they stay there and resolve again when it returns.
#[cfg(any(test, not(target_os = "macos")))]
struct LayeredVault {
    system: Option<Box<dyn SystemStore>>,
    file: FileCredentialVault,
}

#[cfg(any(test, not(target_os = "macos")))]
impl LayeredVault {
    /// Moves what the file holds to the system store. A credential that cannot be
    /// moved stays in the file, where it still resolves; the next launch tries again.
    fn migrate_to_system(&self) -> Result<()> {
        let Some(system) = &self.system else {
            return Ok(());
        };
        for (reference, value) in self.file.read()? {
            let secret = Secret::new(value.as_str());
            system.adopt(&reference, &secret)?;
            let stored = system.resolve(&reference)?;
            anyhow::ensure!(
                stored.expose() == secret.expose(),
                "credential read back differently"
            );
            if !self.file.forget(&reference)? {
                // Removed by the app after the snapshot; do not bring it back.
                system.remove(&reference)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(test, not(target_os = "macos")))]
impl CredentialVault for LayeredVault {
    fn insert(&self, secret: Secret) -> Result<CredentialRef> {
        match &self.system {
            Some(system) => system.insert(secret),
            None => self.file.insert(secret),
        }
    }

    /// The file comes first: a migration puts a credential in the system store
    /// before taking it out of the file, so this order cannot miss one in transit.
    fn resolve(&self, reference: &str) -> Result<Secret> {
        let file_error = match self.file.resolve(reference) {
            Ok(secret) => return Ok(secret),
            Err(error) => error,
        };
        let Some(system) = &self.system else {
            if not_stored(&file_error) {
                bail!(course2md::tr!(
                    "系统凭据库当前不可用，此服务的凭据可能保存在其中。请确认系统凭据服务已启动，或重新保存 API Key"
                ));
            }
            return Err(file_error);
        };
        match system.resolve(reference) {
            Ok(secret) => Ok(secret),
            // Prefer the answer that says what is actually wrong.
            Err(error) if not_stored(&error) && !not_stored(&file_error) => Err(file_error),
            Err(error) => Err(error),
        }
    }

    /// Both places, so a credential never survives its removal in the other one.
    fn remove(&self, reference: &str) -> Result<()> {
        let file = self.file.remove(reference);
        if let Some(system) = &self.system {
            system.remove(reference)?;
        }
        file
    }
}

pub fn system_vault(path: impl Into<PathBuf>) -> Arc<dyn CredentialVault> {
    #[cfg(target_os = "macos")]
    {
        let _ = path;
        Arc::new(KeychainCredentialVault::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let system: Option<Box<dyn SystemStore>> = KeyringCredentialVault::available()
            .then(|| Box::new(KeyringCredentialVault::new()) as _);
        let vault = Arc::new(LayeredVault {
            system,
            file: FileCredentialVault::new(path),
        });
        // A system store can block on an unlock prompt; the app does not wait for it.
        let migrating = Arc::clone(&vault);
        std::thread::spawn(move || {
            if let Err(error) = migrating.migrate_to_system() {
                eprintln!("credentials stay in the file for now: {error:#}");
            }
        });
        vault
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_errors_cannot_expose_secret() {
        let secret = Secret::new("test-only-do-not-print");
        assert_eq!(format!("{secret:?}"), "Secret([REDACTED])");
        let vault = MemoryCredentialVault::new();
        let error = vault.resolve("test-only-do-not-print").unwrap_err();
        assert!(!error.to_string().contains("test-only-do-not-print"));
        assert!(vault.insert(Secret::new("   ")).is_err());
    }

    #[test]
    fn file_vault_persists_credentials_across_instances() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let reference = {
            let vault = FileCredentialVault::new(&path);
            let reference = vault.insert(Secret::new("test-only-file-key")).unwrap();
            assert_eq!(
                vault.resolve(&reference).unwrap().expose(),
                "test-only-file-key"
            );
            reference
        };
        // A fresh instance reads the same credential back from disk.
        let reopened = FileCredentialVault::new(&path);
        assert_eq!(
            reopened.resolve(&reference).unwrap().expose(),
            "test-only-file-key"
        );
        reopened.remove(&reference).unwrap();
        assert!(reopened.resolve(&reference).is_err());
        // Removal is also persisted.
        let after_remove = FileCredentialVault::new(&path);
        assert!(after_remove.resolve(&reference).is_err());
    }

    #[test]
    fn file_vault_resolves_nothing_from_a_missing_or_corrupt_file() {
        let directory = tempfile::tempdir().unwrap();
        let missing = FileCredentialVault::new(directory.path().join("missing.json"));
        assert!(
            missing
                .resolve("credential-00000000-0000-0000-0000-000000000000")
                .is_err()
        );

        let corrupt = directory.path().join("corrupt.json");
        std::fs::write(&corrupt, b"not json").unwrap();
        let vault = FileCredentialVault::new(&corrupt);
        assert!(
            vault
                .resolve("credential-00000000-0000-0000-0000-000000000000")
                .is_err()
        );
    }

    #[test]
    fn corrupt_file_is_never_overwritten_by_an_empty_map() {
        let directory = tempfile::tempdir().unwrap();
        let corrupt = directory.path().join("corrupt.json");
        std::fs::write(&corrupt, b"not json").unwrap();
        let vault = FileCredentialVault::new(&corrupt);
        // 损坏文件存在期间：写入与删除都必须拒绝，不得静默覆盖丢密钥
        assert!(vault.insert(Secret::new("test-only-key")).is_err());
        assert!(
            vault
                .remove("credential-00000000-0000-0000-0000-000000000000")
                .is_err()
        );
        assert_eq!(std::fs::read(&corrupt).unwrap(), b"not json");
    }
    #[test]
    fn two_instances_of_the_file_vault_see_each_others_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let first = FileCredentialVault::new(&path);
        let second = FileCredentialVault::new(&path);
        let from_first = first.insert(Secret::new("first-key")).unwrap();
        let from_second = second.insert(Secret::new("second-key")).unwrap();
        assert_eq!(first.resolve(&from_second).unwrap().expose(), "second-key");
        assert_eq!(second.resolve(&from_first).unwrap().expose(), "first-key");
        first.remove(&from_second).unwrap();
        assert!(second.resolve(&from_second).is_err());
        assert!(second.resolve(&from_first).is_ok());
    }

    #[test]
    fn concurrent_writers_to_the_file_vault_lose_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let writers: Vec<_> = (0..12)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let reference = FileCredentialVault::new(&path)
                        .insert(Secret::new(format!("key-{n}")))
                        .unwrap();
                    (reference, n)
                })
            })
            .collect();
        let vault = FileCredentialVault::new(&path);
        for writer in writers {
            let (reference, n) = writer.join().unwrap();
            assert_eq!(
                vault.resolve(&reference).unwrap().expose(),
                format!("key-{n}")
            );
        }
    }

    #[test]
    fn removing_the_last_key_leaves_no_file_and_removing_nothing_creates_none() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let vault = FileCredentialVault::new(&path);
        vault
            .remove("credential-00000000-0000-0000-0000-000000000000")
            .unwrap();
        assert!(!path.exists());
        let reference = vault.insert(Secret::new("key")).unwrap();
        assert!(path.exists());
        vault.remove(&reference).unwrap();
        assert!(!path.exists());
    }

    /// A system store the test can make refuse or corrupt what it takes.
    #[derive(Clone, Default)]
    struct FakeStore {
        values: Arc<Mutex<BTreeMap<String, String>>>,
        refuse: bool,
        garble: bool,
    }

    impl CredentialVault for FakeStore {
        fn insert(&self, secret: Secret) -> Result<CredentialRef> {
            let reference = new_reference();
            self.adopt(&reference, &secret)?;
            Ok(reference)
        }
        fn resolve(&self, reference: &str) -> Result<Secret> {
            match self.values.lock().unwrap().get(reference) {
                Some(value) => Ok(Secret::new(value.as_str())),
                None => Err(NotStored("not in the system store").into()),
            }
        }
        fn remove(&self, reference: &str) -> Result<()> {
            self.values.lock().unwrap().remove(reference);
            Ok(())
        }
    }

    impl SystemStore for FakeStore {
        fn adopt(&self, reference: &str, secret: &Secret) -> Result<()> {
            anyhow::ensure!(!self.refuse, "store refused");
            let value = if self.garble {
                "garbled".to_owned()
            } else {
                secret.expose().to_owned()
            };
            self.values
                .lock()
                .unwrap()
                .insert(reference.to_owned(), value);
            Ok(())
        }
    }

    fn layered(path: &Path, system: Option<&FakeStore>) -> LayeredVault {
        LayeredVault {
            system: system.map(|store| Box::new(store.clone()) as Box<dyn SystemStore>),
            file: FileCredentialVault::new(path),
        }
    }

    #[test]
    fn a_key_saved_without_the_system_store_moves_there_when_it_returns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let reference = layered(&path, None)
            .insert(Secret::new("saved-in-file"))
            .unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("saved-in-file")
        );
        let store = FakeStore::default();
        let vault = layered(&path, Some(&store));
        assert_eq!(vault.resolve(&reference).unwrap().expose(), "saved-in-file");
        vault.migrate_to_system().unwrap();
        assert_eq!(vault.resolve(&reference).unwrap().expose(), "saved-in-file");
        assert!(store.values.lock().unwrap().contains_key(&reference));
        assert!(!path.exists(), "no plaintext copy stays behind");
    }

    #[test]
    fn a_failed_move_keeps_the_key_in_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let reference = layered(&path, None)
            .insert(Secret::new("precious"))
            .unwrap();
        for store in [
            FakeStore {
                refuse: true,
                ..FakeStore::default()
            },
            FakeStore {
                garble: true,
                ..FakeStore::default()
            },
        ] {
            let vault = layered(&path, Some(&store));
            assert!(vault.migrate_to_system().is_err());
            assert_eq!(vault.resolve(&reference).unwrap().expose(), "precious");
            assert!(std::fs::read_to_string(&path).unwrap().contains("precious"));
        }
    }

    #[test]
    fn a_key_in_the_system_store_waits_out_a_launch_without_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let store = FakeStore::default();
        let reference = layered(&path, Some(&store))
            .insert(Secret::new("in-the-store"))
            .unwrap();
        assert!(
            !path.exists(),
            "a reachable store never gets a plaintext copy"
        );
        // A launch without the store says where the key is, and a new key still saves.
        let without = layered(&path, None);
        let error = without.resolve(&reference).unwrap_err().to_string();
        assert!(error.contains("系统凭据库当前不可用"), "{error}");
        let saved_meanwhile = without.insert(Secret::new("in-the-file")).unwrap();
        // Both are there on the next launch with it.
        let with = layered(&path, Some(&store));
        with.migrate_to_system().unwrap();
        assert_eq!(with.resolve(&reference).unwrap().expose(), "in-the-store");
        assert_eq!(
            with.resolve(&saved_meanwhile).unwrap().expose(),
            "in-the-file"
        );
    }

    #[test]
    fn removal_reaches_both_places_and_a_corrupt_file_is_reported_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop-credentials.json");
        let store = FakeStore::default();
        let in_store = layered(&path, Some(&store))
            .insert(Secret::new("a"))
            .unwrap();
        let in_file = layered(&path, None).insert(Secret::new("b")).unwrap();
        let vault = layered(&path, Some(&store));
        vault.remove(&in_store).unwrap();
        vault.remove(&in_file).unwrap();
        assert!(vault.resolve(&in_store).is_err());
        assert!(vault.resolve(&in_file).is_err());

        std::fs::write(&path, b"not json").unwrap();
        assert!(vault.migrate_to_system().is_err());
        let saved = vault.insert(Secret::new("still works")).unwrap();
        assert_eq!(vault.resolve(&saved).unwrap().expose(), "still works");
        let error = vault
            .resolve("credential-00000000-0000-0000-0000-000000000000")
            .unwrap_err()
            .to_string();
        assert!(error.contains("已损坏"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
    }
}
