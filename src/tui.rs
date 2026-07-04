use std::io::{self, Stdout, Write};

use color_eyre::eyre::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

pub type CrosstermTerminal = Terminal<CrosstermBackend<Stdout>>;

pub struct Tui {
    terminal: CrosstermTerminal,
    keyboard_enhanced: bool,
}

impl Tui {
    pub fn new() -> Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            keyboard_enhanced: false,
        })
    }

    /// Whether the terminal reports modified keys (kitty keyboard
    /// protocol).  Without it Shift+Enter is indistinguishable from
    /// plain Enter.
    pub fn keyboard_enhanced(&self) -> bool {
        self.keyboard_enhanced
    }

    pub fn enter(&mut self) -> Result<()> {
        enable_raw_mode()?;
        // Anything failing past raw mode must not strand the shell in
        // a raw, alt-screen, mouse-captured state.
        if let Err(e) = self.enter_screens() {
            let _ = Self::reset();
            return Err(e);
        }
        Ok(())
    }

    fn enter_screens(&mut self) -> Result<()> {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;

        // Opt in to disambiguated key reports so modifiers on Enter
        // (Shift+Enter newline) reach us.  The protocol is opt-in per
        // application even on terminals that support it.
        self.keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false);
        if self.keyboard_enhanced {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = Self::reset();
            original_hook(panic_info);
        }));

        self.terminal.hide_cursor()?;
        self.terminal.clear()?;

        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        Self::reset()?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn reset() -> Result<()> {
        disable_raw_mode()?;
        // Pop unconditionally: terminals without the kitty protocol
        // ignore the sequence, and popping an empty stack is a no-op,
        // so this is safe even when enter() never pushed.  Show the
        // cursor here too — hide_cursor() sets a global mode that
        // survives leaving the alternate screen, and this is the only
        // cleanup the panic hook runs.
        execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        )?;
        Ok(())
    }

    pub fn draw<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut ratatui::Frame),
    {
        self.terminal.draw(f)?;
        Ok(())
    }

    pub fn ring_bell(&mut self) -> Result<()> {
        let backend = self.terminal.backend_mut();
        backend.write_all(b"\x07")?;
        backend.flush()?;
        Ok(())
    }
}
