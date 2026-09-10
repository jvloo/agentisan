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
    let db = database_path(data_dir)?;
    let dir = data_dir.join("credentials");
    private_dir(&dir)?;
    let mut created = Vec::new();
    let mut paths = Vec::new();
    let mut hashes = Vec::new();
    let prepared = (|| -> Result<()> {
        for principal in &fixture.principals {
            let path = dir.join(format!("{}.token", principal.id.as_str()));
            let token = if path.exists() {
                read_credential(&path)?
            } else {
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
                let mut file = opts.open(&path)?;
                created.push(path.clone());
                writeln!(file, "{token}")?;
                file.sync_all()?;
                token
            };
            hashes.push(CredentialHash {
                principal_id: principal.id.clone(),
                sha256: credential_hash(&token),
            });
            paths.push(path);
        }
        Ok(())
    })();
    let result = async {
        prepared?;
        let registry = Registry::open(&db).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&db, fs::Permissions::from_mode(0o600))?;
        }
        let result = registry.register_fixture(&fixture, &hashes).await;
        registry.close().await;
        result?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
    }
    result?;
    Ok(paths)
}
