// App state container for orc v2.

use std::path::PathBuf;

pub struct App {
    pub project_dir: PathBuf,
    pub should_quit: bool,
    pub tick: u64,
}

impl App {
    pub fn new(project_dir: &str) -> Self {
        Self {
            project_dir: PathBuf::from(project_dir),
            should_quit: false,
            tick: 0,
        }
    }
}
