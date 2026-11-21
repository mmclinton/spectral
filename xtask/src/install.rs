//! `cargo xtask install`: the one list of the files a Spectral installation
//! consists of, copied into place under a prefix. Every package format
//! stages its files through this list, so adding a file is one edit here.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::workspace;

/// The app ID the desktop file, metainfo and icon are named after.
pub const APP_ID: &str = "io.github.mmclinton.Spectral";

/// Files in `assets/` that are deliberately not installed. Only the test
/// that every asset is accounted for reads it.
#[cfg(test)]
const NOT_INSTALLED: [&str; 1] = [
    // Shown by the README from the repository and by the metainfo through a
    // URL; nothing installed reads it.
    "screenshot.png",
];

/// The command line of `cargo xtask install`.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    prefix: PathBuf,
    destdir: Option<PathBuf>,
    licensedir: Option<PathBuf>,
    app_only: bool,
    binaries: Option<PathBuf>,
}

impl Options {
    /// Reads the flags that follow `install`; every directory must be
    /// absolute.
    pub fn parse(args: &[OsString]) -> Result<Self, String> {
        let mut prefix = None;
        let mut destdir = None;
        let mut licensedir = None;
        let mut app_only = false;
        let mut binaries = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let flag = arg.to_string_lossy();
            let slot = match flag.as_ref() {
                "--app-only" => {
                    app_only = true;
                    continue;
                }
                "--prefix" => &mut prefix,
                "--destdir" => &mut destdir,
                "--licensedir" => &mut licensedir,
                "--binaries" => &mut binaries,
                other => return Err(format!("install: unknown argument '{other}'")),
            };
            let value = args
                .next()
                .ok_or_else(|| format!("install: {flag} needs a directory"))?;
            if slot.replace(checked_dir(&flag, value)?).is_some() {
                return Err(format!("install: {flag} given twice"));
            }
        }
        Ok(Self {
            prefix: prefix.ok_or("install: --prefix is required")?,
            destdir,
            licensedir,
            app_only,
            binaries,
        })
    }
}

/// A directory argument must be absolute and free of `..`, so joining it
/// under the destdir can never climb out of the staging root.
fn checked_dir(flag: &str, value: &OsStr) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "install: {flag} must be an absolute path, not '{}'",
            path.display()
        ));
    }
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(format!(
            "install: {flag} must not contain '..': '{}'",
            path.display()
        ));
    }
    Ok(path)
}

/// Where an installed file comes from.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// A built binary, in the `--binaries` directory or else the target
    /// directory's `release/` folder.
    Binary(&'static str),
    /// A file in the source tree, relative to the workspace root.
    Tree(String),
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    source: Source,
    /// The installed path, before any destdir.
    path: PathBuf,
    mode: u32,
}

impl Entry {
    fn binary(name: &'static str, prefix: &Path) -> Self {
        Self {
            source: Source::Binary(name),
            path: prefix.join("bin").join(name),
            mode: 0o755,
        }
    }

    fn data(source: impl Into<String>, path: PathBuf) -> Self {
        Self {
            source: Source::Tree(source.into()),
            path,
            mode: 0o644,
        }
    }
}

/// Every file of an installation. `app_only` leaves out what a sandboxed
/// app cannot expose to the host: the man page and the shell completions.
fn entries(prefix: &Path, licensedir: &Path, app_only: bool) -> Vec<Entry> {
    let share = prefix.join("share");
    let mut entries = vec![
        Entry::binary("spectral", prefix),
        Entry::binary("spectral-daemon", prefix),
        Entry::data(
            format!("assets/{APP_ID}.desktop"),
            share.join("applications").join(format!("{APP_ID}.desktop")),
        ),
        Entry::data(
            format!("assets/{APP_ID}.metainfo.xml"),
            share
                .join("metainfo")
                .join(format!("{APP_ID}.metainfo.xml")),
        ),
        Entry::data(
            "assets/spectral.png",
            share
                .join("icons/hicolor/512x512/apps")
                .join(format!("{APP_ID}.png")),
        ),
        Entry::data("LICENSE", licensedir.join("LICENSE")),
        Entry::data("THIRD-PARTY.md", licensedir.join("THIRD-PARTY.md")),
    ];
    if !app_only {
        entries.extend([
            Entry::data("assets/spectral.1", share.join("man/man1/spectral.1")),
            Entry::data(
                "assets/spectral.bash",
                share.join("bash-completion/completions/spectral"),
            ),
            Entry::data(
                "assets/spectral.fish",
                share.join("fish/vendor_completions.d/spectral.fish"),
            ),
            Entry::data(
                "assets/spectral.zsh",
                share.join("zsh/site-functions/_spectral"),
            ),
        ]);
    }
    entries
}

/// Copies every file of an installation into place, failing on the first
/// one that is missing.
pub fn run(options: &Options) -> Result<(), String> {
    let root = workspace::root();
    let binaries = match &options.binaries {
        Some(dir) => dir.clone(),
        None => workspace::target_dir()?.join("release"),
    };
    let licensedir = options
        .licensedir
        .clone()
        .unwrap_or_else(|| options.prefix.join("share/licenses/spectral"));
    for entry in entries(&options.prefix, &licensedir, options.app_only) {
        let from = match &entry.source {
            Source::Binary(name) => binaries.join(name),
            Source::Tree(path) => root.join(path),
        };
        if !from.is_file() {
            let hint = match entry.source {
                Source::Binary(_) => {
                    "; build it first with `cargo build --release`, and name its directory with --binaries if that build was given --target"
                }
                Source::Tree(_) => "",
            };
            return Err(format!("install: {} is missing{hint}", from.display()));
        }
        let to = staged(options.destdir.as_deref(), &entry.path);
        copy(&from, &to, entry.mode)
            .map_err(|e| format!("install: {} -> {}: {e}", from.display(), to.display()))?;
    }
    Ok(())
}

/// `path` under the staging root, joined by concatenation: `Path::join`
/// with an absolute path would replace the root instead.
fn staged(destdir: Option<&Path>, path: &Path) -> PathBuf {
    match destdir {
        Some(root) => root.join(path.strip_prefix("/").unwrap_or(path)),
        None => path.to_path_buf(),
    }
}

/// Copies like install(1): the destination is replaced rather than written
/// through, so a read-only file from an earlier install is no obstacle.
fn copy(from: &Path, to: &Path, mode: u32) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    if let Err(e) = fs::remove_file(to)
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(e);
    }
    fs::copy(from, to)?;
    fs::set_permissions(to, fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn sources(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .filter_map(|entry| match &entry.source {
                Source::Tree(path) => Some(path.as_str()),
                Source::Binary(_) => None,
            })
            .collect()
    }

    fn full() -> Vec<Entry> {
        entries(
            Path::new("/usr"),
            Path::new("/usr/share/licenses/spectral"),
            false,
        )
    }

    #[test]
    fn every_asset_is_installed_or_named_as_not_installed() {
        let all = full();
        let installed = sources(&all);
        for asset in fs::read_dir(workspace::root().join("assets")).unwrap() {
            let name = asset.unwrap().file_name().into_string().unwrap();
            let listed = installed.contains(&format!("assets/{name}").as_str());
            assert!(
                listed || NOT_INSTALLED.contains(&name.as_str()),
                "assets/{name} is neither installed nor named in NOT_INSTALLED"
            );
        }
    }

    #[test]
    fn every_listed_source_exists() {
        let root = workspace::root();
        for entry in full() {
            if let Source::Tree(path) = &entry.source {
                assert!(root.join(path).is_file(), "{path} is listed but missing");
            }
        }
    }

    #[test]
    fn app_only_leaves_out_the_man_page_and_the_completions() {
        let licenses = Path::new("/app/share/licenses/x");
        let all = entries(Path::new("/app"), licenses, false);
        let app = entries(Path::new("/app"), licenses, true);
        let kept = sources(&app);
        let left_out: Vec<&str> = sources(&all)
            .into_iter()
            .filter(|source| !kept.contains(source))
            .collect();
        assert_eq!(
            left_out,
            [
                "assets/spectral.1",
                "assets/spectral.bash",
                "assets/spectral.fish",
                "assets/spectral.zsh"
            ]
        );
        assert!(
            app.iter()
                .any(|entry| entry.source == Source::Binary("spectral-daemon"))
        );
    }

    #[test]
    fn binaries_are_executable_and_everything_else_is_not() {
        for entry in full() {
            let expected = match entry.source {
                Source::Binary(_) => 0o755,
                Source::Tree(_) => 0o644,
            };
            assert_eq!(entry.mode, expected, "{}", entry.path.display());
        }
    }

    #[test]
    fn the_destdir_is_prefixed_by_concatenation() {
        let path = Path::new("/usr/bin/spectral");
        assert_eq!(
            staged(Some(Path::new("/tmp/stage")), path),
            Path::new("/tmp/stage/usr/bin/spectral")
        );
        assert_eq!(staged(None, path), path);
    }

    #[test]
    fn options_need_an_absolute_prefix_without_parent_steps() {
        assert!(Options::parse(&args(&[])).is_err());
        assert!(Options::parse(&args(&["--prefix"])).is_err());
        assert!(Options::parse(&args(&["--prefix", "usr"])).is_err());
        assert!(Options::parse(&args(&["--prefix", "/usr/../etc"])).is_err());
    }

    #[test]
    fn options_reject_unknown_and_repeated_flags() {
        assert!(Options::parse(&args(&["--prefix", "/usr", "--bindir", "/bin"])).is_err());
        assert!(Options::parse(&args(&["--prefix", "/usr", "--prefix", "/opt"])).is_err());
    }

    #[test]
    fn options_parse_every_flag() {
        let parsed = Options::parse(&args(&[
            "--destdir",
            "/stage",
            "--prefix",
            "/usr",
            "--licensedir",
            "/usr/share/doc/x",
            "--app-only",
            "--binaries",
            "/build/release",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            Options {
                prefix: "/usr".into(),
                destdir: Some("/stage".into()),
                licensedir: Some("/usr/share/doc/x".into()),
                app_only: true,
                binaries: Some("/build/release".into()),
            }
        );
    }

    #[test]
    fn binaries_are_taken_from_the_named_directory() {
        let dir = std::env::temp_dir().join(format!("xtask-binaries-{}", std::process::id()));
        let built = dir.join("built");
        let prefix = dir.join("prefix");
        fs::create_dir_all(&built).unwrap();
        for name in ["spectral", "spectral-daemon"] {
            fs::write(built.join(name), name).unwrap();
        }

        run(&Options {
            prefix: prefix.clone(),
            destdir: None,
            licensedir: None,
            app_only: false,
            binaries: Some(built),
        })
        .unwrap();

        for name in ["spectral", "spectral-daemon"] {
            assert_eq!(
                fs::read_to_string(prefix.join("bin").join(name)).unwrap(),
                name
            );
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_need_not_be_utf8() {
        use std::os::unix::ffi::OsStringExt;

        let stage = OsString::from_vec(b"/stage/\xff".to_vec());
        let mut list = args(&["--prefix", "/usr", "--destdir"]);
        list.push(stage.clone());
        let parsed = Options::parse(&list).unwrap();
        assert_eq!(parsed.destdir, Some(PathBuf::from(stage)));
    }

    #[test]
    fn copying_replaces_a_read_only_file_and_sets_the_mode() {
        let dir = std::env::temp_dir().join(format!("xtask-copy-{}", std::process::id()));
        let from = dir.join("from");
        let to = dir.join("nested/to");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&from, "new").unwrap();
        fs::write(&to, "old").unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o444)).unwrap();

        copy(&from, &to, 0o755).unwrap();

        assert_eq!(fs::read_to_string(&to).unwrap(), "new");
        assert_eq!(
            fs::metadata(&to).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
