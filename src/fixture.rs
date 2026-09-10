//! Explicit local administration for simulated identities. Not an MCP tool.
use crate::{
    model::{CredentialHash, Fixture},
    registry::{Registry, credential_hash},
};
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        bail!("data directory must be a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("data directory must have owner-only permissions (0700)");
        }
    }
    Ok(())
}

/// Acquires an exclusive, non-blocking lock on `admin.lock` inside `data_dir`,
/// held by the returned file for as long as the caller keeps it alive. Guards
/// concurrent administrative operations (e.g. fixture initialization, managed
/// team registration) against interleaving that could tear down state one
/// operation relies on while another is still using it.
pub fn admin_lock(data_dir: &Path) -> Result<fs::File> {
    private_dir(data_dir)?;
    let path = data_dir.join("admin.lock");
    if let Ok(meta) = fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        bail!("admin lock path must be a regular file");
    }
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(&path).context("cannot open admin lock file")?;
    file.try_lock()
        .context("another administrative operation is already in progress")?;
    Ok(file)
}

/// Maps a canonical (case-sensitive) identifier to a filesystem-safe
/// component name that is stable and distinct across case-insensitive
/// filesystems (macOS, Windows). Plain lowercase identifiers made only of
/// `[a-z0-9_-]` are preserved as-is for compatibility with existing files,
/// except reserved Windows device names, which are always encoded. Anything
/// else is encoded as `~` followed by the lowercase hex of its UTF-8 bytes;
/// `~` is outside the canonical ID alphabet so encoded names can never
/// collide with an unchanged plain identifier.
pub fn portable_component(id: &str) -> String {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let is_plain = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && !RESERVED.contains(&id);
    if is_plain {
        id.to_owned()
    } else {
        let mut out = String::with_capacity(1 + id.len() * 2);
        out.push('~');
        for b in id.as_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

pub fn read_credential(path: &Path) -> Result<String> {
    let meta = fs::symlink_metadata(path).context("cannot inspect credential file")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 {
        bail!("invalid credential file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("credential file must have owner-only permissions (0600)");
        }
    }
    let token = fs::read_to_string(path).context("cannot read credential file")?;
    let token = token.trim();
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid credential format");
    }
    Ok(token.to_owned())
}

pub fn database_path(data_dir: &Path) -> Result<PathBuf> {
    private_dir(data_dir)?;
    let path = data_dir.join("registry.sqlite3");
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            bail!("database must be a regular file");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(path)
}

fn generate_credential(path: &Path, created: &mut Vec<PathBuf>) -> Result<String> {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    created.push(path.to_path_buf());
    writeln!(file, "{token}")?;
    file.sync_all()?;
    Ok(token)
}

pub async fn initialize(data_dir: &Path, fixture_path: &Path) -> Result<Vec<PathBuf>> {
    let mut text = String::new();
    fs::File::open(fixture_path)?
        .take(2_097_153)
        .read_to_string(&mut text)?;
    if text.len() > 2_097_152 {
        bail!("fixture is too large");
    }
    let fixture: Fixture = serde_json::from_str(&text).context("invalid fixture JSON")?;
    // IDs become credential filenames; validate before making any filesystem changes.
    for p in &fixture.principals {
        if p.id.0.is_empty()
            || p.id.0.len() > 64
            || !p
                .id
                .0
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!("invalid principal ID");
        }
    }
    // Held for the whole operation so a concurrent initializer can never see a
    // partially-prepared credential set or remove one this run depends on.
    let _lock = admin_lock(data_dir)?;
    let db = database_path(data_dir)?;
    let dir = data_dir.join("credentials");
    private_dir(&dir)?;
    let registry = Registry::open(&db).await?;
    let mut created = Vec::new();
    let mut paths = Vec::new();
    let mut hashes = Vec::new();
    let prepared = async {
        for principal in &fixture.principals {
            let path = dir.join(format!(
                "{}.token",
                portable_component(principal.id.as_str())
            ));
            let legacy_path = dir.join(format!("{}.token", principal.id.as_str()));
            let token = if path.exists() {
                read_credential(&path)?
            } else if legacy_path != path && legacy_path.exists() {
                let legacy_token = read_credential(&legacy_path)?;
                let legacy_hash = credential_hash(&legacy_token);
                let stored: Option<String> =
                    sqlx::query_scalar("SELECT token_hash FROM principals WHERE id = ?")
                        .bind(principal.id.as_str())
                        .fetch_optional(&registry.pool)
                        .await
                        .context("cannot query principal token hash")?;
                if stored.as_deref() == Some(legacy_hash.as_str()) {
                    let mut opts = OpenOptions::new();
                    opts.write(true).create_new(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        opts.mode(0o600);
                    }
                    let mut file = opts.open(&path)?;
                    created.push(path.clone());
                    writeln!(file, "{legacy_token}")?;
                    file.sync_all()?;
                    legacy_token
                } else {
                    generate_credential(&path, &mut created)?
                }
            } else {
                generate_credential(&path, &mut created)?
            };
            hashes.push(CredentialHash {
                principal_id: principal.id.clone(),
                sha256: credential_hash(&token),
            });
            paths.push(path);
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let result = async {
        prepared?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&db, fs::Permissions::from_mode(0o600))?;
        }
        let result = registry.register_fixture(&fixture, &hashes).await;
        result?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    registry.close().await;
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
    }
    result?;
    Ok(paths)
}
