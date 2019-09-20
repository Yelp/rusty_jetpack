rusty_jetpack
=============

A fast and simple tool to assist in migrating to AndroidX.

## Rationale

A hackathon project inspired by [`sd`](https://github.com/chmln/sd) and [Dan
Lew's smallscript](https://gist.github.com/dlew/5db1b780896bbc6f542e7c00a11db6a0),
**rusty_jetpack** attempts to make up for some of the pain points experienced
with the provided AndroidX migration tool. While it makes no attempt to
guarantee post migration compilation or runtime stability, it aims to make
iteration as fast as possible when dealing with the ever changing nature of a
large code base.

### How it works

Relevant (`.gradle`, `gradle.kts`, `.java`, `.kt`, `.pro`, `.xml`) files are
found through `git ls-files` and are distributed evenly to a thread pool. Each
thread employs a [Matcher](src/matcher.rs) that sequentially loads the file
into a memory map.  The file is then read line by line and matches are replaced
according to Android's provided
[class](https://developer.android.com/topic/libraries/support-library/downloads/androidx-class-mapping.csv)
mapping CSV file. Only files that change are then written back to disk.

Old libraries are also searched for with the provided
[library](https://developer.android.com/topic/libraries/support-library/downloads/androidx-artifact-mapping.csv)
mapping file. However, they are not replaced and a notice about the location
and what the library should be updated to are printed to STDERR.

Since only explicit mappings are used, it is still recommended to use the
provided tool to verify as many cases are found when initial migration is
started. rusty_jetpack can then be distributed to developers to significantly
decrease adoption and migration time.

## Usage
### Installation

rusty_jetpack is written in [Rust](https://www.rust-lang.org/). It was written
with `rustc` version 1.33.0, but can be built with version 1.31.0 or higher.
The recommended way to install Rust is from the [official installation page](https://www.rust-lang.org/tools/install).

With Rust installed, installation can be done by cloning the repository and
then installing via `cargo install`. You should then be able to run
rusty_jetpack if cargo is part of your `$PATH`.
```sh
cargo install --git https://github.com/Yelp/rusty_jetpack.git
```

### Command line usage

Usage is as simple as calling `rusty_jetpack` in the root of your Android repository.

### Uninstalling

It can then be unistalled by simply calling `cargo uninstall rusty_jetpack`.

## Performance
_**Note:** This is highly unscientific and not a real benchmark of
performance. All results were taken on a 2018 MacBook Pro with 32GB of RAM and
12 threads._

| App | Execution time |
|:------------:|:----------------------:|
| [Yelp](https://play.google.com/store/apps/details?id=com.yelp.android) | 0.95s |
| [Kickstarter](https://github.com/kickstarter/android-oss) | 0.33s |

## Caveats

* `git ls-files` is used to determine which files to operate on. Therefore,
this tool will not operate on projects not managed by git and will also ignore
submodules and untracked files.
* Star imports and star proguard rules are not migrated since exact matches are
required to map to the correct AndroidX class. Though a warning about them will
be printed.
* Carriage return line feeds (`\r\n`, CRLF) are not respected and will be
replaced with plain line feeds (`\n`, LF). As such Windows based projects are
not fully supported and might experience unexpected changes. See
[`writeln!()`](https://doc.rust-lang.org/std/macro.writeln.html) for more
information.
* Replacements are done in place and imports are therefore likely to be out of
order. Formatters such as Google Java Format and KtLint are better suited to
resolve this issue.

## License

Apache 2.0 - Please read the [LICENSE](LICENSE) file.
