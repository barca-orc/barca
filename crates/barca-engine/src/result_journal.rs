//! Coordinator-owned transient metadata receipts for compact result batches.
//! A worker crash preserves completed leases without sending one result frame per item.
use crate::protocol::WorkerMessage;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;

pub(crate) struct ResultJournal {
    file: tempfile::NamedTempFile,
    pending: Vec<u8>,
    expected: Vec<String>,
    seen: HashMap<String, Value>,
}

impl ResultJournal {
    pub(crate) fn new(command: &mut Value) -> Result<Self, String> {
        let steps = crate::wire::decode_table(&command["items"])?;
        let expected = steps
            .iter()
            .map(|step| {
                step["node_id"]
                    .as_str()
                    .map(String::from)
                    .ok_or("missing node id".to_string())
            })
            .collect::<Result<_, _>>()?;
        let dir = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(".barca/staging")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = tempfile::Builder::new()
            .prefix("results-")
            .tempfile_in(dir)
            .map_err(|e| e.to_string())?;
        command["result_journal"] = json!(file.path());
        Ok(Self {
            file,
            pending: Vec::new(),
            expected,
            seen: HashMap::new(),
        })
    }

    pub(crate) fn accept(&mut self, message: &WorkerMessage) -> Result<bool, String> {
        let id = match message {
            WorkerMessage::StepCompleted { node_id, .. }
            | WorkerMessage::StepError { node_id, .. }
            | WorkerMessage::Blocked { node_id, .. } => node_id,
            _ => return Ok(true),
        };
        let value = serde_json::to_value(message).map_err(|e| e.to_string())?;
        if let Some(previous) = self.seen.get(id) {
            return if *previous == value {
                Ok(false)
            } else {
                Err("conflicting result receipt".into())
            };
        }
        if self.expected.get(self.seen.len()) != Some(id) {
            return Err("result receipt does not match the next leased item".into());
        }
        self.seen.insert(id.clone(), value);
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn read_available(&mut self) -> Result<Vec<WorkerMessage>, String> {
        self.read_through(usize::MAX)
    }

    pub(crate) fn read_through(&mut self, count: usize) -> Result<Vec<WorkerMessage>, String> {
        let count = if count == usize::MAX {
            self.expected.len()
        } else {
            count
        };
        if count > self.expected.len() {
            return Err("checkpoint exceeds its assigned batch".into());
        }

        self.file
            .as_file_mut()
            .take(256 * 1024 * 1024 + 1)
            .read_to_end(&mut self.pending)
            .map_err(|e| e.to_string())?;
        if self.pending.len() > 256 * 1024 * 1024 {
            return Err("result journal frame too large".into());
        }
        let mut messages = Vec::new();
        let mut consumed = 0;
        while self.seen.len() < count {
            let Some(end) = self.pending[consumed..]
                .iter()
                .position(|byte| *byte == b'\n')
            else {
                break;
            };
            let end = consumed + end;
            let row: Value =
                serde_json::from_slice(&self.pending[consumed..end]).map_err(|e| e.to_string())?;
            let row = row
                .as_array()
                .filter(|r| r.len() == 4)
                .ok_or("result journal row needs four cells")?;
            if !row[3].is_object() {
                return Err("result metadata must be an object".into());
            }
            let frame = json!({"type":"results", "node_ids":[row[0]], "statuses":[row[1]], "elapsed_seconds":[row[2]], "details":crate::wire::encode_table(&[row[3].clone()], true)});
            let message = crate::wire::decode_results(&frame)?
                .pop()
                .ok_or("missing journal result")?;
            if self.accept(&message)? {
                messages.push(message);
            }
            consumed = end + 1;
        }
        self.pending.drain(..consumed);
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn complete_receipts_survive_partial_crash_and_duplicate_final_frame_then_are_removed() {
        let mut command =
            crate::wire::encode_steps(&[json!({"node_id":"a"}), json!({"node_id":"b"})]);
        let mut journal = ResultJournal::new(&mut command).unwrap();
        let path = journal.file.path().to_path_buf();
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer.write_all(b"[\"a\",\"step_completed\",0.1,{\"path\":\"/a.json\",\"format\":\"json\",\"size_bytes\":1}]\n[\"b\",\"step_").unwrap();
        let messages = journal.read_available().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!journal.accept(&messages[0]).unwrap());
        assert!(journal.read_available().unwrap().is_empty());
        writer
            .write_all(
                b"completed\",0.2,{\"path\":\"/b.json\",\"format\":\"json\",\"size_bytes\":1}]\n",
            )
            .unwrap();
        let messages = journal.read_available().unwrap();
        assert_eq!(messages.len(), 1);
        let altered = WorkerMessage::Blocked {
            node_id: "b".into(),
            reason: "changed".into(),
        };
        assert!(journal.accept(&altered).is_err());
        drop(writer);
        drop(journal);
        assert!(!path.exists());
    }

    #[test]
    fn malformed_metadata_and_unleased_receipts_are_errors() {
        for record in [
            b"[\"a\",\"step_completed\",0.1,123]\n".as_slice(),
            b"[\"other\",\"blocked\",null,{\"reason\":\"sensor\"}]\n".as_slice(),
        ] {
            let mut command = crate::wire::encode_steps(&[json!({"node_id":"a"})]);
            let mut journal = ResultJournal::new(&mut command).unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(journal.file.path())
                .unwrap()
                .write_all(record)
                .unwrap();
            assert!(journal.read_available().is_err());
        }
    }
}
