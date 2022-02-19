use std::convert::TryFrom;
use std::path::PathBuf;

use anyhow::Result;
use rayon::prelude::*;

use super::definition::{
    build_full_regexp, get_definition_rules, is_comment, DefinitionKind, DefinitionSearchResult,
    Definitions, Occurrences,
};
use crate::dumb_analyzer::get_comments_by_ext;
use crate::process::AsyncCommand;
use crate::tools::ripgrep::{Match, Word};

/// Basic information for searching with ripgrep.
#[derive(Debug, Clone)]
pub struct BasicRunner<'a> {
    /// Directory to perform the ripgrep search.
    pub dir: Option<&'a PathBuf>,
    /// Keyword of searching.
    pub word: &'a Word,
    /// Extension of the source file.
    pub file_ext: &'a str,
}

impl<'a> BasicRunner<'a> {
    pub(super) async fn find_occurrences(&self) -> Result<Vec<Match>> {
        let command = format!(
            "rg --json --word-regexp '{}' -g '*.{}'",
            self.word.raw, self.file_ext
        );
        let comments = get_comments_by_ext(self.file_ext);
        self.find_matches(command, Some(comments))
    }

    /// Executes `command` as a child process.
    ///
    /// Convert the entire output into a stream of ripgrep `Match`.
    fn find_matches(&self, command: String, comments: Option<&[&str]>) -> Result<Vec<Match>> {
        let mut cmd = AsyncCommand::new(command);

        if let Some(ref dir) = self.dir {
            cmd.current_dir(dir);
        }

        let stdout = cmd.stdout()?;

        if let Some(comments) = comments {
            Ok(stdout
                .par_split(|x| x == &b'\n')
                .filter_map(|s| {
                    Match::try_from(s)
                        .ok()
                        .filter(|mat| !is_comment(mat, comments)) // TODO: do not ignore comments?
                })
                .collect())
        } else {
            Ok(stdout
                .par_split(|x| x == &b'\n')
                .filter_map(|s| Match::try_from(s).ok())
                .collect())
        }
    }
}

/// [`BasicRunner`] with a known language type.
#[derive(Debug, Clone)]
pub struct RegexRunner<'a> {
    /// Inner runner.
    pub inner: BasicRunner<'a>,
    /// Language type defined by ripgrep.
    pub lang: &'a str,
}

impl<'a> RegexRunner<'a> {
    pub fn new(inner: BasicRunner<'a>, lang: &'a str) -> Self {
        Self { inner, lang }
    }

    /// Finds the occurrences and all definitions concurrently.
    pub async fn all(&self, comments: &[&str]) -> (Definitions, Occurrences) {
        let (definitions, occurrences) =
            futures::future::join(self.definitions(), self.occurrences(comments)).await;

        (
            Definitions {
                defs: definitions.unwrap_or_default(),
            },
            Occurrences(occurrences.unwrap_or_default()),
        )
    }

    /// Returns all kinds of definitions.
    pub async fn definitions(&self) -> Result<Vec<DefinitionSearchResult>> {
        let all_def_futures = get_definition_rules(self.lang)?
            .0
            .keys()
            .map(|kind| self.find_definitions(kind));

        let maybe_defs = futures::future::join_all(all_def_futures).await;

        Ok(maybe_defs
            .into_par_iter()
            .filter_map(|def| {
                def.ok()
                    .map(|(kind, matches)| DefinitionSearchResult { kind, matches })
            })
            .collect())
    }

    /// Finds all the occurrences of `word`.
    ///
    /// Basically the occurrences are composed of definitions and usages.
    async fn occurrences(&self, comments: &[&str]) -> Result<Vec<Match>> {
        let command = format!(
            "rg --json --word-regexp '{}' --type {}",
            self.inner.word.raw, self.lang
        );

        self.inner.find_matches(command, Some(comments))
    }

    pub(super) async fn regexp_search(&self, comments: &[&str]) -> Result<Vec<Match>> {
        let command = format!(
            "rg --json -e '{}' --type {}",
            self.inner.word.raw.replace(char::is_whitespace, ".*"),
            self.lang
        );
        self.inner.find_matches(command, Some(comments))
    }

    /// Returns a tuple of (definition_kind, ripgrep_matches) by searching given language `lang`.
    async fn find_definitions(
        &self,
        kind: &DefinitionKind,
    ) -> Result<(DefinitionKind, Vec<Match>)> {
        let regexp = build_full_regexp(self.lang, kind, self.inner.word)?;
        let command = format!(
            "rg --trim --json --pcre2 --type {} -e '{}'",
            self.lang, regexp
        );
        self.inner
            .find_matches(command, None)
            .map(|defs| (kind.clone(), defs))
    }
}
