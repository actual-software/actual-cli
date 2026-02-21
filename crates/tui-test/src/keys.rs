/// Represents a key press that can be sent to a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(char),
    Alt(char),
    Char(char),
    Space,
}

impl Key {
    /// Convert this key to the byte sequence that a terminal would send.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Key::Enter => vec![13],
            Key::Escape => vec![27],
            Key::Tab => vec![9],
            Key::Backspace => vec![127],
            Key::Up => vec![27, 91, 65],
            Key::Down => vec![27, 91, 66],
            Key::Right => vec![27, 91, 67],
            Key::Left => vec![27, 91, 68],
            Key::Home => vec![27, 91, 72],
            Key::End => vec![27, 91, 70],
            Key::PageUp => vec![27, 91, 53, 126],
            Key::PageDown => vec![27, 91, 54, 126],
            Key::Ctrl(c) => vec![(*c as u8) & 0x1f],
            Key::Alt(c) => vec![27, *c as u8],
            Key::Char(c) => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
            Key::Space => vec![32],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enter_key() {
        assert_eq!(Key::Enter.to_bytes(), vec![13]);
    }

    #[test]
    fn test_escape_key() {
        assert_eq!(Key::Escape.to_bytes(), vec![27]);
    }

    #[test]
    fn test_tab_key() {
        assert_eq!(Key::Tab.to_bytes(), vec![9]);
    }

    #[test]
    fn test_backspace_key() {
        assert_eq!(Key::Backspace.to_bytes(), vec![127]);
    }

    #[test]
    fn test_arrow_keys() {
        assert_eq!(Key::Up.to_bytes(), vec![27, 91, 65]);
        assert_eq!(Key::Down.to_bytes(), vec![27, 91, 66]);
        assert_eq!(Key::Right.to_bytes(), vec![27, 91, 67]);
        assert_eq!(Key::Left.to_bytes(), vec![27, 91, 68]);
    }

    #[test]
    fn test_home_end_keys() {
        assert_eq!(Key::Home.to_bytes(), vec![27, 91, 72]);
        assert_eq!(Key::End.to_bytes(), vec![27, 91, 70]);
    }

    #[test]
    fn test_page_keys() {
        assert_eq!(Key::PageUp.to_bytes(), vec![27, 91, 53, 126]);
        assert_eq!(Key::PageDown.to_bytes(), vec![27, 91, 54, 126]);
    }

    #[test]
    fn test_ctrl_key() {
        // Ctrl+C = 0x03
        assert_eq!(Key::Ctrl('c').to_bytes(), vec![3]);
        // Ctrl+A = 0x01
        assert_eq!(Key::Ctrl('a').to_bytes(), vec![1]);
        // Ctrl+Z = 0x1A
        assert_eq!(Key::Ctrl('z').to_bytes(), vec![26]);
        // Ctrl+D = 0x04
        assert_eq!(Key::Ctrl('d').to_bytes(), vec![4]);
    }

    #[test]
    fn test_alt_key() {
        // Alt+x = ESC followed by 'x'
        assert_eq!(Key::Alt('x').to_bytes(), vec![27, b'x']);
        assert_eq!(Key::Alt('a').to_bytes(), vec![27, b'a']);
    }

    #[test]
    fn test_char_key() {
        assert_eq!(Key::Char('a').to_bytes(), vec![b'a']);
        assert_eq!(Key::Char('Z').to_bytes(), vec![b'Z']);
        assert_eq!(Key::Char('1').to_bytes(), vec![b'1']);
    }

    #[test]
    fn test_char_key_unicode() {
        // Unicode character should produce its UTF-8 encoding
        let bytes = Key::Char('\u{00e9}').to_bytes(); // é
        assert_eq!(bytes, vec![0xc3, 0xa9]);
    }

    #[test]
    fn test_space_key() {
        assert_eq!(Key::Space.to_bytes(), vec![32]);
    }
}
