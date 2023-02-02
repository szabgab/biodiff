use std::{
    ops::Range,
    sync::{atomic::AtomicBool, mpsc::Sender, Arc},
};

use cursive::{Vec2, View};

use crate::{
    align::{AlignAlgorithm, AlignElement},
    backend::{Action, Backend, Cursiv},
    cursor::{CursorActive, Move},
    datastruct::{DoubleVec, SignedArray},
    doublehex::{DoubleHexContext, DoubleHexLine},
    file::{FileContent, FileState},
    search::{Query, SearchContext, SearchResults},
    style::{ByteData, ColumnSetting},
};

use super::{is_next_search_result, next_difference};
/// Enum that containts events but also allows
/// messages for appending/prepending data to the Aligned view.
pub enum AlignedMessage {
    UserEvent(Action),
    Append(Vec<AlignElement>),
    Prepend(Vec<AlignElement>),
}

impl From<Action> for AlignedMessage {
    fn from(action: Action) -> Self {
        AlignedMessage::UserEvent(action)
    }
}

/// A view that dynamically displays aligned files
pub struct Aligned {
    data: DoubleVec<AlignElement>,
    filenames: (String, String),
    searches: (Option<SearchResults>, Option<SearchResults>),
    original: (FileContent, FileContent),
    index: isize,
    pub dh: DoubleHexContext,
}

impl Aligned {
    /// Creates a new aligned view, starting a new thread that sends new aligned
    /// data of `algo` over the sender.
    /// Note that receiving events and sending them to the view has to be handled by
    /// the caller for unknown reasons.
    pub fn new(
        first: FileState,
        second: FileState,
        dh: DoubleHexContext,
        algo: &AlignAlgorithm,
        sender: Sender<AlignedMessage>,
    ) -> Self {
        let index = -(dh.cursor.get_index() as isize);
        let data = DoubleVec::new();
        let first_arc = first.content.clone();
        let second_arc = second.content.clone();
        algo.start_align(first_arc, second_arc, (first.index, second.index), sender);
        Aligned {
            data,
            filenames: (first.name, second.name),
            original: (first.content, second.content),
            searches: (first.search, second.search),
            index,
            dh,
        }
    }
    /// Checks whether a given range of indexes overlaps with the indexes currently visible.
    fn is_in_view(&self, range: Range<isize>) -> bool {
        let self_range = self.index..self.index + (self.dh.cursor.get_size()) as isize;
        !(self_range.start >= range.end || self_range.end <= range.start)
    }
    /// returns the search results visible in the current view
    fn search_ranges(&self) -> [Vec<(usize, usize)>; 2] {
        let intersect_range =
            |a: Range<isize>, b: Range<isize>| a.start.max(b.start)..a.end.min(b.end);
        let view_bounds = intersect_range(
            self.data.bounds(),
            self.index..self.index + self.dh.cursor.get_size() as isize,
        );
        let starts = self.data.get(view_bounds.start).map(|x| [x.xaddr, x.yaddr]);
        let ends = self
            .data
            .get(view_bounds.end - 1)
            .map(|x| [x.xaddr, x.yaddr]);
        if let ((Some(search1), Some(search2)), Some(starts), Some(ends)) =
            (&self.searches, starts, ends)
        {
            let ret: Vec<Vec<(usize, usize)>> = [search1, search2]
                .iter()
                .zip(starts.iter().zip(ends))
                .map(|(search, (start, end))| {
                    search.lookup_results(*start..end + 1).into_iter().collect()
                })
                .collect();
            [ret[0].clone(), ret[1].clone()]
        } else {
            [vec![], vec![]]
        }
    }
    /// Gets a useful form of the information contained in the alignement data for printing.
    fn get_content(&self) -> Vec<DoubleHexLine> {
        let mut content = Vec::new();
        let [first_range, second_range] = self.search_ranges();
        let mut next_first = first_range.into_iter().peekable();
        let mut next_second = second_range.into_iter().peekable();
        for x in 0..self.dh.cursor.get_size_y() {
            // address of current line to be converted
            let base_addr = (x * self.dh.cursor.bytes_per_row()) as isize + self.index;
            let mut bytes = Vec::new();
            for alignel in self
                .data
                .get_range(base_addr..base_addr + self.dh.cursor.get_size_x() as isize)
            {
                let malignel = match alignel {
                    Some(x) => x,
                    None => {
                        bytes.push((None, None));
                        continue;
                    }
                };
                let is_first_result = is_next_search_result(&mut next_first, malignel.xaddr);
                let is_second_result = is_next_search_result(&mut next_second, malignel.yaddr);
                let first = ByteData::maybe_new(malignel.xbyte, is_first_result);
                let second = ByteData::maybe_new(malignel.ybyte, is_second_result);
                bytes.push((first, second));
            }
            let address = self
                .data
                .get(base_addr)
                .map(|alignel| (Some(alignel.xaddr), Some(alignel.yaddr)))
                .unwrap_or_default();
            content.push(DoubleHexLine { address, bytes });
        }
        content
    }
    fn bytes_in_view(&self) -> [Vec<u8>; 2] {
        let mut ret = [vec![], vec![]];
        for alignel in self
            .data
            .get_range(self.index..self.index + self.dh.cursor.get_size() as isize)
        {
            if let Some(alignel) = alignel {
                if let Some(xbyte) = alignel.xbyte {
                    ret[0].push(xbyte);
                }
                if let Some(ybyte) = alignel.ybyte {
                    ret[1].push(ybyte);
                }
            }
        }
        ret
    }
    /// returns the current index of the cursor into the data
    fn cursor_index(&self) -> isize {
        self.index + self.dh.cursor.get_index() as isize
    }
    /// Paints the cursor at the current position
    fn set_cursor<B: Backend>(&self, printer: &mut B, cursor_act: CursorActive) {
        let cursor_index = self.cursor_index();
        let (a, b) = self
            .data
            .get(cursor_index)
            .map(|alignel| (alignel.xbyte, alignel.ybyte))
            .unwrap_or_default();
        let addresses = self
            .data
            .get(cursor_index)
            .map(|alignel| (Some(alignel.xaddr), Some(alignel.yaddr)))
            .unwrap_or_default();
        let [a, b] = [
            (&self.searches.0, addresses.0, a),
            (&self.searches.1, addresses.1, b),
        ]
        .map(|(search, addr, byte)| {
            let is_search_result = search.as_ref().map_or(false, |s| s.is_in_result(addr));
            ByteData::maybe_new(byte, is_search_result)
        });
        self.dh
            .set_doublehex_cursor(printer, cursor_act, (a, b), addresses);
    }

    /// Prints the top and bottom bar.
    fn print_bars<B: Backend>(&self, printer: &mut B) {
        self.dh
            .print_title_line(printer, " aligned", &self.filenames.0, &self.filenames.1);
        let cursor_index = self.cursor_index();
        let addresses = self
            .data
            .get(cursor_index)
            .map(|alignel| (Some(alignel.xaddr), Some(alignel.yaddr)))
            .unwrap_or_default();
        self.dh.print_bottom_line(printer, addresses);
    }

    /// Moves the cursor xdiff down and ydiff to the right,
    /// redrawing/scrolling if necessary.
    pub fn move_around<B: Backend>(&mut self, printer: &mut B, movement: Move) {
        self.set_cursor(printer, CursorActive::None);
        let relative_bounds =
            (self.data.bounds().start - self.index)..(self.data.bounds().end - self.index);
        let movement = if self.dh.style.right_to_left {
            movement.reflect_rtl()
        } else {
            movement
        };
        let index_diff = self.dh.cursor.mov(movement, relative_bounds);
        self.index += index_diff;
        if let Some(scroll_amount) = self.dh.cursor.full_row_move(index_diff) {
            let content = self.get_content();
            self.dh
                .print_doublehex_scrolled(&content, printer, scroll_amount);
            self.set_cursor(printer, CursorActive::Both);
            if scroll_amount != 0 {
                self.print_bars(printer);
            }
            printer.refresh();
        } else {
            self.redraw(printer, false);
        }
    }
    /// Appends alignment data to the underlying DoubleVec.
    /// Returns true if something in view changed.
    pub fn append(&mut self, vec: Vec<AlignElement>) -> bool {
        let extend_range = self.data.bounds().end..self.data.bounds().end + vec.len() as isize;
        self.data.extend_end(&vec);
        self.is_in_view(extend_range)
    }
    /// Prepends alignment data to the underlying DoubleVec.
    /// Returns true if something in view changed.
    pub fn prepend(&mut self, vec: Vec<AlignElement>) -> bool {
        let extend_range = self.data.bounds().start - vec.len() as isize..self.data.bounds().start;
        self.data.extend_front(&vec);
        self.is_in_view(extend_range)
    }
    /// Resize the view without printing it, returns whether redrawing is necessary.
    pub fn resize(&mut self, dimensions: (usize, usize)) -> bool {
        let (columns, rows) = dimensions;
        let old_dimensions = (self.dh.cursor.get_size_x(), self.dh.cursor.get_size_y());
        let old_bytes_per_row = self.dh.cursor.bytes_per_row();
        let (new_dimensions, bytes_per_row) = self.dh.style.get_doublehex_dims(columns, rows);
        self.index += self.dh.cursor.resize(new_dimensions, bytes_per_row);
        old_dimensions != new_dimensions && old_bytes_per_row != bytes_per_row
    }
    /// Redraws the current view without checking and updating the view for changes.
    pub fn redraw<B: Backend>(&self, printer: &mut B, clear: bool) {
        if clear {
            printer.clear();
        }
        let content = self.get_content();
        self.dh.print_doublehex_screen(&content, printer);
        self.set_cursor(printer, CursorActive::Both);
        self.print_bars(printer);
        printer.refresh();
    }
    /// Updates the view and draws it.
    pub fn refresh<B: Backend>(&mut self, printer: &mut B) {
        let changed = self.resize(printer.size());
        self.redraw(printer, changed);
    }
    /// jump to a given index with the currently active cursor
    pub fn goto_index<B: Backend>(&mut self, printer: &mut B, index: isize) {
        let address_diff = index - self.cursor_index();
        let (col, row) = self.dh.cursor.jump(address_diff);
        self.move_around(printer, Move::Unbounded(col, row));
    }
    /// get the index of the current file address with the side given by `right`
    fn index_address(&self, right: bool, pos: usize) -> Result<isize, isize> {
        self.data.binary_search(&pos, |pos, el| {
            Some(*pos).cmp(&el.map(|a| if right { a.yaddr } else { a.xaddr }))
        })
    }
    /// Go to the address in `pos`, right is true if on the second view, else the first view is used.
    /// Returns true if the address exists.
    pub fn goto<B: Backend>(
        &mut self,
        printer: &mut B,
        right: bool,
        pos: usize,
    ) -> Result<(), String> {
        let address_index = self
            .index_address(right, pos)
            .map_err(|_| "Address does not (yet) exist")?;
        self.goto_index(printer, address_index);
        Ok(())
    }
    /// get the file addresses of the current cursors
    fn current_cursor_addresses(&self) -> Option<[usize; 2]> {
        self.data
            .get(self.cursor_index())
            .map(|x| [x.xaddr, x.yaddr])
    }
    /// Jump to the next search result on either active cursor after the current index
    pub fn jump_next_search_result<B: Backend>(&mut self, printer: &mut B) {
        let [first, second] = self
            .current_cursor_addresses()
            .or_else(|| self.data.first().map(|x| [x.xaddr, x.yaddr]))
            .unwrap_or([0, 0]);
        let next = match SearchResults::nearest_next_result(
            &[
                (&self.searches.0, first, false),
                (&self.searches.1, second, true),
            ],
            |addr, right| self.index_address(right, addr).ok(),
        ) {
            Some(x) => x,
            None => return,
        };
        self.goto_index(printer, next)
    }
    /// Jump to the previous search reult on either active cursor before the current index
    pub fn jump_prev_search_result<B: Backend>(&mut self, printer: &mut B) {
        let [first, second] = match self
            .current_cursor_addresses()
            .or_else(|| self.data.last().map(|x| [x.xaddr, x.yaddr]))
        {
            Some(x) => x,
            None => return,
        };
        let next = match SearchResults::nearest_prev_result(
            &[
                (&self.searches.0, first, false),
                (&self.searches.1, second, true),
            ],
            |addr, right| self.index_address(right, addr).ok(),
        ) {
            Some(x) => x,
            None => return,
        };
        self.goto_index(printer, next)
    }
    /// Jump to the index where the next bytes are different
    pub fn jump_next_difference<B: Backend>(
        &mut self,
        printer: &mut B,
        forward: bool,
        insertion: bool,
    ) {
        let target_address = next_difference(
            self.cursor_index(),
            self.data.bounds(),
            forward,
            |i| match self.data.get(i).map(|x| (x.xbyte, x.ybyte)) {
                None | Some((Some(_), None)) | Some((None, Some(_))) => true,
                Some((x, y)) => x != y && !insertion,
            },
        );
        self.goto_index(printer, target_address);
    }
    /// Go to the first position of the file
    pub fn jump_start<B: Backend>(&mut self, printer: &mut B) {
        self.goto_index(printer, self.data.bounds().start)
    }
    /// Go to the last position of the file
    pub fn jump_end<B: Backend>(&mut self, printer: &mut B) {
        self.goto_index(printer, self.data.bounds().end - 1)
    }
    /// Adds a batch of search results to the current ones if they are of the same query.
    pub fn add_search_results(
        &mut self,
        query: Query,
        results: Vec<Option<Range<usize>>>,
        first: bool,
    ) {
        let search = if first {
            &mut self.searches.0
        } else {
            &mut self.searches.1
        };
        let search = match search {
            Some(s) if s.query() == &query => s,
            _ => return,
        };
        for result in results.iter().flatten() {
            search.add_match(result.clone())
        }
    }
    /// Clears the search results of both cursors
    pub fn clear_search(&mut self) {
        self.searches = (None, None);
    }
    /// Initializes the empty search results for the search query
    /// on the currently active cursors
    pub fn setup_search(
        &mut self,
        query: Query,
    ) -> (
        (SearchContext, FileContent),
        Option<(SearchContext, FileContent)>,
    ) {
        let is_running = Arc::new(AtomicBool::new(true));
        self.searches.0 = Some(SearchResults::new(query.clone()));
        self.searches.1 = Some(SearchResults::new(query.clone()));
        (
            (
                SearchContext {
                    first: true,
                    query: query.clone(),
                    is_running: is_running.clone(),
                },
                self.original.0.clone(),
            ),
            Some((
                SearchContext {
                    first: false,
                    query,
                    is_running,
                },
                self.original.1.clone(),
            )),
        )
    }
    /// Inreases the column count by one and refreshes the view
    pub fn add_column<B: Backend>(&mut self, printer: &mut B) {
        self.dh.inc_columns();
        self.refresh(printer);
    }
    /// Decreases the column count by one and refreshes the view
    pub fn remove_column<B: Backend>(&mut self, printer: &mut B) {
        self.dh.dec_columns();
        self.refresh(printer);
    }
    /// Sets the column count to the peak of the autocorrelation of
    /// the bytes in the current view and refreshes the view
    pub fn auto_column<B: Backend>(&mut self, printer: &mut B) {
        let [first, second] = self.bytes_in_view();
        self.dh.auto_columns([&first, &second]);
        self.refresh(printer);
    }
    /// Process move events
    pub fn process_move<B: Backend>(&mut self, printer: &mut B, action: Action) {
        match action {
            Action::Down => self.move_around(printer, Move::CursorY(1)),
            Action::DownAlt => self.move_around(printer, Move::ViewY(1)),
            Action::Up => self.move_around(printer, Move::CursorY(-1)),
            Action::UpAlt => self.move_around(printer, Move::ViewY(-1)),
            Action::Left => self.move_around(printer, Move::CursorX(-1)),
            Action::LeftAlt => self.move_around(printer, Move::ViewX(-1)),
            Action::Right => self.move_around(printer, Move::CursorX(1)),
            Action::RightAlt => self.move_around(printer, Move::ViewX(1)),
            Action::PgDown => self.move_around(
                printer,
                Move::ViewY(self.dh.cursor.get_size_y() as isize / 2),
            ),
            Action::PgUp => self.move_around(
                printer,
                Move::ViewY(-(self.dh.cursor.get_size_y() as isize) / 2),
            ),
            Action::NextDifference => self.jump_next_difference(printer, true, false),
            Action::NextInsertion => self.jump_next_difference(printer, true, true),
            Action::PrevDifference => self.jump_next_difference(printer, false, false),
            Action::PrevInsertion => self.jump_next_difference(printer, false, true),
            Action::Top => self.jump_start(printer),
            Action::Bottom => self.jump_end(printer),
            Action::NextSearch => self.jump_next_search_result(printer),
            Action::PrevSearch => self.jump_prev_search_result(printer),
            Action::AddColumn => self.add_column(printer),
            Action::RemoveColumn => self.remove_column(printer),
            Action::AutoColumn => self.auto_column(printer),
            Action::ResetColumn => {
                self.dh.style.column_count = ColumnSetting::Fit;
                self.refresh(printer);
            }
            _ => (),
        }
    }
    /// Returns the active search query for one of the currently cursors
    pub fn current_search_query(&self) -> Option<&Query> {
        [&self.searches.0, &self.searches.1]
            .iter()
            .copied()
            .flatten()
            .map(|x| x.query())
            .next()
    }
    /// Process events
    pub fn process_action<B: Backend>(&mut self, printer: &mut B, action: AlignedMessage) {
        match match action {
            AlignedMessage::UserEvent(ev) => ev,
            AlignedMessage::Append(vec) => {
                if self.append(vec) {
                    self.refresh(printer);
                }
                return;
            }
            AlignedMessage::Prepend(vec) => {
                if self.prepend(vec) {
                    self.refresh(printer);
                }
                return;
            }
        } {
            Action::Refresh => self.refresh(printer),
            otherwise => self.process_move(printer, otherwise),
        }
    }
    /// Turn an Aligned view into its part, including information on where it points
    pub fn destruct(self) -> Result<(FileState, FileState, DoubleHexContext), Self> {
        // we return the original view in case the cursor is outside the files
        match (self.data.get(self.cursor_index())).map(|a| (a.xaddr, a.yaddr)) {
            Some((xaddr, yaddr)) => Ok((
                FileState {
                    name: self.filenames.0,
                    content: self.original.0,
                    index: xaddr,
                    search: self.searches.0,
                },
                FileState {
                    name: self.filenames.1,
                    content: self.original.1,
                    index: yaddr,
                    search: self.searches.1,
                },
                self.dh,
            )),
            None => Err(self),
        }
    }
}

// view implementations for cursive
impl View for Aligned {
    fn draw(&self, printer: &cursive::Printer) {
        let mut backend = Cursiv::from_printer(printer);
        self.redraw(&mut backend, true);
    }
    fn layout(&mut self, size: Vec2) {
        self.resize((size.x, size.y));
    }
}