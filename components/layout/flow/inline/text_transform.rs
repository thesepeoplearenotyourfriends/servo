/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! # Logic for text transform in inline formatting contexts
//!
//! Inline formatting contexts do a variety of text transformations on their text content
//! including white space collapsing, application of the `text-transform` CSS property,
//! and application of the `-webkit-text-security` property. This module contains code to
//! handle this as well as code to map from offsets in the original DOM node to the final
//! IFC text and vice-versa.

pub use super::offset_map::OffsetMap;
use arrayvec::ArrayVec;
use icu_segmenter::WordSegmenter;
use servo_base::text::Utf32CodeUnits;
use style::computed_values::_webkit_text_security::T as WebKitTextSecurity;
use style::computed_values::white_space_collapse::T as WhiteSpaceCollapse;
use style::properties::ComputedValues;
use style::values::specified::text::{TextTransform, TextTransformCase};

use crate::flow::inline::construct::InlineFormattingContextBuilder;

/// <https://github.com/rust-lang/rust/blob/1.97.1/library/core/src/char/mod.rs#L523>
///
/// This is the maximum amount of characters that can be produced from case mapping,
/// and by consequence the maximum amount of characters that can be produced during
/// inline formatting context text transformation.
const MAX_CASE_MAPPING_LENGTH: usize = 3;

/// A single iteration in a pipeline of character iterators, that handle things like
/// whitespace collapse and `text-transform` processing for text in an
/// [`InlineFormattingContext`]. Each iteration can consume multiple characters and
/// produce zero or more characters (up to 3). Consumption of characters greater than the
/// characters produced by [`CharacterTransformIteration`] indicate that those characters
/// have been collapsed.
#[derive(Clone)]
pub struct CharacterTransformIteration {
    /// The number of characters consumed during this iteration of character transformation.
    consumed: Utf32CodeUnits,
    /// The characters that were produced during this iteration.
    characters: ArrayVec<char, MAX_CASE_MAPPING_LENGTH>,
}

impl CharacterTransformIteration {
    fn case_mapped(iterator: impl ExactSizeIterator<Item = char>) -> Self {
        debug_assert!(iterator.len() <= MAX_CASE_MAPPING_LENGTH);
        Self {
            consumed: Utf32CodeUnits(1),
            characters: iterator.collect(),
        }
    }

    fn one_to_one(character: char) -> Self {
        Self {
            consumed: Utf32CodeUnits(1),
            characters: std::iter::once(character).collect(),
        }
    }

    fn collapse(amount_collapsed: usize, character: Option<char>) -> Self {
        Self {
            consumed: Utf32CodeUnits(amount_collapsed),
            characters: character.into_iter().collect(),
        }
    }

    fn is_one_to_one(&self) -> bool {
        self.characters.len() == 1 && self.consumed.0 == 1
    }

    pub fn characters(&self) -> &[char] {
        &self.characters
    }
}

pub struct WhitespaceCollapse<InputIterator> {
    input_iterator: InputIterator,
    white_space_collapse: WhiteSpaceCollapse,

    /// Whether or not we are in the process of collapse leading white space. This is true
    /// when the last character handled in our owning [`super::InlineFormattingContext`]
    /// was collapsible white space and we have not seen any non-whitespace characters
    /// during processing of this iterator's input.
    trimming_leading_white_space: bool,

    /// Whether or not the last character produced was newline. There is special behavior
    /// we do after each newline.
    following_newline: bool,

    /// When whitespace collapses before a non-whitespace character, the iterator returns
    /// the collapsed whitespace and in the next iteration the non-whitespace character
    /// must be returned. This value caches it until the next iteration.
    character_pending_to_return: Option<char>,
}

impl<InputIterator: Iterator<Item = char>> WhitespaceCollapse<InputIterator> {
    pub fn new(
        input_iterator: InputIterator,
        white_space_collapse: WhiteSpaceCollapse,
        should_trim_leading_white_space: bool,
    ) -> Self {
        Self {
            input_iterator,
            white_space_collapse,
            following_newline: false,
            trimming_leading_white_space: should_trim_leading_white_space,
            character_pending_to_return: None,
        }
    }

    /// In some cases, white space is replaced by a single character (when not
    /// following a newline and when leading whitespace is not being trimmed). In all
    /// other cases, the white space is simply removed. This method handles that.
    fn iteration_for_collapsed_whitespace(
        &self,
        collapsed_whitespace: usize,
    ) -> CharacterTransformIteration {
        if !self.following_newline && !self.trimming_leading_white_space {
            CharacterTransformIteration::collapse(collapsed_whitespace, Some(' '))
        } else {
            CharacterTransformIteration::collapse(collapsed_whitespace, None)
        }
    }

    fn iteration_for_collected_white_space(
        &self,
        collected_whitespace: usize,
    ) -> Option<CharacterTransformIteration> {
        (collected_whitespace != 0)
            .then(|| self.iteration_for_collapsed_whitespace(collected_whitespace))
    }
}

impl<InputIterator: Iterator<Item = char>> Iterator for WhitespaceCollapse<InputIterator> {
    type Item = CharacterTransformIteration;

    fn next(&mut self) -> Option<Self::Item> {
        // Point 4.1.1 first bullet:
        // > If white-space is set to normal, nowrap, or pre-line, whitespace
        // > characters are considered collapsible
        // If whitespace is not considered collapsible, it is preserved entirely, which
        // means that we can simply return the input string exactly.
        if self.white_space_collapse == WhiteSpaceCollapse::Preserve
            || self.white_space_collapse == WhiteSpaceCollapse::BreakSpaces
        {
            // From <https://drafts.csswg.org/css-text-3/#white-space-processing>:
            // > Carriage returns (U+000D) are treated identically to spaces (U+0020) in all respects.
            //
            // In the non-preserved case these are converted to space below.
            return match self.input_iterator.next() {
                Some('\r') => Some(CharacterTransformIteration::one_to_one(' ')),
                next => next.map(CharacterTransformIteration::one_to_one),
            };
        }

        if let Some(character) = self.character_pending_to_return.take() {
            // Once we produce a non-whitespace character, we are no longer trimming leading whitespace.
            self.trimming_leading_white_space = false;
            self.following_newline = false;
            return Some(CharacterTransformIteration::one_to_one(character));
        }

        // When we enter a collapsible white space region, we may need to wait to produce
        // a single white space character as soon as we encounter a non-white space
        // character. When that happens we queue up the non-white space character for the
        // next iterator call.
        let mut collected_whitespace = 0;

        while let Some(character) = self.input_iterator.next() {
            // Don't push non-newline whitespace immediately. Instead wait to push it until we
            // know that it isn't followed by a newline. See `push_pending_whitespace_if_needed`
            // above.
            if InlineFormattingContextBuilder::is_document_white_space(character)
                && character != '\n'
            {
                collected_whitespace += 1;
                continue;
            }

            // Point 4.1.1:
            // > 2. Collapsible segment breaks are transformed for rendering according to the
            // >    segment break transformation rules.
            if character == '\n' {
                // From <https://drafts.csswg.org/css-text-3/#line-break-transform>
                // (4.1.3 -- the segment break transformation rules):
                //
                // > When white-space is pre, pre-wrap, or pre-line, segment breaks are not
                // > collapsible and are instead transformed into a preserved line feed"
                //
                // > 1. First, any collapsible segment break immediately following another
                // >    collapsible segment break is removed.
                // > 2. Then any remaining segment break is either transformed into a space (U+0020)
                // >    or removed depending on the context before and after the break.
                let iteration = if self.white_space_collapse != WhiteSpaceCollapse::Collapse {
                    CharacterTransformIteration::collapse(collected_whitespace + 1, Some('\n'))
                } else {
                    self.iteration_for_collapsed_whitespace(collected_whitespace + 1)
                };

                self.following_newline = true;
                return Some(iteration);
            }

            // Non-whitespace character

            // Point 4.1.1:
            // > 2. Any sequence of collapsible spaces and tabs immediately preceding or
            // >    following a segment break is removed.
            // > 3. Every collapsible tab is converted to a collapsible space (U+0020).
            // > 4. Any collapsible space immediately following another collapsible space—even
            // >    one outside the boundary of the inline containing that space, provided both
            // >    spaces are within the same inline formatting context—is collapsed to have zero
            // >    advance width.
            if let Some(iteration) = self.iteration_for_collected_white_space(collected_whitespace)
            {
                self.character_pending_to_return = Some(character);
                return Some(iteration);
            }

            // Once we produce a non-whitespace character, we are no longer trimming leading whitespace.
            self.trimming_leading_white_space = false;
            self.following_newline = false;
            return Some(CharacterTransformIteration::one_to_one(character));
        }

        self.iteration_for_collected_white_space(collected_whitespace)
    }
}

pub(crate) struct TextTransformationIterator<'a>(
    Box<dyn Iterator<Item = CharacterTransformIteration> + 'a>,
);

impl<'a> TextTransformationIterator<'a> {
    pub(crate) fn new(
        text: &'a str,
        style: &ComputedValues,
        trim_leading_white_space: bool,
        on_word_boundary: bool,
    ) -> Self {
        let text_security = style.clone__webkit_text_security();
        let chars = text
            .chars()
            .map(move |character| map_character_for_webkit_text_security(text_security, character));
        let white_space_collapse = style.clone_white_space_collapse();
        let iterator =
            WhitespaceCollapse::new(chars, white_space_collapse, trim_leading_white_space);

        // TODO: Not all text transforms are about case, this logic should stop ignoring
        // TextTransform::FULL_WIDTH and TextTransform::FULL_SIZE_KANA.
        let text_transform = style.clone_text_transform();
        let iterator = match text_transform.case() {
            TextTransformCase::None => {
                Box::new(iterator) as Box<dyn Iterator<Item = CharacterTransformIteration>>
            },
            TextTransformCase::Lowercase => {
                Box::new(simple_case_transform_iterator(iterator, |character| {
                    CharacterTransformIteration::case_mapped(character.to_lowercase())
                }))
            },
            TextTransformCase::Uppercase => {
                Box::new(simple_case_transform_iterator(iterator, |character| {
                    CharacterTransformIteration::case_mapped(character.to_uppercase())
                }))
            },
            TextTransformCase::Capitalize => Box::new(capitalization_iterator(
                iterator,
                text.len(),
                on_word_boundary,
            )),
            // TODO: implement `math-auto` and enable it in Stylo
        };
        if text_transform.intersects(TextTransform::FULL_WIDTH) {
            // TODO: implement `full-width`
        }
        if text_transform.intersects(TextTransform::FULL_SIZE_KANA) {
            // TODO: implement `full-size-kana`
        }

        Self(iterator)
    }
}

impl Iterator for TextTransformationIterator<'_> {
    type Item = CharacterTransformIteration;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

fn simple_case_transform_iterator(
    input_iterator: impl Iterator<Item = CharacterTransformIteration>,
    mapping: impl Fn(char) -> CharacterTransformIteration,
) -> impl Iterator<Item = CharacterTransformIteration> {
    input_iterator.map(move |iteration| {
        if iteration.is_one_to_one() {
            mapping(iteration.characters[0])
        } else {
            iteration
        }
    })
}

/// Given an input iterator, a size hint for the number items in the iterator,
/// and a boolean determining whether the start of the input represents a word
/// boundary, return an iterator that capitalizes one-to-one mapped characters
/// from the input iterator.
pub(crate) fn capitalization_iterator(
    input_iterator: impl Iterator<Item = CharacterTransformIteration>,
    size_hint: usize,
    allow_word_at_start: bool,
) -> impl Iterator<Item = CharacterTransformIteration> {
    let mut iterations: Vec<_> = input_iterator.collect();
    let mut string = String::with_capacity(size_hint);
    for iteration in &iterations {
        string.extend(iteration.characters());
    }

    let word_segmenter = WordSegmenter::new_auto();
    let mut bounds = word_segmenter.segment_str(&string).peekable();

    let mut current_byte_index = 0;
    for iteration in iterations.iter_mut() {
        let bytes_to_advance: usize = iteration
            .characters()
            .iter()
            .map(|character| character.len_utf8())
            .sum();
        if bytes_to_advance == 0 {
            continue;
        }

        let at_word_start = bounds.peek() == Some(&current_byte_index);
        if at_word_start {
            bounds.next();
        }

        // TODO: currently we titlecase the first `char` of each word,
        // instead it should be the first typographic letter unit:
        // https://drafts.csswg.org/css-text-4/#typographic-letter-unit
        // WPT /css/css-text/text-transform/text-transform-capitalize-026.html
        if iteration.is_one_to_one()
            && at_word_start
            && (current_byte_index != 0 || allow_word_at_start)
        {
            // TODO: Replace this with a call to `character.to_titlecase()` when available:
            // See: https://github.com/rust-lang/rust/issues/153892
            // See: https://doc.rust-lang.org/stable/std/primitive.char.html#difference-from-uppercase
            *iteration =
                CharacterTransformIteration::case_mapped(iteration.characters[0].to_uppercase());
        }

        current_byte_index += bytes_to_advance;
    }

    iterations.into_iter()
}

/// Map a character according to the rules of the `-webkit-text-security` CSS property.
///
/// Note: The behavior of `-webkit-text-security` isn't specified, so we have some
/// flexibility in the implementation. We just need to maintain a rough compatibility with
/// other browsers.
fn map_character_for_webkit_text_security(mode: WebKitTextSecurity, character: char) -> char {
    if let WebKitTextSecurity::None = mode {
        return character;
    }

    // TODO: When MSRV is 1.95+ use std::hint::cold_path().
    match character {
        // This is not ideal, but zero width space is used for some special reasons in
        // `<input>` fields, so these remain untransformed, otherwise they would show up
        // in empty text fields.
        '\u{200B}' => '\u{200B}',
        // Newlines are preserved, so that `<br>` keeps working as expected.
        '\n' => '\n',
        _ => match mode {
            WebKitTextSecurity::None => character, // unreachable
            WebKitTextSecurity::Circle => '○',
            WebKitTextSecurity::Disc => '●',
            WebKitTextSecurity::Square => '■',
        },
    }
}

impl OffsetMap {
    pub(crate) fn push_iteration(&mut self, iteration: &CharacterTransformIteration) {
        self.push_range(
            iteration.consumed,
            Utf32CodeUnits(iteration.characters.len()),
        );
    }
}
