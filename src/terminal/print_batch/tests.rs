use super::*;
use alloc::{format, string::String, vec::Vec};

fn same_screen(a: &State, b: &State, left: &Screen, right: &Screen) {
    assert_eq!(left.cells, right.cells);
    for (x, y) in left.cells.iter().zip(&right.cells) {
        if x.grapheme_id != 0 || y.grapheme_id != 0 {
            assert_eq!(
                a.graphemes.chars(x).collect::<String>(),
                b.graphemes.chars(y).collect::<String>()
            );
        }
    }
    assert_eq!(
        (
            left.cols,
            left.rows,
            left.row0,
            left.x,
            left.y,
            left.wrap,
            left.top,
            left.bottom,
            left.origin
        ),
        (
            right.cols,
            right.rows,
            right.row0,
            right.x,
            right.y,
            right.wrap,
            right.top,
            right.bottom,
            right.origin
        )
    );
    assert_eq!(left.blank, right.blank);
    assert_eq!(left.wrapped, right.wrapped);
    assert_eq!(left.dirty, right.dirty);
    assert_eq!(
        (
            left.saved.x,
            left.saved.y,
            left.saved.origin,
            left.saved.g0,
            left.saved.g1,
            left.saved.use_g1,
            left.saved.wrap,
            left.saved.style.cell('X', 1)
        ),
        (
            right.saved.x,
            right.saved.y,
            right.saved.origin,
            right.saved.g0,
            right.saved.g1,
            right.saved.use_g1,
            right.saved.wrap,
            right.saved.style.cell('X', 1)
        )
    );
    assert_eq!(
        (left.keyboard, left.keyboard_stack, left.keyboard_depth),
        (right.keyboard, right.keyboard_stack, right.keyboard_depth)
    );
}

fn same(a: &Terminal, b: &Terminal) {
    let (left, right) = (&a.state, &b.state);
    same_screen(left, right, &left.screen, &right.screen);
    for (x, y) in [
        (&left.primary, &right.primary),
        (&left.inactive_alternate, &right.inactive_alternate),
    ] {
        match (x, y) {
            (Some(x), Some(y)) => same_screen(left, right, x, y),
            (None, None) => {}
            _ => panic!("screen presence differs"),
        }
    }
    assert_eq!(left.history.len(), right.history.len());
    for (x, y) in left.history.iter().zip(&right.history) {
        assert_eq!(x.wrapped, y.wrapped);
        assert_eq!(x.cells, y.cells);
        for (x, y) in x.cells.iter().zip(&y.cells) {
            if x.grapheme_id != 0 || y.grapheme_id != 0 {
                assert_eq!(
                    left.graphemes.chars(x).collect::<String>(),
                    right.graphemes.chars(y).collect::<String>()
                );
            }
        }
    }
    assert_eq!(
        (
            left.history_bytes,
            left.line_base,
            left.last_char,
            left.last_grapheme,
            left.grapheme_overflow,
            left.grapheme_writes,
            left.grapheme_collect_after
        ),
        (
            right.history_bytes,
            right.line_base,
            right.last_char,
            right.last_grapheme,
            right.grapheme_overflow,
            right.grapheme_writes,
            right.grapheme_collect_after
        )
    );
    assert_eq!(
        (
            left.modes,
            left.autowrap,
            left.insert,
            left.newline,
            left.g0,
            left.g1,
            left.use_g1
        ),
        (
            right.modes,
            right.autowrap,
            right.insert,
            right.newline,
            right.g0,
            right.g1,
            right.use_g1
        )
    );
    assert_eq!(left.style.cell('X', 1), right.style.cell('X', 1));
    assert_eq!(
        left.style.cell('\u{10eeee}', 1),
        right.style.cell('\u{10eeee}', 1)
    );
    assert_eq!(left.responses, right.responses);
    assert_eq!(
        (&left.title, &left.cwd, &left.tabs),
        (&right.title, &right.cwd, &right.tabs)
    );
    assert_eq!(
        a.commands().collect::<Vec<_>>(),
        b.commands().collect::<Vec<_>>()
    );
    for command in a.commands() {
        assert_eq!(
            a.command_output(command.id, 4096),
            b.command_output(command.id, 4096)
        );
    }
    assert_eq!(a.shell_phase(), b.shell_phase());
    assert_eq!(
        (a.framing, a.osc_probe, a.apc_route, a.text_revision),
        (b.framing, b.osc_probe, b.apc_route, b.text_revision)
    );
    assert_eq!(left.graphics.revision(), right.graphics.revision());
    let draws = |t: &Terminal| {
        t.graphics_draws(0)
            .map(|d| {
                (
                    d.image_id,
                    d.placement_id,
                    d.clip.map(|c| (c.x, c.y, c.width, c.height)),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(draws(a), draws(b));
}

fn feed(a: &mut Terminal, b: &mut Terminal, bytes: &[u8]) {
    a.feed(bytes);
    b.feed_scalar_reference(bytes);
    same(a, b);
}

fn pair(cols: usize) -> (Terminal, Terminal) {
    (Terminal::new(cols, 3, 5), Terminal::new(cols, 3, 5))
}

fn chunks(setup: &[u8], input: &[u8], cols: usize, sizes: &[usize]) {
    for &size in sizes {
        let (mut a, mut b) = pair(cols);
        feed(&mut a, &mut b, setup);
        for part in input.chunks(size) {
            feed(&mut a, &mut b, part);
        }
        feed(&mut a, &mut b, "\x1b[2b\u{301}X\x1b[6n".as_bytes());
        a.resize(11, 4);
        b.resize(11, 4);
        same(&a, &b);
    }
}

#[test]
fn ordinary_unicode_runs_match_scalar_across_capacity_and_pty_chunks() {
    let source = "中文 Ελληνικά 日本語 안녕하세요 abc\r\n".repeat(24);
    chunks(
        b"",
        source.as_bytes(),
        24,
        &[1, 4, 5, 31, 127, 511, 512, 513, source.len()],
    );
}

#[test]
fn real_prefix_context_keeps_ri_zwj_indic_hangul_and_prepend_at_every_split() {
    for (prefix, source) in [
        ("🇦", "🇧🇨🇩X"),
        ("🇦🇧", "🇨🇩🇪X"),
        ("👩\u{301}‍", "💻中文X"),
        ("\u{915}\u{94d}", "\u{915}中文X"),
        ("\u{1100}", "\u{1161}\u{11a8}中文X"),
        ("\u{600}", "abc中文X"),
        ("e", "\x1b[31m\u{301}中文X"),
        ("👩", "\x1b[32m‍💻中文X"),
    ] {
        for split in 0..=source.len() {
            let (mut a, mut b) = pair(24);
            feed(&mut a, &mut b, prefix.as_bytes());
            feed(&mut a, &mut b, &source.as_bytes()[..split]);
            feed(&mut a, &mut b, &source.as_bytes()[split..]);
            feed(&mut a, &mut b, b"\x1b[2b\x1b[6n");
        }
    }
}

#[test]
fn controls_queries_and_outer_graphics_frames_see_flushed_text() {
    let source = concat!(
        "中文\x1b[6n\x1b]133;A\x07$ \x1b]133;B\x07echo\r\n",
        "\x1b]133;C\x07输出\x1b]133;D;0\x07",
        "\x1b_Ga=T,U=1,i=42,p=7,s=2,v=1,c=2,r=1,C=1;/wAA/wD/AP8=\x1b\\",
        "\x1b[38;5;42m\x1b[58;5;7m\u{10eeee}\u{305}\u{30d}\u{30e}中文",
        "\x1b]52;c;?\x07\x1b]10;?\x07\x1bP$qm\x1b\\",
        "中文\x1bPq#1;2;100;0;0~\x1b\\中文"
    );
    for size in [1, 5, 17, 128, source.len()] {
        let (mut a, mut b) = pair(24);
        for part in source.as_bytes().chunks(size) {
            feed(&mut a, &mut b, part);
        }
        let request = |value: Option<crate::ClipboardRequest>| {
            value.map(|value| {
                (
                    value.selectors().to_vec(),
                    value.text,
                    value.bell_terminated,
                )
            })
        };
        assert_eq!(
            request(a.next_clipboard_request()),
            request(b.next_clipboard_request())
        );
        feed(&mut a, &mut b, b"\x1b[2b\x1b[6n");
    }
    let source = format!("中文\x1b]2;{}\x07尾部", "x".repeat(5000));
    chunks(b"", source.as_bytes(), 24, &[7, 511, source.len()]);
}

#[test]
fn invalid_truncated_utf8_controls_and_del_keep_scalar_callback_semantics() {
    let sources: &[&[u8]] = &[
        b"\xe4\xb8\xad\xe6\x96\x87\x7fABC\xc2\x85XYZ",
        b"\xf0\x9f\x91\xa9\xe2\x80\x8d\x7f\xf0\x9f\x92\xbb",
        b"\xe4\xb8\xad\xc2ABC\xe2\x82\x1b[31m\xe6\x96\x87",
        b"\xe4\xb8\xad\xf4\x90\x80\x80\x80\x9bABC\xe2\x82",
        b"\xe4\xb8\xad\x00\x07\x18\x1a\x1b[31m\xe6\x96\x87\xc2",
    ];
    for &source in sources {
        for split in 0..=source.len() {
            let (mut a, mut b) = pair(20);
            feed(&mut a, &mut b, &source[..split]);
            feed(&mut a, &mut b, &source[split..]);
            feed(&mut a, &mut b, b"\xacX\x1b[2b\x1b[6n");
        }
        chunks(b"", source, 20, &[1, 5, source.len()]);
    }
}

#[test]
fn modes_margins_and_character_sets_keep_the_existing_writer() {
    let source = "中文ab👩🏽‍💻e\u{301}lqk\r\n日本語\x1b[2b";
    for cols in [1, 2, 5] {
        for setup in [
            b"".as_slice(),
            b"\x1b[?7l",
            b"\x1b[4h",
            b"\x1b(0",
            b"\x1b)0\x0e",
            b"\x1b[1;5H",
            b"\x1b[?1049h",
        ] {
            chunks(setup, source.as_bytes(), cols, &[1, 5, source.len()]);
        }
    }
}

#[test]
fn bounded_cluster_replacement_and_capacity_seams_keep_scalar_state() {
    for bytes in [255, 256, 257] {
        let base = if bytes % 2 == 0 { "é" } else { "e" };
        let cluster = base.to_string() + &"\u{301}".repeat((bytes - base.len()) / 2);
        let source = format!("{}\u{301}X中文", cluster);
        chunks(
            b"",
            source.as_bytes(),
            24,
            &[1, 5, 255, 256, 512, source.len()],
        );
    }
    for prefix in ["é".repeat(254), "中".repeat(170)] {
        let source = format!("{prefix}🇦🇧🇨🇩🇪中文{}", "\u{301}".repeat(300));
        chunks(
            "🇦".as_bytes(),
            source.as_bytes(),
            24,
            &[5, 511, 512, 513, source.len()],
        );
    }
    let source = "e".to_string() + &"\u{301}".repeat(4096) + "X中文";
    chunks(b"", source.as_bytes(), 24, &[127, 512, source.len()]);
}

#[test]
fn exhausted_grapheme_pool_and_collection_threshold_keep_scalar_behavior() {
    let (mut a, mut b) = pair(24);
    for terminal in [&mut a, &mut b] {
        let mut exhausted = false;
        for index in 0..20_000 {
            let text = format!(
                "x{}{}{}",
                char::from_u32(0x300 + index % 112).unwrap(),
                char::from_u32(0x300 + index / 112 % 112).unwrap(),
                char::from_u32(0x300 + index / (112 * 112)).unwrap()
            );
            if terminal.state.graphemes.intern(&text).is_none() {
                exhausted = true;
                break;
            }
        }
        assert!(exhausted);
    }
    feed(&mut a, &mut b, "e\u{301}中文".as_bytes());
    let source = "中".repeat(4100) + "e\u{301}尾部";
    feed(&mut a, &mut b, source.as_bytes());
    feed(&mut a, &mut b, b"\x1b[2b\x1b[6n");
}
