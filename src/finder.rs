use crossbeam_channel::Sender;
use std::path::PathBuf;
use std::process::Command;

pub struct FinderInfo {
    pub total_files_found: usize,
    pub num_files_per_matcher: Vec<usize>,
}

pub struct Finder;

impl Finder {
    pub fn new() -> Self {
        Finder
    }

    /// Find all applicable files and transmit them with the given list of channels.
    ///
    /// * `matcher_txs` - A vector of transmitters for the different matcher threads
    /// * `tx_info` - A trasmitter back to the main thread to report info
    pub fn find_paths(&self, matcher_txs: Vec<Sender<PathBuf>>, tx_info: Sender<FinderInfo>) {
        // Get all the files from git so we don't have to worry about going through files that the
        // project doesn't even care about, e.g. files in the "build" directory.
        let output = Command::new("git")
            .arg("ls-files")
            .output()
            .expect("Failed to execute `git ls-files`! Are you in a git repo?")
            .stdout;

        let mut files_found = 0;
        let mut matcher_thread = 0;
        let mut files_per_thread: Vec<usize> = vec![0; matcher_txs.len()];
        String::from_utf8(output)
            .unwrap()
            .lines()
            .filter(|f| {
                // Filter on non-binary files that will actually contain anything to change
                f.ends_with(".kt")
                    || f.ends_with(".java")
                    || f.ends_with(".xml")
                    || f.ends_with(".pro")
                    || f.ends_with(".gradle")
            })
            .map(PathBuf::from)
            .for_each(|f| {
                // Send the path in a matcher's channel
                matcher_txs[matcher_thread].send(f).unwrap();
                // Share the love across all the threads
                files_per_thread[matcher_thread] += 1;
                matcher_thread = if matcher_thread == matcher_txs.len() - 1 {
                    0
                } else {
                    matcher_thread + 1
                };
                files_found += 1;
            });
        let _ = tx_info.send(FinderInfo {
            total_files_found: files_found,
            num_files_per_matcher: files_per_thread,
        });
    }
}
