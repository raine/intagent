use intake::terminal::terminal_line;

#[test]
fn dims_timestamps_only_on_color_terminals() {
    unsafe {
        std::env::remove_var("NO_COLOR");
    }
    let mut output = Vec::new();
    terminal_line(&mut output, true, "handled event").unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.starts_with("\x1b[2m"));
    let time = &output[4..12];
    assert!(time.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 2 | 5) {
            byte == b':'
        } else {
            byte.is_ascii_digit()
        }
    }));
    assert!(output.ends_with("\x1b[0m  handled event\n"));

    unsafe {
        std::env::set_var("NO_COLOR", "1");
    }
    let mut plain = Vec::new();
    terminal_line(&mut plain, true, "handled event").unwrap();
    assert!(!String::from_utf8(plain).unwrap().contains("\x1b["));
    unsafe {
        std::env::remove_var("NO_COLOR");
    }
}
