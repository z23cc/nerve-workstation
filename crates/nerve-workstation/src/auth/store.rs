use super::*;
use directories::BaseDirs;
use fs4::TryLockError;

const KEYRING_SERVICE: &str = "nerve";

pub(super) struct AuthLock {
    file: fs::File,
}

impl Drop for AuthLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

pub(super) fn xai_state_and_tokens(store: &AuthStore) -> Result<(XaiProviderState, XaiTokens)> {
    let state =
        store.providers.get(PROVIDER_ID).cloned().ok_or_else(|| {
            anyhow!("no xAI OAuth credentials stored; run `nerve auth login xai`")
        })?;
    let tokens = state.tokens.clone().ok_or_else(|| {
        anyhow!("xAI OAuth state is missing tokens; run `nerve auth login xai --force`")
    })?;
    Ok((state, tokens))
}

pub(super) fn save_xai_state(state: XaiProviderState) -> Result<()> {
    let path = auth_file_path()?;
    let _lock = acquire_auth_lock(&path)?;
    let mut store = load_store(&path)?;
    store.providers.insert(PROVIDER_ID.to_string(), state);
    save_store(&path, &store)
}

pub(super) fn load_xai_state() -> Result<Option<XaiProviderState>> {
    let path = auth_file_path()?;
    let store = load_store(&path)?;
    Ok(store.providers.get(PROVIDER_ID).cloned())
}

pub(super) fn load_store(path: &Path) -> Result<AuthStore> {
    if !path.exists() {
        return Ok(AuthStore::default());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(AuthStore::default());
    }
    let mut store: AuthStore = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    hydrate_keyring_tokens(path, &mut store);
    Ok(store)
}

pub(super) fn save_store(path: &Path, store: &AuthStore) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let store = prepare_store_for_save(path, store);
    let bytes = serde_json::to_vec_pretty(&store).context("failed to encode auth store")?;
    write_private_file(&tmp, &bytes)?;
    replace_file(&tmp, path).with_context(|| format!("failed to save {}", path.display()))
}

pub(super) fn auth_file_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("NERVE_AUTH_FILE") {
        return Ok(PathBuf::from(path));
    }
    Ok(auth_home()?.join("auth.json"))
}

pub(super) fn auth_home() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("NERVE_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("nerve"));
    }
    if let Some(base_dirs) = BaseDirs::new() {
        return Ok(base_dirs.config_dir().join("nerve"));
    }
    bail!("could not determine auth directory; set NERVE_HOME or NERVE_AUTH_FILE")
}

fn prepare_store_for_save(path: &Path, store: &AuthStore) -> AuthStore {
    let mut store = store.clone();
    if let Some(state) = store.providers.get_mut(PROVIDER_ID)
        && let Some(tokens) = state.tokens.as_ref()
        && save_keyring_tokens(path, tokens)
    {
        state.tokens = None;
    }
    store
}

fn hydrate_keyring_tokens(path: &Path, store: &mut AuthStore) {
    if let Some(state) = store.providers.get_mut(PROVIDER_ID)
        && state.tokens.is_none()
        && let Some(tokens) = load_keyring_tokens(path)
    {
        state.tokens = Some(tokens);
    }
}

fn save_keyring_tokens(path: &Path, tokens: &XaiTokens) -> bool {
    if keyring_disabled() {
        return false;
    }
    let Ok(payload) = serde_json::to_string(tokens) else {
        return false;
    };
    keyring_entry(path)
        .and_then(|entry| entry.set_password(&payload))
        .is_ok()
}

fn load_keyring_tokens(path: &Path) -> Option<XaiTokens> {
    if keyring_disabled() {
        return None;
    }
    let payload = keyring_entry(path).ok()?.get_password().ok()?;
    serde_json::from_str(&payload).ok()
}

pub(super) fn delete_xai_keyring_tokens(path: &Path) {
    if keyring_disabled() {
        return;
    }
    if let Ok(entry) = keyring_entry(path) {
        let _ = entry.delete_credential();
    }
}

fn keyring_entry(path: &Path) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, &keyring_account_for_path(path))
}

pub(super) fn keyring_account_for_path(path: &Path) -> String {
    let id = URL_SAFE_NO_PAD.encode(path.to_string_lossy().as_bytes());
    format!("{PROVIDER_ID}:{id}")
}

fn keyring_disabled() -> bool {
    cfg!(test) || std::env::var_os("NERVE_AUTH_DISABLE_KEYRING").is_some()
}

pub(super) fn acquire_auth_lock(auth_path: &Path) -> Result<AuthLock> {
    if let Some(parent) = auth_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let lock_path = auth_path.with_file_name("auth.json.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                file.set_len(0)?;
                writeln!(file, "pid={}", std::process::id()).ok();
                file.sync_all().ok();
                return Ok(AuthLock { file });
            }
            Err(TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for auth lock: {}", lock_path.display());
                }
                sleep(Duration::from_millis(100));
            }
            Err(TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("failed to lock {}", lock_path.display()));
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(not(windows))]
pub(super) fn replace_file(tmp: &Path, target: &Path) -> Result<()> {
    fs::rename(tmp, target).map_err(Into::into)
}

#[cfg(windows)]
pub(super) fn replace_file(tmp: &Path, target: &Path) -> Result<()> {
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
        Err(io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to atomically replace {} with {}",
                target.display(),
                tmp.display()
            )
        })
    } else {
        Ok(())
    }
}
