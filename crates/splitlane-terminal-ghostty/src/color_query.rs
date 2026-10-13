use crate::Rgb;

const MAX_OSC_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
}

#[derive(Default)]
pub(crate) struct ColorQueryResponder {
    state: State,
    pending: Vec<u8>,
    foreground: Option<Rgb>,
    background: Option<Rgb>,
    cursor: Option<Rgb>,
}

impl ColorQueryResponder {
    /// A colour that is `None` is one nobody told this terminal, and a query
    /// for it goes unanswered.
    pub(crate) fn set_colors(
        &mut self,
        foreground: Option<Rgb>,
        background: Option<Rgb>,
        cursor: Option<Rgb>,
    ) {
        self.foreground = foreground;
        self.background = background;
        self.cursor = cursor;
    }

    /// Intercept dynamic-color queries unsupported by the pinned libghostty.
    /// Input and synthetic replies are emitted in their original stream order.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        emit_input: &mut impl FnMut(&[u8]),
        emit_reply: &mut impl FnMut(&[u8]),
    ) {
        let mut plain_start = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            match self.state {
                State::Ground if byte == b'\x1b' => {
                    emit_input(&bytes[plain_start..index]);
                    self.pending.push(byte);
                    self.state = State::Escape;
                    plain_start = index + 1;
                }
                State::Ground => {}
                State::Escape if byte == b']' => {
                    self.pending.push(byte);
                    self.state = State::Osc;
                    plain_start = index + 1;
                }
                State::Escape if byte == b'\x1b' => {
                    emit_input(&self.pending);
                    self.pending.clear();
                    self.pending.push(byte);
                    plain_start = index + 1;
                }
                State::Escape => {
                    self.pending.push(byte);
                    emit_input(&self.pending);
                    self.pending.clear();
                    self.state = State::Ground;
                    plain_start = index + 1;
                }
                State::Osc => {
                    self.pending.push(byte);
                    plain_start = index + 1;
                    match byte {
                        b'\x07' => self.finish(emit_input, emit_reply),
                        b'\x1b' => self.state = State::OscEscape,
                        // CAN and SUB abandon a control string. The parser
                        // is given the cancel too, so that it leaves the
                        // string it was given the start of.
                        b'\x18' | b'\x1a' => {
                            emit_input(&self.pending);
                            self.pending.clear();
                            self.state = State::Ground;
                        }
                        _ if self.pending.len() > MAX_OSC_BYTES => {
                            emit_input(&self.pending);
                            self.pending.clear();
                            self.state = State::Ground;
                        }
                        _ => {}
                    }
                }
                State::OscEscape if byte == b'\\' => {
                    self.pending.push(byte);
                    plain_start = index + 1;
                    self.finish(emit_input, emit_reply);
                }
                State::OscEscape => {
                    // An ESC that is not the start of ST ended the string
                    // and began a sequence of its own. What was held is the
                    // parser's to make of; the ESC is read again as the
                    // start of whatever this byte continues.
                    plain_start = index + 1;
                    let escape = self.pending.pop();
                    debug_assert_eq!(escape, Some(b'\x1b'));
                    emit_input(&self.pending);
                    self.pending.clear();
                    self.pending.push(b'\x1b');
                    match byte {
                        b']' => {
                            self.pending.push(byte);
                            self.state = State::Osc;
                        }
                        b'\x1b' => {
                            emit_input(&self.pending);
                            self.state = State::Escape;
                        }
                        _ => {
                            self.pending.push(byte);
                            emit_input(&self.pending);
                            self.pending.clear();
                            self.state = State::Ground;
                        }
                    }
                }
            }
        }

        if self.state == State::Ground {
            emit_input(&bytes[plain_start..]);
        }
    }

    fn finish(&mut self, emit_input: &mut impl FnMut(&[u8]), emit_reply: &mut impl FnMut(&[u8])) {
        if let Some(reply) = self.reply() {
            emit_reply(&reply);
        } else {
            emit_input(&self.pending);
        }
        self.pending.clear();
        self.state = State::Ground;
    }

    fn reply(&self) -> Option<Vec<u8>> {
        let (terminator_len, terminator): (usize, &[u8]) = if self.pending.ends_with(b"\x1b\\") {
            (2, b"\x1b\\")
        } else if self.pending.ends_with(b"\x07") {
            (1, b"\x07")
        } else {
            return None;
        };
        let body = self
            .pending
            .get(2..self.pending.len().checked_sub(terminator_len)?)?;
        let (code, color) = match body {
            b"10;?" => (10, self.foreground?),
            b"11;?" => (11, self.background?),
            b"12;?" => (12, self.cursor?),
            _ => return None,
        };
        let component = |value: u8| u16::from(value) * 0x101;
        let mut reply = format!(
            "\x1b]{code};rgb:{:04x}/{:04x}/{:04x}",
            component(color.r),
            component(color.g),
            component(color.b),
        )
        .into_bytes();
        reply.extend_from_slice(terminator);
        Some(reply)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn responder() -> ColorQueryResponder {
        let mut responder = ColorQueryResponder::default();
        responder.set_colors(
            Some(Rgb {
                r: 0x11,
                g: 0x22,
                b: 0x33,
            }),
            Some(Rgb {
                r: 0x44,
                g: 0x55,
                b: 0x66,
            }),
            Some(Rgb {
                r: 0x77,
                g: 0x88,
                b: 0x99,
            }),
        );
        responder
    }

    #[test]
    fn replies_survive_chunk_boundaries_and_preserve_stream_order() {
        let mut responder = responder();
        let actions = RefCell::new(Vec::<(&'static str, Vec<u8>)>::new());
        for chunk in [
            b"before\x1b]1".as_slice(),
            b"0;?\x1b\\middle\x1b]11;?\x07after".as_slice(),
        ] {
            responder.feed(
                chunk,
                &mut |bytes| {
                    if !bytes.is_empty() {
                        actions.borrow_mut().push(("input", bytes.to_vec()));
                    }
                },
                &mut |bytes| actions.borrow_mut().push(("reply", bytes.to_vec())),
            );
        }

        assert_eq!(
            actions.into_inner(),
            [
                ("input", b"before".to_vec()),
                ("reply", b"\x1b]10;rgb:1111/2222/3333\x1b\\".to_vec(),),
                ("input", b"middle".to_vec()),
                ("reply", b"\x1b]11;rgb:4444/5555/6666\x07".to_vec()),
                ("input", b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn unrelated_osc_is_forwarded_unchanged() {
        let mut responder = responder();
        let mut input = Vec::new();
        let mut replies = Vec::new();
        responder.feed(
            b"\x1b]0;title\x1b\\",
            &mut |bytes| input.extend_from_slice(bytes),
            &mut |bytes| replies.extend_from_slice(bytes),
        );
        assert_eq!(input, b"\x1b]0;title\x1b\\");
        assert!(replies.is_empty());
    }

    /// What reaches the parser and what is answered, for a stream fed in
    /// the given pieces.
    fn fed(chunks: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
        let mut responder = responder();
        let mut input = Vec::new();
        let mut replies = Vec::new();
        for chunk in chunks {
            responder.feed(
                chunk,
                &mut |bytes| input.extend_from_slice(bytes),
                &mut |bytes| replies.extend_from_slice(bytes),
            );
        }
        (input, replies)
    }

    #[test]
    fn can_and_sub_end_a_string_nothing_terminated() {
        for cancel in [b'\x18', b'\x1a'] {
            // A title that never ended, cancelled, then a status query: the
            // query must reach the parser at once and not wait for a
            // terminator that the cancel made unnecessary.
            let mut stream = b"\x1b]0;abc".to_vec();
            stream.push(cancel);
            stream.extend_from_slice(b"\x1b[5n");
            let (input, replies) = fed(&[&stream]);
            assert_eq!(input, stream);
            assert!(replies.is_empty());

            // And a colour query after it is a query again.
            let mut stream = b"\x1b]0;abc".to_vec();
            stream.push(cancel);
            stream.extend_from_slice(b"\x1b]11;?\x07");
            let (input, replies) = fed(&[&stream[..5], &stream[5..]]);
            let mut cancelled = b"\x1b]0;abc".to_vec();
            cancelled.push(cancel);
            assert_eq!(input, cancelled);
            assert_eq!(replies, b"\x1b]11;rgb:4444/5555/6666\x07");
        }
    }

    #[test]
    fn an_escape_inside_a_string_starts_a_new_sequence() {
        // The string ends at the ESC; what follows is a sequence of its own
        // and is passed on as soon as it is complete.
        let (input, replies) = fed(&[b"\x1b]0;abc\x1b[5n"]);
        assert_eq!(input, b"\x1b]0;abc\x1b[5n");
        assert!(replies.is_empty());

        // Split at every position, the same bytes in the same order.
        let stream = b"\x1b]0;abc\x1b[5nrest";
        for cut in 1..stream.len() {
            let (input, _) = fed(&[&stream[..cut], &stream[cut..]]);
            assert_eq!(input, stream, "cut at {cut}");
        }

        // The new sequence may itself be a query, or another ESC.
        let (input, replies) = fed(&[b"\x1b]0;abc\x1b]10;?\x1b\\"]);
        assert_eq!(input, b"\x1b]0;abc");
        assert_eq!(replies, b"\x1b]10;rgb:1111/2222/3333\x1b\\");
        let (input, _) = fed(&[b"\x1b]0;abc\x1b\x1b[5n"]);
        assert_eq!(input, b"\x1b]0;abc\x1b\x1b[5n");
    }

    #[test]
    fn well_formed_strings_reach_the_parser_as_they_were_sent() {
        let stream: &[u8] =
            b"a\x1b]0;title\x07b\x1b]2;other\x1b\\c\x1b[31mred\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\";
        for cut in 1..stream.len() {
            let (input, replies) = fed(&[&stream[..cut], &stream[cut..]]);
            assert_eq!(input, stream, "cut at {cut}");
            assert!(replies.is_empty());
        }
    }
}
