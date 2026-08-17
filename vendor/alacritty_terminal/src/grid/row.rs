//! Defines the Row type which makes up lines in the grid.

use std::cmp::{max, min};
use std::ops::{Index, IndexMut, Range, RangeFrom, RangeFull, RangeTo, RangeToInclusive};
use std::{ptr, slice};

#[cfg(feature = "serde")]
use serde::de::Deserializer;
#[cfg(feature = "serde")]
use serde::ser::{SerializeSeq, SerializeStruct, Serializer};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::grid::GridCell;
use crate::index::Column;
use crate::term::cell::ResetDiscriminant;

/// A row in the grid.
#[derive(Default, Clone, Debug)]
pub struct Row<T> {
    /// Dense cells, or a compact prefix followed by one repeated suffix cell.
    ///
    /// Scrollback rows frequently end in a long run of identical blank cells.
    /// Keeping one copy of that suffix preserves exact cell contents while
    /// avoiding a full-width allocation for cold history. Active writes thaw
    /// the row before returning mutable access.
    inner: Vec<T>,

    /// Maximum number of occupied entries.
    ///
    /// This is the upper bound on the number of elements in the row, which have been modified
    /// since the last reset. All cells after this point are guaranteed to be equal.
    pub(crate) occ: usize,

    /// Logical column count. This differs from `inner.len()` while compacted.
    columns: usize,

    /// Whether `inner.last()` represents every logical cell after the stored
    /// prefix rather than one physical column.
    compacted: bool,
}

// Keep Alacritty's established `{ inner, occ }` wire shape. The compact form
// is an in-memory optimization and must not invalidate existing ref fixtures
// or leak into persisted representations.
#[cfg(feature = "serde")]
impl<T: Serialize> Serialize for Row<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        struct LogicalCells<'a, T>(&'a Row<T>);

        impl<T: Serialize> Serialize for LogicalCells<'_, T> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let mut sequence = serializer.serialize_seq(Some(self.0.columns))?;
                for cell in self.0 {
                    sequence.serialize_element(cell)?;
                }
                sequence.end()
            }
        }

        let mut row = serializer.serialize_struct("Row", 2)?;
        row.serialize_field("inner", &LogicalCells(self))?;
        row.serialize_field("occ", &self.occ)?;
        row.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Row<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SerializedRow<T> {
            inner: Vec<T>,
            occ: usize,
        }

        let row = SerializedRow::deserialize(deserializer)?;
        Ok(Self::from_vec(row.inner, row.occ))
    }
}

impl<T: PartialEq> PartialEq for Row<T> {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns && self.into_iter().eq(other)
    }
}

impl<T: Default> Row<T> {
    /// Create a new terminal row.
    ///
    /// Ideally the `template` should be `Copy` in all performance sensitive scenarios.
    pub fn new(columns: usize) -> Row<T> {
        debug_assert!(columns >= 1);

        let mut inner: Vec<T> = Vec::with_capacity(columns);

        // This is a slightly optimized version of `std::vec::Vec::resize`.
        unsafe {
            let mut ptr = inner.as_mut_ptr();

            for _ in 1..columns {
                ptr::write(ptr, T::default());
                ptr = ptr.offset(1);
            }
            ptr::write(ptr, T::default());

            inner.set_len(columns);
        }

        Row { inner, occ: 0, columns, compacted: false }
    }

    /// Increase the number of columns in the row.
    #[inline]
    pub fn grow(&mut self, columns: usize)
    where
        T: Clone,
    {
        if self.columns >= columns {
            return;
        }

        self.inflate();
        self.inner.resize_with(columns, T::default);
        self.columns = columns;
    }

    /// Reduce the number of columns in the row.
    ///
    /// This will return all non-empty cells that were removed.
    pub fn shrink(&mut self, columns: usize) -> Option<Vec<T>>
    where
        T: Clone + GridCell,
    {
        if self.columns <= columns {
            return None;
        }

        self.inflate();

        // Split off cells for a new row.
        let mut new_row = self.inner.split_off(columns);
        let index = new_row.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
        new_row.truncate(index);

        self.occ = min(self.occ, columns);
        self.columns = columns;

        if new_row.is_empty() { None } else { Some(new_row) }
    }

    /// Reset all cells in the row to the `template` cell.
    #[inline]
    pub fn reset<D>(&mut self, template: &T)
    where
        T: Clone + ResetDiscriminant<D> + GridCell,
        D: PartialEq,
    {
        debug_assert!(self.columns != 0);
        self.inflate();

        // Mark all cells as dirty if template cell changed.
        let len = self.inner.len();
        if self.inner[len - 1].discriminant() != template.discriminant() {
            self.occ = len;
        }

        // Reset every dirty cell in the row.
        for item in &mut self.inner[0..self.occ] {
            item.reset(template);
        }

        self.occ = 0;
    }
}

#[allow(clippy::len_without_is_empty)]
impl<T> Row<T> {
    #[inline]
    pub fn from_vec(vec: Vec<T>, occ: usize) -> Row<T> {
        let columns = vec.len();
        Row { inner: vec, occ, columns, compacted: false }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.columns
    }

    /// Whether this row uses a single retained cell for its repeated suffix.
    #[inline]
    pub(crate) fn is_compacted(&self) -> bool {
        self.compacted
    }

    /// Heap allocation owned by this row's cell vector. Dynamically allocated
    /// data inside individual cells is intentionally not included.
    #[inline]
    pub(crate) fn heap_storage_bytes(&self) -> usize {
        self.inner.capacity().saturating_mul(std::mem::size_of::<T>())
    }

    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.inner.last()
    }

    /// Return the physical cell capacity retained by this row.
    #[inline]
    pub(crate) fn allocated_cells(&self) -> usize {
        self.inner.capacity()
    }
}

impl<T: Clone> Row<T> {
    /// Restore a compact scrollback row to ordinary dense Alacritty storage.
    #[inline]
    fn inflate(&mut self) {
        if !self.compacted {
            return;
        }

        let fill = self.inner.pop().expect("compacted row must have a suffix cell");
        self.inner.resize(self.columns, fill);
        self.compacted = false;
    }

    /// Collapse an identical trailing run into a single cell.
    ///
    /// This is deliberately lossless: unlike text-only scrollback, the stored
    /// suffix retains its original colors, flags, hyperlink, and grapheme data.
    /// Returns whether the physical representation became smaller.
    pub(crate) fn compact_trailing(&mut self) -> bool
    where
        T: PartialEq,
    {
        if self.compacted || self.columns <= 1 {
            return false;
        }

        debug_assert_eq!(self.inner.len(), self.columns);
        let fill = self.inner[self.columns - 1].clone();
        let prefix_len = self.inner[..self.columns - 1]
            .iter()
            .rposition(|cell| cell != &fill)
            .map_or(0, |index| index + 1);

        // A one-cell suffix cannot save storage.
        if prefix_len + 1 >= self.columns {
            return false;
        }

        self.inner.truncate(prefix_len);
        self.inner.push(fill);
        self.inner.shrink_to_fit();
        self.compacted = true;
        true
    }

    #[inline]
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.inflate();
        self.occ = self.columns;
        self.inner.last_mut()
    }

    #[inline]
    pub fn append(&mut self, vec: &mut Vec<T>)
    where
        T: GridCell,
    {
        self.inflate();
        self.occ += vec.len();
        self.inner.append(vec);
        self.columns = self.inner.len();
    }

    #[inline]
    pub fn append_front(&mut self, mut vec: Vec<T>) {
        self.inflate();
        self.occ += vec.len();

        vec.append(&mut self.inner);
        self.inner = vec;
        self.columns = self.inner.len();
    }

    /// Check if all cells in the row are empty.
    #[inline]
    pub fn is_clear(&self) -> bool
    where
        T: GridCell,
    {
        self.into_iter().all(GridCell::is_empty)
    }

    #[inline]
    pub fn front_split_off(&mut self, at: usize) -> Vec<T> {
        self.inflate();
        self.occ = self.occ.saturating_sub(at);

        let mut split = self.inner.split_off(at);
        std::mem::swap(&mut split, &mut self.inner);
        self.columns = self.inner.len();
        split
    }
}

/// Logical row iterator which transparently repeats a compact suffix cell.
pub struct RowIter<'a, T> {
    row: &'a Row<T>,
    front: usize,
    back: usize,
}

impl<'a, T> Iterator for RowIter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let index = self.front;
        self.front += 1;
        Some(&self.row[Column(index)])
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.back - self.front;
        (len, Some(len))
    }
}

impl<T> DoubleEndedIterator for RowIter<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        Some(&self.row[Column(self.back)])
    }
}

impl<T> ExactSizeIterator for RowIter<'_, T> {}

impl<'a, T> IntoIterator for &'a Row<T> {
    type IntoIter = RowIter<'a, T>;
    type Item = &'a T;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        RowIter { row: self, front: 0, back: self.columns }
    }
}

impl<'a, T: Clone> IntoIterator for &'a mut Row<T> {
    type IntoIter = slice::IterMut<'a, T>;
    type Item = &'a mut T;

    #[inline]
    fn into_iter(self) -> slice::IterMut<'a, T> {
        self.inflate();
        self.occ = self.columns;
        self.inner.iter_mut()
    }
}

impl<T> Index<Column> for Row<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: Column) -> &T {
        debug_assert!(index.0 < self.columns);
        if self.compacted {
            let prefix_len = self.inner.len() - 1;
            &self.inner[index.0.min(prefix_len)]
        } else {
            &self.inner[index.0]
        }
    }
}

impl<T: Clone> IndexMut<Column> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut T {
        self.inflate();
        self.occ = max(self.occ, *index + 1);
        &mut self.inner[index.0]
    }
}

impl<T> Index<Range<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[T] {
        assert!(!self.compacted, "range indexing requires a dense row");
        &self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T: Clone> IndexMut<Range<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [T] {
        self.inflate();
        self.occ = max(self.occ, *index.end);
        &mut self.inner[(index.start.0)..(index.end.0)]
    }
}

impl<T> Index<RangeTo<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[T] {
        assert!(!self.compacted, "range indexing requires a dense row");
        &self.inner[..(index.end.0)]
    }
}

impl<T: Clone> IndexMut<RangeTo<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [T] {
        self.inflate();
        self.occ = max(self.occ, *index.end);
        &mut self.inner[..(index.end.0)]
    }
}

impl<T> Index<RangeFrom<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[T] {
        assert!(!self.compacted, "range indexing requires a dense row");
        &self.inner[(index.start.0)..]
    }
}

impl<T: Clone> IndexMut<RangeFrom<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [T] {
        self.inflate();
        self.occ = self.columns;
        &mut self.inner[(index.start.0)..]
    }
}

impl<T> Index<RangeFull> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, _: RangeFull) -> &[T] {
        assert!(!self.compacted, "range indexing requires a dense row");
        &self.inner[..]
    }
}

impl<T: Clone> IndexMut<RangeFull> for Row<T> {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [T] {
        self.inflate();
        self.occ = self.columns;
        &mut self.inner[..]
    }
}

impl<T> Index<RangeToInclusive<Column>> for Row<T> {
    type Output = [T];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[T] {
        assert!(!self.compacted, "range indexing requires a dense row");
        &self.inner[..=(index.end.0)]
    }
}

impl<T: Clone> IndexMut<RangeToInclusive<Column>> for Row<T> {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [T] {
        self.inflate();
        self.occ = max(self.occ, *index.end + 1);
        &mut self.inner[..=(index.end.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_suffix_is_transparent_and_mutation_inflates() {
        let mut row = Row::<char>::new(8);
        row[Column(0)] = 'a';
        row[Column(1)] = 'b';

        assert!(row.compact_trailing());
        assert!(row.is_compacted());
        assert_eq!(row.len(), 8);
        assert_eq!(row.inner, vec!['a', 'b', '\0']);
        assert_eq!(row[Column(0)], 'a');
        assert_eq!(row[Column(7)], '\0');
        assert_eq!(row.into_iter().copied().collect::<Vec<_>>(), vec!['a', 'b', '\0', '\0', '\0', '\0', '\0', '\0']);

        row[Column(7)] = 'z';
        assert!(!row.is_compacted());
        assert_eq!(row.inner.len(), 8);
        assert_eq!(row[Column(0)], 'a');
        assert_eq!(row[Column(7)], 'z');
    }

    #[test]
    fn compact_suffix_preserves_non_default_fill() {
        let mut row = Row::<char>::new(6);
        for cell in &mut row {
            *cell = '-';
        }
        row[Column(0)] = 'x';

        assert!(row.compact_trailing());
        assert_eq!(row.inner, vec!['x', '-']);
        assert_eq!(row.into_iter().copied().collect::<String>(), "x-----");
    }
}
