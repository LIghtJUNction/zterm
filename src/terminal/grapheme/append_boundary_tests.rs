use super::{appended_scalar_extends, confirmed_append_boundary};
use crate::Terminal;
use alloc::string::{String, ToString};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn assert_oracle(prefix: &str, scalar: char) {
    assert!(prefix.is_empty() || prefix.graphemes(true).count() == 1);
    let mut text = prefix.to_string();
    text.push(scalar);
    if confirmed_append_boundary(prefix, scalar) {
        assert_ne!(
            text.graphemes(true).count(),
            1,
            "chunked boundary split one cluster: prefix={prefix:?}, scalar={scalar:?}"
        );
    }
    assert_eq!(
        appended_scalar_extends(&text, prefix.len()),
        text.graphemes(true).count() == 1,
        "prefix={prefix:?}, scalar={scalar:?}"
    );
}

#[test]
fn append_boundary_matches_complete_segmentation_contexts() {
    let samples = [
        "\r\n",
        "\0",
        "a",
        "终",
        "Ελληνικά",
        "日本語",
        "안녕하세요",
        "e\u{301}\u{308}",
        "\u{600}\u{605}A",
        "\u{600}\r\n",
        "\u{915}\u{94d}\u{915}",
        "\u{915}\u{94d}\u{301}\u{915}",
        "\u{915}\u{301}\u{915}",
        "\u{915}\u{93e}",
        "\u{1100}\u{1161}\u{11a8}",
        "\u{ac00}\u{11a8}",
        "\u{ac01}\u{1161}",
        "🇦🇧🇨🇩🇪",
        "👩\u{301}‍💻",
        "a‍💻",
        "👩‍\u{301}💻",
        "👨‍👩‍👧‍👦",
        "👩🏽‍💻",
        "1️⃣",
        "❤️",
        "🏳️‍🌈",
        "🏴\u{e0067}\u{e0062}\u{e007f}",
    ];
    let next = [
        '\0',
        '\r',
        '\n',
        'a',
        ' ',
        '#',
        '终',
        'Ε',
        '안',
        '\u{301}',
        '\u{308}',
        '\u{200d}',
        '\u{200c}',
        '\u{600}',
        '\u{605}',
        '\u{915}',
        '\u{94d}',
        '\u{93e}',
        '\u{1100}',
        '\u{1161}',
        '\u{11a8}',
        '\u{ac00}',
        '\u{ac01}',
        '🇦',
        '🇧',
        '👩',
        '💻',
        '🏽',
        '\u{fe0e}',
        '\u{fe0f}',
        '\u{20e3}',
        '\u{e0067}',
    ];
    for sample in samples {
        for cluster in sample.graphemes(true) {
            for end in cluster
                .char_indices()
                .map(|(i, _)| i)
                .chain(core::iter::once(cluster.len()))
            {
                for scalar in next {
                    assert_oracle(&cluster[..end], scalar);
                }
            }
        }
    }
}

#[test]
fn append_boundary_matches_oracle_at_cluster_byte_budget() {
    for base in ["e", "é"] {
        let mut prefix = base.to_string();
        while prefix.len() <= crate::MAX_GRAPHEME_BYTES {
            for scalar in ['\u{301}', '\u{20dd}', '\u{200d}', '👩', 'X'] {
                assert_oracle(&prefix, scalar);
            }
            prefix.push('\u{301}');
        }
    }
}

#[test]
fn contextual_clusters_keep_text_width_and_every_pty_split() {
    for source in [
        "🇦🇧🇨🇩🇪",
        "\u{1100}\u{1161}\u{11a8}\u{1102}\u{1161}",
        "\u{915}\u{94d}\u{937}\u{915}\u{93e}",
        "👩\u{301}‍💻a‍💻👩‍\u{301}💻",
    ] {
        for split in 0..=source.len() {
            let mut terminal = Terminal::new(32, 2, 8);
            terminal.feed(&source.as_bytes()[..split]);
            terminal.feed(&source.as_bytes()[split..]);
            let mut column = 0;
            for cluster in source.graphemes(true) {
                let cell = terminal.cell(column, 0);
                assert_eq!(
                    terminal.cell_chars(cell).collect::<String>(),
                    cluster,
                    "split={split}"
                );
                let width = cluster.width().clamp(1, 2);
                assert_eq!(usize::from(cell.width), width);
                column += width;
            }
            assert_eq!(terminal.cursor(), (column, 0));
            terminal.feed(b"X");
            assert_eq!(terminal.cell(column, 0).ch, 'X');
        }
    }
}

#[test]
fn exact_cluster_budget_keeps_text_then_uses_existing_overflow_replacement() {
    for base in ["e", "é"] {
        let cluster =
            base.to_string() + &"\u{301}".repeat((crate::MAX_GRAPHEME_BYTES - base.len()) / 2);
        let mut terminal = Terminal::new(8, 2, 8);
        terminal.feed(cluster.as_bytes());
        assert_eq!(
            terminal.cell_chars(terminal.cell(0, 0)).collect::<String>(),
            cluster
        );
        terminal.feed("\u{301}".as_bytes());
        assert_eq!(
            terminal.cell_chars(terminal.cell(0, 0)).collect::<String>(),
            "\u{fffd}"
        );
        terminal.feed(b"X");
        assert_eq!(terminal.cell(1, 0).ch, 'X');
    }
}

#[test]
fn chunked_boundary_handles_scalar_and_retained_prefixes_without_false_splits() {
    assert!(!confirmed_append_boundary("", 'a'));
    for (prefix, scalar) in [
        ("终", '端'),
        ("é", 'X'),
        ("e\u{301}", '终'),
        ("🇦🇧", '🇨'),
        ("👩\u{301}‍💻", 'X'),
    ] {
        assert_oracle(prefix, scalar);
        assert!(confirmed_append_boundary(prefix, scalar));
        let text = alloc::format!("{prefix}{scalar}");
        for split in 0..=text.len() {
            let mut terminal = Terminal::new(16, 2, 8);
            terminal.feed(&text.as_bytes()[..split]);
            terminal.feed(&text.as_bytes()[split..]);
            assert_eq!(
                terminal.cell_chars(terminal.cell(0, 0)).collect::<String>(),
                prefix
            );
            let column = prefix.width().clamp(1, 2);
            assert_eq!(terminal.cell(column, 0).ch, scalar);
            assert_eq!(terminal.cursor(), (column + scalar.to_string().width(), 0));
        }
    }
}

#[test]
fn full_cluster_budget_accepts_a_new_boundary_without_overflow() {
    for base in ["e", "é"] {
        let prefix =
            base.to_string() + &"\u{301}".repeat((crate::MAX_GRAPHEME_BYTES - base.len()) / 2);
        for scalar in ['X', '终', '👩'] {
            assert!(confirmed_append_boundary(&prefix, scalar));
            assert_oracle(&prefix, scalar);
            let mut terminal = Terminal::new(8, 2, 8);
            terminal.feed(prefix.as_bytes());
            terminal.feed(scalar.encode_utf8(&mut [0; 4]).as_bytes());
            assert_eq!(
                terminal.cell_chars(terminal.cell(0, 0)).collect::<String>(),
                prefix
            );
            assert_eq!(terminal.cell(1, 0).ch, scalar);
        }
    }
}
