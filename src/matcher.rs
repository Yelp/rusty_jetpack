use crossbeam_channel::{Receiver, Sender};
use lazy_static::lazy_static;
use memmap::MmapOptions;
use regex::{Regex, RegexSet};
use serde::Deserialize;
use serde_regex;
use tempfile::NamedTempFile;

use std::borrow::Cow;
use std::fs;
use std::io::prelude::*;
use std::io::Result;
use std::ops::Deref;
use std::path::PathBuf;
use std::str;
use std::vec::Vec;

// Include the csv mapping files. They are separated by the first difference in their package
// names: android.support, android.databinding, and android.arch. While not a massive difference
// in performance, separating them out does trim the total amount of regex that needs to be
// searched per line. This is especially true with the support library given it is the majority of
// package name changes and used the most frequently in files.
const SUPPORT_MAPPING_CSV: &str = include_str!("../android_support_mappings.csv");
const DATABIND_MAPPING_CSV: &str = include_str!("../android_databinding_mappings.csv");
const ARCH_MAPPING_CSV: &str = include_str!("../android_arch_mappings.csv");

// Also include the artifact mappings so it's easy to know what packages you actually need to
// replace. Since it's quite a bit more complex than just find and replace for the artifacts,
// printing out the ones actually used in the project is good enough.
const ARTIFACT_MAPPING_CSV: &str = include_str!("../android_artifact_mappings.csv");

#[derive(Debug, Deserialize)]
struct Mapping {
    #[serde(with = "serde_regex", rename = "Support Library class")]
    pattern: Regex,
    #[serde(rename = "Android X class")]
    replacement: String,
}

#[derive(Debug, Deserialize)]
pub struct ArtifactMapping {
    #[serde(with = "serde_regex", rename = "Old build artifact")]
    pub pattern: Regex,
    #[serde(rename = "AndroidX build artifact")]
    pub replacement: String,
}

// Compiling the regex patterns is decently expensive and since they are used across all possible
// threads they are set up as static references so they are only created once.
//
// Some simple heuristics are also done to short circuit searching all the patterns in an attempt
// to speed up performance. They are simply just the minimum string length to match as well as the
// minimum pattern to even begin checking all the patterns. For example, if the minimum length of a
// pattern in the support library is 30 characters we shouldn't even bother searching the line if
// it is only 25 characters long. Similarly, the minimum match for the support library changes is
// "android.support" and if that isn't in the line then no other support library patterns will
// match either.
lazy_static! {
    // Regex and checks for support library changes
    static ref SUPPORT_MAPPINGS: Vec<Mapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(SUPPORT_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: Mapping = result.unwrap();
            vec.push(mapping)
        }
        // Sort with longest pattern first. This prevents collisions and false mappings in cases
        // like "Toolbar" and "ToolbarWidgetWrapper". Sorting is in theory less expensive to do
        // once then have a more complex pattern that checks for boundaries.
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    static ref SUPPORT_MIN_MATCH_LEN: usize =
        SUPPORT_MAPPINGS.last().unwrap().pattern.as_str().len();
    // Check most common boundaries to make sure false positives aren't found, e.g.
    // 'com.example.android.support'
    // Known bounderies:
    // - " ": start of a new "word" and likely never a false postive
    // - <: xml start tag
    // - /: xml end tag
    // - "/': start of strings or dependencies
    // - @: annotations
    // - ;: likely in lint baseline files for representing "<" or ">"
    // - (: full path as a parameter to a function
    static ref SUPPORT_MIN_MATCH: Regex = Regex::new(r#"[ </"@';(]android\.support"#).unwrap();

    // Regex and checks for databinding changes
    static ref DATABIND_MAPPINGS: Vec<Mapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(DATABIND_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: Mapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    static ref DATABIND_MIN_MATCH_LEN: usize =
        DATABIND_MAPPINGS.last().unwrap().pattern.as_str().len();
    static ref DATABIND_MIN_MATCH: Regex = Regex::new(r#"[ </"@';(]android\.databinding"#).unwrap();

    // Regex and checks for architecture changes
    static ref ARCH_MAPPINGS: Vec<Mapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(ARCH_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: Mapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    static ref ARCH_MIN_MATCH_LEN: usize = ARCH_MAPPINGS.last().unwrap().pattern.as_str().len();
    static ref ARCH_MIN_MATCH: Regex = Regex::new(r#"[ </"@';(]android\.arch"#).unwrap();

    // Regex anc checks for artifact changes
    static ref ARTIFACT_MAPPINGS: Vec<ArtifactMapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(ARTIFACT_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: ArtifactMapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    static ref ARTIFACT_MIN_MATCH_LEN: usize =
        ARTIFACT_MAPPINGS.last().unwrap().pattern.as_str().len();
    static ref ARTIFACT_MIN_MATCH: RegexSet = RegexSet::new(&[
        r#"["'']com\.android\.support[a-z\.]*:"#,
        r#"["']android\.arch[a-z\.]*:"#
    ]).unwrap();
}

pub struct MatchInfo {
    pub matcher_id: usize,
    pub path: PathBuf,
    pub matches_found: usize,
    pub artifacts_found: Vec<&'static ArtifactMapping>,
}

pub struct Matcher {
    id: usize,
    tx: Sender<Result<MatchInfo>>,
}

impl Matcher {
    /// Create a Matcher
    ///
    /// * `id` - The thread number of the matcher
    /// * `tx` - The transmitter to send information with
    pub fn new(id: usize, tx: Sender<Result<MatchInfo>>) -> Self {
        Matcher { id, tx }
    }

    /// Start the matcher.
    ///
    /// The matcher will wait on receiving a file to operate on from the given receiver, and will
    /// then finish once the receiver channel signals it is both empty and disconnected.
    /// Information on completion of checking a file will be sent via the Matcher's transmitter.
    ///
    /// * `rx` - The receiver to listen to for files
    pub fn run(self, rx: Receiver<PathBuf>) {
        while let Ok(path) = rx.recv() {
            let _ = self.tx.send(self.search_and_replace(path));
        }
    }

    /// Find and replace all occurrences of androidx migrated name spaces within the given file.
    ///
    /// The given file path will be opened as a memory mapped file to improve performance given
    /// that most source code files are less than 1 MB. Each line is then scanned for any of the
    /// migrated package names and updated to the new androidx package name.
    ///
    /// All updates to the file are first done in a temporary file in the extremely unlikely chance
    /// that another program is also accessing the file while we are modifying it. If no changes
    /// are made, the temporary file is never persisted. Otherwise, the temporary is persisted to
    /// disk and overwrites the original file with the same attributes.
    ///
    /// * `path` - The file path to operate on
    /// Returns a MatchInfo with information about any matches in the line if successful
    fn search_and_replace(&self, path: PathBuf) -> Result<MatchInfo> {
        let file = fs::File::open(&path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        let source = mmap.deref();

        // To make sure not too much performance is lost finding artifacts assume that artifacts
        // can only be located in the buildSrc directory, a top level file in the project or one
        // level down for module's build files.
        let check_artifact = path.extension().map_or(false, |x| x != "xml" && x != "pro")
            && (path.starts_with("buildSrc")
                || path.parent().map_or(true, |x| x.parent().is_none()));

        // Create a simple "buffer" to write to as we change lines
        let mut output = Vec::with_capacity(mmap.len());
        let mut replacements = 0;
        let mut artifacts: Vec<&'static ArtifactMapping> = Vec::new();
        for line in str::from_utf8(source).unwrap().lines() {
            let (line_to_write, found_match) = self.find_match(&line);

            if found_match {
                // Count the number of replacements we've made
                replacements += 1;
            } else if check_artifact {
                // Only check if for artifacts if nothing else matches since it's almost impossible
                // an artifact declaration would be on the same line as a package.
                if let Some(artifact) = self.find_artifact_match(&line) {
                    artifacts.push(artifact);
                }
            };
            // Write out to the buffer
            writeln!(output, "{}", &line_to_write)?;
        }

        // Make sure to only create the temp file if anything actually changed
        if replacements > 0 {
            let mut tempfile = NamedTempFile::new_in(&path.parent().unwrap_or(&path))?;

            // Write out the changes to disk
            tempfile.write_all(&output)?;
            tempfile.flush()?;

            // Persist the tempfile and override the original
            let real_path = fs::canonicalize(&path)?;
            let metadata = fs::metadata(&real_path)?;
            fs::set_permissions(tempfile.path(), metadata.permissions())?;
            tempfile.persist(&real_path)?;
        }

        Ok(MatchInfo {
            matcher_id: self.id,
            path,
            matches_found: replacements,
            artifacts_found: artifacts,
        })
    }

    /// Given a line of code, return the potentially new line with androidx package names and if
    /// that replacement occurred.
    ///
    /// * `line` - The source code line
    fn find_match<'a>(&self, line: &'a str) -> (Cow<'a, str>, bool) {
        // Do some simple heuristics to make sure it even worth checking the full set of patterns
        if line.len() >= *SUPPORT_MIN_MATCH_LEN && SUPPORT_MIN_MATCH.is_match(line) {
            self.match_line_with_patterns(line, &*SUPPORT_MAPPINGS)
        } else if line.len() >= *ARCH_MIN_MATCH_LEN && ARCH_MIN_MATCH.is_match(line) {
            self.match_line_with_patterns(line, &*ARCH_MAPPINGS)
        } else if line.len() >= *DATABIND_MIN_MATCH_LEN && DATABIND_MIN_MATCH.is_match(line) {
            self.match_line_with_patterns(line, &*DATABIND_MAPPINGS)
        } else {
            (Cow::Borrowed(line), false)
        }
    }

    /// Given a line of code, return it with the first, if any, mapping found in the list of
    /// patterns to check and whether a replacement occurred.
    ///
    /// * `line` - The source code line
    /// * `patterns` - An array of patterns mapped to replacements
    fn match_line_with_patterns<'a, 'b>(
        &self,
        line: &'a str,
        patterns: &'b [Mapping],
    ) -> (Cow<'a, str>, bool) {
        for mapping in patterns.iter() {
            // Finish fast, it's very unlikely that there will be more than one match on a line
            if mapping.pattern.find(&line).is_some() {
                return (
                    mapping.pattern.replace(line, mapping.replacement.as_str()),
                    true,
                );
            }
        }
        (Cow::Borrowed(line), false)
    }

    /// Given a line of code finds any artifacts that need to be updated. The matching
    /// ArtifactMapping will be returned if there are any.
    ///
    /// * `line` - The source code line
    fn find_artifact_match(&self, line: &str) -> Option<&'static ArtifactMapping> {
        if line.len() >= *ARTIFACT_MIN_MATCH_LEN || ARTIFACT_MIN_MATCH.is_match(line) {
            for mapping in ARTIFACT_MAPPINGS.iter() {
                if mapping.pattern.find(line).is_some() {
                    return Some(&mapping);
                }
            }
        }

        None
    }
}
