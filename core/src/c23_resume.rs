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

use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResumeJob {
    pub song_url: String,
    pub destination: PathBuf,
    pub count_in: bool,
    pub transpose: i8,
    pub selected_tracks: Vec<String>,
    pub completed: Vec<CompletedStem>,
}

impl ResumeJob {
    pub fn new(
        song_url: &str,
        destination: &Path,
        count_in: bool,
        transpose: i8,
        selected_tracks: Vec<String>,
    ) -> Result<Self> {
        ensure!(!song_url.trim().is_empty(), "Missing song URL");
        ensure!(destination.is_absolute(), "Destination must be absolute");
        ensure!(!selected_tracks.is_empty(), "No tracks selected");

        let mut unique = std::collections::HashSet::new();
        for track in &selected_tracks {
            ensure!(!track.trim().is_empty(), "Empty track name");
            ensure!(
                unique.insert(track.to_lowercase()),
                "Duplicate selected track: {}",
                track
            );
        }

        Ok(Self {
            song_url: song_url.to_owned(),
            destination: destination.to_path_buf(),
            count_in,
            transpose,
            selected_tracks,
            completed: Vec::new(),
        })
    }

    pub fn record_completed(&mut self, track: &str, path: &Path) -> Result<()> {
        ensure!(
            self.selected_tracks.iter().any(|name| name == track),
            "Track was not selected: {}",
            track
        );

        ensure!(
            path.parent() == Some(self.destination.as_path()),
            "Stem is not in the expected destination"
        );

        let record = CompletedStem::capture(track, path)?;
        self.completed.retain(|stem| stem.track_name != track);
        self.completed.push(record);
        Ok(())
    }

    pub fn pending_tracks(&self) -> Result<Vec<String>> {
        let mut pending = Vec::new();

        for track in &self.selected_tracks {
            let saved = self.completed.iter()
                .find(|stem| &stem.track_name == track);

            let valid = match saved {
                Some(stem) => stem.matches_in(&self.destination)?,
                None => false,
            };

            if !valid {
                pending.push(track.clone());
            }
        }

        Ok(pending)
    }

    fn same_request(&self, other: &Self) -> bool {
        self.song_url == other.song_url
            && self.destination == other.destination
            && self.count_in == other.count_in
            && self.transpose == other.transpose
            && self.selected_tracks.len() == other.selected_tracks.len()
            && self.selected_tracks.iter().all(|track| {
                other.selected_tracks.iter()
                    .any(|name| name.eq_ignore_ascii_case(track))
            })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResumeStore {
    pub version: u32,
    pub jobs: Vec<ResumeJob>,
}

impl Default for ResumeStore {
    fn default() -> Self {
        Self { version: 1, jobs: Vec::new() }
    }
}

impl ResumeStore {
    pub fn default_path() -> Result<PathBuf> {
        let root = dirs::state_dir()
            .or_else(dirs::data_local_dir)
            .or_else(|| dirs::home_dir().map(|p| p.join(".local/state")))
            .ok_or_else(|| anyhow!("Cannot locate application data directory"))?;

        Ok(root.join("kv-downloader").join("c23-resume.json"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let store: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(store.version == 1, "Unsupported resume file version");
        Ok(store)
    }

    pub fn upsert(&mut self, job: ResumeJob) {
        if let Some(existing) = self.jobs.iter_mut()
            .find(|saved| saved.same_request(&job))
        {
            *existing = job;
        } else {
            self.jobs.push(job);
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent()
            .ok_or_else(|| anyhow!("Invalid resume file path"))?;

        fs::create_dir_all(parent)?;

        let filename = path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Invalid resume filename"))?;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();

        let temp = parent.join(format!(
            "{filename}.{}-{unique}.tmp",
            std::process::id()
        ));

        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;

            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            drop(file);

            fs::rename(&temp, path)?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }

        result
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn saves_multiple_jobs_and_recovers_pending_tracks() -> Result<()> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();

        let root = std::env::temp_dir().join(format!(
            "c23-state-{}-{unique}", std::process::id()
        ));

        let destination = root.join("song");
        fs::create_dir_all(&destination)?;

        let bass = destination.join("Bass.mp3");
        fs::write(&bass, b"test audio")?;

        let mut first = ResumeJob::new(
            "https://example.com/song1",
            &destination,
            false,
            0,
            vec!["Bass".into(), "Drums".into()],
        )?;

        first.record_completed("Bass", &bass)?;

        let second = ResumeJob::new(
            "https://example.com/song2",
            &destination,
            true,
            -2,
            vec!["Piano".into()],
        )?;

        let mut store = ResumeStore::default();
        store.upsert(first);
        store.upsert(second);

        let state = root.join("resume.json");
        store.save(&state)?;
        store.save(&state)?;

        let restored = ResumeStore::load(&state)?;

        assert_eq!(restored.jobs.len(), 2);
        assert_eq!(
            restored.jobs[0].pending_tracks()?,
            vec!["Drums"]
        );

        fs::remove_file(&bass)?;

        assert_eq!(
            restored.jobs[0].pending_tracks()?,
            vec!["Bass", "Drums"]
        );

        fs::remove_dir_all(root)?;
        Ok(())
    }
}
