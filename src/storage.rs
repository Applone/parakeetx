use std::{path::{Path, PathBuf}, time::Duration};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::{LibraryItem, Recording, RecordingStatus};

#[derive(Debug, Clone)]
pub struct Library {
    path: PathBuf,
}

impl Library {
    pub fn open(path: PathBuf) -> Result<Self> {
        let library = Self { path };
        let connection = library.connection()?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(version <= 1, "This library was created by a newer parakeetx version");
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS recordings (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 duration REAL NOT NULL,
                 status TEXT NOT NULL,
                 transcript TEXT NOT NULL,
                 document TEXT NOT NULL
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS recording_search USING fts5(
                 title, transcript, content='recordings', content_rowid='rowid',
                 tokenize='unicode61 remove_diacritics 2'
             );
             CREATE TRIGGER IF NOT EXISTS recordings_insert AFTER INSERT ON recordings BEGIN
                 INSERT INTO recording_search(rowid, title, transcript)
                 VALUES (new.rowid, new.title, new.transcript);
             END;
             CREATE TRIGGER IF NOT EXISTS recordings_delete AFTER DELETE ON recordings BEGIN
                 INSERT INTO recording_search(recording_search, rowid, title, transcript)
                 VALUES ('delete', old.rowid, old.title, old.transcript);
             END;
             CREATE TRIGGER IF NOT EXISTS recordings_update AFTER UPDATE ON recordings BEGIN
                 INSERT INTO recording_search(recording_search, rowid, title, transcript)
                 VALUES ('delete', old.rowid, old.title, old.transcript);
                 INSERT INTO recording_search(rowid, title, transcript)
                 VALUES (new.rowid, new.title, new.transcript);
             END;
             PRAGMA user_version=1;"
        )?;
        Ok(library)
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path).context("Cannot open recording library")?;
        connection.busy_timeout(Duration::from_secs(10))?;
        Ok(connection)
    }

    pub fn save(&self, recording: &Recording) -> Result<()> {
        ensure!(recording.duration.is_finite() && recording.duration >= 0.0, "Invalid recording duration");
        self.connection()?.execute(
            "INSERT INTO recordings(id, title, created_at, duration, status, transcript, document)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, duration=excluded.duration,
             status=excluded.status, transcript=excluded.transcript, document=excluded.document",
            params![recording.id, recording.title, recording.created_at.to_rfc3339(), recording.duration,
                recording.status.to_string(), recording.transcript(), serde_json::to_string(recording)?],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Recording> {
        let document: Option<String> = self.connection()?.query_row(
            "SELECT document FROM recordings WHERE id=?1", [id], |row| row.get(0)
        ).optional()?;
        serde_json::from_str(&document.context("Recording no longer exists")?).context("Recording metadata is corrupt")
    }

    pub fn search(&self, query: &str) -> Result<Vec<LibraryItem>> {
        let connection = self.connection()?;
        let expression = search_expression(query);
        let sql = if expression.is_empty() {
            "SELECT id, title, created_at, duration, status, substr(transcript, 1, 180)
             FROM recordings ORDER BY created_at DESC"
        } else {
            "SELECT recordings.id, recordings.title, recordings.created_at, recordings.duration,
             recordings.status, snippet(recording_search, 1, '[', ']', ' ... ', 24)
             FROM recording_search JOIN recordings ON recordings.rowid=recording_search.rowid
             WHERE recording_search MATCH ?1 ORDER BY bm25(recording_search), recordings.created_at DESC"
        };
        let mut statement = connection.prepare(sql)?;
        let arguments: Vec<&str> = if expression.is_empty() { vec![] } else { vec![&expression] };
        let results = statement.query_map(rusqlite::params_from_iter(arguments), |row| {
            Ok(LibraryItem { id: row.get(0)?, title: row.get(1)?, created_at: row.get(2)?, duration: row.get(3)?, status: row.get(4)?, preview: row.get(5)? })
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(results)
    }

    pub fn recover_interrupted(&self) -> Result<usize> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT document FROM recordings WHERE status IN ('Recording', 'Transcribing')")?;
        let documents = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut recovered = 0;
        for document in documents {
            let mut recording: Recording = serde_json::from_str(&document)?;
            if recording.audio_path.exists() {
                if let Ok(reader) = hound::WavReader::open(&recording.audio_path) {
                    recording.duration = f64::from(reader.duration()) / f64::from(reader.spec().sample_rate);
                }
                recording.status = if recording.segments.is_empty() { RecordingStatus::Recorded } else { RecordingStatus::Partial };
                recording.error = Some("The previous session ended early. Saved audio is available; run transcription to finish.".into());
            } else {
                recording.status = RecordingStatus::Failed;
                recording.error = Some("The previous session ended before an audio file was saved.".into());
            }
            self.save(&recording)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    pub fn remove_from_library(&self, id: &str) -> Result<()> {
        self.connection()?.execute("DELETE FROM recordings WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn search_expression(query: &str) -> String {
    query.split_whitespace()
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect::<Vec<_>>().join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Segment;

    #[test]
    fn search_tracks_transcript_updates_and_removal() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        let mut recording = Recording::new("Physics lecture".into(), directory.path());
        recording.segments.push(Segment { start: 0.0, end: 1.0, text: "Quantum entanglement résumé".into(), words: vec![], speaker: None });
        library.save(&recording).unwrap();
        assert_eq!(library.search("entang").unwrap().len(), 1);
        assert_eq!(library.search("resume").unwrap().len(), 1);
        assert_eq!(library.get(&recording.id).unwrap().title, recording.title);
        recording.segments[0].text = "Classical mechanics".into();
        library.save(&recording).unwrap();
        assert!(library.search("quantum").unwrap().is_empty());
        assert_eq!(library.search("classical").unwrap().len(), 1);
        library.remove_from_library(&recording.id).unwrap();
        assert!(library.search("").unwrap().is_empty());
        assert!(library.search("classical").unwrap().is_empty());
    }

    #[test]
    fn interrupted_recordings_recover_audio_and_preserve_text() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        let mut recording = Recording::new("Interrupted".into(), directory.path());
        let mut writer = hound::WavWriter::create(&recording.audio_path, crate::audio::wave_spec()).unwrap();
        crate::audio::write_samples(&mut writer, &vec![0.0; 16_000]).unwrap();
        writer.finalize().unwrap();
        recording.status = RecordingStatus::Recording;
        recording.segments.push(Segment { start: 0.0, end: 0.5, text: "Saved text".into(), words: vec![], speaker: None });
        library.save(&recording).unwrap();
        assert_eq!(library.recover_interrupted().unwrap(), 1);
        let recovered = library.get(&recording.id).unwrap();
        assert_eq!(recovered.status, RecordingStatus::Partial);
        assert_eq!(recovered.duration, 1.0);
        assert_eq!(recovered.transcript(), "Saved text");
        assert_eq!(library.recover_interrupted().unwrap(), 0);
    }

    #[test]
    fn arbitrary_queries_do_not_execute_fts_syntax() {
        let directory = tempfile::tempdir().unwrap();
        let library = Library::open(directory.path().join("library.db")).unwrap();
        for query in ["\"", "OR * NOT", "title:secret", "(hello)", "' OR 1=1 --", "привет", "日本語"] {
            assert!(library.search(query).is_ok(), "query: {query}");
        }
    }
}
