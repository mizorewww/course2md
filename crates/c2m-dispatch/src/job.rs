//! A running task's place on disk and its control.

use std::path::PathBuf;

/// A running task's layout and control, kept apart from its settings: the
/// resolved [`PipelineConfig`] never changes, while each entry point decides
/// where the task works.
#[derive(Clone)]
pub struct Job {
    /// The work directory the task owns: checkpoints, receipts, media.
    pub dir: PathBuf,
    /// The request ledger and control file every safe boundary checks.
    pub control: crate::dispatch::TaskControl,
}

impl Job {
    /// A job without a task context: nothing pauses it and requests are not recorded.
    pub fn detached(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            control: Default::default(),
        }
    }
    pub fn media_path(&self) -> PathBuf {
        self.dir.join("media.mp4")
    }
    pub fn audio_path(&self) -> PathBuf {
        self.dir.join("audio.wav")
    }
    pub fn frames_dir(&self) -> PathBuf {
        self.dir.join("frames")
    }
    pub fn timeline_path(&self) -> PathBuf {
        self.dir.join("timeline.jsonl")
    }
    pub fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }
}
