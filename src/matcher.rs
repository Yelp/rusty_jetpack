use crate::mappings::{
    ArtifactMapping, Mapping, ARCH_MAPPINGS, ARCH_MIN_MATCH, ARCH_MIN_MATCH_LEN, ARTIFACT_MAPPINGS,
    ARTIFACT_MIN_MATCH, ARTIFACT_MIN_MATCH_LEN, DATABIND_MAPPINGS, DATABIND_MIN_MATCH,
    DATABIND_MIN_MATCH_LEN, STAR_IMPORT_MATCH, SUPPORT_MAPPINGS, SUPPORT_MIN_MATCH,
    SUPPORT_MIN_MATCH_LEN,
};
use crossbeam_channel::{Receiver, Sender};
use memmap::MmapOptions;
use tempfile::NamedTempFile;

use std::borrow::Cow;
use std::fs;
use std::io::prelude::*;
use std::io::Result;
use std::ops::Deref;
use std::path::PathBuf;
use std::str;
use std::vec::Vec;

pub struct MatchInfo {
    pub matcher_id: usize,
    pub path: PathBuf,
    pub matches_found: usize,
    pub artifacts_found: Vec<&'static ArtifactMapping>,
    pub matched_star_imports: Vec<String>,
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
    /// All updates to the file are first done in memory for performance and in the extremely
    /// unlikely chance that another program is also accessing the file while we are modifying it.
    /// If there are changes that need to be made to the file, the new contents are written to a
    /// temporary file which is then persisted to disk and overwrites the original file with the
    /// same attributes.
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
            && (path.starts_with("buildSrc") || path.iter().count() <= 2);

        // Create a simple "buffer" to write to as we change lines
        let mut output = Vec::with_capacity(mmap.len());
        let mut replacements = 0;
        let mut artifacts: Vec<&'static ArtifactMapping> = Vec::new();
        let mut star_imports: Vec<String> = Vec::new();
        for line in str::from_utf8(source).unwrap().lines() {
            let (line_to_write, found_match, found_star_import) = self.find_match(&line);

            if found_match {
                // Count the number of replacements we've made
                replacements += 1;
            } else if found_star_import {
                star_imports.push(String::from(line));
            } else if check_artifact {
                // Only check for artifacts if nothing else matches since it's almost impossible an
                // artifact declaration would be on the same line as a package.
                if let Some(artifact) = self.find_artifact_match(&line) {
                    artifacts.push(artifact);
                }
            }
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
            matched_star_imports: star_imports,
        })
    }

    /// Given a line of code, return the potentially new line with androidx package names, if that
    /// replacement occurred, and if a star import that matched was found (which is not replacable).
    ///
    /// * `line` - The source code line
    fn find_match<'a>(&self, line: &'a str) -> (Cow<'a, str>, bool, bool) {
        // Do some simple heuristics to make sure it even worth checking the full set of patterns
        if line.trim().len() >= *SUPPORT_MIN_MATCH_LEN && SUPPORT_MIN_MATCH.is_match(line) {
            self.match_line_with_patterns(line, &*SUPPORT_MAPPINGS)
        } else if line.trim().len() >= *ARCH_MIN_MATCH_LEN && ARCH_MIN_MATCH.is_match(line) {
            self.match_line_with_patterns(line, &*ARCH_MAPPINGS)
        } else if line.trim().len() >= *DATABIND_MIN_MATCH_LEN && DATABIND_MIN_MATCH.is_match(line)
        {
            self.match_line_with_patterns(line, &*DATABIND_MAPPINGS)
        } else {
            (Cow::Borrowed(line), false, false)
        }
    }

    /// Given a line of code, return it with the first, if any, mapping found in the list of
    /// patterns to check, whether a replacement occurred, and if the line contained a star import.
    ///
    /// * `line` - The source code line
    /// * `patterns` - An array of patterns mapped to replacements
    fn match_line_with_patterns<'a, 'b>(
        &self,
        line: &'a str,
        patterns: &'b [Mapping],
    ) -> (Cow<'a, str>, bool, bool) {
        // Fast fail on star import that matches one of the migration minimum matchings
        if STAR_IMPORT_MATCH.is_match(line) {
            return (Cow::Borrowed(line), false, true);
        }

        for mapping in patterns.iter() {
            // Finish fast, it's very unlikely that there will be more than one match on a line
            if mapping.pattern.is_match(&line) {
                return (
                    mapping.pattern.replace(line, mapping.replacement.as_str()),
                    true,
                    false,
                );
            }
        }
        (Cow::Borrowed(line), false, false)
    }

    /// Given a line of code finds any artifacts that need to be updated. The matching
    /// ArtifactMapping will be returned if there are any.
    ///
    /// * `line` - The source code line
    fn find_artifact_match(&self, line: &str) -> Option<&'static ArtifactMapping> {
        if line.trim().len() >= *ARTIFACT_MIN_MATCH_LEN && ARTIFACT_MIN_MATCH.is_match(line) {
            for mapping in ARTIFACT_MAPPINGS.iter() {
                if mapping.pattern.find(line).is_some() {
                    return Some(&mapping);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::fs;
    use tempfile::Builder;

    // search_and_replace tests

    #[test]
    fn build_file_suggets_replacement() {
        // Set up the test file
        let mut file = Builder::new()
            .prefix("build")
            .suffix(".gradle")
            .tempfile_in("")
            .unwrap();
        file.write_all(
            "dependencies {
                implemenation 'com.android.support:support-compat:28.0.0'
            }\n"
            .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        // Run it
        let path_buf = file.path().to_path_buf();
        let path = path_buf.file_name().unwrap();
        let match_info = create_matcher()
            .search_and_replace(PathBuf::from(path))
            .unwrap();

        assert!(match_info.matches_found == 0);
        assert!(match_info.artifacts_found.len() == 1);
        assert!(match_info
            .artifacts_found
            .first()
            .unwrap()
            .replacement
            .contains("androidx.core:core:"));
    }

    #[test]
    fn xml_file_has_instance_replaced() {
        // Set up the test file
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            "<android.support.design.widget.CoordinatorLayout
                android:layout_width=\"match_parent\"
                android:layout_height=\"match_parent\">
            </android.support.design.widget.CoordinatorLayout>\n"
                .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        // Run it
        let path = file.path().to_path_buf();
        let match_info = create_matcher().search_and_replace(path.clone()).unwrap();

        let expected = "<androidx.coordinatorlayout.widget.CoordinatorLayout
                android:layout_width=\"match_parent\"
                android:layout_height=\"match_parent\">
            </androidx.coordinatorlayout.widget.CoordinatorLayout>\n";

        let contents = fs::read_to_string(path).unwrap();

        assert!(match_info.matches_found == 2);
        assert!(match_info.matched_star_imports.len() == 0);
        assert_eq!(contents, expected);
    }

    #[test]
    fn proguard_file_has_several_instances_replaced() {
        // Set up the test file
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            "-keep class android.support.v4.app.Fragment { *; }
            -keep android.support.design.drawable.DrawableUtils
            -dontwarn android.support.design.**
            -keepclassmembers,allowobfuscation class * extends android.arch.lifecycle.ViewModel\n"
                .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        // Run it
        let path = file.path().to_path_buf();
        let match_info = create_matcher().search_and_replace(path.clone()).unwrap();

        let expected = "-keep class androidx.fragment.app.Fragment { *; }
            -keep com.google.android.material.drawable.DrawableUtils
            -dontwarn android.support.design.**
            -keepclassmembers,allowobfuscation class * extends androidx.lifecycle.ViewModel\n";

        let contents = fs::read_to_string(path).unwrap();

        assert!(match_info.matches_found == 3);
        assert!(match_info.matched_star_imports.len() == 1);
        assert_eq!(
            match_info.matched_star_imports.first().unwrap(),
            "            -dontwarn android.support.design.**"
        );
        assert_eq!(contents, expected);
    }

    #[test]
    fn java_source_file_has_several_instances_replaced() {
        // Set up the test file
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            "package com.example.java;

            public class ExampleActivity extends android.support.v4.app.ActivityCompat {
                @android.support.annotation.NonNull
                public void doSomething(android.support.constraint.ConstraintSet set) { }
            }\n"
            .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        // Run it
        let path = file.path().to_path_buf();
        let match_info = create_matcher().search_and_replace(path.clone()).unwrap();

        let expected = "package com.example.java;

            public class ExampleActivity extends androidx.core.app.ActivityCompat {
                @androidx.annotation.NonNull
                public void doSomething(androidx.constraintlayout.widget.ConstraintSet set) { }
            }\n";
        let contents = fs::read_to_string(path).unwrap();

        assert!(match_info.matches_found == 3);
        assert!(match_info.matched_star_imports.len() == 0);
        assert_eq!(contents, expected);
    }

    #[test]
    fn kotlin_source_file_has_several_instances_replaced() {
        // Set up the test file
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(
            "package com.example.kotlin
            import com.example.package
            import android.arch.lifecycle.ViewModel
            import android.databinding.*

            /**
             * Might or might not use [android.databinding.ObservableInt].
             */
            class Example {
                @set:android.support.annotation.VisibleForTesting
                var something: String? = null
            }\n"
            .as_bytes(),
        )
        .unwrap();
        file.flush().unwrap();

        // Run it
        let path = file.path().to_path_buf();
        let match_info = create_matcher().search_and_replace(path.clone()).unwrap();

        let expected = "package com.example.kotlin
            import com.example.package
            import androidx.lifecycle.ViewModel
            import android.databinding.*

            /**
             * Might or might not use [androidx.databinding.ObservableInt].
             */
            class Example {
                @set:androidx.annotation.VisibleForTesting
                var something: String? = null
            }\n";
        let contents = fs::read_to_string(path).unwrap();

        assert!(match_info.matches_found == 3);
        assert!(match_info.matched_star_imports.len() == 1);
        assert_eq!(contents, expected);
    }

    // find_match/match_line_with_patterns tests

    #[test]
    fn xml_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "</android.support.constraint.ConstraintLayout>";
        let new_line = "</androidx.constraintlayout.widget.ConstraintLayout>";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star)
    }

    #[test]
    fn annotation_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "        @set:android.support.annotation.VisibleForTesting";
        let new_line = "        @set:androidx.annotation.VisibleForTesting";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star)
    }

    #[test]
    fn kdoc_comment_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "* uses [android.arch.lifecycle.ViewModel] to do stuff.";
        let new_line = "* uses [androidx.lifecycle.ViewModel] to do stuff.";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star);
    }

    #[test]
    fn proguard_matching_line_is_replaced() {
        let matcher = create_matcher();
        let line = "-keep public class * extends android.support.v4.app.Fragment";
        let new_line = "-keep public class * extends androidx.fragment.app.Fragment";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star);
    }

    #[test]
    fn import_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "import android.support.animation.Force;";
        let new_line = "import androidx.dynamicanimation.animation.Force;";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star)
    }

    #[test]
    fn field_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "val page: android.arch.paging.PageResult? = null";
        let new_line = "val page: androidx.paging.PageResult? = null";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star)
    }

    #[test]
    fn function_param_matching_is_replaced() {
        let matcher = create_matcher();
        let line = "public void (android.databinding.Observable obs) {";
        let new_line = "public void (androidx.databinding.Observable obs) {";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, new_line);
        assert!(changed);
        assert!(!found_star)
    }

    #[test]
    fn too_short_of_line_is_ignored() {
        let matcher = create_matcher();
        let line = "}";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, line);
        assert!(!changed);
        assert!(!found_star)
    }

    #[test]
    fn star_import_gives_back_same_line() {
        let matcher = create_matcher();
        let line = "import android.support.annotation.*";
        let (replacement, changed, found_star) = matcher.find_match(line);

        assert_eq!(replacement, line);
        assert!(!changed);
        assert!(found_star)
    }

    // find_artifact_match tests

    #[test]
    fn artifact_line_returns_mapping() {
        let matcher = create_matcher();
        let line = r#"    implemenation "com.android.support:car:28.0.0""#;

        assert!(matcher.find_artifact_match(line).is_some())
    }

    #[test]
    fn artifact_line_with_single_quote_returns_mapping() {
        let matcher = create_matcher();
        let line = "    implemenation 'com.android.support:car:$version'";

        assert!(matcher.find_artifact_match(line).is_some())
    }

    #[test]
    fn false_positive_artifact_line_returns_none() {
        let matcher = create_matcher();
        let line = r#"val LIB = "com.example.android.support:lib:$VERSION""#;

        assert!(matcher.find_artifact_match(line).is_none())
    }

    fn create_matcher() -> Matcher {
        let (tx, _) = unbounded();

        Matcher { id: 0, tx }
    }
}
