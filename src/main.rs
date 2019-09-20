use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use lazy_static::lazy_static;
use matcher::MatchInfo;
use structopt::StructOpt;

use std::cmp::min;
use std::io::Result;
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

mod finder;
mod mappings;
mod matcher;

lazy_static! {
    static ref MAX_THREADS: usize = num_cpus::get();
}

#[derive(Debug, StructOpt)]
#[structopt(name = "rusty_jetpack")]
/// A fast and simple tool to assist in migrating to AndroidX.
///
/// rusty_jetpack only seeks to replace all occurrences of support library classes with their
/// updated Androidx locations. rusty_jetpack makes no attempts to update artifact and library
/// updates in gradle files or solve any other issues that might arise during the migration.
///
/// `git ls-files` is used to determine what files will be touched so ignored files and submodules
/// will not be impacted.
///
/// Class mapping information: https://developer.android.com/jetpack/androidx/migrate#class_mappings
struct Opt {
    /// Silences all output to stdout
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,

    /// Max number of threads to execute with
    #[structopt(long = "threads")]
    threads: Option<usize>,
}

fn main() {
    let start = Instant::now();

    // Parse the cli options and start execution
    let opts = Opt::from_args();
    let rx_matcher = start_execution(&opts);
    listen_for_messages(start, &opts, rx_matcher);
}

/// Starts the execution of the matchers by creating a matcher per number of specified threads or
/// the max number of threads available. It then spawns a finder to feed the matchers with files
/// that can be migrated.
///
/// * opts - The CLI options passed in
/// Returns the Receiver listening to the unbounded channel the matchers will respond on
fn start_execution(opts: &Opt) -> Receiver<Result<MatchInfo>> {
    let num_threads = min(opts.threads.unwrap_or(*MAX_THREADS), *MAX_THREADS);

    if !opts.quiet {
        println!("Starting with {} threads...", num_threads);
    }

    // Set up the channel for the matchers to report their progress. The transmitters will be
    // cloned so they all use one channel the main thread can listen on.
    let (tx_matcher, rx_matcher) = unbounded();
    let mut matcher_txs: Vec<Sender<PathBuf>> = Vec::new();

    for i in 0..num_threads {
        let (tx_in, rx_in) = unbounded();
        matcher_txs.push(tx_in);
        let tx_main_clone = tx_matcher.clone();

        // Spawn a new thread and kick off a matcher
        thread::Builder::new()
            .name("matcher".to_string())
            .spawn(move || {
                matcher::Matcher::new(i, tx_main_clone).run(rx_in);
            })
            .unwrap();
    }
    // Drop this thread's transmitter so the channel doesn't remain open even when all the other
    // threads have finished.
    drop(tx_matcher);

    // Start up a finder, still use channels despite it not being threaded.
    let (tx_finder, rx_finder) = bounded(1);
    finder::Finder::new().find_paths(matcher_txs, tx_finder);
    let message = rx_finder.recv().unwrap();
    if !opts.quiet {
        println!(
            "Found {} files (.gradle, .gradle.kts, .java, .kt, .pro, .xml)...",
            message.total_files_found
        );
    }

    rx_matcher
}

/// Listens to the given Receiver for MatchInfo messages, printing useful output to stdout and
/// stderr if necessary.
///
/// * start - The instant the program started
/// * opts - The CLI options passed in
/// * rx_matcher - The Receiver to listen to
fn listen_for_messages(start: Instant, opts: &Opt, rx_matcher: Receiver<Result<MatchInfo>>) {
    let mut num_files_changed = 0;
    let mut num_changes = 0;
    while let Ok(message) = rx_matcher.recv() {
        match message {
            Ok(match_info) => {
                if match_info.matches_found > 0 {
                    num_changes += match_info.matches_found;
                    num_files_changed += 1;
                }

                // Print out any star imports found
                if !match_info.matched_star_imports.is_empty() {
                    eprintln!(
                        "Found {} star import(s) that must be updated in {}:",
                        match_info.matched_star_imports.len(),
                        match_info.path.to_string_lossy()
                    );

                    match_info
                        .matched_star_imports
                        .iter()
                        .for_each(|line| eprintln!("  * {}", line));
                }

                // Print out any artifacts found that need to be updated
                if !match_info.artifacts_found.is_empty() {
                    // Print to error so it can't be ignored
                    eprintln!(
                        "Found {} artifact(s) that must be updated in {}:",
                        match_info.artifacts_found.len(),
                        match_info.path.to_string_lossy()
                    );
                    match_info.artifacts_found.iter().for_each(|mapping| {
                        // The longest artifact is 59 characters so pad for that
                        eprintln!(
                            "  * {:<60}=> {}",
                            mapping.pattern.as_str(),
                            mapping.replacement
                        )
                    });
                }
            }
            Err(e) => eprintln!("{}", e),
        };
    }

    // Report final stats of the run
    let duration = start.elapsed();
    if !opts.quiet {
        println!(
            "Replaced {} occurrence(s) in {} file(s) in {}.{}s!",
            num_changes,
            num_files_changed,
            duration.as_secs(),
            duration.subsec_millis() / 10
        );
    }
}
