//! Session log: one JSONL file per server process, appended as tools are
//! called. It is the evidence base for crystallising patterns, so it must
//! never get in the way of a tool call: IO errors are swallowed after one
//! warning on stderr.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

#[derive(Clone, Copy)]
pub enum Kind {
    Start,
    Render,
    Answer,
    Pending,
    Refusal,
    Error,
    Close,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Start => "start",
            Kind::Render => "render",
            Kind::Answer => "answer",
            Kind::Pending => "pending",
            Kind::Refusal => "refusal",
            Kind::Error => "error",
            Kind::Close => "close",
        }
    }
}

pub struct Log {
    dir: PathBuf,
    pane: Option<String>,
    file: Option<File>,
    seq: u64,
    warned: bool,
}

impl Log {
    /// `$VOLVELLE_LOG_DIR`, else `$XDG_STATE_HOME/volvelle/sessions`, else
    /// `~/.local/state/volvelle/sessions`. Nothing is created until the first
    /// record.
    pub fn from_env() -> Self {
        let var = |k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let dir = var("VOLVELLE_LOG_DIR")
            .or_else(|| var("XDG_STATE_HOME").map(|s| s.join("volvelle/sessions")))
            .or_else(|| var("HOME").map(|h| h.join(".local/state/volvelle/sessions")))
            .unwrap_or_else(|| std::env::temp_dir().join("volvelle/sessions"));
        Self::new(dir, std::env::var("HERDR_PANE_ID").ok())
    }

    pub fn new(dir: PathBuf, pane: Option<String>) -> Self {
        Self {
            dir,
            pane,
            file: None,
            seq: 0,
            warned: false,
        }
    }

    /// Appends one record. `body` holds extra fields such as `spec`, `answer`
    /// or `message`.
    pub fn record(&mut self, kind: Kind, view: Option<&str>, body: &[(&str, Value)]) {
        let mut rec = Map::new();
        rec.insert("kind".into(), kind.as_str().into());
        if let Some(view) = view {
            rec.insert("view".into(), view.into());
        }
        for (k, v) in body {
            rec.insert((*k).into(), v.clone());
        }
        if let Err(e) = self.append(rec)
            && !self.warned
        {
            self.warned = true;
            eprintln!("volvelle-mcp: session log disabled: {e}");
        }
    }

    fn append(&mut self, rec: Map<String, Value>) -> std::io::Result<()> {
        if self.file.is_none() {
            fs::create_dir_all(&self.dir)?;
            let path = self
                .dir
                .join(format!("{}-{}.jsonl", now_ms(), std::process::id()));
            self.file = Some(File::options().create_new(true).append(true).open(path)?);
            let start = json!({
                "kind": Kind::Start.as_str(),
                "version": env!("CARGO_PKG_VERSION"),
                "pane": self.pane,
            });
            self.write_line(start.as_object().cloned().unwrap_or_default())?;
        }
        self.write_line(rec)
    }

    fn write_line(&mut self, mut rec: Map<String, Value>) -> std::io::Result<()> {
        rec.insert("seq".into(), self.seq.into());
        rec.insert("ts_ms".into(), now_ms().into());
        let mut line = Value::Object(rec).to_string();
        line.push('\n');
        let file = self.file.as_mut().expect("opened by append");
        file.write_all(line.as_bytes())?;
        file.flush()?;
        self.seq += 1;
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "volvelle-log-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    fn lines(dir: &PathBuf) -> Vec<Value> {
        let mut files: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        let text = fs::read_to_string(files.pop().unwrap()).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn nothing_is_created_until_the_first_record() {
        let dir = temp_dir("lazy");
        let _log = Log::new(dir.clone(), None);
        assert!(!dir.exists());
    }

    #[test]
    fn records_follow_a_start_record_with_increasing_seq() {
        let dir = temp_dir("seq");
        let mut log = Log::new(dir.clone(), Some("p1".into()));
        let spec = json!({"type": "button", "id": "go", "label": "Go"});
        log.record(
            Kind::Render,
            Some("deploy"),
            &[("spec", spec.clone()), ("interactive", true.into())],
        );
        log.record(Kind::Pending, Some("deploy"), &[]);
        let answer = json!({"type": "submit", "view": "deploy", "button": "go", "values": {}});
        log.record(Kind::Answer, Some("deploy"), &[("answer", answer.clone())]);
        log.record(
            Kind::Refusal,
            Some("deploy"),
            &[("message", "already answered".into())],
        );
        log.record(Kind::Error, None, &[("message", "bad spec".into())]);
        log.record(Kind::Close, None, &[]);

        let recs = lines(&dir);
        let kinds: Vec<_> = recs.iter().map(|r| r["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "start", "render", "pending", "answer", "refusal", "error", "close"
            ]
        );
        for (i, r) in recs.iter().enumerate() {
            assert_eq!(r["seq"], i);
            assert!(r["ts_ms"].as_u64().unwrap() > 0);
        }
        assert_eq!(recs[0]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(recs[0]["pane"], "p1");
        assert_eq!(recs[1]["spec"], spec);
        assert_eq!(recs[1]["interactive"], true);
        assert_eq!(recs[3]["answer"], answer);
        assert_eq!(recs[4]["message"], "already answered");
        assert!(recs[5].get("view").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn io_errors_are_swallowed() {
        let dir = temp_dir("blocked");
        fs::write(&dir, "a file where the directory should be").unwrap();
        let mut log = Log::new(dir.clone(), None);
        log.record(Kind::Close, None, &[]);
        log.record(Kind::Close, None, &[]);
        assert!(log.warned);
        fs::remove_file(dir).unwrap();
    }
}
