//! Single-line editor mechanism matching pi's Input component primitives.
use crate::{kill_ring::KillRing, undo_stack::UndoStack};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    value: String,
    cursor: usize,
}
#[derive(Debug, Default)]
pub struct Editor {
    state: State,
    undo: UndoStack<State>,
    ring: KillRing,
    last: Action,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Action {
    #[default]
    None,
    Kill,
    Yank,
    TypeWord,
}
impl Editor {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self {
            state: State { value, cursor },
            ..Default::default()
        }
    }
    pub fn value(&self) -> &str {
        &self.state.value
    }
    pub fn cursor(&self) -> usize {
        self.state.cursor
    }
    fn save(&mut self) {
        self.undo.push(&self.state);
    }
    fn left(&mut self) {
        if self.state.cursor > 0 {
            let s = &self.state.value[..self.state.cursor];
            self.state.cursor -= s.graphemes(true).next_back().map_or(1, str::len);
        }
        self.last = Action::None
    }
    fn right(&mut self) {
        if self.state.cursor < self.state.value.len() {
            let s = &self.state.value[self.state.cursor..];
            self.state.cursor += s.graphemes(true).next().map_or(1, str::len);
        }
        self.last = Action::None
    }
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if text.chars().all(char::is_whitespace) || self.last != Action::TypeWord {
            self.save()
        }
        self.state.value.insert_str(self.state.cursor, text);
        self.state.cursor += text.len();
        self.last = Action::TypeWord;
    }
    pub fn backspace(&mut self) {
        if self.state.cursor == 0 {
            return;
        }
        self.save();
        let n = {
            let s = &self.state.value[..self.state.cursor];
            s.graphemes(true).next_back().map_or(1, str::len)
        };
        self.state
            .value
            .drain(self.state.cursor - n..self.state.cursor);
        self.state.cursor -= n;
        self.last = Action::None
    }
    pub fn delete(&mut self) {
        if self.state.cursor >= self.state.value.len() {
            return;
        }
        self.save();
        let n = self.state.value[self.state.cursor..]
            .graphemes(true)
            .next()
            .map_or(1, str::len);
        self.state
            .value
            .drain(self.state.cursor..self.state.cursor + n);
        self.last = Action::None
    }
    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.state = s;
            self.last = Action::None
        }
    }
    pub fn kill_to_start(&mut self) {
        if self.state.cursor == 0 {
            return;
        }
        self.save();
        let t = self.state.value[..self.state.cursor].to_owned();
        self.ring.push(&t, true, self.last == Action::Kill);
        self.state.value.drain(..self.state.cursor);
        self.state.cursor = 0;
        self.last = Action::Kill
    }
    pub fn kill_to_end(&mut self) {
        if self.state.cursor == self.state.value.len() {
            return;
        }
        self.save();
        let t = self.state.value[self.state.cursor..].to_owned();
        self.ring.push(&t, false, self.last == Action::Kill);
        self.state.value.truncate(self.state.cursor);
        self.last = Action::Kill
    }
    pub fn yank(&mut self) {
        if let Some(t) = self.ring.peek().map(str::to_owned) {
            self.save();
            self.insert_raw(&t);
            self.last = Action::Yank
        }
    }
    pub fn yank_pop(&mut self) {
        if self.last != Action::Yank || self.ring.len() < 2 {
            return;
        }
        if let Some(old) = self.ring.peek().map(str::to_owned) {
            self.save();
            let start = self.state.cursor - old.len();
            self.state.value.drain(start..self.state.cursor);
            self.state.cursor = start;
            self.ring.rotate();
            if let Some(t) = self.ring.peek().map(str::to_owned) {
                self.insert_raw(&t)
            }
            self.last = Action::Yank
        }
    }
    fn insert_raw(&mut self, t: &str) {
        self.state.value.insert_str(self.state.cursor, t);
        self.state.cursor += t.len()
    }
    pub fn move_word_left(&mut self) {
        let before = &self.state.value[..self.state.cursor];
        let mut i = self.state.cursor;
        for g in before.graphemes(true).rev() {
            if !g.chars().all(char::is_whitespace) {
                break;
            }
            i -= g.len()
        }
        while i > 0 {
            let g = self.state.value[..i]
                .graphemes(true)
                .next_back()
                .unwrap_or("");
            if g.chars().all(char::is_whitespace)
                || g.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                if g.chars().all(char::is_whitespace) {
                    break;
                }
                i -= g.len()
            } else {
                break;
            }
        }
        self.state.cursor = i;
        self.last = Action::None
    }
    pub fn move_word_right(&mut self) {
        let mut i = self.state.cursor;
        let tail = &self.state.value[i..];
        for g in tail.graphemes(true) {
            if !g.chars().all(char::is_whitespace) {
                break;
            }
            i += g.len()
        }
        let mut word = false;
        for g in self.state.value[i..].graphemes(true) {
            let al = g.chars().all(|c| c.is_alphanumeric() || c == '_');
            if word && !al {
                break;
            }
            if !al && !word {
                i += g.len();
                break;
            }
            i += g.len();
            word = true
        }
        self.state.cursor = i.min(self.state.value.len());
        self.last = Action::None
    }
    pub fn handle(&mut self, data: &str) -> Option<String> {
        match decode_key(data).as_deref() {
            Some("left") => self.left(),
            Some("right") => self.right(),
            Some("home") => self.state.cursor = 0,
            Some("end") => self.state.cursor = self.state.value.len(),
            Some("backspace") => self.backspace(),
            Some("delete") => self.delete(),
            Some("ctrl+w") | Some("alt+backspace") => self.kill_to_start(),
            Some("ctrl+u") => self.kill_to_start(),
            Some("ctrl+k") => self.kill_to_end(),
            Some("ctrl+y") => self.yank(),
            Some("alt+y") => self.yank_pop(),
            Some("ctrl+-") => self.undo(),
            Some("alt+left") | Some("ctrl+left") | Some("alt+b") => self.move_word_left(),
            Some("alt+right") | Some("ctrl+right") | Some("alt+f") => self.move_word_right(),
            Some(k) if k.starts_with("ctrl+") => {}
            _ => {
                if let Some(t) = decode_printable(data) {
                    self.insert(&t)
                } else {
                    return None;
                }
            }
        }
        Some(self.value().to_owned())
    }
}
pub fn decode_key(data: &str) -> Option<String> {
    match data {
        "\t" => Some("tab".to_owned()),
        "\r" | "\n" => Some("enter".to_owned()),
        "\x7f" | "\x08" => Some("backspace".to_owned()),
        "\x1b[A" => Some("up".to_owned()),
        "\x1b[B" => Some("down".to_owned()),
        "\x1b[C" => Some("right".to_owned()),
        "\x1b[D" => Some("left".to_owned()),
        "\x1b[H" | "\x1bOH" => Some("home".to_owned()),
        "\x1b[F" | "\x1bOF" => Some("end".to_owned()),
        "\x1b[3~" => Some("delete".to_owned()),
        "\x1b\x7f" => Some("alt+backspace".to_owned()),
        "\x1bB" => Some("alt+left".to_owned()),
        "\x1bF" => Some("alt+right".to_owned()),
        d if d.len() == 1
            && (d.as_bytes()[0] as char).is_ascii_control()
            && d.as_bytes()[0] > 0 =>
        {
            Some(format!("ctrl+{}", (d.as_bytes()[0] + 96) as char))
        }
        d if d.len() == 2 && d.as_bytes()[0] == 27 => Some(format!("alt+{}", d.chars().nth(1)?)),
        _ => None,
    }
}
pub fn decode_printable(data: &str) -> Option<String> {
    let caps = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let mut parts = caps.split(';');
    let cp: u32 = parts.next()?.parse().ok()?;
    let mods = parts
        .next()
        .and_then(|x| x.parse::<u32>().ok())
        .unwrap_or(1);
    if mods > 2 || cp < 32 {
        return None;
    }
    char::from_u32(cp).map(|c| c.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode() {
        let mut e = Editor::new("a😀b");
        e.left();
        assert_eq!(e.cursor(), 5);
        e.backspace();
        assert_eq!(e.value(), "ab")
    }
    #[test]
    fn kitty() {
        assert_eq!(decode_printable("\x1b[97u"), Some("a".into()))
    }
}
