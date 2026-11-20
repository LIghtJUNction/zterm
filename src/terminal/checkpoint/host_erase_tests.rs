use super::*;

fn roundtrip(terminal: &Terminal) {
    let bytes = terminal.checkpoint().unwrap();
    let restored = Terminal::restore_checkpoint(&bytes).unwrap();
    assert_eq!(restored.checkpoint().unwrap(), bytes);
    assert_eq!(restored.responses(), terminal.responses());
}

#[test]
fn fresh_ed2_and_native_plain_control_remain_checkpointable() {
    for host_enabled in [false, true] {
        let mut terminal = Terminal::new(80, 20, 500);
        terminal.set_ui_host_enabled(host_enabled);
        terminal.set_cell_size(11, 23);
        terminal.feed(b"\x1b[2J");
        assert!(terminal.graphics().is_query_only());
        roundtrip(&terminal);
        terminal.feed(b"\x1b[?25lHELLO reload-query-probes\r\n\x1b[6n");
        terminal.feed(b"\x1b[?25l\x1b[48;2;15;28;39m\x1b[2J\x1b[HQUERY-ONLY-SHELL\r\n\x1b[6n");
        assert!(terminal.graphics().is_query_only());
        roundtrip(&terminal);
    }
}

#[test]
fn completed_queries_followed_by_ed2_keep_replies_and_allow_checkpoint() {
    for probe in [
        b"\x1b_Ga=q,i=31,f=32,s=1,v=1;AAAA/w==\x1b\\".as_slice(),
        b"\x1b_Ga=q,i=31,e=T\x1b\\",
        b"\x1b_ZUI;1;{\"v\":1,\"op\":\"capabilities\"}\x1b\\",
    ] {
        let mut terminal = Terminal::new(16, 3, 8);
        terminal.set_ui_host_enabled(true);
        terminal.feed(probe);
        let replies = terminal.responses().to_vec();
        assert!(!replies.is_empty());
        terminal.feed(b"\x1b[2J\x1b[Hprompt");
        assert!(terminal.graphics().is_query_only());
        assert_eq!(terminal.responses(), replies);
        roundtrip(&terminal);
    }
}

#[test]
fn ed2_keeps_its_existing_cancellation_of_pending_queries() {
    for payload in [b"".as_slice(), b"AAAA"] {
        let mut terminal = Terminal::new(16, 3, 8);
        terminal.feed(b"\x1b_Ga=q,i=31,f=32,s=1,v=1,m=1;");
        terminal.feed(payload);
        terminal.feed(b"\x1b\\");
        assert!(!terminal.graphics().is_query_only());
        assert!(terminal.checkpoint().unwrap_err().contains("graphics"));
        assert!(terminal.responses().is_empty());
        terminal.feed(b"\x1b[2J");
        // ED2 has always cancelled this transfer. The empty store must not
        // acquire non-query history merely from the internal erase command.
        assert!(terminal.graphics().is_query_only());
        assert_eq!(terminal.graphics().pending_bytes(), 0);
        assert!(terminal.responses().is_empty());
        roundtrip(&terminal);
        terminal.feed(b"\x1b_Ga=q,i=32,f=32,s=1,v=1;AAAA/w==\x1b\\");
        assert_eq!(terminal.responses(), b"\x1b_Gi=32;OK\x1b\\");
        roundtrip(&terminal);
    }
}

#[test]
fn ed2_cannot_hide_stateful_image_or_pending_transfer_history() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b_Ga=T,f=32,s=1,v=1,C=1;AAAA/w==\x1b\\");
    assert_eq!(terminal.graphics().image_count(), 1);
    assert_eq!(terminal.graphics().placement_count(), 1);
    terminal.feed(b"\x1b[2J");
    assert_eq!(terminal.graphics().image_count(), 0);
    assert_eq!(terminal.graphics().placement_count(), 0);
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("non-query graphics")
    );

    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b_Ga=t,i=31,f=32,s=1,v=1,m=1;\x1b\\\x1b[2J");
    assert_eq!(terminal.graphics().pending_bytes(), 0);
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("non-query graphics")
    );
}

#[test]
fn external_kitty_delete_still_acquires_guarded_history() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b_Ga=d\x1b\\");
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("non-query graphics")
    );
    terminal.feed(b"\x1b[2J");
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("non-query graphics")
    );
}
