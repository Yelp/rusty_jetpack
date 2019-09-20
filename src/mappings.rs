use lazy_static::lazy_static;
use regex::{Regex, RegexSet};
use serde::Deserialize;
use serde_regex;

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
pub struct Mapping {
    #[serde(with = "serde_regex", rename = "Support Library class")]
    pub pattern: Regex,
    #[serde(rename = "Android X class")]
    pub replacement: String,
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
    pub static ref SUPPORT_MAPPINGS: Vec<Mapping> = {
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
    pub static ref SUPPORT_MIN_MATCH_LEN: usize =
        SUPPORT_MAPPINGS.last().unwrap().pattern.as_str().len();
    // Check most common boundaries to make sure false positives aren't found, e.g.
    // 'com.example.android.support'
    // Known bounderies:
    // - " ": start of a new "word" and likely never a false postive
    // - <: xml start tag
    // - /: xml end tag
    // - "/': start of strings or dependencies
    // - :/@: annotations
    // - ;: likely in lint baseline files for representing "<" or ">"
    // - (: full path as a parameter to a function
    // - [: kdoc link
    pub static ref SUPPORT_MIN_MATCH: Regex = Regex::new(r#"[ </"@:\[';(]android\.support"#).unwrap();

    // Regex and checks for databinding changes
    pub static ref DATABIND_MAPPINGS: Vec<Mapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(DATABIND_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: Mapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    pub static ref DATABIND_MIN_MATCH_LEN: usize =
        DATABIND_MAPPINGS.last().unwrap().pattern.as_str().len();
    pub static ref DATABIND_MIN_MATCH: Regex = Regex::new(r#"[ </"@:\[';(]android\.databinding"#).unwrap();

    // Regex and checks for architecture changes
    pub static ref ARCH_MAPPINGS: Vec<Mapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(ARCH_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: Mapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    pub static ref ARCH_MIN_MATCH_LEN: usize = ARCH_MAPPINGS.last().unwrap().pattern.as_str().len();
    pub static ref ARCH_MIN_MATCH: Regex = Regex::new(r#"[ </"@:\[';(]android\.arch"#).unwrap();

    // Regex and checks for artifact changes
    pub static ref ARTIFACT_MAPPINGS: Vec<ArtifactMapping> = {
        let mut vec = Vec::new();
        let mut rdr = csv::Reader::from_reader(ARTIFACT_MAPPING_CSV.as_bytes());
        for result in rdr.deserialize() {
            let mapping: ArtifactMapping = result.unwrap();
            vec.push(mapping)
        }
        vec.sort_unstable_by(|a, b| b.pattern.as_str().len().cmp(&a.pattern.as_str().len()));
        vec
    };
    pub static ref ARTIFACT_MIN_MATCH_LEN: usize =
        ARTIFACT_MAPPINGS.last().unwrap().pattern.as_str().len();
    pub static ref ARTIFACT_MIN_MATCH: RegexSet = RegexSet::new(&[
        r#"["']com\.android\.support[a-z\.]*:"#,
        r#"["']android\.arch[a-z\.]*:"#
    ]).unwrap();

    // Match star import statements and proguard glob statements
    pub static ref STAR_IMPORT_MATCH: Regex = Regex::new(r#"\.\*[;]?"#).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_mappings_is_sorted_longest_to_shortest() {
        let mut size = SUPPORT_MAPPINGS.first().unwrap().pattern.as_str().len();

        for mapping in SUPPORT_MAPPINGS.iter() {
            assert!(mapping.pattern.as_str().len() <= size);
            size = mapping.pattern.as_str().len();
        }
        assert_eq!(*SUPPORT_MIN_MATCH_LEN, size)
    }

    #[test]
    fn databind_mappings_is_sorted_longest_to_shortest() {
        let mut size = DATABIND_MAPPINGS.first().unwrap().pattern.as_str().len();

        for mapping in DATABIND_MAPPINGS.iter() {
            assert!(mapping.pattern.as_str().len() <= size);
            size = mapping.pattern.as_str().len();
        }
        assert_eq!(*DATABIND_MIN_MATCH_LEN, size)
    }

    #[test]
    fn arch_mappings_is_sorted_longest_to_shortest() {
        let mut size = ARCH_MAPPINGS.first().unwrap().pattern.as_str().len();

        for mapping in ARCH_MAPPINGS.iter() {
            assert!(mapping.pattern.as_str().len() <= size);
            size = mapping.pattern.as_str().len();
        }
        assert_eq!(*ARCH_MIN_MATCH_LEN, size)
    }

    #[test]
    fn artifact_mappings_is_sorted_longest_to_shortest() {
        let mut size = ARTIFACT_MAPPINGS.first().unwrap().pattern.as_str().len();

        for mapping in ARTIFACT_MAPPINGS.iter() {
            assert!(mapping.pattern.as_str().len() <= size);
            size = mapping.pattern.as_str().len();
        }
        assert_eq!(*ARTIFACT_MIN_MATCH_LEN, size)
    }

    #[test]
    fn support_import_statements_are_matched() {
        let line = "import android.support.animation.DynamicAnimation";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_full_path_annotation_matched() {
        let line = "@android.support.annotation.AnyRes";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_full_path_kotlin_annotation_matched() {
        let line = "@set:android.support.annotation.VisibleForTesting";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_xml_close_tag_matched() {
        let line = "</android.support.design.card.MaterialCardView>";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_xml_tag_matched() {
        let line = "<android.support.design.card.MaterialCardView>";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_full_path_param_matched() {
        let line = "public void example(android.support.v4.widget.TextViewCompat x) {";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_proguard_line_matched() {
        let line = "-keep public class * extends android.support.v4.app.Fragment";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_kdoc_comment_link_matched() {
        let line = "* uses [android.support.v4.app.Fragment] to do stuff.";
        assert!(SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_false_positive_not_matched() {
        let line = "import com.example.android.support;";
        assert!(!SUPPORT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_import_statements_are_matched() {
        let line = "import android.databinding.adapters.AbsListViewBindingAdapter";
        assert!(DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_full_path_annotation_matched() {
        let line = "@android.databinding.BindingAdapter";
        assert!(DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_full_path_kotlin_annotation_matched() {
        let line = "@get:android.databinding.Bindable";
        assert!(DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_full_path_param_matched() {
        let line = "public void example(android.databinding.ObservableInt x) {";
        assert!(DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_kdoc_comment_link_matched() {
        let line = "* uses [android.databinding.ObservableInt] to do stuff.";
        assert!(DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn databinding_false_positive_not_matched() {
        let line = "import com.example.android.databinding;";
        assert!(!DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_import_statements_are_matched() {
        let line = "import android.arch.persistence.room.ForeignKey";
        assert!(ARCH_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_full_path_annotation_matched() {
        let line = "@android.arch.persistence.room.ForeignKey";
        assert!(ARCH_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_full_path_kotlin_annotation_matched() {
        let line = "@param:android.arch.persistence.room.ForeignKey";
        assert!(ARCH_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_full_path_param_matched() {
        let line = "public void example(android.arch.lifecycle.ViewModel x) {";
        assert!(ARCH_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_kdoc_comment_link_matched() {
        let line = "* uses [android.arch.lifecycle.ViewModel] to do stuff.";
        assert!(ARCH_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_false_positive_not_matched() {
        let line = "import com.example.android.arch;";
        assert!(!DATABIND_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_artifact_in_buildsrc_file_matched() {
        let line = r#"val CORE_COMMON = "android.arch.core:common:$VERSION""#;
        assert!(ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_artifact_in_build_file_matched() {
        let line = "compileOnly('android.arch.core:common:$VERSION')";
        assert!(ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn arch_artifact_false_positive_not_matched() {
        let line = "implementation('com.example.android.arch:example-lib:1.0.0')";
        assert!(!ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_artifact_in_buildsrc_file_matched() {
        let line = r#"val CORE_COMMON = "com.android.support:collections:$VERSION""#;
        assert!(ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_artifact_in_build_file_matched() {
        let line = "'compileOnly('com.android.support.test:monitor:$VERSION')";
        assert!(ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn support_artifact_false_positive_not_matched() {
        let line = "implementation('com.example.android.support:example-lib:1.0.0')";
        assert!(!ARTIFACT_MIN_MATCH.is_match(line))
    }

    #[test]
    fn star_import_matches_java_star_import() {
        let line = "import android.arch.persistence.*;";
        assert!(STAR_IMPORT_MATCH.is_match(line))
    }

    #[test]
    fn star_import_matches_kotlin_star_import() {
        let line = "import android.arch.persistence.*";
        assert!(STAR_IMPORT_MATCH.is_match(line))
    }

    #[test]
    fn star_import_does_not_match_progaurd_line() {
        let line = "-keep public class * extends android.support.v4.app.Fragment";
        assert!(!STAR_IMPORT_MATCH.is_match(line))
    }

    #[test]
    fn star_import_does_match_wildcard_proguard_line() {
        let line = "-dontwarn android.support.design.**";
        assert!(STAR_IMPORT_MATCH.is_match(line))
    }

    #[test]
    fn star_import_does_not_match_generics() {
        let line = "): ArrayList<*>";
        assert!(!STAR_IMPORT_MATCH.is_match(line))
    }

    #[test]
    fn star_import_does_not_match_comment() {
        let line = "    * uses [android.support.v4.app.Fragment]";
        assert!(!STAR_IMPORT_MATCH.is_match(line))
    }
}
