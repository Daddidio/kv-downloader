use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletedStem {
    pub track_name: String,
    pub filename: String,
    pub bytes: u64,
    pub sha256: String,
}

impl CompletedStem {
    pub fn capture(track_name: &str, path: &Path) -> Result<Self> {
        ensure!(!track_name.trim().is_empty(), "Missing track name");

        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Invalid stem filename"))?
            .to_owned();

        let (bytes, sha256) = fingerprint(path)?;

        Ok(Self {
            track_name: track_name.to_owned(),
            filename,
            bytes,
            sha256,
        })
    }

    pub fn matches_in(&self, directory: &Path) -> Result<bool> {
        let mut components = Path::new(&self.filename).components();

        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
        {
            return Ok(false);
        }

        let path = directory.join(&self.filename);

        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(err) => return Err(err.into()),
        };

        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() != self.bytes
        {
            return Ok(false);
        }

        let (_, actual_hash) = fingerprint(&path)?;
        Ok(actual_hash == self.sha256)
    }
}

fn fingerprint(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;

    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }

        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }

    ensure!(bytes > 0, "Empty stem file");

    Ok((bytes, format!("{:x}", hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn recognizes_saved_stem_and_detects_changes() -> Result<()> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos();

        let dir = std::env::temp_dir().join(format!(
            "c23-resume-{}-{unique}",
            std::process::id()
        ));

        fs::create_dir_all(&dir)?;
        let path = dir.join("Bass.mp3");

        fs::write(&path, b"original audio")?;

        let record = CompletedStem::capture("Bass", &path)?;

        assert!(record.matches_in(&dir)?);

        fs::write(&path, b"modified audio")?;
        assert!(!record.matches_in(&dir)?);

        fs::remove_file(&path)?;
        assert!(!record.matches_in(&dir)?);

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn rejects_empty_and_unsafe_files() -> Result<()> {
        let dir = std::env::temp_dir();

        let record = CompletedStem {
            track_name: "Bass".to_owned(),
            filename: "../other.mp3".to_owned(),
            bytes: 100,
            sha256: String::new(),
        };

        assert!(!record.matches_in(&dir)?);
        Ok(())
    }
}
