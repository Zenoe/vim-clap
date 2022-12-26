use anyhow::Result;
use chrono::prelude::*;
use directories::ProjectDirs;
use icon::Icon;
use itertools::Itertools;
use matcher::{ExactMatcher, InverseMatcher};
use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};
use subprocess::Exec;
use types::{CaseMatching, ExactTerm, InverseTerm};
use utility::{println_json, println_json_with_length, read_first_lines};

/// Project directory for Vim Clap.
///
/// All the files generated by vim-clap are stored there.
pub static PROJECT_DIRS: Lazy<ProjectDirs> = Lazy::new(|| {
    ProjectDirs::from("org", "vim", "Vim Clap")
        .expect("Couldn't create project directory for vim-clap")
});

pub static HOME_DIR: Lazy<PathBuf> = Lazy::new(|| {
    directories::BaseDirs::new()
        .expect("Failed to construct BaseDirs")
        .home_dir()
        .to_path_buf()
});

/// Yes or no terms.
#[derive(Debug, Clone, Default)]
pub struct UsageMatcher {
    pub exact_matcher: ExactMatcher,
    pub inverse_matcher: InverseMatcher,
}

impl UsageMatcher {
    pub fn new(exact_terms: Vec<ExactTerm>, inverse_terms: Vec<InverseTerm>) -> Self {
        Self {
            exact_matcher: ExactMatcher::new(exact_terms, CaseMatching::Smart),
            inverse_matcher: InverseMatcher::new(inverse_terms),
        }
    }

    /// Returns the match indices of exact terms if given `line` passes all the checks.
    fn match_indices(&self, line: &str) -> Option<Vec<usize>> {
        match (
            self.exact_matcher.find_matches(line),
            self.inverse_matcher.match_any(line),
        ) {
            (Some((_, indices)), false) => Some(indices),
            _ => None,
        }
    }

    /// Returns `true` if the result of The results of applying `self`
    /// is a superset of applying `other` on the same source.
    pub fn is_superset(&self, other: &Self) -> bool {
        self.exact_matcher
            .exact_terms()
            .iter()
            .zip(other.exact_matcher.exact_terms().iter())
            .all(|(local, other)| local.is_superset(other))
            && self
                .inverse_matcher
                .inverse_terms()
                .iter()
                .zip(other.inverse_matcher.inverse_terms().iter())
                .all(|(local, other)| local.is_superset(other))
    }

    pub fn match_jump_line(
        &self,
        (jump_line, mut indices): (String, Vec<usize>),
    ) -> Option<(String, Vec<usize>)> {
        if let Some(exact_indices) = self.match_indices(&jump_line) {
            indices.extend_from_slice(&exact_indices);
            indices.sort_unstable();
            indices.dedup();
            Some((jump_line, indices))
        } else {
            None
        }
    }
}

pub type UtcTime = DateTime<Utc>;

/// Returns a `PathBuf` using given file name under the project data directory.
pub fn generate_data_file_path(filename: &str) -> std::io::Result<PathBuf> {
    let data_dir = PROJECT_DIRS.data_dir();
    std::fs::create_dir_all(data_dir)?;

    let mut file = data_dir.to_path_buf();
    file.push(filename);

    Ok(file)
}

/// Returns a `PathBuf` using given file name under the project cache directory.
pub fn generate_cache_file_path(filename: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let cache_dir = PROJECT_DIRS.cache_dir();
    std::fs::create_dir_all(cache_dir)?;

    let mut file = cache_dir.to_path_buf();
    file.push(filename);

    Ok(file)
}

fn read_json_as<P: AsRef<Path>, T: serde::de::DeserializeOwned>(path: P) -> Result<T> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let deserializd = serde_json::from_reader(reader)?;

    Ok(deserializd)
}

pub fn load_json<T: serde::de::DeserializeOwned, P: AsRef<Path>>(path: Option<P>) -> Option<T> {
    path.and_then(|json_path| {
        if json_path.as_ref().exists() {
            read_json_as::<_, T>(json_path).ok()
        } else {
            None
        }
    })
}

pub fn write_json<T: serde::Serialize, P: AsRef<Path>>(
    obj: T,
    path: Option<P>,
) -> std::io::Result<()> {
    if let Some(json_path) = path.as_ref() {
        utility::create_or_overwrite(json_path, serde_json::to_string(&obj)?.as_bytes())?;
    }

    Ok(())
}

#[derive(Debug, Clone)]
#[allow(unused)]
pub enum SendResponse {
    Json,
    JsonWithContentLength,
}

/// Reads the first lines from cache file and send back the cached info.
pub fn send_response_from_cache(
    tempfile: &Path,
    total: usize,
    response_ty: SendResponse,
    icon: Icon,
) {
    let using_cache = true;
    if let Ok(iter) = read_first_lines(&tempfile, 100) {
        let lines: Vec<String> = if let Some(icon_kind) = icon.icon_kind() {
            iter.map(|x| icon_kind.add_icon_to_text(&x)).collect()
        } else {
            iter.collect()
        };
        match response_ty {
            SendResponse::Json => println_json!(total, tempfile, using_cache, lines),
            SendResponse::JsonWithContentLength => {
                println_json_with_length!(total, tempfile, using_cache, lines)
            }
        }
    } else {
        match response_ty {
            SendResponse::Json => println_json!(total, tempfile, using_cache),
            SendResponse::JsonWithContentLength => {
                println_json_with_length!(total, tempfile, using_cache)
            }
        }
    }
}

pub(crate) fn expand_tilde(path: impl AsRef<str>) -> PathBuf {
    static HOME_PREFIX: Lazy<String> = Lazy::new(|| format!("~{}", std::path::MAIN_SEPARATOR));

    if let Some(stripped) = path.as_ref().strip_prefix(HOME_PREFIX.as_str()) {
        HOME_DIR.clone().join(stripped)
    } else {
        path.as_ref().into()
    }
}

/// Build the absolute path using cwd and relative path.
pub fn build_abs_path(cwd: impl AsRef<Path>, curline: impl AsRef<Path>) -> PathBuf {
    let mut path: PathBuf = cwd.as_ref().into();
    path.push(curline);
    path
}

/// Counts lines in the source `handle`.
///
/// # Examples
/// ```ignore
/// let lines: usize = count_lines(std::fs::File::open("Cargo.toml").unwrap()).unwrap();
/// ```
///
/// Credit: https://github.com/eclarke/linecount/blob/master/src/lib.rs
pub fn count_lines<R: std::io::Read>(handle: R) -> std::io::Result<usize> {
    let mut reader = std::io::BufReader::with_capacity(1024 * 32, handle);
    let mut count = 0;
    loop {
        let len = {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            count += bytecount::count(buf, b'\n');
            buf.len()
        };
        reader.consume(len);
    }

    Ok(count)
}

#[inline]
pub fn lines(cmd: Exec) -> Result<Lines<impl BufRead>> {
    // We usually have a decent amount of RAM nowdays.
    Ok(std::io::BufReader::with_capacity(8 * 1024 * 1024, cmd.stream_stdout()?).lines())
}

/// Returns the width of displaying `n` on the screen.
///
/// Same with `n.to_string().len()` but without allocation.
pub fn display_width(n: usize) -> usize {
    if n == 0 {
        return 1;
    }

    let mut n = n;
    let mut len = 0;
    while n > 0 {
        len += 1;
        n /= 10;
    }

    len
}

// /home/xlc/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/string.rs
pub(crate) fn truncate_absolute_path(abs_path: &str, max_len: usize) -> Cow<'_, str> {
    if abs_path.len() > max_len {
        let gap = abs_path.len() - max_len;

        const SEP: char = std::path::MAIN_SEPARATOR;

        if let Some(home_dir) = crate::utils::HOME_DIR.as_path().to_str() {
            if abs_path.starts_with(home_dir) {
                // ~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/string.rs
                if home_dir.len() > gap {
                    return abs_path.replacen(home_dir, "~", 1).into();
                }

                // ~/.rustup/.../github.com/paritytech/substrate/frame/system/src/lib.rs
                let home_stripped = &abs_path.trim_start_matches(home_dir)[1..];
                if let Some((first, target)) = home_stripped.split_once(SEP) {
                    let mut hidden = 0usize;
                    for component in target.split(SEP) {
                        if hidden > gap + 2 {
                            let mut target = target.to_string();
                            target.replace_range(..hidden - 1, "...");
                            return format!("~{SEP}{first}{SEP}{target}").into();
                        } else {
                            hidden += component.len() + 1;
                        }
                    }
                }
            } else {
                let top = abs_path.splitn(4, SEP).collect::<Vec<_>>();
                if let Some(last) = top.last() {
                    if let Some((_first, target)) = last.split_once(SEP) {
                        let mut hidden = 0usize;
                        for component in target.split(SEP) {
                            if hidden > gap + 2 {
                                let mut target = target.to_string();
                                target.replace_range(..hidden - 1, "...");
                                let head = top.iter().take(top.len() - 1).join(&SEP.to_string());
                                return format!("{head}{SEP}{target}").into();
                            } else {
                                hidden += component.len() + 1;
                            }
                        }
                    }
                }
            }
        } else {
            // Truncate the left of absolute path string.
            // ../stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/string.rs
            if let Some((offset, _)) = abs_path.char_indices().nth(abs_path.len() - max_len + 2) {
                let mut abs_path = abs_path.to_string();
                abs_path.replace_range(..offset, "..");
                return abs_path.into();
            }
        }
    }

    abs_path.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_lines() {
        let f: &[u8] = b"some text\nwith\nfour\nlines\n";
        assert_eq!(count_lines(f).unwrap(), 4);
    }

    #[test]
    fn test_truncate_absolute_path() {
        #[cfg(not(target_os = "windows"))]
        let p = ".rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/string.rs";
        #[cfg(target_os = "windows")]
        let p = r#".rustup\toolchains\stable-x86_64-unknown-linux-gnu\lib\rustlib\src\rust\library\alloc\src\string.rs"#;
        let abs_path = format!(
            "{}{}{}",
            crate::utils::HOME_DIR.as_path().to_str().unwrap(),
            std::path::MAIN_SEPARATOR,
            p
        );
        let max_len = 60;
        #[cfg(not(target_os = "windows"))]
        let expected = "~/.rustup/.../src/rust/library/alloc/src/string.rs";
        #[cfg(target_os = "windows")]
        let expected = r#"~\.rustup\...\src\rust\library\alloc\src\string.rs"#;
        assert_eq!(truncate_absolute_path(&abs_path, max_len), expected);

        let abs_path = "/media/xlc/Data/src/github.com/paritytech/substrate/bin/node/cli/src/command_helper.rs";
        let expected = "/media/xlc/.../bin/node/cli/src/command_helper.rs";
        assert_eq!(truncate_absolute_path(abs_path, max_len), expected);
    }
}
