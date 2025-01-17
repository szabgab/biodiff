use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections::BTreeMap, sync::Arc};

use regex::bytes::{Regex, RegexBuilder};

use crate::cursor::CursorActive;
use crate::file::FileContent;

#[derive(Clone, Debug, PartialEq, Eq)]
/// The three query types, which are all compiled to a regex, but with
/// different options
pub enum QueryType {
    /// plain unescaped text
    Text,
    /// a normal regex
    Regex,
    /// a regex using hex characters
    Hexagex,
}

#[derive(Clone, Debug)]
pub struct Query {
    text: String,
    query_type: QueryType,
    regex: Arc<Regex>,
}

impl PartialEq for Query {
    fn eq(&self, other: &Self) -> bool {
        // we do not compare the compiled regex, since it is already uniquely determined
        // by text and query_type
        self.text == other.text && self.query_type == other.query_type
    }
}

impl Eq for Query {}

impl Query {
    pub fn new(query_type: QueryType, text: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let regex = match query_type {
            // unicode is disabled because it is likely that one wants to search for non-unicode
            // in a hex viewer
            QueryType::Text => RegexBuilder::new(&regex::escape(text))
                .multi_line(true)
                .unicode(true)
                .build()?,
            QueryType::Regex => RegexBuilder::new(text)
                .multi_line(true)
                .unicode(false)
                .build()?,
            QueryType::Hexagex => hexagex::hexagex(text)?,
        };
        Ok(Query {
            text: text.to_owned(),
            query_type,
            regex: Arc::new(regex),
        })
    }
    pub fn query_type(&self) -> QueryType {
        self.query_type.clone()
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug)]
/// contains a query and its results
pub struct SearchResults {
    /// maps the start address of a match to its end
    starts: BTreeMap<usize, usize>,
    /// maps the end address of a match to its start
    ends: BTreeMap<usize, usize>,
    /// the query this belongs to
    query: Query,
}

fn map_both<T, S>(r: Result<T, T>, f: impl FnOnce(T) -> S) -> Result<S, S> {
    match r {
        Ok(s) => Ok(f(s)),
        Err(s) => Err(f(s)),
    }
}

fn transpose_both<T>(r: Result<Option<T>, Option<T>>) -> Option<Result<T, T>> {
    match r {
        Ok(Some(s)) => Some(Ok(s)),
        Err(Some(s)) => Some(Err(s)),
        Ok(None) | Err(None) => None,
    }
}

fn unwrap_both<T>(r: Result<T, T>) -> T {
    match r {
        Ok(s) | Err(s) => s,
    }
}

impl SearchResults {
    /// Get a new empty search result store for a given query
    pub fn new(query: Query) -> Self {
        SearchResults {
            starts: BTreeMap::new(),
            ends: BTreeMap::new(),
            query,
        }
    }
    /// get the query associated with this SearchResults set
    pub fn query(&self) -> &Query {
        &self.query
    }
    /// add a match range to the set
    pub fn add_match(&mut self, range: Range<usize>) {
        self.starts.insert(range.start, range.end);
        self.ends.insert(range.end, range.start);
    }
    /// calculates whether the given address is inside a result
    pub fn is_in_result(&self, addr: Option<usize>) -> bool {
        let addr = match addr {
            Some(a) => a,
            None => return false,
        };

        self.starts
            .range(..=addr)
            .rev()
            .next()
            .map_or(false, |(x, y)| (*x..*y).contains(&addr))
    }
    /// get the next result after addr
    /// Returns None if there is no result, and Some(Err) if the result is after wraparound
    pub fn next_result(&self, addr: usize) -> Option<Result<Range<usize>, Range<usize>>> {
        match self
            .starts
            .range(addr + 1..)
            .map(|(a, b)| *a..*b)
            .next()
            .ok_or_else(|| self.starts.range(..).map(|(a, b)| *a..*b).next())
        {
            Ok(o) => Some(Ok(o)),
            Err(Some(e)) => Some(Err(e)),
            Err(None) => None,
        }
    }
    /// from a list of search results, find the next result from any of them
    /// the T is supposed to be data to disambiguate between the multiple
    /// usize addresses from different search results
    pub fn nearest_next_result<T: Ord + Copy>(
        list: &[(&Option<Self>, usize, T)],
        to_index: impl Fn(usize, T) -> Option<isize>,
    ) -> Option<isize> {
        // note that Ok(_) < Err(_), so by using min here,
        // we prioritize results that are not wraparound
        let next = list
            .iter()
            .flat_map(|x| x.0.as_ref().into_iter().map(move |y| (y, x.1, x.2)))
            .flat_map(|(search, addr, right)| {
                search
                    .next_result(addr)
                    .and_then(|x| transpose_both(map_both(x, |y| to_index(y.start, right))))
            })
            .min()?;
        Some(unwrap_both(next))
    }
    /// get the previous result before addr
    /// Returns None if there is no result, and Some(Err) if the result is after wraparound
    pub fn prev_result(&self, addr: usize) -> Option<Result<Range<usize>, Range<usize>>> {
        match self
            .ends
            .range(..=addr)
            .rev()
            .map(|(a, b)| *b..*a)
            .next()
            .ok_or_else(|| self.ends.range(..).rev().map(|(a, b)| *b..*a).next())
        {
            Ok(o) => Some(Ok(o)),
            Err(Some(e)) => Some(Err(e)),
            Err(None) => None,
        }
    }
    /// from a list of search results, find the previous result from any of them
    /// the T is supposed to be data to disambiguate between the multiple
    /// usize addresses from different search results
    pub fn nearest_prev_result<T: Ord + Copy>(
        list: &[(&Option<Self>, usize, T)],
        to_index: impl Fn(usize, T) -> Option<isize>,
    ) -> Option<isize> {
        // note that Ok(_) < Err(_), so by using min here,
        // we prioritize results that are not wraparound
        let next = list
            .iter()
            .flat_map(|x| x.0.as_ref().into_iter().map(move |y| (y, x.1, x.2)))
            .flat_map(|(search, addr, right)| {
                search
                    .prev_result(addr)
                    .and_then(|x| transpose_both(map_both(x, |y| to_index(y.start, right))))
                    // since we use min to prioritize Ok over Err, reverse the order of the addresses to prioritize later addresses
                    // (those that are nearer to the original address)
                    .map(|x| map_both(x, std::cmp::Reverse))
            })
            .min()?;
        Some(unwrap_both(next).0)
    }
}

pub struct SearchPair(pub Option<SearchResults>, pub Option<SearchResults>);

impl SearchPair {
    pub fn is_in_result(&self, addr: [Option<usize>; 2]) -> [bool; 2] {
        [(&self.0, addr[0]), (&self.1, addr[1])]
            .map(|(x, addr)| x.as_ref().map_or(false, |y| y.is_in_result(addr)))
    }
    pub fn clear(&mut self, cursor_act: CursorActive) {
        if cursor_act.is_first() {
            self.0 = None;
        }
        if cursor_act.is_second() {
            self.1 = None;
        }
    }
    pub fn current_search_query(&self, cursor_act: CursorActive) -> Option<&Query> {
        if cursor_act.is_first() {
            [&self.0, &self.1]
        } else {
            [&self.1, &self.0]
        }
        .iter()
        .copied()
        .flatten()
        .map(|x| x.query())
        .next()
    }
    /// Initializes the empty search results for the search query
    /// on the currently active cursors
    pub fn setup_search(
        &mut self,
        query: Query,
        cursor_act: CursorActive,
        files: [FileContent; 2],
    ) -> (
        (SearchContext, FileContent),
        Option<(SearchContext, FileContent)>,
    ) {
        let [ffirst, fsecond] = files;
        let is_running = Arc::new(AtomicBool::new(true));
        match cursor_act {
            CursorActive::None | CursorActive::Both => {
                self.0 = Some(SearchResults::new(query.clone()));
                self.1 = Some(SearchResults::new(query.clone()));
                (
                    (
                        SearchContext {
                            first: true,
                            query: query.clone(),
                            is_running: is_running.clone(),
                        },
                        ffirst,
                    ),
                    Some((
                        SearchContext {
                            first: false,
                            query,
                            is_running,
                        },
                        fsecond,
                    )),
                )
            }
            CursorActive::First => {
                self.0 = Some(SearchResults::new(query.clone()));
                (
                    (
                        SearchContext {
                            first: true,
                            query,
                            is_running,
                        },
                        ffirst,
                    ),
                    None,
                )
            }
            CursorActive::Second => {
                self.1 = Some(SearchResults::new(query.clone()));
                (
                    (
                        SearchContext {
                            first: false,
                            query,
                            is_running,
                        },
                        fsecond,
                    ),
                    None,
                )
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchContext {
    /// what hexpanel this is on (effectively an identifier for the search process)
    pub first: bool,
    /// original query
    pub query: Query,
    /// bool for cancelling the search
    pub is_running: Arc<std::sync::atomic::AtomicBool>,
}

impl SearchContext {
    pub fn start_search<Sender>(self, mut send: Sender, file: FileContent)
    where
        Sender: FnMut(Option<Range<usize>>) -> bool + Send + 'static,
    {
        std::thread::spawn(move || {
            for m in self.query.regex.find_iter(&file) {
                let r = if self.is_running.load(Ordering::Relaxed) {
                    Some(m.range())
                } else {
                    None
                };
                let res = send(r.clone());
                if !res || r.is_none() {
                    return;
                }
            }
            send(None);
        });
    }
}
