use crate::PluginManifest;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::Serialize;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub enabled: bool,
    pub missing_commands: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("plugin store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid plugin package: {0}")]
    Invalid(String),
    #[error("plugin `{0}` is already installed (use --replace)")]
    Exists(String),
    #[error("plugin `{0}` is not installed")]
    NotFound(String),
}

#[derive(Debug, Clone)]
pub struct PluginStore {
    root: PathBuf,
}

impl PluginStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn discover(&self) -> Result<(Vec<InstalledPlugin>, Vec<String>), StoreError> {
        if !self.root.exists() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut entries = fs::read_dir(&self.root)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        let mut plugins = Vec::new();
        let mut diagnostics = Vec::new();
        for entry in entries {
            let path = entry.path();
            if !path.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match load_installed(&path) {
                Ok(plugin) => plugins.push(plugin),
                Err(error) => diagnostics.push(format!("{}: {error}", path.display())),
            }
        }
        Ok((plugins, diagnostics))
    }

    pub fn install<R: Read>(
        &self,
        reader: R,
        replace: bool,
    ) -> Result<InstalledPlugin, StoreError> {
        fs::create_dir_all(&self.root)?;
        let staging = self
            .root
            .join(format!(".install-{}-{}", std::process::id(), nonce()));
        fs::create_dir(&staging)?;
        let result = (|| {
            unpack(reader, &staging)?;
            let manifest = read_manifest(&staging)?;
            make_local_entrypoint_executable(&staging, &manifest)?;
            let destination = self.root.join(&manifest.id);
            if destination.exists() && !replace {
                return Err(StoreError::Exists(manifest.id));
            }
            if destination.exists() {
                let backup = self
                    .root
                    .join(format!(".backup-{}-{}", std::process::id(), nonce()));
                fs::rename(&destination, &backup)?;
                if let Err(error) = fs::rename(&staging, &destination) {
                    let _ = fs::rename(&backup, &destination);
                    return Err(error.into());
                }
                fs::remove_dir_all(backup)?;
            } else {
                fs::rename(&staging, &destination)?;
            }
            load_installed(&destination)
        })();
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), StoreError> {
        validate_id(id)?;
        let path = self.root.join(id);
        if !path.is_dir() {
            return Err(StoreError::NotFound(id.into()));
        }
        let marker = path.join(".disabled");
        if enabled {
            match fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            fs::write(marker, b"disabled by ate\n")?;
        }
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        validate_id(id)?;
        let path = self.root.join(id);
        if !path.is_dir() {
            return Err(StoreError::NotFound(id.into()));
        }
        let removed = self
            .root
            .join(format!(".removed-{}-{}", std::process::id(), nonce()));
        fs::rename(path, &removed)?;
        fs::remove_dir_all(removed)?;
        Ok(())
    }
}

pub fn pack_directory<W: Write>(directory: &Path, writer: W) -> Result<(), StoreError> {
    let manifest = read_manifest(directory)?;
    manifest.validate().map_err(StoreError::Invalid)?;
    ensure_safe_tree(directory)?;
    let encoder = GzEncoder::new(writer, Compression::default());
    let mut archive = tar::Builder::new(encoder);
    archive.append_dir_all(".", directory)?;
    archive.into_inner()?.finish()?;
    Ok(())
}

fn ensure_safe_tree(directory: &Path) -> Result<(), StoreError> {
    for item in fs::read_dir(directory)? {
        let item = item?;
        let metadata = fs::symlink_metadata(item.path())?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err(StoreError::Invalid(format!(
                "package contains a link or special file: {}",
                item.path().display()
            )));
        }
        if metadata.is_dir() {
            ensure_safe_tree(&item.path())?;
        }
    }
    Ok(())
}

pub fn validate_package<R: Read>(reader: R) -> Result<PluginManifest, StoreError> {
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    let mut manifest = None;
    for item in archive.entries()? {
        let entry = item?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(StoreError::Invalid(
                "links and special archive entries are forbidden".into(),
            ));
        }
        let path = entry.path()?.into_owned();
        if !safe_relative(&path) {
            return Err(StoreError::Invalid(format!(
                "unsafe archive path `{}`",
                path.display()
            )));
        }
        let normalized = path.strip_prefix(".").unwrap_or(&path);
        if kind.is_file() && normalized == Path::new("tool.json") {
            let mut raw = String::new();
            entry.take(1_048_577).read_to_string(&mut raw)?;
            if raw.len() > 1_048_576 {
                return Err(StoreError::Invalid("tool.json exceeds 1 MiB".into()));
            }
            manifest = Some(PluginManifest::parse(&raw).map_err(StoreError::Invalid)?);
        }
    }
    manifest.ok_or_else(|| StoreError::Invalid("tool.json is missing".into()))
}

fn unpack<R: Read>(reader: R, destination: &Path) -> Result<(), StoreError> {
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    for item in archive.entries()? {
        let mut entry = item?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(StoreError::Invalid(
                "links and special archive entries are forbidden".into(),
            ));
        }
        let path = entry.path()?.into_owned();
        if !safe_relative(&path) {
            return Err(StoreError::Invalid(format!(
                "unsafe archive path `{}`",
                path.display()
            )));
        }
        let output = destination.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(output)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

fn read_manifest(directory: &Path) -> Result<PluginManifest, StoreError> {
    let raw = fs::read_to_string(directory.join("tool.json"))?;
    PluginManifest::parse(&raw).map_err(StoreError::Invalid)
}

fn load_installed(path: &Path) -> Result<InstalledPlugin, StoreError> {
    let manifest = read_manifest(path)?;
    if path.file_name().and_then(|v| v.to_str()) != Some(&manifest.id) {
        return Err(StoreError::Invalid(
            "directory name does not match plugin id".into(),
        ));
    }
    if manifest.entrypoint_is_local() && !path.join(&manifest.entrypoint[0]).is_file() {
        return Err(StoreError::Invalid(
            "local entrypoint does not exist".into(),
        ));
    }
    let mut required = manifest.required_commands.clone();
    if !manifest.entrypoint_is_local() {
        required.push(manifest.entrypoint[0].clone());
    }
    required.sort();
    required.dedup();
    let missing_commands = required
        .into_iter()
        .filter(|name| !command_exists(name))
        .collect();
    Ok(InstalledPlugin {
        manifest,
        path: path.to_path_buf(),
        enabled: !path.join(".disabled").exists(),
        missing_commands,
    })
}

fn command_exists(command: &str) -> bool {
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
        .unwrap_or(false)
}

fn validate_id(id: &str) -> Result<(), StoreError> {
    let fake = format!(
        r#"{{"schema_version":1,"id":"{id}","version":"0.0.0","entrypoint":["x"],"tools":[{{"name":"x","description":"x","input_schema":{{}}}}]}}"#
    );
    PluginManifest::parse(&fake)
        .map(|_| ())
        .map_err(StoreError::Invalid)
}

#[cfg(unix)]
fn make_local_entrypoint_executable(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    if manifest.entrypoint_is_local() {
        let path = root.join(&manifest.entrypoint[0]);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_local_entrypoint_executable(_: &Path, _: &PluginManifest) -> Result<(), StoreError> {
    Ok(())
}

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn unsafe_paths_are_rejected() {
        assert!(!safe_relative(Path::new("../escape")));
        assert!(!safe_relative(Path::new("/absolute")));
        assert!(safe_relative(Path::new("bin/run")));
    }

    #[test]
    fn package_round_trip_install_disable_and_remove() {
        let base = std::env::temp_dir().join(format!(
            "ate-plugin-store-{}-{}",
            std::process::id(),
            nonce()
        ));
        let source = base.join("source");
        let store_path = base.join("store");
        fs::create_dir_all(source.join("bin")).unwrap();
        fs::write(source.join("bin/run"), "#!/bin/sh\n").unwrap();
        fs::write(
            source.join("tool.json"),
            r#"{
            "schema_version": 1,
            "id": "com.example.echo",
            "version": "1.0.0",
            "entrypoint": ["bin/run"],
            "tools": [{"name":"example_echo","description":"echo","input_schema":{}}]
        }"#,
        )
        .unwrap();
        let mut package = Vec::new();
        pack_directory(&source, &mut package).unwrap();
        assert_eq!(
            validate_package(Cursor::new(&package)).unwrap().id,
            "com.example.echo"
        );
        let store = PluginStore::new(&store_path);
        let installed = store.install(Cursor::new(&package), false).unwrap();
        assert!(installed.enabled);
        store.set_enabled("com.example.echo", false).unwrap();
        assert!(!store.discover().unwrap().0[0].enabled);
        store.remove("com.example.echo").unwrap();
        assert!(store.discover().unwrap().0.is_empty());
        fs::remove_dir_all(base).unwrap();
    }
}
