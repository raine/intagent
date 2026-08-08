use std::io::{IsTerminal, Write};

use chrono::Local;

pub fn terminal_line(
    output: &mut impl Write,
    is_terminal: bool,
    value: &str,
) -> std::io::Result<()> {
    let time = Local::now().format("%H:%M:%S");
    if is_terminal && std::env::var_os("NO_COLOR").is_none() {
        writeln!(output, "\x1b[2m{time}\x1b[0m  {value}")
    } else {
        writeln!(output, "{time}  {value}")
    }
}

pub fn stdout_line(value: &str) {
    let mut output = std::io::stdout().lock();
    let is_terminal = std::io::stdout().is_terminal();
    let _ = terminal_line(&mut output, is_terminal, value);
}

pub fn stderr_line(value: &str) {
    let mut output = std::io::stderr().lock();
    let is_terminal = std::io::stderr().is_terminal();
    let _ = terminal_line(&mut output, is_terminal, value);
}
