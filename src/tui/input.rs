use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// Exercised directly by this file's unit tests below; wired into the TUI's
/// `SourceForm`/`RestoreTarget`/`ConfirmRestore` modes by plan-6 Tasks 5-6.
#[allow(dead_code)]
pub struct TextField {
    pub masked: bool,
    buffer: String,
}

#[allow(dead_code)]
impl TextField {
    pub fn new(masked: bool) -> TextField {
        TextField {
            masked,
            buffer: String::new(),
        }
    }

    pub fn handle(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => self.buffer.push(c),
            KeyCode::Backspace => {
                self.buffer.pop();
            }
            _ => {}
        }
    }

    pub fn value(&self) -> &str {
        &self.buffer
    }

    pub fn display(&self) -> String {
        if self.masked {
            "*".repeat(self.buffer.chars().count())
        } else {
            self.buffer.clone()
        }
    }

    pub fn set(&mut self, v: &str) {
        self.buffer = v.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    #[test]
    fn typing_and_backspace() {
        let mut f = TextField::new(false);
        f.handle(KeyEvent::from(KeyCode::Char('a')));
        f.handle(KeyEvent::from(KeyCode::Char('b')));
        f.handle(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(f.value(), "a");
    }

    #[test]
    fn masked_display_hides_content() {
        let mut f = TextField::new(true);
        f.set("hunter2");
        assert_eq!(f.display(), "*******");
        assert_eq!(f.value(), "hunter2");
    }
}
