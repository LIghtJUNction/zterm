use super::*;
use alloc::string::String;

fn roundtrip(terminal: &Terminal) -> Terminal {
    let bytes = terminal.checkpoint().unwrap();
    let restored = Terminal::restore_checkpoint(&bytes).unwrap();
    assert_eq!(restored.checkpoint().unwrap(), bytes);
    restored
}

fn continued_equally(mut terminal: Terminal, suffix: &[u8]) {
    let mut restored = roundtrip(&terminal);
    terminal.feed(suffix);
    restored.feed(suffix);
    assert_eq!(
        terminal.checkpoint().unwrap(),
        restored.checkpoint().unwrap()
    );
}

#[test]
fn ordinary_shell_history_styles_modes_markers_and_replies_survive() {
    let mut terminal = Terminal::new(18, 4, 32);
    terminal.feed(b"\x1b]2;checkpoint title\x07\x1b]7;file:///tmp/work\x07");
    terminal.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07printf test\x1b]133;C\x07");
    terminal.feed("\r\nline one\r\nline two\r\n👩🏽‍💻 e\u{301}\r\nlast\r\n".as_bytes());
    terminal.feed(b"\x1b]133;D;7\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    terminal.feed(b"\x1b]8;id=link;https://example.test/path\x07\x1b[1;3;38;2;12;34;56mX");
    terminal.feed(b"\x1b7\x1b[?1;9;1004;1006;2004h\x1b[>7u\x1b[5 q\x1b[3g\x1bH");
    terminal.feed(b"\x1b[6n\x1b[c");
    assert!(terminal.history_len() > 0);
    assert!(!terminal.responses().is_empty());
    assert!(terminal.commands().next().is_some());
    continued_equally(terminal, b"\x1b8\x1b[<u\x1b]8;;\x07next\r\n\x1b[6n");
}

#[test]
fn active_and_cached_alternate_screens_keep_cursors_and_primary_history() {
    let mut terminal = Terminal::new(12, 3, 16);
    terminal.feed(b"primary\r\none\r\ntwo\r\nthree");
    terminal.feed(b"\x1b[?1049h\x1b[2;3H\x1b[31mTUI\x1b7\x1b[>3u");
    let mut restored = roundtrip(&terminal);
    for source in [
        b"\x1b[?1049l".as_slice(),
        b"\x1b[?47h",
        b"\x1b8\x1b[<uX",
        b"\x1b[?47l",
    ] {
        terminal.feed(source);
        restored.feed(source);
        assert_eq!(
            terminal.checkpoint().unwrap(),
            restored.checkpoint().unwrap()
        );
        restored = roundtrip(&restored);
    }
    assert!(!restored.modes().alternate_screen);
    assert!(restored.history_len() > 0);
}

#[test]
fn deferred_wrap_wide_cells_tabs_and_scroll_margins_survive() {
    for (cols, rows) in [(1, 3), (4, 4), (17, 5)] {
        let mut terminal = Terminal::new(cols, rows, 12);
        terminal.feed("中 e\u{301} 👩🏽‍💻".as_bytes());
        terminal.feed(b"\x1b[2;3r\x1b[?6h\x1b[H\x1b7");
        terminal.feed(&alloc::vec![b'x'; cols]);
        continued_equally(terminal, "Y\r\n🇺🇳\x1b8\tZ".as_bytes());
    }
}

#[test]
fn grapheme_pool_vacancies_live_ids_and_continuations_survive() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed("👩🏽‍💻e\u{301}".as_bytes());
    let dead = terminal.state.graphemes.intern("A\u{301}").unwrap();
    let ids: Vec<_> = terminal
        .state
        .screen
        .cells
        .iter()
        .map(|cell| cell.grapheme_id)
        .collect();
    terminal.state.graphemes.collect(ids.into_iter());
    assert!(terminal.state.graphemes.get(dead).is_none());
    let mut restored = roundtrip(&terminal);
    for source in ["\u{308}", " B\u{301}", "\r\nnew"] {
        terminal.feed(source.as_bytes());
        restored.feed(source.as_bytes());
        assert_eq!(
            terminal.checkpoint().unwrap(),
            restored.checkpoint().unwrap()
        );
    }
}

#[test]
fn split_utf8_csi_and_osc_are_rejected_without_resetting_the_parser() {
    for sequence in [
        "👩🏽‍💻".as_bytes(),
        b"\x1b[38;2;12;34;56mX",
        b"\x1b]2;new title\x07X",
    ] {
        for split in 1..sequence.len() {
            let mut terminal = Terminal::new(16, 3, 8);
            let mut control = Terminal::new(16, 3, 8);
            terminal.feed(&sequence[..split]);
            control.feed(&sequence[..split]);
            let busy = !terminal.parser.is_ground() || terminal.framing != 0;
            if busy {
                assert!(terminal.checkpoint().unwrap_err().contains("pending input"));
            } else {
                roundtrip(&terminal);
            }
            terminal.feed(&sequence[split..]);
            control.feed(&sequence[split..]);
            assert_eq!(
                terminal.checkpoint().unwrap(),
                control.checkpoint().unwrap()
            );
        }
    }
}

#[test]
fn completed_graphics_and_ui_probes_keep_replies_and_allow_checkpoint() {
    for host_enabled in [false, true] {
        for probe in [
            b"\x1b_Ga=q,i=31,f=32,s=1,v=1;AAAA/w==\x1b\\".as_slice(),
            b"\x1b_Ga=q,i=31,e=T,f=32,U=1,C=1\x1b\\",
            b"\x1b_ZUI;1;{\"v\":1,\"op\":\"capabilities\"}\x1b\\",
        ] {
            let mut terminal = Terminal::new(16, 3, 8);
            terminal.set_ui_host_enabled(host_enabled);
            terminal.feed(b"ordinary shell\r\n");
            terminal.feed(probe);
            assert!(!terminal.responses().is_empty());
            let mut restored = roundtrip(&terminal);
            assert_eq!(restored.responses(), terminal.responses());
            assert_eq!(restored.ui_revision(), 0);
            for suffix in [probe, b"\x1b[?1049h\x1b[?1049l", b"\x1bcplain text"] {
                terminal.feed(suffix);
                restored.feed(suffix);
                assert_eq!(
                    terminal.checkpoint().unwrap(),
                    restored.checkpoint().unwrap()
                );
            }
        }
    }
}

#[test]
fn fragmented_probe_frames_are_rejected_until_their_real_terminator() {
    for probe in [
        b"\x1b_Ga=q,i=31,f=32,s=1,v=1;AAAA/w==\x1b\\".as_slice(),
        b"\x1b_Ga=q,i=31,e=T\x1b\\",
        b"\x1b_ZUI;1;{\"v\":1,\"op\":\"capabilities\"}\x1b\\",
    ] {
        for split in 1..probe.len() {
            let mut terminal = Terminal::new(16, 3, 8);
            terminal.set_ui_host_enabled(true);
            terminal.feed(&probe[..split]);
            assert!(terminal.checkpoint().unwrap_err().contains("pending input"));
            terminal.feed(&probe[split..]);
            assert!(!terminal.responses().is_empty());
            roundtrip(&terminal);
        }
    }
}

#[test]
fn zero_byte_query_transfer_is_not_lost_at_a_complete_apc_boundary() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b_Ga=q,i=31,f=32,s=1,v=1,m=1;\x1b\\");
    assert!(terminal.parser.is_ground());
    assert_eq!(terminal.framing, 0);
    assert_eq!(terminal.graphics().pending_bytes(), 0);
    assert!(!terminal.graphics().is_query_only());
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("pending graphics")
    );
    assert!(terminal.responses().is_empty());

    terminal.feed(b"\x1b_Ga=q,i=32,e=T\x1b\\");
    assert_eq!(terminal.responses(), b"\x1b_Gi=32,e=T;OK\x1b\\");
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("pending graphics")
    );
    terminal.feed(b"\x1b_Gm=0;AAAA/w==\x1b\\");
    assert!(terminal.responses().ends_with(b"\x1b_Gi=31;OK\x1b\\"));
    assert!(terminal.graphics().is_query_only());
    roundtrip(&terminal);
}

#[test]
fn cancelled_queries_and_ignored_ui_frames_leave_no_protocol_identity() {
    for cancel in [0x18, 0x1a] {
        let mut terminal = Terminal::new(16, 3, 8);
        terminal.feed(b"\x1b_Ga=q,i=31,f=32,s=1,v=1;AAAA");
        assert!(terminal.checkpoint().is_err());
        terminal.feed(&[cancel]);
        assert!(terminal.graphics().is_query_only());
        roundtrip(&terminal);
    }
    for host_enabled in [false, true] {
        let mut terminal = Terminal::new(16, 3, 8);
        terminal.set_ui_host_enabled(host_enabled);
        for ignored in [
            b"\x1b_ZUI;1;{\"v\":1,\"type\":\"capabilities\"}\x1b\\".as_slice(),
            b"\x1b_ZUI;2;unsupported\x1b\\",
            b"\x1b_ZUI;1;{\"v\":1,\"op\":\"replace\",\"surface\":0,\"rev\":1}\x1b\\",
        ] {
            terminal.feed(ignored);
            assert!(terminal.ui_scene().is_none());
            assert_eq!(terminal.ui_revision(), 0);
            roundtrip(&terminal);
        }
    }
}

#[test]
fn stateful_graphics_are_never_silently_lost_or_forgotten_after_reset() {
    for input in [
        b"\x1b_Ga=t,f=32,s=1,v=1,m=1;\x1b\\".as_slice(),
        b"\x1b_Ga=T,f=32,s=1,v=1;AAAA/w==\x1b\\",
        b"\x1bPq~\x1b\\",
    ] {
        let mut terminal = Terminal::new(16, 3, 8);
        terminal.feed(input);
        let before = (
            terminal.graphics().image_count(),
            terminal.graphics().pending_bytes(),
        );
        assert!(terminal.checkpoint().unwrap_err().contains("graphics"));
        assert_eq!(
            (
                terminal.graphics().image_count(),
                terminal.graphics().pending_bytes()
            ),
            before
        );
        terminal.feed(b"\x1bcplain text");
        assert!(
            terminal
                .checkpoint()
                .unwrap_err()
                .contains("non-query graphics")
        );
        terminal.feed(b"\x1b_Ga=q,i=31,e=T\x1b\\");
        assert!(
            terminal
                .checkpoint()
                .unwrap_err()
                .contains("non-query graphics")
        );
    }
}

#[test]
fn inactive_and_deleted_images_do_not_become_query_only() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b_Ga=T,i=31,f=32,s=1,v=1;AAAA/w==\x1b\\");
    assert_eq!(terminal.graphics().image_count(), 1);
    terminal.feed(b"\x1b[?1049h");
    assert_eq!(terminal.graphics().image_count(), 0);
    assert_eq!(terminal.state.inactive_graphics.image_count(), 1);
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("graphics resources")
    );
    terminal.feed(b"\x1b[?1049l\x1b_Ga=d,d=A\x1b\\");
    assert_eq!(terminal.graphics().image_count(), 0);
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("non-query graphics")
    );
}

fn native_scene(revision: u64) -> Vec<u8> {
    alloc::format!(
        "\x1b_ZUI;1;{{\"v\":1,\"op\":\"replace\",\"surface\":9,\"rev\":{revision},\"root\":1,\"events\":true,\"nodes\":[{{\"id\":1,\"type\":\"button\",\"label\":\"Run\",\"action\":\"run\"}}]}}\x1b\\"
    ).into_bytes()
}

#[test]
fn native_ui_clear_keeps_revision_floor_and_event_identity_guarded() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.set_ui_host_enabled(true);
    terminal.feed(&native_scene(1));
    assert_eq!(terminal.ui_action(1, 9, 1).unwrap().event_id, 1);
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("native UI resources")
    );
    assert_eq!(terminal.ui_scene().unwrap().revision, 1);
    terminal.feed(
        b"\x1b_ZUI;1;{\"v\":1,\"op\":\"clear\",\"surface\":9,\"rev\":2,\"base_rev\":1}\x1b\\",
    );
    assert!(terminal.ui_scene().is_none());
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("native UI protocol identity")
    );
    terminal.feed(&native_scene(1));
    assert!(
        terminal.ui_scene().is_none(),
        "old revision must remain rejected"
    );
    terminal.feed(&native_scene(3));
    assert_eq!(terminal.ui_action(1, 9, 3).unwrap().event_id, 2);
    terminal.feed(b"\x1bcplain text");
    assert!(terminal.ui_scene().is_none());
    assert!(
        terminal
            .checkpoint()
            .unwrap_err()
            .contains("native UI protocol identity")
    );
}

#[test]
fn unsupported_ui_host_ignores_scenes_without_acquiring_protocol_identity() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(&native_scene(1));
    assert!(terminal.ui_scene().is_none());
    assert_eq!(terminal.ui_revision(), 0);
    roundtrip(&terminal);
}

#[test]
fn pending_clipboard_work_is_never_silently_lost() {
    let mut terminal = Terminal::new(16, 3, 8);
    terminal.feed(b"\x1b]52;c;?\x07");
    assert!(terminal.checkpoint().unwrap_err().contains("OSC 52"));
    assert!(terminal.state.clipboard.pop().is_some());
    roundtrip(&terminal);
}

#[test]
fn truncated_changed_unknown_and_trailing_checkpoints_are_rejected() {
    let mut terminal = Terminal::new(3, 2, 0);
    terminal.feed(b"hello");
    let bytes = terminal.checkpoint().unwrap();
    for length in 0..bytes.len() {
        assert!(Terminal::restore_checkpoint(&bytes[..length]).is_err());
    }
    for offset in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[offset] ^= 0x80;
        assert!(
            Terminal::restore_checkpoint(&changed).is_err(),
            "offset={offset}"
        );
    }
    let mut trailing = bytes;
    trailing.push(0);
    let len = (trailing.len() - HEADER) as u32;
    trailing[12..16].copy_from_slice(&len.to_le_bytes());
    let checksum = digest(&trailing[HEADER..]);
    trailing[16..24].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(
        Terminal::restore_checkpoint(&trailing).err(),
        Some("trailing terminal checkpoint data")
    );
}

#[test]
fn decoder_checks_wide_pairs_grapheme_ids_and_mode_invariants() {
    let mut terminal = Terminal::new(4, 2, 0);
    terminal.state.screen.cells[0].width = 0;
    assert!(Terminal::restore_checkpoint(&terminal.checkpoint().unwrap()).is_err());
    terminal.state.screen.cells[0] = Cell::BLANK;
    terminal.state.screen.cells[0].grapheme_id = 999;
    assert!(Terminal::restore_checkpoint(&terminal.checkpoint().unwrap()).is_err());
    terminal.state.screen.cells[0] = Cell::BLANK;
    terminal.state.modes.alternate_screen = true;
    assert!(Terminal::restore_checkpoint(&terminal.checkpoint().unwrap()).is_err());
}

#[test]
fn restored_unicode_text_remains_complete_after_history_and_resize() {
    let mut terminal = Terminal::new(8, 3, 16);
    terminal.feed("👩🏽‍💻 e\u{301}\r\n一二三\r\n🇺🇳\r\nlast".as_bytes());
    let mut restored = roundtrip(&terminal);
    terminal.resize(5, 4);
    restored.resize(5, 4);
    assert_eq!(
        terminal.checkpoint().unwrap(),
        restored.checkpoint().unwrap()
    );
    let source: String = restored.cell_chars(restored.cell(0, 3)).collect();
    assert!(!source.is_empty());
}

#[test]
fn alternate_resize_preserves_saved_wrap_before_the_new_right_margin() {
    for cached in [false, true] {
        let mut terminal = Terminal::new(4, 3, 8);
        terminal.feed(b"\x1b[?47h1234\x1b7");
        assert!(terminal.state.screen.saved.wrap);
        if cached {
            terminal.feed(b"\x1b[?47l");
        }
        terminal.resize(9, 3);
        let mut restored = roundtrip(&terminal);
        if cached {
            terminal.feed(b"\x1b[?47h");
            restored.feed(b"\x1b[?47h");
        }
        terminal.feed(b"\x1b8");
        restored.feed(b"\x1b8");
        assert!(terminal.state.screen.wrap);
        assert!(terminal.state.screen.x + 1 < terminal.cols());
        restored = roundtrip(&restored);
        terminal.feed(b"Z\r\nnext");
        restored.feed(b"Z\r\nnext");
        assert_eq!(
            terminal.checkpoint().unwrap(),
            restored.checkpoint().unwrap()
        );
    }
}

#[test]
fn full_history_budget_does_not_lose_extra_rows_after_restore() {
    let mut terminal = Terminal::new(120, 3, 65_536);
    for _ in 0..2_400 {
        terminal.feed(b"line\r\n");
    }
    let history = &terminal.state.history;
    assert!(history.len() > 2_048 && !history.len().is_power_of_two());
    assert!(
        terminal.state.history_bytes + history.capacity() * mem::size_of::<HistoryRow>()
            <= MAX_HISTORY_BYTES
    );
    let mut restored = roundtrip(&terminal);
    for _ in 0..8 {
        terminal.feed(b"next\r\n");
        restored.feed(b"next\r\n");
        assert_eq!(terminal.history_len(), restored.history_len());
    }
    assert!(
        restored.state.history_bytes
            + restored.state.history.capacity() * mem::size_of::<HistoryRow>()
            <= MAX_HISTORY_BYTES
    );
    assert_eq!(
        terminal.checkpoint().unwrap(),
        restored.checkpoint().unwrap()
    );
}
