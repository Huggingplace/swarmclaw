use std::io::{self, Write};
use crossterm::{cursor, terminal, execute};
use termimad::MadSkin;

fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::MoveTo(0, 0))?;
    
    let skin = MadSkin::default();
    
    let mut content = String::new();
    let mut prev_lines = 0;
    
    for i in 0..50 {
        content.push_str(&format!("Line {}\n", i));
        
        let (cols, rows) = terminal::size()?;
        
        if prev_lines > 0 {
            let move_up = prev_lines.min(rows - 1);
            execute!(stdout, cursor::MoveUp(move_up), cursor::MoveToColumn(0), terminal::Clear(terminal::ClearType::FromCursorDown))?;
        }
        
        let rendered = format!("{}", skin.term_text(&content));
        let lines = rendered.lines().count() as u16;
        
        // Termimad output already wraps and has newlines
        // In raw mode, we must convert \n to \r\n
        let safe = rendered.replace("\r\n", "\n").replace("\n", "\r\n");
        write!(stdout, "{}", safe)?;
        stdout.flush()?;
        
        prev_lines = lines;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    
    std::thread::sleep(std::time::Duration::from_secs(2));
    execute!(stdout, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}
