//! Secure, per-provider credential persistence.
//!
//! Each [`Credential`] is keyed by [`ProviderId::as_str`] in an `auth.json`
//! under the platform config dir. Writes are atomic (temp file + rename) and
//! private (`0o600` on Unix), serialized by an `fs4` advisory lock. The bearer
//! and refresh tokens are stored in the OS keyring when available; the JSON
//! file then holds only non-secret metadata. When the keyring is unavailable
//! the tokens fall back into the (private) JSON file so login still persists.
//!
//! This mirrors the storage design in `crates/nerve-workstation/src/auth/`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use directories::BaseDirs;
use fs4::TryLockError;
use serde::{Deserialize, Serialize};

use super::{AuthMode, Credential, ProviderId};
use crate::error::{AgentError, AgentResult};

const KEYRING_SERVICE: &str = "nerve-agent";

/// On-disk root: a map of provider id -> stored credential record.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct AuthStore {
    #[serde(default)]
    providers: BTreeMap<String, StoredCredential>,
}

/// The persisted form of a [`Credential`]. Secret fields are skipped when
/// empty so that a keyring-backed record carries only metadata in the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCredential {
    provider: ProviderId,
    mode: AuthMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    base_url: String,
}

/// Just the secret material, stored in the OS keyring out of the JSON file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SecretBundle {
    #[serde(default)]
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

impl StoredCredential {
    fn from_credential(cred: &Credential) -> Self {
        Self {
            provider: cred.provider,
            mode: cred.mode,
            access_token: cred.access_token.clone(),
            refresh_token: cred.refresh_token.clone(),
            expires_at_unix: cred.expires_at_unix,
            account_id: cred.account_id.clone(),
            base_url: cred.base_url.clone(),
        }
    }

    fn into_credential(self) -> Credential {
        Credential {
            provider: self.provider,
            mode: self.mode,
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at_unix: self.expires_at_unix,
            account_id: self.account_id,
            base_url: self.base_url,
        }
    }

    fn secrets(&self) -> SecretBundle {
        SecretBundle {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
        }
    }

    fn absorb_secrets(&mut self, secrets: SecretBundle) {
        self.access_token = secrets.access_token;
        if secrets.refresh_token.is_some() {
            self.refresh_token = secrets.refresh_token;
        }
    }
}

/// Persist `cred`, replacing any existing record for its provider.
pub fn save_credential(cred: &Credential) -> AgentResult<()> {
    save_credential_at(&auth_file_path()?, cred)
}

/// Load the stored credential for `provider`, if one exists.
pub fn load_credential(provider: ProviderId) -> AgentResult<Option<Credential>> {
    load_credential_at(&auth_file_path()?, provider)
}

/// Remove the stored credential (file entry + keyring secret) for `provider`.
pub fn delete_credential(provider: ProviderId) -> AgentResult<()> {
    delete_credential_at(&auth_file_path()?, provider)
}

/// Path-explicit form of [`save_credential`], used by callers (and tests) that
/// supply their own store location.
fn save_credential_at(path: &Path, cred: &Credential) -> AgentResult<()> {
    let _lock = acquire_auth_lock(path)?;
    let mut store = load_store(path)?;
    store.providers.insert(
        cred.provider.as_str().to_string(),
        StoredCredential::from_credential(cred),
    );
    save_store(path, &store)
}

/// Path-explicit form of [`load_credential`].
fn load_credential_at(path: &Path, provider: ProviderId) -> AgentResult<Option<Credential>> {
    let store = load_store(path)?;
    Ok(store
        .providers
        .get(provider.as_str())
        .cloned()
        .map(StoredCredential::into_credential))
}

/// Path-explicit form of [`delete_credential`].
fn delete_credential_at(path: &Path, provider: ProviderId) -> AgentResult<()> {
    let _lock = acquire_auth_lock(path)?;
    let mut store = load_store(path)?;
    if store.providers.remove(provider.as_str()).is_some() {
        delete_keyring_secret(path, provider);
        save_store(path, &store)?;
    }
    Ok(())
}

fn load_store(path: &Path) -> AgentResult<AuthStore> {
    if !path.exists() {
        return Ok(AuthStore::default());
    }
    let text = fs::read_to_string(path)
        .map_err(|err| AgentError::Auth(format!("failed to read {}: {err}", path.display())))?;
    if text.trim().is_empty() {
        return Ok(AuthStore::default());
    }
    let mut store: AuthStore = serde_json::from_str(&text)
        .map_err(|err| AgentError::Auth(format!("failed to parse {}: {err}", path.display())))?;
    hydrate_keyring_secrets(path, &mut store);
    Ok(store)
}

fn save_store(path: &Path, store: &AuthStore) -> AgentResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AgentError::Auth(format!("failed to create {}: {err}", parent.display()))
        })?;
    }
    let store = prepare_store_for_save(path, store);
    let bytes = serde_json::to_vec_pretty(&store)
        .map_err(|err| AgentError::Auth(format!("failed to encode auth store: {err}")))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    write_private_file(&tmp, &bytes)?;
    replace_file(&tmp, path)
}

/// Move each provider's secrets into the keyring when possible, clearing them
/// from the on-disk record; otherwise leave them inline in the private file.
fn prepare_store_for_save(path: &Path, store: &AuthStore) -> AuthStore {
    let mut store = store.clone();
    for record in store.providers.values_mut() {
        if save_keyring_secret(path, record.provider, &record.secrets()) {
            record.access_token = String::new();
            record.refresh_token = None;
        }
    }
    store
}

fn hydrate_keyring_secrets(path: &Path, store: &mut AuthStore) {
    for record in store.providers.values_mut() {
        if !record.access_token.is_empty() {
            continue;
        }
        if let Some(secrets) = load_keyring_secret(path, record.provider) {
            record.absorb_secrets(secrets);
        }
    }
}

fn save_keyring_secret(path: &Path, provider: ProviderId, secrets: &SecretBundle) -> bool {
    if keyring_disabled() {
        return false;
    }
    let Ok(payload) = serde_json::to_string(secrets) else {
        return false;
    };
    keyring_entry(path, provider)
        .and_then(|entry| entry.set_password(&payload))
        .is_ok()
}

fn load_keyring_secret(path: &Path, provider: ProviderId) -> Option<SecretBundle> {
    if keyring_disabled() {
        return None;
    }
    let payload = keyring_entry(path, provider).ok()?.get_password().ok()?;
    serde_json::from_str(&payload).ok()
}

fn delete_keyring_secret(path: &Path, provider: ProviderId) {
    if keyring_disabled() {
        return;
    }
    if let Ok(entry) = keyring_entry(path, provider) {
        let _ = entry.delete_credential();
    }
}

fn keyring_entry(path: &Path, provider: ProviderId) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &keyring_account_for(path, provider))
}

/// Keyring account name, scoped by both the auth-file path and provider id so
/// distinct config homes (and providers) never collide.
fn keyring_account_for(path: &Path, provider: ProviderId) -> String {
    let id = URL_SAFE_NO_PAD.encode(path.to_string_lossy().as_bytes());
    format!("{}:{id}", provider.as_str())
}

fn keyring_disabled() -> bool {
    cfg!(test) || std::env::var_os("NERVE_AUTH_DISABLE_KEYRING").is_some()
}

/// Resolve the path to the agent credential store, honoring overrides.
fn auth_file_path() -> AgentResult<PathBuf> {
    if let Ok(path) = std::env::var("NERVE_AGENT_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(test)]
    {
        Ok(std::env::temp_dir().join(format!("nerve-agent-test-auth-{}.json", std::process::id())))
    }
    #[cfg(not(test))]
    Ok(config_home()?.join("agent-auth.json"))
}

/// Resolve nerve's per-user config home (`$NERVE_HOME`, else
/// `$XDG_CONFIG_HOME/nerve`, else the platform config dir + `nerve`). This is the
/// single source of truth for where global, user-scoped nerve data lives —
/// credentials here, capability definitions (agents/skills) in the workstation.
pub fn config_home() -> AgentResult<PathBuf> {
    if let Ok(path) = std::env::var("NERVE_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("nerve"));
    }
    if let Some(base_dirs) = BaseDirs::new() {
        return Ok(base_dirs.config_dir().join("nerve"));
    }
    Err(AgentError::Auth(
        "could not determine the nerve config directory; set NERVE_HOME".into(),
    ))
}

/// An advisory lock over auth-related critical sections, released on drop.
pub(crate) struct AuthLock {
    file: fs::File,
}

impl Drop for AuthLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

pub(crate) fn acquire_refresh_lock(provider: ProviderId) -> AgentResult<AuthLock> {
    let auth_path = auth_file_path()?;
    let lock_path =
        auth_path.with_file_name(format!("agent-auth.{}.refresh.lock", provider.as_str()));
    acquire_lock_file(&lock_path)
}

fn acquire_auth_lock(auth_path: &Path) -> AgentResult<AuthLock> {
    let lock_path = auth_path.with_file_name("agent-auth.json.lock");
    acquire_lock_file(&lock_path)
}

fn acquire_lock_file(lock_path: &Path) -> AgentResult<AuthLock> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AgentError::Auth(format!("failed to create {}: {err}", parent.display()))
        })?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|err| {
            AgentError::Auth(format!("failed to open {}: {err}", lock_path.display()))
        })?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                let _ = file.set_len(0);
                writeln!(file, "pid={}", std::process::id()).ok();
                file.sync_all().ok();
                return Ok(AuthLock { file });
            }
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(AgentError::Auth(format!(
                        "timed out waiting for auth lock: {}",
                        lock_path.display()
                    )));
                }
                sleep(Duration::from_millis(100));
            }
            Err(TryLockError::Error(err)) => {
                return Err(AgentError::Auth(format!(
                    "failed to lock {}: {err}",
                    lock_path.display()
                )));
            }
        }
    }
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> AgentResult<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| AgentError::Auth(format!("failed to write {}: {err}", path.display())))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| AgentError::Auth(format!("failed to write {}: {err}", path.display())))
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> AgentResult<()> {
    fs::write(path, bytes)
        .map_err(|err| AgentError::Auth(format!("failed to write {}: {err}", path.display())))
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, target: &Path) -> AgentResult<()> {
    fs::rename(tmp, target)
        .map_err(|err| AgentError::Auth(format!("failed to save {}: {err}", target.display())))
}

#[cfg(windows)]
fn replace_file(tmp: &Path, target: &Path) -> AgentResult<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    let tmp_wide = wide(tmp);
    let target_wide = wide(target);
    let ok = unsafe {
        MoveFileExW(
            tmp_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(AgentError::Auth(format!(
            "failed to atomically replace {} with {}",
            target.display(),
            tmp.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(provider: ProviderId) -> Credential {
        Credential {
            provider,
            mode: AuthMode::Oauth,
            access_token: "access-tok".into(),
            refresh_token: Some("refresh-tok".into()),
            expires_at_unix: Some(4_102_444_800),
            account_id: Some("acct_1".into()),
            base_url: provider.default_base_url().to_string(),
        }
    }

    /// A throwaway directory removed on drop. Avoids a `tempfile` dev-dep and,
    /// by giving every test its own store path, sidesteps the process-global
    /// `NERVE_AGENT_AUTH_FILE` env var entirely (tests run in parallel).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "nerve-agent-store-{}-{tag}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn store_path(&self) -> PathBuf {
            self.path.join("agent-auth.json")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn save_then_load_roundtrips() {
        // Note: keyring is disabled under cfg(test), so secrets persist in the
        // (private) JSON file — exercising the fallback path.
        let dir = TempDir::new("roundtrip");
        let path = dir.store_path();
        let cred = sample(ProviderId::Anthropic);
        save_credential_at(&path, &cred).expect("save");
        let loaded = load_credential_at(&path, ProviderId::Anthropic)
            .expect("load")
            .expect("present");
        assert_eq!(loaded.access_token, cred.access_token);
        assert_eq!(loaded.refresh_token, cred.refresh_token);
        assert_eq!(loaded.account_id, cred.account_id);
        assert_eq!(loaded.base_url, cred.base_url);
        assert_eq!(loaded.mode, AuthMode::Oauth);
    }

    #[test]
    fn providers_are_isolated() {
        let dir = TempDir::new("isolated");
        let path = dir.store_path();
        save_credential_at(&path, &sample(ProviderId::OpenAi)).expect("save openai");
        assert!(
            load_credential_at(&path, ProviderId::Xai)
                .expect("load xai")
                .is_none()
        );
        assert!(
            load_credential_at(&path, ProviderId::OpenAi)
                .expect("load openai")
                .is_some()
        );
    }

    #[test]
    fn delete_removes_record() {
        let dir = TempDir::new("delete");
        let path = dir.store_path();
        save_credential_at(&path, &sample(ProviderId::Xai)).expect("save");
        delete_credential_at(&path, ProviderId::Xai).expect("delete");
        assert!(
            load_credential_at(&path, ProviderId::Xai)
                .expect("load")
                .is_none()
        );
    }

    #[test]
    fn overwrites_existing_record_for_same_provider() {
        let dir = TempDir::new("overwrite");
        let path = dir.store_path();
        save_credential_at(&path, &sample(ProviderId::OpenAi)).expect("first save");
        let mut updated = sample(ProviderId::OpenAi);
        updated.access_token = "rotated".into();
        save_credential_at(&path, &updated).expect("second save");
        let loaded = load_credential_at(&path, ProviderId::OpenAi)
            .expect("load")
            .expect("present");
        assert_eq!(loaded.access_token, "rotated");
    }

    #[test]
    fn keyring_accounts_scoped_by_path_and_provider() {
        let a = keyring_account_for(Path::new("/tmp/a/agent-auth.json"), ProviderId::Anthropic);
        let b = keyring_account_for(Path::new("/tmp/b/agent-auth.json"), ProviderId::Anthropic);
        let c = keyring_account_for(Path::new("/tmp/a/agent-auth.json"), ProviderId::OpenAi);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("anthropic:"));
    }
}
